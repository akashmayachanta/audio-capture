//! Turns raw key down/up events into dictation gestures. Pure: time comes in
//! as `now_ms`, never from a clock.
//!
//! With one key for both gestures (Right Alt by default) a press is a hold if it
//! lasts `HOLD_MS`, else a tap; two taps within `TAP_GAP_MS` are a double-tap.
//! `Pressed` fires at the key-down so capture can start without losing speech;
//! a tap then reports `TapDiscarded`.

use super::keys::KeyId;

/// Down for at least this long is a hold; shorter is a tap.
pub const HOLD_MS: u64 = 300;
/// A held key shows the overlay after this long (so a quick tap doesn't flash it).
pub const SHOW_OVERLAY_MS: u64 = 150;
/// Max gap between the first tap's release and the second tap's press.
pub const TAP_GAP_MS: u64 = 400;
/// Another key "held" for this long is assumed to have lost its key-up (Win+L, Ctrl+Alt+Del).
const STALE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: KeyId,
    pub down: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// The hold key went down (may still turn out to be a tap).
    Pressed,
    /// The hold key has been down long enough to show the overlay.
    ShowOverlay,
    /// A real hold ended.
    HoldReleased,
    /// A quick press of the hold key ended: not a dictation.
    TapDiscarded,
    /// The toggle key was double-tapped.
    DoubleTap,
    /// Another key went down during the press: it was a shortcut, not a dictation.
    Cancelled,
}

struct Tracked {
    key: KeyId,
    at: u64,
    shown: bool,
    cancelled: bool,
}

pub struct Detector {
    hold: Option<KeyId>,
    toggle: Option<KeyId>,
    tracked: Option<Tracked>,
    /// Other keys currently down, with when they went down.
    others: Vec<(KeyId, u64)>,
    /// The toggle key's last tap: (key, release time).
    last_tap: Option<(KeyId, u64)>,
}

impl Detector {
    pub fn new(hold: Option<KeyId>, toggle: Option<KeyId>) -> Self {
        Self { hold, toggle, tracked: None, others: Vec::new(), last_tap: None }
    }

    /// New keys from the config; a press in progress is forgotten.
    pub fn set_keys(&mut self, hold: Option<KeyId>, toggle: Option<KeyId>) {
        self.hold = hold;
        self.toggle = toggle;
        self.tracked = None;
        self.last_tap = None;
    }

    fn is_hold(&self, key: &KeyId) -> bool {
        self.hold.as_ref() == Some(key)
    }

    fn is_toggle(&self, key: &KeyId) -> bool {
        self.toggle.as_ref() == Some(key)
    }

    pub fn key(&mut self, ev: &KeyEvent, now_ms: u64) -> Vec<Gesture> {
        self.others.retain(|(_, at)| now_ms.saturating_sub(*at) < STALE_MS);
        if ev.down { self.on_down(&ev.key, now_ms) } else { self.on_up(&ev.key, now_ms) }
    }

    fn note_other(&mut self, key: &KeyId, now_ms: u64) {
        self.others.retain(|(k, _)| k != key);
        self.others.push((key.clone(), now_ms));
    }

    fn on_down(&mut self, key: &KeyId, now_ms: u64) -> Vec<Gesture> {
        if let Some(t) = &self.tracked {
            if t.key == *key {
                return vec![]; // auto-repeat
            }
            let (cancelled, tracked_is_hold) = (t.cancelled, self.is_hold(&t.key));
            self.last_tap = None;
            self.note_other(key, now_ms);
            if cancelled {
                return vec![];
            }
            self.tracked.as_mut().expect("checked above").cancelled = true;
            return if tracked_is_hold { vec![Gesture::Cancelled] } else { vec![] };
        }
        if !self.is_hold(key) && !self.is_toggle(key) {
            self.last_tap = None;
            self.note_other(key, now_ms);
            return vec![];
        }
        // Another key is already down: this is part of a shortcut.
        let blocked = !self.others.is_empty();
        if blocked {
            self.last_tap = None;
        }
        self.tracked = Some(Tracked { key: key.clone(), at: now_ms, shown: false, cancelled: blocked });
        if !blocked && self.is_hold(key) { vec![Gesture::Pressed] } else { vec![] }
    }

