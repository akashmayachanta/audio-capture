//! The recording state machine. Pure: no audio, no Win32, time is passed in.
//!
//! The tray loop feeds it inputs (tray menu, hotkeys, mic-usage detector,
//! config reloads, power events, a 1 s tick) and executes the actions it returns.

use std::time::{Duration, Instant};

use crate::session::Trigger;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// Auto-record off, not recording.
    Idle,
    /// Auto-record on, waiting for a meeting app to take the mic.
    Armed,
    Recording { trigger: Trigger, app: Option<String> },
    Paused { trigger: Trigger, app: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// An allow-listed app started using the mic.
    MicInUse(String),
    /// No allow-listed app is using the mic any more.
    MicReleased,
    ManualStart,
    ManualStop,
    Pause,
    Resume,
    Split,
    Marker,
    DeviceChanged,
    AutoRecordChanged(bool),
    Suspend,
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Start { trigger: Trigger, app: Option<String> },
    /// `apply_min_duration`: discard the file if it is shorter than the minimum.
    Stop { apply_min_duration: bool },
    Pause,
    Resume,
    Split,
    Marker,
    AddApp(String),
    RestartCapture,
    Notify(String),
}

pub struct Controller {
    state: State,
    auto_record: bool,
    grace: Duration,
    /// The allow-listed app currently holding the mic, as last reported.
    mic_app: Option<String>,
    /// Apps already recorded in the current session.
    session_apps: Vec<String>,
    /// When the mic was released during an auto recording (grace timer).
    released_at: Option<Instant>,
}

