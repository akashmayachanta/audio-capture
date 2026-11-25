//! Puts dictated text into the focused field with the clipboard and Ctrl+V, then puts
//! back whatever the clipboard held before.
//!
//! Before pasting, every format on the clipboard is copied out (plain and rich text,
//! HTML, copied files, images as DIBs, and the tags apps add), and afterwards all of it
//! is put back unchanged. Typing the text with Unicode key events is only the fallback
//! for a clipboard that can't be copied that way (GDI objects with no memory form, such
//! as a lone enhanced metafile, app-private handles, or more than `MAX_SNAPSHOT_BYTES`):
//! some apps (Windows 11 Notepad) drop and repeat characters that are typed this way.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};

const CF_BITMAP: u32 = 2;
const CF_DIB: u32 = 8;
const CF_PALETTE: u32 = 9;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;
/// Standard formats whose data is a memory block: TEXT, SYLK, DIF, TIFF, OEMTEXT, DIB,
/// PENDATA, RIFF, WAVE, UNICODETEXT, HDROP (copied files), LOCALE, DIBV5.
const MEMORY_FORMATS: [u32; 13] = [1, 4, 5, 6, 7, 8, 10, 11, 12, 13, 15, 16, 17];

/// The most the clipboard may hold for it to be copied and put back (a large image).
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Characters per `SendInput` batch when typing, and the pause after each batch. One big
/// burst makes some apps (Windows 11 Notepad) drop and repeat characters.
const TYPE_BATCH_CHARS: usize = 4;
const TYPE_BATCH_PAUSE: Duration = Duration::from_millis(8);

/// How long the dictation key and modifiers may stay down before the paste gives up waiting.
const KEYS_UP_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long the pasted text stays on the clipboard before the old text returns.
const RESTORE_AFTER: Duration = Duration::from_millis(500);

/// What the clipboard held before the paste: each format and its bytes, in the order
/// Windows listed them. Empty when the clipboard was empty.
pub type Snapshot = Vec<(u32, Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// Put the text on the clipboard, press Ctrl+V, then put the snapshot back.
    Clipboard(Snapshot),
    /// Type it with Unicode key events; the clipboard is not touched.
    Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteOutcome {
    Pasted,
    /// The text is on the clipboard but could not be pasted (an elevated window, keys still held).
    LeftOnClipboard,
}

/// A memory block, copyable byte for byte. Registered formats (0xC000 and up: HTML,
/// RTF, app tags) always are.
fn is_memory_format(f: u32) -> bool {
    f >= 0xC000 || MEMORY_FORMATS.contains(&f)
}

/// The formats to copy out, or None when one can't be copied. A bitmap or palette
/// handle needs no copy when a DIB is there too: Windows makes them from it again.
fn snapshot_plan(formats: &[u32]) -> Option<Vec<u32>> {
    let has_dib = formats.contains(&CF_DIB) || formats.contains(&CF_DIBV5);
    let mut plan = Vec::new();
    for &f in formats {
        if is_memory_format(f) {
            plan.push(f);
        } else if !(has_dib && (f == CF_BITMAP || f == CF_PALETTE)) {
            return None;
        }
    }
    Some(plan)
}

struct Clipboard;

impl Clipboard {
    /// Another program may hold the clipboard for a moment: retry briefly.
    fn open() -> Result<Clipboard> {
        for _ in 0..30 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Clipboard);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        bail!("the clipboard is busy")
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// The formats on the clipboard, in Windows' order; the clipboard must be open.
fn list_formats() -> Vec<u32> {
    let mut formats = Vec::new();
    let mut f = unsafe { EnumClipboardFormats(0) };
    while f != 0 {
        formats.push(f);
        f = unsafe { EnumClipboardFormats(f) };
    }
    formats
}

/// Copies everything off the clipboard, or None when some of it can't be copied.
fn read_previous() -> Result<Option<Snapshot>> {
    let _clipboard = Clipboard::open()?;
    let Some(plan) = snapshot_plan(&list_formats()) else {
        return Ok(None);
    };
    let mut snapshot = Vec::with_capacity(plan.len());
    let mut total = 0;
    for format in plan {
        let Some(bytes) = read_bytes(format) else {
            return Ok(None);
        };
        total += bytes.len();
        if total > MAX_SNAPSHOT_BYTES {
            return Ok(None);
        }
        snapshot.push((format, bytes));
    }
    Ok(Some(snapshot))
}

/// The bytes of a memory-block format; the clipboard must be open.
fn read_bytes(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let size = GlobalSize(hglobal);
        if size == 0 {
            return Some(Vec::new());
        }
        let ptr = GlobalLock(hglobal) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(hglobal);
        Some(bytes)
    }
}

/// Moves `bytes` into a movable global block and hands it to the clipboard as `format`.
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let ptr = GlobalLock(block) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(block));
            bail!("could not lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(block);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(block.0))) {
            let _ = GlobalFree(Some(block)); // the clipboard did not take it
            return Err(e.into());
        }
    }
    Ok(())
}

