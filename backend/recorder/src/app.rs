//! The tray process: a plain Win32 message loop that feeds the controller and
//! carries out its actions.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};
use tray_icon::menu::MenuEvent;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT,
    WM_QUIT,
};

use crate::audio::device;
use crate::autostart;
use crate::config::{self, Config, ConfigWatcher};
use crate::controller::{Action, Controller, Input, State};
use crate::detect::mic_usage::{self, UsageTracker};
use crate::hotkeys::Hotkeys;
use crate::power::{self, PowerEvent};
use crate::recorder::Recorder;
use crate::session::{self, Closed};
use crate::tray::{MenuCommand, Tray};

const TICK: Duration = Duration::from_secs(1);
/// How long the loop sleeps when no window message arrives.
const IDLE_WAIT_MS: u32 = 50;

pub struct App {
    config_path: PathBuf,
    config: Config,
    watcher: ConfigWatcher,
    controller: Controller,
    recorder: Option<Recorder>,
    tray: Tray,
    hotkeys: Hotkeys,
    detector: Receiver<BTreeSet<String>>,
    device_changes: Receiver<()>,
    power: Receiver<power::Notification>,
    /// Latest set of apps holding the mic (unfiltered).
    mic_holders: BTreeSet<String>,
    usage: UsageTracker,
    last_tick: Instant,
    quit: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let config_path = config::config_path();
        let config = Config::load_or_create(&config_path);
        tracing::info!("config {}", config_path.display());
        match session::recover_parts(&config.output_dir) {
            Ok(r) if !r.is_empty() => tracing::info!("recovered {} interrupted recording(s)", r.len()),
            Ok(_) => {}
            Err(e) => tracing::warn!("recovery scan failed: {e:#}"),
        }
        sync_autostart(config.start_with_windows);
        let controller = Controller::new(config.auto_record, Duration::from_secs(config.release_grace_secs));
        let tray = Tray::new()?;
        tray.sync_checks(&config);
        let mut hotkeys = Hotkeys::new()?;
        hotkeys.apply(&config.hotkeys);
        let (tx, detector) = mpsc::channel();
        mic_usage::spawn_detector(tx);
        let (dev_tx, device_changes) = mpsc::channel();
        device::watch_default_devices(move || {
            let _ = dev_tx.send(());
        });
        let (power_tx, power_rx) = mpsc::channel();
        if let Err(e) = power::watch(power_tx) {
            tracing::warn!("no suspend/resume notifications: {e:#}");
        }
        Ok(Self {
            watcher: ConfigWatcher::new(&config_path),
            config_path,
            config,
            controller,
            recorder: None,
            tray,
            hotkeys,
            detector,
            device_changes,
            power: power_rx,
            mic_holders: BTreeSet::new(),
            usage: UsageTracker::default(),
            last_tick: Instant::now(),
            quit: false,
        })
    }

    pub fn run(mut self) -> Result<()> {
        while !self.quit {
            if !pump_messages() {
                break;
            }
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if let Some(cmd) = self.tray.command_for(ev.id()) {
                    self.on_command(cmd);
                }
            }
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                tracing::debug!("hotkey event {ev:?}");
                if ev.state() == HotKeyState::Pressed {
                    if let Some(cmd) = self.hotkeys.command_for(ev.id()) {
                        self.on_command(cmd);
                    }
                }
            }
            // Several notifications arrive per switch (one per role/flow); restart once.
            if self.device_changes.try_iter().count() > 0 {
                self.feed(Input::DeviceChanged);
            }
            while let Ok((event, ack)) = self.power.try_recv() {
                self.on_power(event);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            let mut holders_changed = false;
            while let Ok(holders) = self.detector.try_recv() {
                self.mic_holders = holders;
                holders_changed = true;
            }
            if holders_changed {
                self.refresh_mic_usage();
            }
            if self.last_tick.elapsed() >= TICK {
                self.last_tick = Instant::now();
                self.check_config();
                self.check_recorder();
                self.feed(Input::Tick);
            }
            let elapsed = self.recorder.as_ref().map(Recorder::elapsed).unwrap_or_default();
            self.tray.update(self.controller.state(), elapsed);
            unsafe { MsgWaitForMultipleObjects(None, false, IDLE_WAIT_MS, QS_ALLINPUT) };
        }
        self.feed(Input::ManualStop);
        tracing::info!("exiting");
        Ok(())
    }

    fn on_power(&mut self, event: PowerEvent) {
        tracing::info!("power: {event:?}");
        // Either way, whoever holds the mic afterwards is reported afresh.
        self.usage.reset();
        match event {
            PowerEvent::Suspend => self.feed(Input::Suspend),
            PowerEvent::Resume => self.refresh_mic_usage(),
        }
    }

    /// Re-derives MicInUse/MicReleased from the latest holders and the allow-list.
    fn refresh_mic_usage(&mut self) {
        let allowed = mic_usage::filter_allowed(&self.mic_holders, &self.config.trigger_apps);
        for input in self.usage.update(allowed) {
            self.feed(input);
        }
    }

    fn recording(&self) -> bool {
        matches!(self.controller.state(), State::Recording { .. } | State::Paused { .. })
    }

    fn on_command(&mut self, cmd: MenuCommand) {
        match cmd {
            MenuCommand::StartStop => {
                self.feed(if self.recording() { Input::ManualStop } else { Input::ManualStart })
            }
            MenuCommand::PauseResume => {
                let paused = matches!(self.controller.state(), State::Paused { .. });
                self.feed(if paused { Input::Resume } else { Input::Pause })
            }
            MenuCommand::Split => self.feed(Input::Split),
            MenuCommand::Marker => self.feed(Input::Marker),
            MenuCommand::ToggleAuto => {
                self.config.auto_record = !self.config.auto_record;
                self.save_config();
                self.feed(Input::AutoRecordChanged(self.config.auto_record));
            }
            MenuCommand::Mode(mode) => {
                // Takes effect from the next recording.
                self.config.capture_mode = mode;
                self.save_config();
            }
            MenuCommand::OpenFolder => {
                let _ = std::fs::create_dir_all(&self.config.output_dir);
                if let Err(e) = std::process::Command::new("explorer").arg(&self.config.output_dir).spawn() {
                    tracing::warn!("open folder: {e}");
                }
            }
            MenuCommand::Recordings => self.open_window("--recordings"),
            MenuCommand::Settings => self.open_window("--settings"),
            MenuCommand::Quit => self.quit = true,
        }
        self.tray.sync_checks(&self.config);
    }

    /// The settings/recordings window runs as its own short-lived process.
    fn open_window(&self, arg: &str) {
        let spawned = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg(arg).spawn());
        if let Err(e) = spawned {
            tracing::warn!("open {arg}: {e}");
        }
    }

    /// Picks up edits from the settings window (or a text editor).
    fn check_config(&mut self) {
        match self.watcher.poll() {
            Some(Ok(new)) if new != self.config => self.apply_config(new),
            Some(Err(e)) => tracing::warn!("config not reloaded: {e:#}"),
            _ => {}
        }
    }

    fn apply_config(&mut self, new: Config) {
        tracing::info!("config reloaded");
        let old = std::mem::replace(&mut self.config, new);
        let new = &self.config;
        if old.release_grace_secs != new.release_grace_secs {
            self.controller.set_grace(Duration::from_secs(new.release_grace_secs));
        }
        if old.hotkeys != new.hotkeys {
            self.hotkeys.apply(&new.hotkeys);
        }
        if old.start_with_windows != new.start_with_windows {
            sync_autostart(new.start_with_windows);
        }
        self.tray.sync_checks(new);
        if old.auto_record != new.auto_record {
            self.feed(Input::AutoRecordChanged(self.config.auto_record));
        }
        if old.trigger_apps != self.config.trigger_apps {
            self.refresh_mic_usage();
        }
    }

    fn save_config(&self) {
        if let Err(e) = self.config.save(&self.config_path) {
            tracing::warn!("saving config: {e:#}");
        }
    }

    fn feed(&mut self, input: Input) {
        tracing::debug!("input {input:?}");
        for action in self.controller.handle(input, Instant::now()) {
            self.execute(action);
        }
    }

    fn execute(&mut self, action: Action) {
        tracing::debug!("action {action:?}");
        match action {
            Action::Start { trigger, app } => match Recorder::start(&self.config, trigger, app) {
                Ok(r) => self.recorder = Some(r),
                Err(e) => {
                    tracing::error!("could not start recording: {e:#}");
                    self.notify(&format!("Could not start recording: {e}"));
                    self.feed(Input::ManualStop);
                }
            },
            Action::Stop { apply_min_duration } => {
                if let Some(r) = self.recorder.take() {
                    let min = if apply_min_duration { self.config.min_duration_secs as f64 } else { 0.0 };
                    match r.stop(min) {
                        Ok(Closed::Saved(p)) => tracing::info!("saved {}", p.display()),
                        Ok(Closed::Discarded) => tracing::info!("discarded recording shorter than {min} s"),
                        Err(e) => {
                            tracing::error!("closing recording failed: {e:#}");
                            self.notify("The recording could not be finalized; it will be recovered on the next start.");
                        }
                    }
                }
            }
            Action::Pause => self.with_recorder(Recorder::pause),
            Action::Resume => self.with_recorder(Recorder::resume),
            Action::Split => self.with_recorder(Recorder::split),
            Action::Marker => self.with_recorder(Recorder::marker),
            Action::RestartCapture => self.with_recorder(Recorder::restart_capture),
            Action::AddApp(app) => self.with_recorder(|r| r.add_app(&app)),
            Action::Notify(msg) => self.notify(&msg),
        }
    }

    fn with_recorder(&self, f: impl FnOnce(&Recorder)) {
        if let Some(r) = &self.recorder {
            f(r);
        }
    }

    /// A recorder whose engine died (disk full, file error) ends the session.
    fn check_recorder(&mut self) {
        if self.recorder.as_ref().is_some_and(|r| !r.is_alive()) {
            if let Some(r) = self.recorder.take() {
                if let Err(e) = r.stop(0.0) {
                    tracing::error!("recording failed: {e:#}");
                }
            }
            self.notify("Recording stopped because of an error; see the log.");
            self.feed(Input::ManualStop);
        }
    }

    fn notify(&self, msg: &str) {
        tracing::info!("notify: {msg}");
        let msg = msg.to_string();
        // Showing a toast is a COM round trip; keep it off the UI thread.
        std::thread::spawn(move || {
            use tauri_winrt_notification::Toast;
            if let Err(e) = Toast::new(Toast::POWERSHELL_APP_ID).title("MeetRec").text1(&msg).show() {
                tracing::warn!("toast: {e}");
            }
        });
    }
}

fn sync_autostart(enabled: bool) {
    if autostart::is_enabled() != enabled {
        match autostart::set_enabled(enabled) {
            Ok(()) => tracing::info!("start with Windows: {enabled}"),
            Err(e) => tracing::warn!("updating autostart: {e:#}"),
        }
    }
}

/// Dispatches pending window messages; false once WM_QUIT arrives.
fn pump_messages() -> bool {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}
