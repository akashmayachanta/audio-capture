//! Puts dictated text into the focused field: clipboard + Ctrl+V when that loses
//! nothing, otherwise typed with Unicode key events.
//!
//! Overwriting the clipboard destroys whatever else was on it (an image, copied
//! files, rich text), and restoring only the text would not bring that back. So
//! the clipboard is used only when it held plain text or nothing; otherwise the
//! text is typed and the clipboard is left alone.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
};

const CF_TEXT: u32 = 1;
const CF_OEMTEXT: u32 = 7;
const CF_UNICODETEXT: u32 = 13;
const CF_LOCALE: u32 = 16;
/// Formats Windows keeps alongside plain text (it synthesizes them from `CF_UNICODETEXT`).
const TEXT_FORMATS: [u32; 4] = [CF_TEXT, CF_OEMTEXT, CF_UNICODETEXT, CF_LOCALE];

/// How long the dictation key and modifiers may stay down before the paste gives up waiting.
const KEYS_UP_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long the pasted text stays on the clipboard before the old text returns.
const RESTORE_AFTER: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Previous {
    Text(String),
    Empty,
    /// Anything else (or text we could not read): must not be overwritten.
    NonText,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restore {
    Text(String),
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    /// Put the text on the clipboard, press Ctrl+V, then restore.
    Clipboard(Restore),
    /// Type it with Unicode key events; the clipboard is not touched.
    Type,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteOutcome {
    Pasted,
    /// The text is on the clipboard but could not be pasted (an elevated window, keys still held).
    LeftOnClipboard,
}

/// What the clipboard holds, from its format list and (when there is `CF_UNICODETEXT`) its text.
fn classify(formats: &[u32], text: Option<String>) -> Previous {
    if formats.is_empty() {
        return Previous::Empty;
    }
    if formats.iter().any(|f| !TEXT_FORMATS.contains(f)) {
        return Previous::NonText;
    }
    text.map_or(Previous::NonText, Previous::Text)
}

fn method_for(previous: Previous) -> Method {
    match previous {
        Previous::Text(t) => Method::Clipboard(Restore::Text(t)),
        Previous::Empty => Method::Clipboard(Restore::Clear),
        Previous::NonText => Method::Type,
    }
}

/// UTF-16 units to type; newlines become spaces (a dictation is one line).
fn typing_units(text: &str) -> Vec<u16> {
    text.replace("\r\n", " ").replace(['\n', '\r'], " ").encode_utf16().collect()
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

fn read_previous() -> Result<Previous> {
    let _clipboard = Clipboard::open()?;
    let mut formats = Vec::new();
    let mut f = unsafe { EnumClipboardFormats(0) };
    while f != 0 {
        formats.push(f);
        f = unsafe { EnumClipboardFormats(f) };
    }
    let text = if formats.contains(&CF_UNICODETEXT) { read_unicode_text() } else { None };
    Ok(classify(&formats, text))
}

/// The clipboard's `CF_UNICODETEXT`; the clipboard must be open.
fn read_unicode_text() -> Option<String> {
    unsafe {
        let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let max = GlobalSize(hglobal) / 2;
        let units = std::slice::from_raw_parts(ptr, max);
        let len = units.iter().position(|&u| u == 0).unwrap_or(max);
        let text = String::from_utf16_lossy(&units[..len]);
        let _ = GlobalUnlock(hglobal);
        Some(text)
    }
}

/// Moves `bytes` into a movable global block and hands it to the clipboard as `format`.
fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> Result<()> {
    unsafe {
        let block = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
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

fn restore(what: Restore) -> Result<()> {
    match what {
        Restore::Text(t) => set_text(&t),
        Restore::Clear => {
            let _clipboard = Clipboard::open()?;
            unsafe { EmptyClipboard()? };
            Ok(())
        }
    }
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

fn type_text(text: &str) -> bool {
    let events: Vec<INPUT> = typing_units(text)
        .into_iter()
        .flat_map(|unit| {
            [
                key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE),
                key_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect();
    events.is_empty() || send(&events)
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
    match method_for(read_previous()?) {
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
            // Only put the old text back if nobody copied something in the meantime.
            if unsafe { GetClipboardSequenceNumber() } == ours {
                restore(old)?;
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
        let _clipboard = Clipboard::open().expect("open the clipboard");
        unsafe { EmptyClipboard().unwrap() };
        set_clipboard_bytes(format, b"precious").unwrap();
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
            assert_eq!(got, text);
            after();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CF_TEXT: u32 = 1;
    const CF_BITMAP: u32 = 2;
    const CF_OEMTEXT: u32 = 7;
    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;
    const CF_LOCALE: u32 = 16;

    fn text(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn an_empty_clipboard_is_empty() {
        assert_eq!(classify(&[], None), Previous::Empty);
    }

    #[test]
    fn plain_text_in_all_its_text_formats_is_text() {
        let formats = [CF_UNICODETEXT, CF_TEXT, CF_OEMTEXT, CF_LOCALE];
        assert_eq!(classify(&formats, text("hello")), Previous::Text("hello".into()));
        assert_eq!(classify(&[CF_UNICODETEXT], text("")), Previous::Text(String::new()));
    }

    #[test]
    fn anything_besides_text_makes_it_non_text() {
        // Rich text from Word or a browser, an image, files: overwriting would lose them.
        assert_eq!(classify(&[CF_UNICODETEXT, 49_300 /* CF_HTML */], text("x")), Previous::NonText);
        assert_eq!(classify(&[CF_BITMAP], None), Previous::NonText);
        assert_eq!(classify(&[CF_HDROP], None), Previous::NonText);
    }

    #[test]
    fn text_formats_without_readable_text_are_not_restorable() {
        assert_eq!(classify(&[CF_UNICODETEXT], None), Previous::NonText);
    }

    #[test]
    fn the_method_depends_on_what_the_clipboard_holds() {
        assert_eq!(method_for(Previous::Text("old".into())), Method::Clipboard(Restore::Text("old".into())));
        assert_eq!(method_for(Previous::Empty), Method::Clipboard(Restore::Clear));
        assert_eq!(method_for(Previous::NonText), Method::Type);
    }

    #[test]
    fn typing_units_are_utf16_with_newlines_as_spaces() {
        assert_eq!(typing_units("hi"), vec![0x68, 0x69]);
        assert_eq!(typing_units("a\r\nb\nc"), vec![0x61, 0x20, 0x62, 0x20, 0x63]);
        // Outside the BMP: a surrogate pair, as SendInput wants it.
        assert_eq!(typing_units("😀"), vec![0xD83D, 0xDE00]);
        assert_eq!(typing_units("héllo")[1], 0xE9);
    }

    #[test]
    #[ignore = "needs an interactive desktop that lets this process take the foreground"]
    fn paste_roundtrip_in_a_scratch_edit_box() {
        live::roundtrip("héllo wörld");
    }

    #[test]
    #[ignore = "needs an interactive desktop; overwrites the clipboard"]
    fn a_non_text_clipboard_is_typed_over_and_left_alone() {
        let format = live::register(w!("MeetRecTestPrivateFormat"));
        live::roundtrip_with(
            "typed not pasted",
            || live::set_private_format(format),
            || {
                let formats = live::clipboard_formats();
                assert!(formats.contains(&format), "the private format must survive: {formats:?}");
                assert!(!formats.contains(&CF_UNICODETEXT), "no text may have been put on the clipboard: {formats:?}");
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