/// Replaces the clipboard with `text`. Marked so Windows' clipboard history and
/// cloud clipboard skip it: dictated text is nobody's clipboard history.
fn set_text(text: &str) -> Result<()> {
    let _clipboard = Clipboard::open()?;
    unsafe { EmptyClipboard()? };
    let bytes: Vec<u8> = text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    set_clipboard_bytes(CF_UNICODETEXT, &bytes)?;
    for name in [w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard"), w!("ExcludeClipboardContentFromMonitorProcessing")] {
        let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.0)) };
        if format != 0 {
            let _ = set_clipboard_bytes(format, &0u32.to_le_bytes()); // best effort
        }
    }
    Ok(())
}

/// Puts the snapshot back as it was (an empty snapshot leaves the clipboard empty).
fn restore(snapshot: &Snapshot) -> Result<()> {
    let _clipboard = Clipboard::open()?;
    unsafe { EmptyClipboard()? };
    for (format, bytes) in snapshot {
        if let Err(e) = set_clipboard_bytes(*format, bytes) {
            tracing::warn!("could not put clipboard format {format} back: {e:#}");
        }
    }
    Ok(())
}

fn key_input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// True when every event went in (false: blocked, e.g. by an elevated foreground window).
fn send(inputs: &[INPUT]) -> bool {
    unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
}

fn send_ctrl_v() -> bool {
    let none = KEYBD_EVENT_FLAGS(0);
    send(&[
        key_input(VK_CONTROL, 0, none),
        key_input(VK_V, 0, none),
        key_input(VK_V, 0, KEYEVENTF_KEYUP),
        key_input(VK_CONTROL, 0, KEYEVENTF_KEYUP),
    ])
}

/// Presses and releases an unassigned key. Sent while Alt or Win is held, it stops
/// Windows treating the later release as a lone tap that opens a menu bar or the Start
/// menu. (Raw Input ignores this key: `KeyId::from_raw` drops VK 0xFF.)
pub fn send_menu_mask_key() -> bool {
    send(&[
        key_input(VIRTUAL_KEY(0xFF), 0, KEYBD_EVENT_FLAGS(0)),
        key_input(VIRTUAL_KEY(0xFF), 0, KEYEVENTF_KEYUP),
    ])
}

/// The text as batches of UTF-16 units, `per_batch` characters each; newlines become
/// spaces (a dictation is one line) and a surrogate pair is never split across batches.
fn typing_batches(text: &str, per_batch: usize) -> Vec<Vec<u16>> {
    let flat = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
    let chars: Vec<char> = flat.chars().collect();
    chars
        .chunks(per_batch.max(1))
        .map(|c| c.iter().collect::<String>().encode_utf16().collect())
        .collect()
}

fn type_text(text: &str) -> bool {
    for batch in typing_batches(text, TYPE_BATCH_CHARS) {
        let events: Vec<INPUT> = batch
            .into_iter()
            .flat_map(|unit| {
                [
                    key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
                    key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                ]
            })
            .collect();
        if !send(&events) {
            return false;
        }
        std::thread::sleep(TYPE_BATCH_PAUSE);
    }
    true
}

