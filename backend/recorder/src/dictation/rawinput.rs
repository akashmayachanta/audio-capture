//! Keyboard events for the dictation gestures via Raw Input: passive (it cannot
//! delay or swallow a key, unlike a low-level hook) and it sees lone modifiers,
//! which `RegisterHotKey` cannot. A thread with a message-only window receives
//! `WM_INPUT` for every key, in any app, and stamps each event with its arrival time.

use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, HWND_MESSAGE,
    MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_QUIT,
};

use super::gesture::KeyEvent;
use super::keys::KeyId;

/// `RAWKEYBOARD.Flags` bit: the key was released.
const RI_KEY_BREAK: u16 = 0x01;

/// A key event with the moment it arrived, so gesture timing does not depend on how
/// often the app loop gets round to reading it.
#[derive(Clone, Debug)]
pub struct Stamped {
    pub event: KeyEvent,
    pub at: Instant,
}

/// A `RAWKEYBOARD` record as a key event; `None` for records that are not a user key
/// (the fake AltGr Ctrl, overrun markers).
fn key_event(vkey: u16, make_code: u16, flags: u16) -> Option<KeyEvent> {
    let key = KeyId::from_raw(vkey, make_code, flags)?;
    Some(KeyEvent { key, down: flags & RI_KEY_BREAK == 0 })
}

/// Reads the `RAWINPUT` behind a `WM_INPUT` lparam.
unsafe fn read_key(lparam: LPARAM) -> Option<KeyEvent> {
    unsafe {
        let handle = HRAWINPUT(lparam.0 as *mut c_void);
        let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        GetRawInputData(handle, RID_INPUT, None, &mut size, header);
        // u64 storage: RAWINPUT needs 8-byte alignment.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        if size == 0 || GetRawInputData(handle, RID_INPUT, Some(buf.as_mut_ptr() as *mut c_void), &mut size, header) != size {
            return None;
        }
        let raw = &*(buf.as_ptr() as *const RAWINPUT);
        if raw.header.dwType != RIM_TYPEKEYBOARD.0 {
            return None;
        }
        let k = raw.data.keyboard;
        key_event(k.VKey, k.MakeCode, k.Flags)
    }
}

/// Listens for keys on its own thread until dropped.
pub struct RawKeys {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl RawKeys {
    pub fn start() -> Result<(RawKeys, Receiver<Stamped>)> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<u32, String>>();
        let thread = std::thread::Builder::new().name("raw-keys".into()).spawn(move || run(tx, ready_tx))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok((RawKeys { thread_id, thread: Some(thread) }, rx)),
            Ok(Err(e)) => bail!(e),
            Err(_) => Err(anyhow!("the keyboard listener did not start")),
        }
    }
}

impl Drop for RawKeys {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn device(hwnd: HWND, flags: windows::Win32::UI::Input::RAWINPUTDEVICE_FLAGS) -> RAWINPUTDEVICE {
    // Generic desktop (1), keyboard (6).
    RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: flags, hwndTarget: hwnd }
}

fn run(tx: Sender<Stamped>, ready: Sender<std::result::Result<u32, String>>) {
    unsafe {
        let hwnd = match CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("could not create the keyboard listener window: {e}")));
                return;
            }
        };
        let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
        if let Err(e) = RegisterRawInputDevices(&[device(hwnd, RIDEV_INPUTSINK)], size) {
            let _ = DestroyWindow(hwnd);
            let _ = ready.send(Err(format!("could not register for keyboard input: {e}")));
            return;
        }
        let _ = ready.send(Ok(GetCurrentThreadId()));
        let mut msg = MSG::default();
        // A WM_INPUT must still be dispatched (to the default handler) so Windows can free it.
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_INPUT {
                if let Some(event) = read_key(msg.lParam) {
                    if tx.send(Stamped { event, at: Instant::now() }).is_err() {
                        break;
                    }
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RegisterRawInputDevices(&[device(HWND::default(), RIDEV_REMOVE)], size);
        let _ = DestroyWindow(hwnd);
    }
}

/// The first key pressed within `timeout`, with a name for the Settings page; `None` if none was.
pub fn detect_next_key(timeout: Duration) -> Option<(KeyId, String)> {
    let (_keys, events) = RawKeys::start().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let stamped = events.recv_timeout(left).ok()?;
        if stamped.event.down {
            let name = stamped.event.key.display_name();
            return Some((stamped.event.key, name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const E0: u16 = 0x02;
    const BREAK: u16 = 0x01;

    fn key(s: &str) -> KeyId {
        KeyId::parse(s).unwrap()
    }

    #[test]
    fn a_make_code_is_a_key_down() {
        let ev = key_event(0x12, 0x38, E0).unwrap();
        assert_eq!(ev, KeyEvent { key: key("RightAlt"), down: true });
    }

    #[test]
    fn the_break_flag_is_a_key_up() {
        let ev = key_event(0x12, 0x38, E0 | BREAK).unwrap();
        assert_eq!(ev, KeyEvent { key: key("RightAlt"), down: false });
    }

    #[test]
    fn the_fake_ctrl_before_altgr_makes_no_event() {
        assert_eq!(key_event(0x11, 0x21D, 0), None);
        assert_eq!(key_event(0x11, 0x21D, BREAK), None);
    }

    #[test]
    fn escape_and_ordinary_keys_come_through() {
        assert_eq!(key_event(0x1B, 0x01, 0), Some(KeyEvent { key: key("Escape"), down: true }));
        assert_eq!(key_event(0x45, 0x12, BREAK), Some(KeyEvent { key: key("vk:0x45"), down: false }));
    }

    /// Presses and releases F24 (nothing uses it) after a short delay, on another thread.
    fn tap_f24_soon() -> std::thread::JoinHandle<()> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY,
        };
        fn press(up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0x87),
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            unsafe { SendInput(&[press(false), press(true)], std::mem::size_of::<INPUT>() as i32) };
        })
    }

    #[test]
    #[ignore = "injects an F24 key press into the foreground app"]
    fn sees_a_synthetic_key_press_and_detect_finds_it() {
        let (_keys, events) = RawKeys::start().unwrap();
        let injector = tap_f24_soon();
        let first = events.recv_timeout(Duration::from_secs(3)).expect("a key-down event");
        assert_eq!(first.event, KeyEvent { key: key("F24"), down: true });
        let second = events.recv_timeout(Duration::from_secs(3)).expect("a key-up event");
        assert_eq!(second.event, KeyEvent { key: key("F24"), down: false });
        assert!(second.at >= first.at);
        injector.join().unwrap();

        let injector = tap_f24_soon();
        let detected = detect_next_key(Duration::from_secs(3)).expect("Detect finds the key");
        assert_eq!(detected, (key("F24"), "F24".to_string()));
        injector.join().unwrap();
        assert_eq!(detect_next_key(Duration::from_millis(300)), None, "nothing pressed: no key");
    }
}
