//! The speech service (the Python transcriber) as the tray sees it: its state,
//! the tray submenu that follows it (start with a model, switch model, stop),
//! and starting/stopping it. Dictation starts it too (with the model loaded
//! last), but never stops it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use super::client::Health;
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

/// The models the tray offers: (id, label), as in the transcriber's `CATALOG`
/// (`backend/transcriber/app/engines.py`), in the same order.
pub const MODELS: &[(&str, &str)] = &[("whistle", "Cactus Whistle"), ("bark", "Underdog Bark 3.0")];

fn label(model: &str) -> &str {
    MODELS.iter().find(|(id, _)| *id == model).map_or(model, |(_, l)| l)
}

/// One model entry of the tray's speech service submenu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelItem {
    pub text: String,
    pub enabled: bool,
    pub checked: bool,
}

/// The tray's speech service submenu: its title, one item per `MODELS` entry, and Stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuView {
    pub title: String,
    pub models: Vec<ModelItem>,
    pub stop_enabled: bool,
}

/// `requested`: the model the tray last started the service with (shown while it
/// starts). `health`: the last answer of a running server (its model wins).
pub fn menu_view(state: ServiceState, requested: Option<&str>, health: &Health) -> MenuView {
    use ServiceState::*;
    let shown = match state {
        Down => None,
        Starting => requested,
        Up | Stopping => health.model.as_deref(),
    };
    let title = match state {
        Down => "off".to_string(),
        Starting => "starting…".to_string(),
        Stopping => "stopping…".to_string(),
        Up => match shown {
            None => "on, no model".to_string(),
            Some(m) if health.failed => format!("{} failed to load", label(m)),
            Some(m) if health.loading => format!("loading {}…", label(m)),
            Some(m) => label(m).to_string(),
        },
    };
    let models = MODELS
        .iter()
        .map(|(id, l)| ModelItem {
            text: if state == Down { format!("Start with {l}") } else { l.to_string() },
            enabled: matches!(state, Down | Up),
            checked: shown == Some(*id),
        })
        .collect();
    MenuView { title: format!("Speech service ({title})"), models, stop_enabled: matches!(state, Starting | Up) }
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
    polls: Receiver<Option<Health>>,
    /// The last poll's answer; `None` = the API is not up.
    health: Option<Health>,
    stopping: Option<Instant>,
    /// The model the last start from this tray asked for.
    requested: Mutex<Option<String>>,
    errors_tx: Sender<String>,
    errors: Receiver<String>,
}

impl SpeechService {
    pub fn new(log_path: PathBuf) -> Self {
        let (tx, polls) = mpsc::channel();
        std::thread::Builder::new()
            .name("speech-service-poll".into())
            .spawn(move || {
                let client = Client::local();
                while tx.send(client.status()).is_ok() {
                    std::thread::sleep(POLL_EVERY);
                }
            })
            .expect("spawn the speech service poller");
        let (errors_tx, errors) = mpsc::channel();
        Self {
            launcher: Launcher::new(log_path),
            polls,
            health: None,
            stopping: None,
            requested: Mutex::new(None),
            errors_tx,
            errors,
        }
    }

    fn api_up(&self) -> bool {
        self.health.is_some()
    }

    /// Takes in the latest poll results; call often (it is cheap).
    pub fn poll(&mut self, now: Instant) -> ServiceState {
        while let Ok(health) = self.polls.try_recv() {
            self.health = health;
        }
        if !still_stopping(self.stopping, self.api_up(), now) {
            self.stopping = None;
        }
        ServiceState::derive(self.api_up(), self.launcher.status().running, self.stopping.is_some())
    }

    /// The tray submenu for `state` (from the last `poll`).
    pub fn menu(&self, state: ServiceState) -> MenuView {
        let requested = self.requested.lock().unwrap().clone();
        menu_view(state, requested.as_deref(), &self.health.clone().unwrap_or_default())
    }

    /// Messages for the user from background requests (a model switch that failed).
    pub fn take_errors(&self) -> Vec<String> {
        self.errors.try_iter().collect()
    }