    fn on_up(&mut self, key: &KeyId, now_ms: u64) -> Vec<Gesture> {
        if self.tracked.as_ref().is_none_or(|t| t.key != *key) {
            self.others.retain(|(k, _)| k != key);
            return vec![];
        }
        let t = self.tracked.take().expect("checked above");
        if t.cancelled {
            self.last_tap = None;
            return vec![];
        }
        let (is_hold, is_toggle) = (self.is_hold(&t.key), self.is_toggle(&t.key));
        if now_ms.saturating_sub(t.at) >= HOLD_MS {
            self.last_tap = None;
            return if is_hold { vec![Gesture::HoldReleased] } else { vec![] };
        }
        let double = is_toggle
            && self.last_tap.as_ref().is_some_and(|(k, up)| *k == t.key && t.at.saturating_sub(*up) <= TAP_GAP_MS);
        if double {
            self.last_tap = None;
            return vec![Gesture::DoubleTap];
        }
        if is_toggle {
            self.last_tap = Some((t.key.clone(), now_ms));
        }
        if is_hold { vec![Gesture::TapDiscarded] } else { vec![] }
    }

    /// Call regularly (the app loop does): reports `ShowOverlay` once a hold is long enough.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Gesture> {
        let hold = self.hold.clone();
        match &mut self.tracked {
            Some(t)
                if !t.cancelled
                    && !t.shown
                    && hold.as_ref() == Some(&t.key)
                    && now_ms.saturating_sub(t.at) >= SHOW_OVERLAY_MS =>
            {
                t.shown = true;
                vec![Gesture::ShowOverlay]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> KeyId {
        KeyId::parse(s).unwrap()
    }

    fn ev(key: &str, down: bool) -> KeyEvent {
        KeyEvent { key: k(key), down }
    }

    /// Right Alt for both gestures, the default.
    fn detector() -> Detector {
        Detector::new(Some(k("RightAlt")), Some(k("RightAlt")))
    }

    fn down(d: &mut Detector, key: &str, at: u64) -> Vec<Gesture> {
        d.key(&ev(key, true), at)
    }

    fn up(d: &mut Detector, key: &str, at: u64) -> Vec<Gesture> {
        d.key(&ev(key, false), at)
    }

    #[test]
    fn quick_tap_is_pressed_then_discarded() {
        let mut d = detector();
        assert_eq!(down(&mut d, "RightAlt", 0), vec![Gesture::Pressed]);
        assert_eq!(up(&mut d, "RightAlt", 100), vec![Gesture::TapDiscarded]);
    }

    #[test]
    fn hold_shows_overlay_once_then_releases() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        assert_eq!(d.tick(100), vec![]);
        assert_eq!(d.tick(160), vec![Gesture::ShowOverlay]);
        assert_eq!(d.tick(200), vec![]);
        assert_eq!(up(&mut d, "RightAlt", 800), vec![Gesture::HoldReleased]);
    }

    #[test]
    fn hold_threshold_is_300_ms() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        assert_eq!(up(&mut d, "RightAlt", 299), vec![Gesture::TapDiscarded]);
        down(&mut d, "RightAlt", 1000);
        assert_eq!(up(&mut d, "RightAlt", 1300), vec![Gesture::HoldReleased]);
    }