/// Waits until Ctrl, Alt, Shift and Win are physically up (so the target sees a plain
/// Ctrl+V, not Ctrl+Alt+V). False if they are still down at the timeout.
fn wait_keys_up(timeout: Duration) -> bool {
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0;
    let deadline = Instant::now() + timeout;
    loop {
        if ![VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].into_iter().any(down) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Puts `text` into the focused field of the foreground window.
pub fn paste_text(text: &str) -> Result<PasteOutcome> {
    if text.is_empty() {
        return Ok(PasteOutcome::Pasted);
    }
    let keys_up = wait_keys_up(KEYS_UP_TIMEOUT);
    let method = match read_previous()? {
        Some(snapshot) => Method::Clipboard(snapshot),
        None => Method::Type,
    };
    tracing::info!(
        "dictation paste: {} chars via {}{}",
        text.chars().count(),
        match &method {
            Method::Type => "typing (the clipboard can't be copied and put back)".to_string(),
            Method::Clipboard(s) => format!("clipboard + Ctrl+V, {} clipboard formats kept", s.len()),
        },
        if keys_up { "" } else { ", modifier keys still down" }
    );
    match method {
        Method::Type => {
            if !keys_up {
                bail!("modifier keys are still held down");
            }
            if type_text(text) {
                Ok(PasteOutcome::Pasted)
            } else {
                Err(anyhow!("could not type into the active window (is it running as administrator?)"))
            }
        }
        Method::Clipboard(old) => {
            set_text(text)?;
            let ours = unsafe { GetClipboardSequenceNumber() };
            if !keys_up || !send_ctrl_v() {
                return Ok(PasteOutcome::LeftOnClipboard);
            }
            std::thread::sleep(RESTORE_AFTER);
            // Only put the old contents back if nobody copied something in the meantime.
            if unsafe { GetClipboardSequenceNumber() } == ours {
                restore(&old)?;
            }
            Ok(PasteOutcome::Pasted)
        }
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
        PeekMessageW, SetForegroundWindow, TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE, WINDOW_STYLE,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    const ES_MULTILINE: u32 = 0x0004;

    /// Pastes into a real EDIT control that has the focus and checks what it received.
    pub fn roundtrip(text: &str) {
        roundtrip_with(text, || {}, || {});
    }

    /// The format ids on the clipboard right now.
    pub fn clipboard_formats() -> Vec<u32> {
        let _clipboard = Clipboard::open().expect("open the clipboard");
        let mut out = Vec::new();
        let mut f = unsafe { EnumClipboardFormats(0) };
        while f != 0 {
            out.push(f);
            f = unsafe { EnumClipboardFormats(f) };
        }
        out
    }

    pub fn register(name: PCWSTR) -> u32 {
        unsafe { RegisterClipboardFormatW(name) }
    }

    /// Replaces the clipboard with a single private format (stands in for an image or files).
    pub fn set_private_format(format: u32) {
        set_formats(&[(format, b"precious")]);
    }

    /// Replaces the clipboard with these formats.
    pub fn set_formats(formats: &[(u32, &[u8])]) {
        let _clipboard = Clipboard::open().expect("open the clipboard");
        unsafe { EmptyClipboard().unwrap() };
        for (format, bytes) in formats {
            set_clipboard_bytes(*format, bytes).unwrap();
        }
    }

    /// One format's bytes from the clipboard (empty when it isn't there).
    pub fn read_format(format: u32) -> Vec<u8> {
        let _clipboard = Clipboard::open().expect("open the clipboard");
        read_bytes(format).unwrap_or_default()
    }

    pub fn roundtrip_with(text: &str, prepare: impl FnOnce(), after: impl FnOnce()) {
        prepare();
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE | WINDOW_STYLE(ES_MULTILINE),
                100,
                100,
                500,
                200,
                None,
                None,
                None,
                None,
            )
            .expect("create the edit box");
            // Windows lets only the foreground thread hand the foreground on: join its input queue.
            let current = GetCurrentThreadId();
            let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
            let attached = foreground != 0 && foreground != current && AttachThreadInput(current, foreground, true).as_bool();
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            if attached {
                let _ = AttachThreadInput(current, foreground, false);
            }
            // Never paste into somebody's real window if the scratch box didn't get the foreground.
            let t = Instant::now();
            while GetForegroundWindow() != hwnd {
                if t.elapsed() > Duration::from_secs(2) {
                    let _ = DestroyWindow(hwnd);
                    panic!("the scratch edit box could not take the foreground; nothing was pasted");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let owned = text.to_string();
            let worker = std::thread::spawn(move || paste_text(&owned));
            let mut msg = MSG::default();
            while !worker.is_finished() {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let outcome = worker.join().unwrap().expect("paste");
            // Let the control process the last key messages.
            for _ in 0..40 {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf) as usize;
            let got = String::from_utf16_lossy(&buf[..n]);
            let _ = DestroyWindow(hwnd);
            assert_eq!(outcome, PasteOutcome::Pasted);
            assert_eq!(got, text, "the edit box got {got:?} instead of {text:?}");
            after();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CF_TEXT: u32 = 1;
    const CF_METAFILEPICT: u32 = 3;
    const CF_OEMTEXT: u32 = 7;
    const CF_ENHMETAFILE: u32 = 14;
    const CF_HDROP: u32 = 15;
    const CF_LOCALE: u32 = 16;
    const CF_OWNERDISPLAY: u32 = 0x80;
    const CF_PRIVATEFIRST: u32 = 0x200;
    const HTML: u32 = 49_460; // a registered format, as a browser puts it there

    #[test]
    fn an_empty_clipboard_is_copied_as_nothing() {
        assert_eq!(snapshot_plan(&[]), Some(vec![]));
    }

    #[test]
    fn text_html_files_and_app_tags_are_copied() {
        let browser = [HTML, CF_UNICODETEXT, 49_886, 49_410, CF_LOCALE, CF_TEXT, CF_OEMTEXT];
        assert_eq!(snapshot_plan(&browser), Some(browser.to_vec()));
        assert_eq!(snapshot_plan(&[CF_HDROP]), Some(vec![CF_HDROP]));
    }

    #[test]
    fn an_image_is_copied_as_its_dib() {
        // Windows lists a bitmap and palette made from the DIB; the DIB alone brings them back.
        assert_eq!(snapshot_plan(&[CF_DIB, CF_BITMAP, CF_PALETTE, CF_DIBV5]), Some(vec![CF_DIB, CF_DIBV5]));
    }

    #[test]
    fn handles_that_are_not_memory_make_it_uncopyable() {
        assert_eq!(snapshot_plan(&[CF_BITMAP]), None, "a bitmap with no DIB");
        assert_eq!(snapshot_plan(&[CF_ENHMETAFILE, CF_METAFILEPICT]), None);
        assert_eq!(snapshot_plan(&[CF_UNICODETEXT, CF_OWNERDISPLAY]), None);
        assert_eq!(snapshot_plan(&[CF_PRIVATEFIRST]), None);
    }

    #[test]
    fn typing_batches_keep_characters_whole() {
        assert_eq!(typing_batches("abcdefghi", 4), vec![vec![0x61, 0x62, 0x63, 0x64], vec![0x65, 0x66, 0x67, 0x68], vec![0x69]]);
        assert_eq!(typing_batches("a\r\nb", 4), vec![vec![0x61, 0x20, 0x62]]);
        // A surrogate pair stays in one batch.
        assert_eq!(typing_batches("x😀", 1), vec![vec![0x78], vec![0xD83D, 0xDE00]]);
        assert!(typing_batches("", 4).is_empty());
    }

    #[test]
    #[ignore = "needs an interactive desktop that lets this process take the foreground"]
    fn paste_roundtrip_in_a_scratch_edit_box() {
        live::roundtrip("héllo wörld");
    }

    #[test]
    #[ignore = "needs an interactive desktop; overwrites the clipboard"]
    fn a_registered_format_survives_the_paste() {
        let format = live::register(w!("AudCapTestPrivateFormat"));
        live::roundtrip_with(
            "pasted, then put back",
            || live::set_private_format(format),
            || {
                let formats = live::clipboard_formats();
                assert!(formats.contains(&format), "the private format must survive: {formats:?}");
                assert!(!formats.contains(&CF_UNICODETEXT), "the dictation must not stay on the clipboard: {formats:?}");
                assert_eq!(live::read_format(format), b"precious");
            },
        );
    }

    #[test]
    #[ignore = "needs an interactive desktop; overwrites the clipboard"]
    fn text_copied_from_a_browser_survives_the_paste() {
        let html = live::register(w!("HTML Format"));
        let page = b"Version:0.9\r\nStartHTML:0\r\n<b>copied from a page</b>".to_vec();
        let text: Vec<u8> = "copied from a page".encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let (page2, text2) = (page.clone(), text.clone());
        live::roundtrip_with(
            "dictated text",
            move || live::set_formats(&[(html, &page), (CF_UNICODETEXT, &text)]),
            move || {
                assert!(live::read_format(html).starts_with(&page2), "the HTML must come back");
                // GlobalSize may round a block up, so compare the start.
                assert!(live::read_format(CF_UNICODETEXT).starts_with(&text2), "the copied text must come back");
            },
        );
    }

    #[test]
    #[ignore = "overwrites the clipboard"]
    fn dictated_text_is_kept_out_of_clipboard_history() {
        set_text("secret words").unwrap();
        let formats = live::clipboard_formats();
        for name in [w!("CanIncludeInClipboardHistory"), w!("CanUploadToCloudClipboard"), w!("ExcludeClipboardContentFromMonitorProcessing")] {
            assert!(formats.contains(&live::register(name)), "missing exclusion format on {formats:?}");
        }
        assert!(formats.contains(&CF_UNICODETEXT));
    }
}