    /// Starts it (a second start while it runs does nothing). `configured_dir` is
    /// the config's `transcriber_dir`; empty = look next to the exe. `model`: load
    /// this one (it becomes the one loaded next time too); `None` = the one loaded last.
    pub fn start(&self, configured_dir: &Path, model: Option<&str>) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = transcriber::find_dir(configured_dir, &exe).ok_or_else(|| {
            "The speech service folder was not found next to audcap.exe. Set transcriber_dir in config.toml.".to_string()
        })?;
        self.launcher.start(&dir, model)?;
        *self.requested.lock().unwrap() = model.map(str::to_string);
        Ok(())
    }

    /// Switches the running service to `model`. The request runs on a thread;
    /// a failure comes back through `take_errors`.
    pub fn load(&mut self, model: &str) -> Result<(), String> {
        let Some(health) = self.health.as_mut() else {
            return Err("The speech service is not running.".into());
        };
        if health.model.as_deref() == Some(model) && !health.failed {
            return Ok(());
        }
        // Shown right away; the next poll brings the server's view.
        *health = Health { model: Some(model.to_string()), loading: true, failed: false };
        let (model, errors) = (model.to_string(), self.errors_tx.clone());
        std::thread::spawn(move || {
            if let Err(e) = Client::local().load(&model) {
                let _ = errors.send(format!("Couldn't switch the speech model: {e}"));
            }
        });
        Ok(())
    }

    pub fn stop(&mut self, now: Instant) -> Result<(), String> {
        match stop_plan(self.api_up(), self.launcher.status().running) {
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

    fn item(text: &str, enabled: bool, checked: bool) -> ModelItem {
        ModelItem { text: text.into(), enabled, checked }
    }

    fn health(model: Option<&str>, loading: bool, failed: bool) -> Health {
        Health { model: model.map(String::from), loading, failed }
    }

    #[test]
    fn off_offers_to_start_with_each_model() {
        let v = menu_view(ServiceState::Down, None, &Health::default());
        assert_eq!(v.title, "Speech service (off)");
        assert_eq!(
            v.models,
            vec![item("Start with Cactus Whistle", true, false), item("Start with Underdog Bark 3.0", true, false)]
        );
        assert!(!v.stop_enabled);
    }

    #[test]
    fn starting_ticks_the_requested_model_and_can_be_stopped() {
        let v = menu_view(ServiceState::Starting, Some("bark"), &Health::default());
        assert_eq!(v.title, "Speech service (starting…)");
        assert_eq!(v.models, vec![item("Cactus Whistle", false, false), item("Underdog Bark 3.0", false, true)]);
        assert!(v.stop_enabled, "a start that hangs can be ended");
        let v = menu_view(ServiceState::Starting, None, &Health::default());
        assert!(v.models.iter().all(|m| !m.checked), "started without a choice: the last model, unknown here");
    }

    #[test]
    fn running_ticks_the_active_model_and_allows_switching() {
        let v = menu_view(ServiceState::Up, Some("bark"), &health(Some("whistle"), false, false));
        assert_eq!(v.title, "Speech service (Cactus Whistle)", "the server's model wins over the requested one");
        assert_eq!(v.models, vec![item("Cactus Whistle", true, true), item("Underdog Bark 3.0", true, false)]);
        assert!(v.stop_enabled);
    }

    #[test]
    fn running_titles_show_loading_failure_and_no_model() {
        let title = |h: Health| menu_view(ServiceState::Up, None, &h).title;
        assert_eq!(title(health(Some("bark"), true, false)), "Speech service (loading Underdog Bark 3.0…)");
        assert_eq!(title(health(Some("bark"), false, true)), "Speech service (Underdog Bark 3.0 failed to load)");
        assert_eq!(title(health(None, false, false)), "Speech service (on, no model)");
        assert_eq!(title(health(Some("tiny"), false, false)), "Speech service (tiny)", "a model this build doesn't list");
    }

    #[test]
    fn stopping_locks_everything() {
        let v = menu_view(ServiceState::Stopping, None, &health(Some("whistle"), false, false));
        assert_eq!(v.title, "Speech service (stopping…)");
        assert!(v.models.iter().all(|m| !m.enabled));
        assert!(!v.stop_enabled);
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
        s.start(&transcriber, None).unwrap();
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
