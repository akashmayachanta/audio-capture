//! The speech service (the Python transcriber) as the tray sees it: its state,
//! the tray menu item that follows it, and starting/stopping it. Dictation
//! starts it too, but never stops it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::client::Client;
use crate::transcriber::{self, Launcher};

/// How often the API is asked whether it is up.
const POLL_EVERY: Duration = Duration::from_secs(3);
/// A stop request older than this is given up on (the menu item must not stay locked).
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Down,
    Starting,
    Up,
    Stopping,
}

impl ServiceState {
    /// `api_up`: the API answered the last poll. `child_running`: the process this
    /// tray started is alive. `stopping`: a stop was requested and has not finished.
    pub fn derive(api_up: bool, child_running: bool, stopping: bool) -> Self {
        match (api_up, child_running, stopping) {
            (true, _, true) => Self::Stopping,
            (true, _, false) => Self::Up,
            (false, true, false) => Self::Starting,
            (false, _, _) => Self::Down,
        }
    }
}

/// The tray menu item's text and whether it can be clicked.
pub fn menu_item(state: ServiceState) -> (&'static str, bool) {
    match state {
        ServiceState::Down => ("Start speech service", true),
        ServiceState::Starting => ("Starting speech service…", false),
        ServiceState::Up => ("Stop speech service", true),
        ServiceState::Stopping => ("Stopping speech service…", false),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPlan {
    /// `POST /api/shutdown`: works whoever started it.
    Shutdown,
    /// The API does not answer; end the process this tray started.
    KillChild,
    CannotStop,
}

pub fn stop_plan(api_up: bool, have_child: bool) -> StopPlan {
    match (api_up, have_child) {
        (true, _) => StopPlan::Shutdown,
        (false, true) => StopPlan::KillChild,
        (false, false) => StopPlan::CannotStop,
    }
}

/// Whether a stop requested at `since` is still in progress.
pub fn still_stopping(since: Option<Instant>, api_up: bool, now: Instant) -> bool {
    since.is_some_and(|t| api_up && now.saturating_duration_since(t) < STOP_TIMEOUT)
}

/// The tray's handle on the speech service: a launcher for the process it starts,
/// and a thread that keeps asking the API whether it is up.
pub struct SpeechService {
    launcher: Launcher,
    polls: Receiver<bool>,
    api_up: bool,
    stopping: Option<Instant>,
}

impl SpeechService {
    pub fn new(log_path: PathBuf) -> Self {
        let (tx, polls) = mpsc::channel();
        std::thread::Builder::new()
            .name("speech-service-poll".into())
            .spawn(move || {
                let client = Client::local();
                while tx.send(client.health()).is_ok() {
                    std::thread::sleep(POLL_EVERY);
                }
            })
            .expect("spawn the speech service poller");
        Self { launcher: Launcher::new(log_path), polls, api_up: false, stopping: None }
    }

    /// Takes in the latest poll results; call often (it is cheap).
    pub fn poll(&mut self, now: Instant) -> ServiceState {
        while let Ok(up) = self.polls.try_recv() {
            self.api_up = up;
        }
        if !still_stopping(self.stopping, self.api_up, now) {
            self.stopping = None;
        }
        ServiceState::derive(self.api_up, self.launcher.status().running, self.stopping.is_some())
    }

    /// Starts it (a second start while it runs does nothing). `configured_dir` is
    /// the config's `transcriber_dir`; empty = look next to the exe.
    pub fn start(&self, configured_dir: &Path) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = transcriber::find_dir(configured_dir, &exe).ok_or_else(|| {
            "The speech service folder was not found next to audcap.exe. Set transcriber_dir in config.toml.".to_string()
        })?;
        self.launcher.start(&dir)
    }

    pub fn stop(&mut self, now: Instant) -> Result<(), String> {
        match stop_plan(self.api_up, self.launcher.status().running) {
            StopPlan::Shutdown => {
                self.stopping = Some(now);
                // The request can take a few seconds; keep it off the UI thread.
                std::thread::spawn(|| {
                    Client::local().shutdown();
                });
                Ok(())
            }
            StopPlan::KillChild => {
                if self.launcher.kill_tree() { Ok(()) } else { Err("Couldn't stop the speech service.".into()) }
            }
            StopPlan::CannotStop => {
                Err("Couldn't stop the speech service: it isn't answering and wasn't started from this tray.".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn state_follows_the_api_the_process_and_a_stop_request() {
        use ServiceState::*;
        // (api up, process running, stop requested)
        assert_eq!(ServiceState::derive(true, false, false), Up);
        assert_eq!(ServiceState::derive(true, true, false), Up);
        assert_eq!(ServiceState::derive(false, true, false), Starting);
        assert_eq!(ServiceState::derive(false, false, false), Down);
        assert_eq!(ServiceState::derive(true, true, true), Stopping);
        assert_eq!(ServiceState::derive(true, false, true), Stopping);
        assert_eq!(ServiceState::derive(false, true, true), Down, "it has stopped answering: done");
    }

    #[test]
    fn menu_item_text_and_enabled() {
        assert_eq!(menu_item(ServiceState::Down), ("Start speech service", true));
        assert_eq!(menu_item(ServiceState::Starting), ("Starting speech service…", false));
        assert_eq!(menu_item(ServiceState::Up), ("Stop speech service", true));
        assert_eq!(menu_item(ServiceState::Stopping), ("Stopping speech service…", false));
    }

    #[test]
    fn stopping_prefers_the_api_then_the_process_we_started() {
        assert_eq!(stop_plan(true, true), StopPlan::Shutdown);
        assert_eq!(stop_plan(true, false), StopPlan::Shutdown, "started by the window or by hand");
        assert_eq!(stop_plan(false, true), StopPlan::KillChild);
        assert_eq!(stop_plan(false, false), StopPlan::CannotStop);
    }

    /// Starts and stops a real transcriber. Needs a scratch server on its own port, e.g.
    /// `AUDCAP_TRANSCRIBER_ADDR=127.0.0.1:8799` and `AUDCAP_TRANSCRIBER_SETTINGS=<scratch settings.toml, port 8799>`.
    #[test]
    #[ignore = "starts the real transcriber (see the doc comment for the env vars)"]
    fn start_and_stop_the_real_service() {
        let addr = std::env::var("AUDCAP_TRANSCRIBER_ADDR").expect("AUDCAP_TRANSCRIBER_ADDR");
        assert_ne!(addr, super::super::client::TRANSCRIBER_ADDR, "use a scratch port, not the real service");
        std::env::var("AUDCAP_TRANSCRIBER_SETTINGS").expect("AUDCAP_TRANSCRIBER_SETTINGS");
        let log = tempfile::tempdir().unwrap();
        let mut s = SpeechService::new(log.path().join("transcriber.log"));
        let transcriber = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../transcriber");

        let wait_for = |s: &mut SpeechService, want: ServiceState, secs: u64| {
            let t = Instant::now();
            loop {
                let state = s.poll(Instant::now());
                if state == want {
                    return;
                }
                assert!(t.elapsed() < Duration::from_secs(secs), "still {state:?}, wanted {want:?}");
                std::thread::sleep(Duration::from_millis(100));
            }
        };

        assert_eq!(s.poll(Instant::now()), ServiceState::Down);
        s.start(&transcriber).unwrap();
        assert_eq!(s.poll(Instant::now()), ServiceState::Starting);
        wait_for(&mut s, ServiceState::Up, 90);
        s.stop(Instant::now()).unwrap();
        assert_eq!(s.poll(Instant::now()), ServiceState::Stopping);
        wait_for(&mut s, ServiceState::Down, 30);
    }

    #[test]
    fn a_stop_request_expires() {
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        assert!(!still_stopping(None, true, at(1)));
        assert!(still_stopping(Some(t0), true, at(5)));
        assert!(!still_stopping(Some(t0), false, at(5)), "the API is gone: stopped");
        assert!(!still_stopping(Some(t0), true, at(16)), "a server that refuses to quit must not lock the menu item");
    }
}