impl Controller {
    pub fn new(auto_record: bool, grace: Duration) -> Self {
        let state = if auto_record { State::Armed } else { State::Idle };
        Self { state, auto_record, grace, mic_app: None, session_apps: Vec::new(), released_at: None }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn set_grace(&mut self, grace: Duration) {
        self.grace = grace;
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Vec<Action> {
        let recording = self.recording();
        match input {
            Input::MicInUse(app) => {
                self.mic_app = Some(app.clone());
                self.released_at = None;
                match recording {
                    None if self.state == State::Armed => {
                        let mut actions = self.start(Trigger::Auto, Some(app.clone()));
                        actions.push(Action::Notify(format!("Recording {app} meeting")));
                        actions
                    }
                    Some(_) if !self.session_apps.contains(&app) => {
                        self.session_apps.push(app.clone());
                        vec![Action::AddApp(app)]
                    }
                    _ => vec![],
                }
            }
            Input::MicReleased => {
                self.mic_app = None;
                if recording == Some(Trigger::Auto) {
                    self.released_at = Some(now);
                }
                vec![]
            }
            Input::Tick => match self.released_at {
                Some(at) if recording == Some(Trigger::Auto) && now.duration_since(at) >= self.grace => self.stop(),
                _ => vec![],
            },
            Input::ManualStart if recording.is_none() => self.start(Trigger::Manual, self.mic_app.clone()),
            Input::ManualStop if recording.is_some() => self.stop(),
            Input::Suspend => {
                // After resume the detector reports whoever holds the mic afresh.
                self.mic_app = None;
                if recording.is_some() { self.stop() } else { vec![] }
            }
            Input::Pause => match &self.state {
                State::Recording { trigger, app } => {
                    self.state = State::Paused { trigger: *trigger, app: app.clone() };
                    vec![Action::Pause]
                }
                _ => vec![],
            },
            Input::Resume => match &self.state {
                State::Paused { trigger, app } => {
                    self.state = State::Recording { trigger: *trigger, app: app.clone() };
                    vec![Action::Resume]
                }
                _ => vec![],
            },
            Input::Split if recording.is_some() => vec![Action::Split],
            Input::Marker if recording.is_some() => vec![Action::Marker],
            Input::DeviceChanged if recording.is_some() => vec![Action::RestartCapture],
            Input::AutoRecordChanged(on) => {
                self.auto_record = on;
                if recording.is_none() {
                    self.state = self.resting();
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// The trigger of the current session, if any.
    fn recording(&self) -> Option<Trigger> {
        match &self.state {
            State::Recording { trigger, .. } | State::Paused { trigger, .. } => Some(*trigger),
            _ => None,
        }
    }

    fn resting(&self) -> State {
        if self.auto_record { State::Armed } else { State::Idle }
    }

    fn start(&mut self, trigger: Trigger, app: Option<String>) -> Vec<Action> {
        self.session_apps = app.iter().cloned().collect();
        self.released_at = None;
        self.state = State::Recording { trigger, app: app.clone() };
        vec![Action::Start { trigger, app }]
    }

    fn stop(&mut self) -> Vec<Action> {
        let trigger = self.recording();
        self.state = self.resting();
        self.released_at = None;
        self.session_apps.clear();
        vec![Action::Stop { apply_min_duration: trigger == Some(Trigger::Auto) }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(20);

    fn rec(trigger: Trigger, app: Option<&str>) -> State {
        State::Recording { trigger, app: app.map(String::from) }
    }

    #[test]
    fn starts_idle_or_armed_from_auto_record_setting() {
        assert_eq!(Controller::new(false, GRACE).state(), &State::Idle);
        assert_eq!(Controller::new(true, GRACE).state(), &State::Armed);
    }

    #[test]
    fn manual_start_and_stop() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        assert_eq!(c.handle(Input::ManualStart, t), vec![Action::Start { trigger: Trigger::Manual, app: None }]);
        assert_eq!(c.state(), &rec(Trigger::Manual, None));
        assert_eq!(c.handle(Input::ManualStop, t), vec![Action::Stop { apply_min_duration: false }]);
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn manual_stop_returns_to_armed_when_auto_record_on() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::ManualStart, t);
        c.handle(Input::ManualStop, t);
        assert_eq!(c.state(), &State::Armed);
    }

    #[test]
    fn manual_start_while_recording_is_ignored() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        assert!(c.handle(Input::ManualStart, t).is_empty());
    }

    #[test]
    fn stop_split_pause_marker_do_nothing_when_not_recording() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        for i in [Input::ManualStop, Input::Split, Input::Pause, Input::Resume, Input::Marker, Input::DeviceChanged] {
            assert!(c.handle(i.clone(), t).is_empty(), "{i:?}");
            assert_eq!(c.state(), &State::Idle);
        }
    }

    #[test]
    fn pause_and_resume_keep_the_session() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        assert_eq!(c.handle(Input::Pause, t), vec![Action::Pause]);
        assert_eq!(c.state(), &State::Paused { trigger: Trigger::Manual, app: None });
        assert!(c.handle(Input::Pause, t).is_empty());
        assert_eq!(c.handle(Input::Resume, t), vec![Action::Resume]);
        assert_eq!(c.state(), &rec(Trigger::Manual, None));
    }

    #[test]
    fn split_and_marker_while_recording_or_paused() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        assert_eq!(c.handle(Input::Split, t), vec![Action::Split]);
        assert_eq!(c.handle(Input::Marker, t), vec![Action::Marker]);
        c.handle(Input::Pause, t);
        assert_eq!(c.handle(Input::Split, t), vec![Action::Split]);
        assert_eq!(c.handle(Input::Marker, t), vec![Action::Marker]);
    }

    #[test]
    fn stop_while_paused() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        c.handle(Input::Pause, t);
        assert_eq!(c.handle(Input::ManualStop, t), vec![Action::Stop { apply_min_duration: false }]);
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn device_change_restarts_capture_only_while_recording() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        assert_eq!(c.handle(Input::DeviceChanged, t), vec![Action::RestartCapture]);
    }

    #[test]
    fn toggling_auto_record_switches_idle_and_armed() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::AutoRecordChanged(true), t);
        assert_eq!(c.state(), &State::Armed);
        c.handle(Input::AutoRecordChanged(false), t);
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn auto_record_change_does_not_interrupt_a_recording() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::ManualStart, t);
        assert!(c.handle(Input::AutoRecordChanged(false), t).is_empty());
        assert_eq!(c.state(), &rec(Trigger::Manual, None));
        c.handle(Input::ManualStop, t);
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn suspend_closes_the_session() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::ManualStart, t);
        assert_eq!(c.handle(Input::Suspend, t), vec![Action::Stop { apply_min_duration: false }]);
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn manual_start_takes_the_name_of_the_app_holding_the_mic() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        c.handle(Input::MicInUse("Zoom".into()), t);
        assert_eq!(c.state(), &State::Idle, "auto-record is off");
        assert_eq!(c.handle(Input::ManualStart, t), vec![Action::Start { trigger: Trigger::Manual, app: Some("Zoom".into()) }]);
    }

    // ---- auto mode ----

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn armed_starts_recording_when_a_meeting_app_takes_the_mic() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        let actions = c.handle(Input::MicInUse("Teams".into()), t);
        assert_eq!(actions[0], Action::Start { trigger: Trigger::Auto, app: Some("Teams".into()) });
        assert!(matches!(&actions[1], Action::Notify(m) if m.contains("Teams")), "{actions:?}");
        assert_eq!(c.state(), &rec(Trigger::Auto, Some("Teams")));
    }

    #[test]
    fn idle_does_not_auto_start() {
        let t = Instant::now();
        let mut c = Controller::new(false, GRACE);
        assert!(c.handle(Input::MicInUse("Teams".into()), t).is_empty());
        assert_eq!(c.state(), &State::Idle);
    }

    #[test]
    fn auto_recording_stops_only_after_the_grace_period() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        assert!(c.handle(Input::MicReleased, t + secs(60)).is_empty());
        assert!(c.handle(Input::Tick, t + secs(60) + GRACE - secs(1)).is_empty());
        assert_eq!(c.state(), &rec(Trigger::Auto, Some("Teams")));
        assert_eq!(c.handle(Input::Tick, t + secs(60) + GRACE), vec![Action::Stop { apply_min_duration: true }]);
        assert_eq!(c.state(), &State::Armed);
        assert!(c.handle(Input::Tick, t + secs(200)).is_empty(), "stops once");
    }

    #[test]
    fn mic_taken_again_within_grace_keeps_recording() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        c.handle(Input::MicReleased, t + secs(10));
        assert!(c.handle(Input::MicInUse("Teams".into()), t + secs(15)).is_empty());
        assert!(c.handle(Input::Tick, t + secs(10) + GRACE + secs(5)).is_empty());
        assert_eq!(c.state(), &rec(Trigger::Auto, Some("Teams")));
    }

    #[test]
    fn another_app_joining_is_added_to_the_session() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        assert_eq!(c.handle(Input::MicInUse("Chrome".into()), t), vec![Action::AddApp("Chrome".into())]);
    }

    #[test]
    fn manual_recording_ignores_mic_release() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::ManualStart, t);
        c.handle(Input::MicInUse("Zoom".into()), t);
        c.handle(Input::MicReleased, t + secs(5));
        assert!(c.handle(Input::Tick, t + secs(500)).is_empty());
        assert_eq!(c.state(), &rec(Trigger::Manual, None));
    }

    #[test]
    fn manual_stop_of_auto_recording_does_not_retrigger_until_mic_is_taken_again() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        assert_eq!(c.handle(Input::ManualStop, t + secs(40)), vec![Action::Stop { apply_min_duration: true }]);
        assert_eq!(c.state(), &State::Armed);
        assert!(c.handle(Input::Tick, t + secs(100)).is_empty());
        assert_eq!(c.state(), &State::Armed);
        c.handle(Input::MicReleased, t + secs(120));
        let again = c.handle(Input::MicInUse("Teams".into()), t + secs(300));
        assert_eq!(again[0], Action::Start { trigger: Trigger::Auto, app: Some("Teams".into()) });
    }

    #[test]
    fn paused_auto_recording_also_stops_after_grace() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        c.handle(Input::Pause, t);
        c.handle(Input::MicReleased, t + secs(1));
        assert_eq!(c.handle(Input::Tick, t + secs(1) + GRACE), vec![Action::Stop { apply_min_duration: true }]);
    }

    #[test]
    fn grace_period_follows_config() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.set_grace(secs(5));
        c.handle(Input::MicInUse("Teams".into()), t);
        c.handle(Input::MicReleased, t);
        assert_eq!(c.handle(Input::Tick, t + secs(5)), vec![Action::Stop { apply_min_duration: true }]);
    }

    #[test]
    fn suspend_closes_auto_recording_and_forgets_the_mic_holder() {
        let t = Instant::now();
        let mut c = Controller::new(true, GRACE);
        c.handle(Input::MicInUse("Teams".into()), t);
        assert_eq!(c.handle(Input::Suspend, t), vec![Action::Stop { apply_min_duration: true }]);
        assert_eq!(c.state(), &State::Armed);
        // After resume the detector reports the mic afresh.
        let again = c.handle(Input::MicInUse("Teams".into()), t + secs(5));
        assert_eq!(again[0], Action::Start { trigger: Trigger::Auto, app: Some("Teams".into()) });
    }
}