    #[test]
    fn double_tap_on_the_same_key() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        up(&mut d, "RightAlt", 80);
        assert_eq!(down(&mut d, "RightAlt", 300), vec![Gesture::Pressed]);
        assert_eq!(up(&mut d, "RightAlt", 380), vec![Gesture::DoubleTap]);
    }

    #[test]
    fn a_third_tap_starts_over() {
        let mut d = detector();
        for t in [0, 200] {
            down(&mut d, "RightAlt", t);
            up(&mut d, "RightAlt", t + 50);
        }
        down(&mut d, "RightAlt", 400);
        assert_eq!(up(&mut d, "RightAlt", 450), vec![Gesture::TapDiscarded]);
    }

    #[test]
    fn slow_second_tap_is_a_new_first_tap() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        up(&mut d, "RightAlt", 80);
        down(&mut d, "RightAlt", 600);
        assert_eq!(up(&mut d, "RightAlt", 680), vec![Gesture::TapDiscarded]);
    }

    #[test]
    fn a_long_second_press_is_a_hold_not_a_double_tap() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        up(&mut d, "RightAlt", 80);
        assert_eq!(down(&mut d, "RightAlt", 300), vec![Gesture::Pressed]);
        assert_eq!(up(&mut d, "RightAlt", 900), vec![Gesture::HoldReleased]);
    }

    #[test]
    fn other_key_while_held_cancels() {
        let mut d = detector();
        assert_eq!(down(&mut d, "RightAlt", 0), vec![Gesture::Pressed]);
        assert_eq!(down(&mut d, "vk:0x45", 50), vec![Gesture::Cancelled]);
        assert_eq!(down(&mut d, "vk:0x46", 60), vec![], "cancelled only once");
        assert_eq!(up(&mut d, "RightAlt", 100), vec![]);
        assert_eq!(d.tick(500), vec![]);
    }

    #[test]
    fn other_key_held_before_the_key_blocks_the_gesture() {
        let mut d = detector();
        down(&mut d, "vk:0x45", 0);
        assert_eq!(down(&mut d, "RightAlt", 10), vec![]);
        assert_eq!(up(&mut d, "RightAlt", 50), vec![]);
    }

    #[test]
    fn a_stale_other_key_does_not_block_forever() {
        // A key-up swallowed by Win+L or Ctrl+Alt+Del must not disable dictation.
        let mut d = detector();
        down(&mut d, "vk:0x45", 0);
        assert_eq!(down(&mut d, "RightAlt", 20_000), vec![Gesture::Pressed]);
    }

    #[test]
    fn other_key_between_taps_breaks_the_double_tap() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        up(&mut d, "RightAlt", 80);
        down(&mut d, "vk:0x45", 150);
        up(&mut d, "vk:0x45", 200);
        down(&mut d, "RightAlt", 300);
        assert_eq!(up(&mut d, "RightAlt", 380), vec![Gesture::TapDiscarded]);
    }

    #[test]
    fn auto_repeat_is_ignored() {
        let mut d = detector();
        assert_eq!(down(&mut d, "RightAlt", 0), vec![Gesture::Pressed]);
        assert_eq!(down(&mut d, "RightAlt", 500), vec![]);
        assert_eq!(down(&mut d, "RightAlt", 530), vec![]);
        assert_eq!(up(&mut d, "RightAlt", 800), vec![Gesture::HoldReleased]);
    }

    #[test]
    fn separate_toggle_key_only_double_taps() {
        let mut d = Detector::new(Some(k("RightAlt")), Some(k("RightCtrl")));
        assert_eq!(down(&mut d, "RightCtrl", 0), vec![]);
        assert_eq!(up(&mut d, "RightCtrl", 80), vec![]);
        assert_eq!(down(&mut d, "RightCtrl", 200), vec![]);
        assert_eq!(up(&mut d, "RightCtrl", 280), vec![Gesture::DoubleTap]);
        // The hold key never double-taps.
        down(&mut d, "RightAlt", 1000);
        up(&mut d, "RightAlt", 1050);
        down(&mut d, "RightAlt", 1100);
        assert_eq!(up(&mut d, "RightAlt", 1150), vec![Gesture::TapDiscarded]);
    }

    #[test]
    fn a_long_press_of_the_toggle_key_does_nothing() {
        let mut d = Detector::new(None, Some(k("RightCtrl")));
        down(&mut d, "RightCtrl", 0);
        assert_eq!(d.tick(500), vec![]);
        assert_eq!(up(&mut d, "RightCtrl", 900), vec![]);
    }

    #[test]
    fn no_hold_key_means_no_hold_gestures() {
        let mut d = Detector::new(None, Some(k("RightAlt")));
        assert_eq!(down(&mut d, "RightAlt", 0), vec![]);
        assert_eq!(d.tick(200), vec![]);
        assert_eq!(up(&mut d, "RightAlt", 800), vec![]);
        down(&mut d, "RightAlt", 1000);
        up(&mut d, "RightAlt", 1050);
        down(&mut d, "RightAlt", 1200);
        assert_eq!(up(&mut d, "RightAlt", 1250), vec![Gesture::DoubleTap]);
    }

    #[test]
    fn unrelated_keys_do_nothing_when_idle() {
        let mut d = detector();
        assert_eq!(down(&mut d, "vk:0x45", 0), vec![]);
        assert_eq!(up(&mut d, "vk:0x45", 40), vec![]);
        assert_eq!(d.tick(1000), vec![]);
        let mut off = Detector::new(None, None);
        assert_eq!(down(&mut off, "RightAlt", 0), vec![]);
        assert_eq!(up(&mut off, "RightAlt", 50), vec![]);
    }

    #[test]
    fn changing_the_keys_forgets_a_press_in_progress() {
        let mut d = detector();
        down(&mut d, "RightAlt", 0);
        d.set_keys(Some(k("RightAlt")), Some(k("RightAlt")));
        assert_eq!(up(&mut d, "RightAlt", 800), vec![]);
        assert_eq!(d.tick(900), vec![]);
    }
}
