//! The dictation state machine. Pure, like `controller.rs`: gestures and
//! results in, actions out; `Dictation` carries the actions out.

use super::gesture::Gesture;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Gesture(Gesture),
    Esc,
    TranscriptReady(String),
    /// Transcribing is over and its text has already been pasted phrase by phrase.
    Finished,
    TranscriptFailed(String),
    CaptureFailed(String),
    /// Once a second; `listening_secs` is how long the capture has run.
    Tick { listening_secs: u64 },
    Suspend,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the mic (silently: the press may still turn out to be a tap).
    StartCapture,
    /// Start the speech service if it is not running.
    EnsureServer,
    ShowListening,
    HideOverlay,
    /// Drop the capture (a tap): nothing worth keeping.
    DiscardCapture,
    /// Stop the capture without transcribing (Esc, another key).
    CancelCapture,
    /// Stop the capture, save it and transcribe it.
    FinishCapture,
    CancelTranscription,
    Paste(String),
    Notify(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    /// Hold key is down; not yet known to be a hold, a tap or a shortcut.
    Pending,
    /// Held long enough: listening until release.
    Hold,
    /// Hands-free: listening until the next double-tap.
    Toggle,
    Transcribing,
}

pub struct Controller {
    state: State,
    max_secs: u64,
}

