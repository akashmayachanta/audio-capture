//! The dictation overlay (a round microphone badge): a small always-on-top, click-through window at the
//! bottom of the screen the cursor is on. It never takes focus (so the field
//! being dictated into keeps it) and is drawn with `UpdateLayeredWindow` from
//! the software-rendered `overlay_scene`. Plain Win32: the tray process never
//! loads the GUI stack. Lives on the tray's thread, which pumps its messages.

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDeviceCaps, GetMonitorInfoW, MonitorFromPoint,
    ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, LOGPIXELSX, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetCursorPos, SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE, SW_SHOWNA, ULW_ALPHA, WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::overlay_scene::{self as scene, Phase, Scene, Smoother};

const FADE_IN: Duration = Duration::from_millis(120);
const FADE_OUT: Duration = Duration::from_millis(160);
/// Gap between the overlay and the bottom of the work area, in design pixels.
const BOTTOM_GAP: f32 = 24.0;

pub struct Overlay {
    hwnd: HWND,
    mem_dc: HDC,
    bitmap: HBITMAP,
    bits: *mut u32,
    size: (usize, usize),
    /// Top-left of the window on screen.
    origin: Option<POINT>,
    phase: Phase,
    smoother: Smoother,
    level: f32,
    dark: bool,
    /// Wanted on screen (false = fading out).
    wanted: bool,
    /// The window is shown (it stays shown while it fades out).
    shown: bool,
    fade: f32,
    shown_at: Instant,
    last_tick: Instant,
}

impl Overlay {
    /// Creates the (hidden) window.
    pub fn new() -> Result<Overlay> {
        let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT;
        // "STATIC" is a built-in class: no window class of our own to register.
        let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(ex.0), w!("STATIC"), w!("AudCap dictation"), WS_POPUP, 0, 0, 1, 1, None, None, None, None)? };
        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            unsafe { DestroyWindow(hwnd).ok() };
            bail!("could not create a drawing surface for the overlay");
        }
        let now = Instant::now();
        Ok(Overlay {
            hwnd,
            mem_dc,
            bitmap: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            size: (0, 0),
            origin: None,
            phase: Phase::Listening,
            smoother: Smoother::default(),
            level: 0.0,
            dark: true,
            wanted: false,
            shown: false,
            fade: 0.0,
            shown_at: now,
            last_tick: now,
        })
    }

    /// Shows the overlay (or switches its phase while it is up).
    pub fn show(&mut self, phase: Phase) {
        self.phase = phase;
        self.wanted = true;
        if !self.shown {
            self.shown = true;
            self.fade = 0.0;
            self.shown_at = Instant::now();
            self.last_tick = self.shown_at;
            self.dark = system_uses_dark_theme();
            self.place();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
        }
    }

    /// The raw mic level, 0..1.
    pub fn set_level(&mut self, level: f32) {
        self.level = level;
    }

    /// Starts fading out; the window hides itself when done.
    pub fn hide(&mut self) {
        self.wanted = false;
    }

    /// True while anything is on screen: the app loop should then wake at ~60 fps.
    pub fn is_animating(&self) -> bool {
        self.shown
    }

    /// Advances the animation and redraws. Cheap when hidden.
    pub fn tick(&mut self) {
        if !self.shown {
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        let step = dt / if self.wanted { FADE_IN } else { FADE_OUT }.as_secs_f32();
        self.fade = if self.wanted { (self.fade + step).min(1.0) } else { (self.fade - step).max(0.0) };
        if !self.wanted && self.fade <= 0.0 {
            self.shown = false;
            self.smoother = Smoother::default();
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        let level = self.smoother.push(self.level, dt);
        let s = Scene { phase: self.phase, level, t: now.duration_since(self.shown_at).as_secs_f32(), dark: self.dark, fade: self.fade };
        if let Err(e) = self.draw(&s) {
            tracing::warn!("overlay: {e:#}");
            self.shown = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Bottom centre of the work area of the monitor under the cursor.
    fn place(&mut self) {
        unsafe {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let scale = display_scale();
            let (w, h) = ((scene::W as f32 * scale).round() as i32, (scene::H as f32 * scale).round() as i32);
            let work = info.rcWork;
            let x = work.left + (work.right - work.left - w) / 2;
            let y = work.bottom - h - (BOTTOM_GAP * scale).round() as i32;
            self.ensure_surface(w as usize, h as usize);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
            self.origin = Some(POINT { x, y });
        }
    }

    fn draw(&mut self, s: &Scene) -> Result<()> {
        let (w, h) = self.size;
        if self.bits.is_null() || w == 0 || h == 0 {
            bail!("no drawing surface");
        }
        // The DIB section is `w * h` u32s, owned by `self.bitmap`.
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.bits, w * h) };
        scene::render(s, w, h, pixels);
        let origin = self.origin.unwrap_or_default();
        let size = SIZE { cx: w as i32, cy: h as i32 };
        let source = POINT::default();
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        unsafe {
            let screen = GetDC(None);
            let result = UpdateLayeredWindow(self.hwnd, Some(screen), Some(&origin), Some(&size), Some(self.mem_dc), Some(&source), COLORREF(0), Some(&blend), ULW_ALPHA);
            ReleaseDC(None, screen);
            result?;
        }
        Ok(())
    }

    /// (Re)creates the 32-bit top-down DIB the scene is drawn into.
    fn ensure_surface(&mut self, w: usize, h: usize) {
        if self.size == (w, h) && !self.bits.is_null() {
            return;
        }
        unsafe {
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                self.bitmap = HBITMAP::default();
                self.bits = std::ptr::null_mut();
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            match CreateDIBSection(Some(self.mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) if !bits.is_null() => {
                    SelectObject(self.mem_dc, HGDIOBJ(bitmap.0));
                    self.bitmap = bitmap;
                    self.bits = bits as *mut u32;
                    self.size = (w, h);
                }
                _ => tracing::warn!("overlay: could not create the drawing surface"),
            }
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            }
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

/// Display scale of the primary screen (1.0 at 96 dpi).
fn display_scale() -> f32 {
    unsafe {
        let dc = GetDC(None);
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX);
        ReleaseDC(None, dc);
        if dpi > 0 { dpi as f32 / 96.0 } else { 1.0 }
    }
}

/// Apps follow the user's "app mode" setting.
fn system_uses_dark_theme() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|light| light == 0)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays every phase for two seconds each so it can be looked at (screenshots).
    /// Prints `PHASE <name>` as each starts.
    #[test]
    #[ignore = "puts a window on the screen for ~10 s"]
    fn demo_every_phase() {
        let mut overlay = Overlay::new().unwrap();
        let start = Instant::now();
        for (name, phase) in [("listening", Phase::Listening), ("starting", Phase::Starting), ("transcribing", Phase::Transcribing), ("error", Phase::Error)] {
            println!("PHASE {name}");
            overlay.show(phase);
            let phase_start = Instant::now();
            while phase_start.elapsed() < Duration::from_secs(2) {
                // A voice-like level that swells and pauses.
                let t = start.elapsed().as_secs_f32();
                overlay.set_level(((t * 3.0).sin() * 0.5 + 0.5) * if (t * 0.9).sin() > -0.3 { 1.0 } else { 0.1 });
                overlay.tick();
                pump();
                std::thread::sleep(Duration::from_millis(16));
            }
        }
        overlay.hide();
        for _ in 0..30 {
            overlay.tick();
            pump();
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(!overlay.is_animating(), "it hides itself after fading out");
    }

    fn pump() {
        use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
