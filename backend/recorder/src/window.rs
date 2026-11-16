//! `meetrec.exe --settings` / `--recordings`: a native window (tao) showing the
//! Svelte app in WebView2 (wry). The page and its API are served over the
//! `meetrec` custom protocol (`http://meetrec.localhost/`); see `web_api`.
//! Runs in its own short-lived process, so the tray never loads WebView2.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use windows::core::HSTRING;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

use crate::audio::device::{self, DeviceInfo};
use crate::web_api::{self, Host};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Recordings,
    Settings,
}

const APP_ORIGIN: &str = "http://meetrec.localhost/";

/// Only the app itself may load in the window (not a dropped file, not a link).
pub fn is_app_url(url: &str) -> bool {
    url.starts_with(APP_ORIGIN)
}

struct WindowHost {
    config_path: PathBuf,
    /// Held while the folder dialog is open, so a second Browse doesn't open a second dialog.
    picking: Mutex<()>,
}

impl Host for WindowHost {
    fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }

    fn devices(&self) -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
        // Device enumeration needs an MTA thread.
        std::thread::spawn(|| {
            let _ = wasapi::initialize_mta();
            (
                device::list(wasapi::Direction::Capture).unwrap_or_default(),
                device::list(wasapi::Direction::Render).unwrap_or_default(),
            )
        })
        .join()
        .unwrap_or_default()
    }

    fn pick_folder(&self, start: &Path) -> Option<PathBuf> {
        let _one_at_a_time = self.picking.try_lock().ok()?;
        rfd::FileDialog::new().set_directory(start).pick_folder()
    }

    fn open_folder(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = std::process::Command::new("explorer").arg(dir).spawn() {
            tracing::warn!("open folder: {e}");
        }
    }
}

fn webview_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("MeetRec").join("webview")
}

pub fn run(config_path: PathBuf, page: Page) -> Result<()> {
    let result = open(config_path, page);
    if let Err(e) = &result {
        let text = HSTRING::from(format!("MeetRec could not open its window.\n\n{e:#}\n\nIs the Microsoft Edge WebView2 runtime installed?"));
        unsafe {
            MessageBoxW(None, &text, &HSTRING::from("MeetRec"), MB_OK | MB_ICONERROR);
        }
    }
    result
}

fn open(config_path: PathBuf, page: Page) -> Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("MeetRec")
        .with_inner_size(LogicalSize::new(1100.0, 720.0))
        .with_min_inner_size(LogicalSize::new(760.0, 520.0))
        .build(&event_loop)
        .context("create window")?;

    let host = Arc::new(WindowHost { config_path, picking: Mutex::new(()) });
    let fragment = match page {
        Page::Recordings => "#/recordings",
        Page::Settings => "#/settings",
    };
    let mut context = WebContext::new(Some(webview_data_dir()));
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(format!("{APP_ORIGIN}{fragment}"))
        .with_devtools(cfg!(debug_assertions))
        // A dropped file or a followed link must not replace the app.
        .with_drag_drop_handler(|_| true)
        .with_navigation_handler(|url| is_app_url(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_asynchronous_custom_protocol("meetrec".into(), move |_id, request, responder| {
            // Off the UI thread: the folder picker blocks, and big audio reads take a moment.
            let host = Arc::clone(&host);
            std::thread::spawn(move || responder.respond(web_api::handle(host.as_ref(), &request)));
        })
        .build(&window)
        .context("create WebView2")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _keep_alive = (&webview, &context);
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_app_origin_may_load_in_the_window() {
        assert!(is_app_url("http://meetrec.localhost/#/settings"));
        assert!(is_app_url("http://meetrec.localhost/api/audio?id=a.opus"));
        assert!(!is_app_url("file:///C:/Users/me/Documents/a.opus"));
        assert!(!is_app_url("https://example.com/"));
        assert!(!is_app_url("http://meetrec.localhost.example.com/"));
        assert!(!is_app_url("http://127.0.0.1:8765/api/state"));
    }
}