impl Controller {
    pub fn new(max_secs: u64) -> Self {
        Self { state: State::Idle, max_secs }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn set_max_secs(&mut self, max_secs: u64) {
        self.max_secs = max_secs;
    }

    pub fn handle(&mut self, input: Input) -> Vec<Action> {
        use Action as A;
        use Gesture as G;
        use State as S;
        let (next, actions) = match (self.state, input) {
            (S::Idle, Input::Gesture(G::Pressed)) => (S::Pending, vec![A::StartCapture]),
            (S::Idle, Input::Gesture(G::DoubleTap)) => {
                (S::Toggle, vec![A::StartCapture, A::EnsureServer, A::ShowListening])
            }
            (S::Pending, Input::Gesture(G::ShowOverlay)) => (S::Hold, vec![A::EnsureServer, A::ShowListening]),
            (S::Pending, Input::Gesture(G::TapDiscarded)) => (S::Idle, vec![A::DiscardCapture]),
            (S::Hold, Input::Gesture(G::TapDiscarded)) => (S::Idle, vec![A::DiscardCapture, A::HideOverlay]),
            (S::Pending | S::Hold, Input::Gesture(G::HoldReleased)) => (S::Transcribing, vec![A::FinishCapture]),
            (S::Pending, Input::Gesture(G::DoubleTap)) => (S::Toggle, vec![A::EnsureServer, A::ShowListening]),
            (S::Hold, Input::Gesture(G::DoubleTap)) => (S::Toggle, vec![]),
            (S::Pending | S::Hold, Input::Gesture(G::Cancelled)) => (S::Idle, vec![A::CancelCapture, A::HideOverlay]),
            (S::Toggle, Input::Gesture(G::DoubleTap | G::HoldReleased)) => (S::Transcribing, vec![A::FinishCapture]),
            (S::Pending | S::Hold | S::Toggle, Input::Esc | Input::Suspend) => {
                (S::Idle, vec![A::CancelCapture, A::HideOverlay])
            }
            (S::Transcribing, Input::Esc | Input::Suspend) => (S::Idle, vec![A::CancelTranscription, A::HideOverlay]),
            (S::Transcribing, Input::TranscriptReady(text)) => {
                let text = text.trim();
                if text.is_empty() {
                    (S::Idle, vec![A::HideOverlay, A::Notify("No speech heard".into())])
                } else {
                    (S::Idle, vec![A::Paste(text.into()), A::HideOverlay])
                }
            }
            (S::Transcribing, Input::Finished) => (S::Idle, vec![A::HideOverlay]),
            (S::Transcribing, Input::TranscriptFailed(msg)) => (S::Idle, vec![A::HideOverlay, A::Notify(msg)]),
            (S::Pending | S::Hold | S::Toggle, Input::CaptureFailed(msg)) => {
                (S::Idle, vec![A::HideOverlay, A::Notify(msg)])
            }
            (S::Hold | S::Toggle, Input::Tick { listening_secs }) if listening_secs >= self.max_secs => {
                (S::Transcribing, vec![A::FinishCapture])
            }
            (state, _) => (state, vec![]),
        };
        self.state = next;
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::gesture::Gesture as G;

    fn c() -> Controller {
        Controller::new(300)
    }

    fn g(c: &mut Controller, gesture: G) -> Vec<Action> {
        c.handle(Input::Gesture(gesture))
    }

    /// A controller in `Pending` (key down, undecided).
    fn pending() -> Controller {
        let mut c = c();
        g(&mut c, G::Pressed);
        c
    }

    fn hold() -> Controller {
        let mut c = pending();
        g(&mut c, G::ShowOverlay);
        c
    }

    fn toggle() -> Controller {
        let mut c = pending();
        g(&mut c, G::DoubleTap);
        c
    }

    fn transcribing() -> Controller {
        let mut c = hold();
        g(&mut c, G::HoldReleased);
        c
    }

    #[test]
    fn press_starts_capturing_silently() {
        let mut c = c();
        assert_eq!(g(&mut c, G::Pressed), vec![Action::StartCapture]);
        assert_eq!(c.state(), State::Pending);
    }

    #[test]
    fn holding_shows_the_overlay_and_makes_sure_the_server_is_up() {
        let mut c = pending();
        assert_eq!(g(&mut c, G::ShowOverlay), vec![Action::EnsureServer, Action::ShowListening]);
        assert_eq!(c.state(), State::Hold);
    }

    #[test]
    fn tap_is_discarded() {
        let mut c = pending();
        assert_eq!(g(&mut c, G::TapDiscarded), vec![Action::DiscardCapture]);
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn a_tap_held_long_enough_to_show_the_overlay_hides_it_again() {
        let mut c = hold();
        assert_eq!(g(&mut c, G::TapDiscarded), vec![Action::DiscardCapture, Action::HideOverlay]);
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn release_after_a_hold_transcribes() {
        let mut c = hold();
        assert_eq!(g(&mut c, G::HoldReleased), vec![Action::FinishCapture]);
        assert_eq!(c.state(), State::Transcribing);
    }

    #[test]
    fn release_before_the_overlay_tick_still_transcribes() {
        let mut c = pending();
        assert_eq!(g(&mut c, G::HoldReleased), vec![Action::FinishCapture]);
        assert_eq!(c.state(), State::Transcribing);
    }

    #[test]
    fn double_tap_toggles_on_keeping_the_capture_from_the_second_press() {
        let mut c = pending();
        assert_eq!(g(&mut c, G::DoubleTap), vec![Action::EnsureServer, Action::ShowListening]);
        assert_eq!(c.state(), State::Toggle);
    }

    #[test]
    fn double_tap_whose_second_press_already_showed_the_overlay_just_switches_to_toggle() {
        // The second tap was held 150-300 ms: ShowOverlay fired before DoubleTap.
        let mut c = hold();
        assert_eq!(g(&mut c, G::DoubleTap), vec![]);
        assert_eq!(c.state(), State::Toggle);
    }

    #[test]
    fn double_tap_from_idle_with_separate_keys_starts_a_capture() {
        let mut c = c();
        assert_eq!(
            g(&mut c, G::DoubleTap),
            vec![Action::StartCapture, Action::EnsureServer, Action::ShowListening]
        );
        assert_eq!(c.state(), State::Toggle);
    }

    #[test]
    fn double_tap_toggles_off() {
        let mut c = toggle();
        assert_eq!(g(&mut c, G::DoubleTap), vec![Action::FinishCapture]);
        assert_eq!(c.state(), State::Transcribing);
    }

    #[test]
    fn a_hold_release_also_ends_toggle_listening() {
        let mut c = toggle();
        assert_eq!(g(&mut c, G::HoldReleased), vec![Action::FinishCapture]);
        assert_eq!(c.state(), State::Transcribing);
    }

    #[test]
    fn the_taps_around_toggle_listening_are_ignored() {
        let mut c = toggle();
        assert_eq!(g(&mut c, G::Pressed), vec![]);
        assert_eq!(g(&mut c, G::ShowOverlay), vec![]);
        assert_eq!(g(&mut c, G::TapDiscarded), vec![]);
        assert_eq!(c.state(), State::Toggle);
    }

    #[test]
    fn another_key_cancels_a_press() {
        for mut c in [pending(), hold()] {
            assert_eq!(g(&mut c, G::Cancelled), vec![Action::CancelCapture, Action::HideOverlay]);
            assert_eq!(c.state(), State::Idle);
        }
    }

    #[test]
    fn esc_cancels_listening() {
        for mut c in [pending(), hold(), toggle()] {
            assert_eq!(c.handle(Input::Esc), vec![Action::CancelCapture, Action::HideOverlay]);
            assert_eq!(c.state(), State::Idle);
        }
    }

    #[test]
    fn esc_cancels_transcribing_and_a_late_transcript_is_dropped() {
        let mut c = transcribing();
        assert_eq!(c.handle(Input::Esc), vec![Action::CancelTranscription, Action::HideOverlay]);
        assert_eq!(c.state(), State::Idle);
        assert_eq!(c.handle(Input::TranscriptReady("late".into())), vec![]);
        assert_eq!(c.handle(Input::TranscriptFailed("late".into())), vec![]);
    }

    #[test]
    fn esc_when_idle_does_nothing() {
        assert_eq!(c().handle(Input::Esc), vec![]);
    }

    #[test]
    fn second_trigger_while_transcribing_is_ignored() {
        for gesture in [G::Pressed, G::ShowOverlay, G::HoldReleased, G::TapDiscarded, G::DoubleTap, G::Cancelled] {
            let mut c = transcribing();
            assert_eq!(g(&mut c, gesture), vec![], "{gesture:?}");
            assert_eq!(c.state(), State::Transcribing);
        }
    }

    #[test]
    fn a_transcript_is_pasted_trimmed() {
        let mut c = transcribing();
        assert_eq!(
            c.handle(Input::TranscriptReady(" hello world \n".into())),
            vec![Action::Paste("hello world".into()), Action::HideOverlay]
        );
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn empty_transcript_pastes_nothing() {
        let mut c = transcribing();
        assert_eq!(
            c.handle(Input::TranscriptReady("  ".into())),
            vec![Action::HideOverlay, Action::Notify("No speech heard".into())]
        );
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn finished_after_live_pasting_just_hides_the_overlay() {
        let mut t = transcribing();
        assert_eq!(t.handle(Input::Finished), vec![Action::HideOverlay]);
        assert_eq!(t.state(), State::Idle);
        // Not while listening, and not after Esc.
        for mut c in [hold(), toggle(), c()] {
            let before = c.state();
            assert_eq!(c.handle(Input::Finished), vec![]);
            assert_eq!(c.state(), before);
        }
    }

    #[test]
    fn failures_are_reported_and_nothing_is_pasted() {
        let mut c = transcribing();
        assert_eq!(
            c.handle(Input::TranscriptFailed("The speech service did not start".into())),
            vec![Action::HideOverlay, Action::Notify("The speech service did not start".into())]
        );
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn a_capture_failure_ends_listening() {
        for mut c in [pending(), hold(), toggle()] {
            assert_eq!(
                c.handle(Input::CaptureFailed("no microphone".into())),
                vec![Action::HideOverlay, Action::Notify("no microphone".into())]
            );
            assert_eq!(c.state(), State::Idle);
        }
        assert_eq!(c().handle(Input::CaptureFailed("x".into())), vec![]);
    }

    #[test]
    fn listening_stops_by_itself_at_the_maximum() {
        for mut c in [hold(), toggle()] {
            assert_eq!(c.handle(Input::Tick { listening_secs: 299 }), vec![]);
            assert_eq!(c.handle(Input::Tick { listening_secs: 300 }), vec![Action::FinishCapture]);
            assert_eq!(c.state(), State::Transcribing);
        }
        let mut idle = c();
        assert_eq!(idle.handle(Input::Tick { listening_secs: 9999 }), vec![]);
    }

    #[test]
    fn suspend_cancels_whatever_is_going_on() {
        let mut l = hold();
        assert_eq!(l.handle(Input::Suspend), vec![Action::CancelCapture, Action::HideOverlay]);
        let mut t = transcribing();
        assert_eq!(t.handle(Input::Suspend), vec![Action::CancelTranscription, Action::HideOverlay]);
        assert_eq!(c().handle(Input::Suspend), vec![]);
    }
}
