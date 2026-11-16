//! Tray icon + context menu. The icon colour tracks the controller state:
//! grey = idle, blue = armed, red = recording, amber = paused.

use std::time::Duration;

use anyhow::Result;
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::{CaptureMode, Config};
use crate::controller::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    Idle,
    Armed,
    Recording,
    Paused,
}

impl IconKind {
    pub fn for_state(state: &State) -> Self {
        match state {
            State::Idle => IconKind::Idle,
            State::Armed => IconKind::Armed,
            State::Recording { .. } => IconKind::Recording,
            State::Paused { .. } => IconKind::Paused,
        }
    }

    fn rgb(self) -> [u8; 3] {
        match self {
            IconKind::Idle => [128, 128, 128],
            IconKind::Armed => [30, 120, 230],
            IconKind::Recording => [220, 40, 40],
            IconKind::Paused => [240, 170, 0],
        }
    }
}

pub const ICON_SIZE: u32 = 32;

/// A filled disc in the state colour on a transparent square, RGBA.
pub fn icon_rgba(kind: IconKind) -> Vec<u8> {
    let [r, g, b] = kind.rgb();
    let c = ICON_SIZE as f32 / 2.0;
    let radius = c - 2.0;
    let mut px = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    for y in 0..ICON_SIZE {
        for x in 0..ICON_SIZE {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            // One pixel of anti-aliasing at the edge.
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            px.extend_from_slice(&[r, g, b, (alpha * 255.0) as u8]);
        }
    }
    px
}

pub fn tooltip(state: &State, elapsed: Duration) -> String {
    let clock = || {
        let s = elapsed.as_secs();
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    };
    let with_app = |verb: &str, app: &Option<String>| match app {
        Some(a) => format!("MeetRec — {verb} {a} {}", clock()),
        None => format!("MeetRec — {verb} {}", clock()),
    };
    match state {
        State::Idle => "MeetRec — idle".into(),
        State::Armed => "MeetRec — waiting for a meeting".into(),
        State::Recording { app, .. } => with_app("recording", app),
        State::Paused { app, .. } => with_app("paused", app),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuCommand {
    StartStop,
    PauseResume,
    Split,
    Marker,
    ToggleAuto,
    Mode(CaptureMode),
    OpenFolder,
    Recordings,
    Settings,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    kind: Option<IconKind>,
    tooltip: String,
    start_stop: MenuItem,
    pause_resume: MenuItem,
    split: MenuItem,
    marker: MenuItem,
    auto: CheckMenuItem,
    modes: [(CaptureMode, CheckMenuItem); 3],
    open_folder: MenuItem,
    recordings: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
}

impl Tray {
    pub fn new() -> Result<Self> {
        let start_stop = MenuItem::new("Start recording", true, None);
        let pause_resume = MenuItem::new("Pause", false, None);
        let split = MenuItem::new("Split now", false, None);
        let marker = MenuItem::new("Add marker", false, None);
        let auto = CheckMenuItem::new("Auto-record meetings", true, false, None);
        let modes = [
            (CaptureMode::Mic, CheckMenuItem::new("Microphone only", true, false, None)),
            (CaptureMode::System, CheckMenuItem::new("System audio only", true, false, None)),
            (CaptureMode::Both, CheckMenuItem::new("Microphone + system", true, false, None)),
        ];
        let capture = Submenu::with_items("Capture", true, &[&modes[0].1, &modes[1].1, &modes[2].1])?;
        let open_folder = MenuItem::new("Open recordings folder", true, None);
        let recordings = MenuItem::new("Recordings…", true, None);
        let settings = MenuItem::new("Settings…", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::with_items(&[
            &start_stop,
            &pause_resume,
            &split,
            &marker,
            &PredefinedMenuItem::separator(),
            &auto,
            &capture,
            &PredefinedMenuItem::separator(),
            &open_folder,
            &recordings,
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("MeetRec")
            .with_icon(make_icon(IconKind::Idle)?)
            .build()?;
        Ok(Self {
            icon,
            kind: None,
            tooltip: String::new(),
            start_stop,
            pause_resume,
            split,
            marker,
            auto,
            modes,
            open_folder,
            recordings,
            settings,
            quit,
        })
    }

    pub fn command_for(&self, id: &MenuId) -> Option<MenuCommand> {
        let is = |item_id: &MenuId| item_id == id;
        if is(self.start_stop.id()) {
            Some(MenuCommand::StartStop)
        } else if is(self.pause_resume.id()) {
            Some(MenuCommand::PauseResume)
        } else if is(self.split.id()) {
            Some(MenuCommand::Split)
        } else if is(self.marker.id()) {
            Some(MenuCommand::Marker)
        } else if is(self.auto.id()) {
            Some(MenuCommand::ToggleAuto)
        } else if is(self.open_folder.id()) {
            Some(MenuCommand::OpenFolder)
        } else if is(self.recordings.id()) {
            Some(MenuCommand::Recordings)
        } else if is(self.settings.id()) {
            Some(MenuCommand::Settings)
        } else if is(self.quit.id()) {
            Some(MenuCommand::Quit)
        } else {
            self.modes.iter().find(|(_, item)| is(item.id())).map(|(m, _)| MenuCommand::Mode(*m))
        }
    }

    /// Syncs icon, tooltip and menu with the current state. Cheap when nothing changed.
    pub fn update(&mut self, state: &State, elapsed: Duration) {
        let kind = IconKind::for_state(state);
        if self.kind != Some(kind) {
            self.kind = Some(kind);
            match make_icon(kind) {
                Ok(icon) => {
                    let _ = self.icon.set_icon(Some(icon));
                }
                Err(e) => tracing::warn!("icon: {e:#}"),
            }
            let recording = matches!(kind, IconKind::Recording | IconKind::Paused);
            self.start_stop.set_text(if recording { "Stop recording" } else { "Start recording" });
            self.pause_resume.set_text(if kind == IconKind::Paused { "Resume" } else { "Pause" });
            self.pause_resume.set_enabled(recording);
            self.split.set_enabled(recording);
            self.marker.set_enabled(recording);
        }
        let tip = tooltip(state, elapsed);
        if tip != self.tooltip {
            let _ = self.icon.set_tooltip(Some(&tip));
            self.tooltip = tip;
        }
    }

    /// Check items toggle themselves on click; call after any click or config
    /// change to re-assert what the config says.
    pub fn sync_checks(&self, config: &Config) {
        self.auto.set_checked(config.auto_record);
        for (mode, item) in &self.modes {
            item.set_checked(*mode == config.capture_mode);
        }
    }
}

fn make_icon(kind: IconKind) -> Result<Icon> {
    Ok(Icon::from_rgba(icon_rgba(kind), ICON_SIZE, ICON_SIZE)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Trigger;

    fn rec(app: Option<&str>) -> State {
        State::Recording { trigger: Trigger::Auto, app: app.map(String::from) }
    }

    #[test]
    fn icon_kind_follows_state() {
        assert_eq!(IconKind::for_state(&State::Idle), IconKind::Idle);
        assert_eq!(IconKind::for_state(&State::Armed), IconKind::Armed);
        assert_eq!(IconKind::for_state(&rec(None)), IconKind::Recording);
        assert_eq!(
            IconKind::for_state(&State::Paused { trigger: Trigger::Manual, app: None }),
            IconKind::Paused
        );
    }

    #[test]
    fn icon_is_a_coloured_disc_on_transparent() {
        let px = icon_rgba(IconKind::Recording);
        assert_eq!(px.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        let at = |x: u32, y: u32| {
            let i = ((y * ICON_SIZE + x) * 4) as usize;
            [px[i], px[i + 1], px[i + 2], px[i + 3]]
        };
        assert_eq!(at(16, 16), [220, 40, 40, 255]);
        assert_eq!(at(0, 0)[3], 0, "corner is transparent");
    }

    #[test]
    fn tooltip_shows_state_app_and_elapsed() {
        assert_eq!(tooltip(&State::Idle, Duration::ZERO), "MeetRec — idle");
        assert_eq!(tooltip(&State::Armed, Duration::ZERO), "MeetRec — waiting for a meeting");
        assert_eq!(
            tooltip(&rec(Some("Teams")), Duration::from_secs(3723)),
            "MeetRec — recording Teams 1:02:03"
        );
        assert_eq!(tooltip(&rec(None), Duration::from_secs(65)), "MeetRec — recording 0:01:05");
        assert_eq!(
            tooltip(&State::Paused { trigger: Trigger::Manual, app: Some("Zoom".into()) }, Duration::from_secs(5)),
            "MeetRec — paused Zoom 0:00:05"
        );
    }
}
