//! Cuts the live microphone audio into phrases at pauses, so each phrase can be
//! transcribed while the user is still talking. Pure: 20 ms frames in, finished
//! phrases (48 kHz mono samples) out.

use crate::audio::FRAME;

/// The lowest speech threshold (on `level_of`'s 0..1 scale; about -44 dBFS), and the
/// threshold while the room's noise floor is still unknown.
const SPEECH_LEVEL: f32 = 0.12;
/// The noise floor is the quietest frame of the last 3 s (minimum statistics):
/// pauses between words reach it even in continuous speech.
const FLOOR_FRAMES: usize = 150;
/// Without a seed, the floor is unknown (0) for the first second, so speech right
/// after the mic opens is not mistaken for the room.
const WARMUP_FRAMES: usize = 50;
/// Highest threshold (about -20 dBFS): a loud room can't make normal speech "silence".
pub const MAX_THRESHOLD: f32 = 0.6;
/// `level_of` units per dB.
const PER_DB: f32 = 1.0 / 50.0;
/// Speech must be this many dB above the noise floor (`[dictation] noise_margin_db`).
pub const DEFAULT_MARGIN_DB: f32 = 10.0;
/// This much silence after speech ends a phrase (0.6 s)...
const PAUSE_FRAMES: usize = 30;
/// ...once the phrase is `min_phrase` long; a pause this long (2 s) ends it whatever its length,
/// so a short remark is not held back (and padded with silence Whisper may fill with words).
const LONG_PAUSE_FRAMES: usize = 100;
/// Silence kept before and after the speech of a phrase (0.2 s).
const PAD_FRAMES: usize = 10;
/// Fewer speech frames than this before a pause is a click or a cough, not a phrase.
const MIN_SPEECH_FRAMES: usize = 5;
/// A phrase without a pause is cut at this length (15 s) ...
const MAX_FRAMES: usize = 750;
/// ... at the quietest frame of its last 3 s.
const CUT_SEARCH_FRAMES: usize = 150;

/// 0..1 from the frame's RMS: -50 dBFS (room noise) is 0, full scale is 1.
pub fn level_of(frame: &[f32]) -> f32 {
    let mean_square = frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32;
    if mean_square <= 0.0 {
        return 0.0;
    }
    ((10.0 * mean_square.log10() + 50.0) / 50.0).clamp(0.0, 1.0)
}

/// The room's noise level, learned from the quietest recent frames.
pub struct NoiseFloor {
    /// `level_of` the last `FLOOR_FRAMES` frames.
    recent: std::collections::VecDeque<f32>,
    /// A floor learned earlier (the previous dictation), trusted until `recent` is full.
    seed: Option<f32>,
}

impl NoiseFloor {
    pub fn new(seed: Option<f32>) -> Self {
        Self { recent: std::collections::VecDeque::with_capacity(FLOOR_FRAMES), seed }
    }

    pub fn push(&mut self, level: f32) {
        if self.recent.len() == FLOOR_FRAMES {
            self.recent.pop_front();
        }
        self.recent.push_back(level);
    }

    /// 0..1 on the `level_of` scale; 0 = too quiet to measure (or not known yet).
    pub fn level(&self) -> f32 {
        let lowest = self.recent.iter().copied().fold(f32::INFINITY, f32::min);
        match self.seed {
            Some(seed) if self.recent.len() < FLOOR_FRAMES => seed.min(lowest),
            None if self.recent.len() < WARMUP_FRAMES => 0.0,
            _ => lowest,
        }
    }

    /// The floor, once it is measured (or seeded); `None` during the warm-up.
    pub fn known(&self) -> Option<f32> {
        (self.seed.is_some() || self.recent.len() >= WARMUP_FRAMES).then(|| self.level())
    }

    /// Frames louder than this are speech.
    pub fn threshold(&self, margin_db: f32) -> f32 {
        let floor = self.level();
        if floor <= 0.0 {
            return SPEECH_LEVEL;
        }
        (floor + margin_db * PER_DB).clamp(SPEECH_LEVEL, MAX_THRESHOLD)
    }
}

pub struct Segmenter {
    /// The phrase being collected, whole frames.
    samples: Vec<f32>,
    /// `level_of` each frame in `samples`.
    levels: Vec<f32>,
    speech_frames: usize,
    /// Silent frames at the end of `samples`.
    silence_run: usize,
    floor: NoiseFloor,
    margin_db: f32,
    threshold: f32,
    /// A normal pause only ends a phrase this many frames long or longer (0 = any).
    min_phrase: usize,
}

impl Default for Segmenter {
    fn default() -> Self {
        Self::new(DEFAULT_MARGIN_DB, None)
    }
}

impl Segmenter {
    /// `margin_db`: how far above the noise floor speech must be; `seed`: a floor learned before.
    pub fn new(margin_db: f32, seed: Option<f32>) -> Self {
        let floor = NoiseFloor::new(seed);
        let threshold = floor.threshold(margin_db);
        Self { samples: Vec::new(), levels: Vec::new(), speech_frames: 0, silence_run: 0, floor, margin_db, threshold, min_phrase: 0 }
    }

    /// Phrases shorter than `secs` are not cut at a normal pause (`[dictation] min_phrase_secs`):
    /// a sentence read with small pauses ("4 7 3 9") reaches the transcriber whole, with its context.
    pub fn with_min_phrase(mut self, secs: f32) -> Self {
        self.min_phrase = (secs.max(0.0) * 50.0) as usize; // 50 frames a second
        self
    }

    pub fn noise_floor(&self) -> f32 {
        self.floor.level()
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// The floor to seed the next dictation with; `None` until it is known.
    pub fn learned_floor(&self) -> Option<f32> {
        self.floor.known()
    }

    /// Adds one frame (`FRAME` samples); returns a phrase when this frame completes one.
    pub fn push(&mut self, frame: &[f32]) -> Option<Vec<f32>> {
        debug_assert_eq!(frame.len(), FRAME);
        let level = level_of(frame);
        self.floor.push(level);
        let threshold = self.floor.threshold(self.margin_db);
        self.samples.extend_from_slice(frame);
        self.levels.push(level);
        if threshold != self.threshold {
            // The floor moved: judge what is buffered again (hum taken for speech
            // before the floor was known stops counting).
            self.threshold = threshold;
            self.recount();
        } else if level > threshold {
            self.speech_frames += 1;
            self.silence_run = 0;
        } else {
            self.silence_run += 1;
        }
        if self.speech_frames == 0 {
            // Only silence so far: keep just the lead-in.
            self.keep_last(PAD_FRAMES);
            return None;
        }
        let spoken = self.levels.len() - self.silence_run;
        if self.silence_run >= LONG_PAUSE_FRAMES || (self.silence_run >= PAUSE_FRAMES && spoken >= self.min_phrase) {
            let real = self.speech_frames >= MIN_SPEECH_FRAMES;
            let end = self.levels.len() - (self.silence_run - PAD_FRAMES);
            let phrase = real.then(|| self.samples[..end * FRAME].to_vec());
            self.keep_last(PAD_FRAMES);
            return phrase;
        }
        if self.levels.len() >= MAX_FRAMES {
            let from = self.levels.len() - CUT_SEARCH_FRAMES;
            let quietest = (from..self.levels.len()).min_by(|a, b| self.levels[*a].total_cmp(&self.levels[*b])).unwrap_or(from);
            let cut = quietest.max(1);
            let phrase = self.samples[..cut * FRAME].to_vec();
            self.keep_last(self.levels.len() - cut);
            return Some(phrase);
        }
        None
    }

    /// The recording ended: whatever speech is left.
    pub fn flush(&mut self) -> Option<Vec<f32>> {
        let end = self.levels.len() - self.silence_run.saturating_sub(PAD_FRAMES);
        let phrase = (self.speech_frames > 0).then(|| self.samples[..end * FRAME].to_vec());
        self.keep_last(0);
        phrase
    }

    /// Drops everything but the last `frames` frames and recounts.
    fn keep_last(&mut self, frames: usize) {
        let drop = self.levels.len().saturating_sub(frames);
        self.samples.drain(..drop * FRAME);
        self.levels.drain(..drop);
        self.recount();
    }

    fn recount(&mut self) {
        let t = self.threshold;
        self.speech_frames = self.levels.iter().filter(|l| **l > t).count();
        self.silence_run = self.levels.iter().rev().take_while(|l| **l <= t).count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SAMPLE_RATE;

    fn frames(secs: f64) -> usize {
        (secs * SAMPLE_RATE as f64) as usize / FRAME
    }

    fn speech() -> Vec<f32> {
        (0..FRAME).map(|i| 0.3 * (i as f32 * 0.1).sin()).collect()
    }

    fn silence() -> Vec<f32> {
        vec![0.0; FRAME]
    }

    #[test]
    fn a_short_sentence_with_small_pauses_stays_one_phrase() {
        // "The invoice number is 4 7 3 9": pauses of 0.6-1 s inside a 4 s sentence.
        let runs = [(true, 1.5), (false, 0.7), (true, 0.5), (false, 0.9), (true, 0.5), (false, 0.7), (true, 0.8), (false, 0.8)];
        let mut s = Segmenter::default().with_min_phrase(5.0);
        let mut out = feed(&mut s, &runs);
        out.extend(s.flush().map(|p| p.len() as f64 / SAMPLE_RATE as f64));
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0] > 5.0, "{out:?}");
        let mut old = Segmenter::default();
        assert_eq!(feed(&mut old, &runs).len(), 4, "without a minimum every pause cuts, as before");
    }

    #[test]
    fn a_long_dictation_is_still_cut_at_its_pauses_once_phrases_are_long_enough() {
        let mut s = Segmenter::default().with_min_phrase(5.0);
        let out = feed(&mut s, &[(true, 3.0), (false, 0.7), (true, 3.0), (false, 0.7), (true, 2.0), (false, 0.7), (true, 6.0), (false, 0.7)]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.iter().all(|p| *p >= 5.0), "{out:?}");
    }

    #[test]
    fn a_long_silence_ends_even_a_short_phrase() {
        let mut s = Segmenter::default().with_min_phrase(5.0);
        let out = feed(&mut s, &[(true, 1.0), (false, 2.5)]);
        assert_eq!(out.len(), 1, "a remark followed by 2 s of quiet is sent: {out:?}");
        assert!(out[0] < 1.6, "without the silence: {out:?}");
    }

    /// Feeds `(is_speech, secs)` runs; returns the phrases that came out, in seconds.
    fn feed(s: &mut Segmenter, runs: &[(bool, f64)]) -> Vec<f64> {
        let mut out = Vec::new();
        for (is_speech, secs) in runs {
            for _ in 0..frames(*secs) {
                let frame = if *is_speech { speech() } else { silence() };
                if let Some(p) = s.push(&frame) {
                    out.push(p.len() as f64 / SAMPLE_RATE as f64);
                }
            }
        }
        out
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn level_tracks_amplitude() {
        assert_eq!(level_of(&vec![0.0; FRAME]), 0.0);
        let quiet: Vec<f32> = speech().iter().map(|s| s * 0.002).collect();
        assert_eq!(level_of(&quiet), 0.0, "room noise is no level");
        let loud: Vec<f32> = speech().iter().map(|s| s * 3.0).collect();
        assert!(level_of(&speech()) > 0.2 && level_of(&speech()) < level_of(&loud));
        assert_eq!(level_of(&vec![1.0; FRAME]), 1.0);
    }

    /// A steady hum of `amp` (0.025 is about -35 dBFS, level 0.3: above the old fixed threshold).
    fn hum(amp: f32) -> Vec<f32> {
        (0..FRAME).map(|i| amp * (i as f32 * 0.05).sin()).collect()
    }

    /// Like `feed`, with `hum(noise)` in place of silence.
    fn feed_noisy(s: &mut Segmenter, noise: f32, runs: &[(bool, f64)]) -> Vec<f64> {
        let mut out = Vec::new();
        for (is_speech, secs) in runs {
            for _ in 0..frames(*secs) {
                let frame = if *is_speech { speech() } else { hum(noise) };
                if let Some(p) = s.push(&frame) {
                    out.push(p.len() as f64 / SAMPLE_RATE as f64);
                }
            }
        }
        out
    }

    #[test]
    fn steady_room_noise_is_learned_and_pauses_still_end_phrases() {
        let mut s = Segmenter::default();
        let out = feed_noisy(&mut s, 0.025, &[(false, 2.0), (true, 1.0), (false, 1.0), (true, 1.5), (false, 1.0)]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(close(out[0], 1.4) && close(out[1], 1.9), "{out:?}");
        assert!((s.noise_floor() - level_of(&hum(0.025))).abs() < 0.02, "floor {}", s.noise_floor());
        assert!(s.threshold() > level_of(&hum(0.025)));
    }

    #[test]
    fn noise_alone_gives_nothing() {
        let mut s = Segmenter::default();
        assert!(feed_noisy(&mut s, 0.025, &[(false, 5.0)]).is_empty());
        assert!(s.flush().is_none());
    }

    #[test]
    fn speech_right_after_the_mic_opens_is_kept() {
        let mut s = Segmenter::default();
        let out = feed(&mut s, &[(true, 1.0), (false, 1.0)]);
        assert!(out.len() == 1 && close(out[0], 1.2), "{out:?}");
    }

    #[test]
    fn a_seeded_floor_applies_at_once_and_falls_when_the_room_gets_quieter() {
        let noise = level_of(&hum(0.025));
        let mut s = Segmenter::new(10.0, Some(noise));
        assert!((s.noise_floor() - noise).abs() < 1e-6);
        // The hum is silence from the first frame: no phrase.
        assert!(feed_noisy(&mut s, 0.025, &[(false, 0.5), (true, 1.0), (false, 1.0)]).len() == 1);
        feed(&mut s, &[(false, 2.0)]);
        assert_eq!(s.noise_floor(), 0.0);
    }

    #[test]
    fn the_margin_sets_the_threshold_above_the_floor() {
        let s = Segmenter::new(10.0, Some(0.3));
        assert!((s.threshold() - 0.5).abs() < 1e-6, "10 dB = 0.2 on the level scale");
        assert_eq!(Segmenter::new(10.0, None).threshold(), SPEECH_LEVEL, "never below the old fixed level");
        assert!(Segmenter::new(30.0, Some(0.9)).threshold() <= MAX_THRESHOLD);
    }

    #[test]
    fn silence_alone_gives_nothing() {
        let mut s = Segmenter::default();
        assert!(feed(&mut s, &[(false, 5.0)]).is_empty());
        assert!(s.flush().is_none());
        assert!(s.samples.len() <= PAD_FRAMES * FRAME, "silence does not pile up");
    }

    #[test]
    fn a_pause_ends_a_phrase() {
        let mut s = Segmenter::default();
        // Nothing while the pause is still short.
        assert!(feed(&mut s, &[(false, 1.0), (true, 2.0), (false, 0.5)]).is_empty());
        let out = feed(&mut s, &[(false, 0.2)]);
        // 0.2 s lead-in + 2 s speech + 0.2 s tail.
        assert_eq!(out.len(), 1);
        assert!(close(out[0], 2.4), "{out:?}");
    }

    #[test]
    fn phrases_come_out_one_per_pause() {
        let mut s = Segmenter::default();
        let out = feed(&mut s, &[(true, 1.0), (false, 1.0), (true, 3.0), (false, 1.0), (true, 0.5)]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(close(out[0], 1.2) && close(out[1], 3.4), "{out:?}");
        // The last words have no pause after them: flush returns them.
        let tail = s.flush().expect("the tail");
        assert!(close(tail.len() as f64 / SAMPLE_RATE as f64, 0.7), "{}", tail.len());
        assert!(s.flush().is_none(), "flushed once");
    }

    #[test]
    fn a_short_gap_between_words_does_not_cut() {
        let mut s = Segmenter::default();
        assert!(feed(&mut s, &[(true, 1.0), (false, 0.3), (true, 1.0)]).is_empty());
        let out = feed(&mut s, &[(false, 1.0)]);
        assert!(out.len() == 1 && close(out[0], 2.5), "{out:?}");
    }

    #[test]
    fn a_click_is_not_a_phrase() {
        let mut s = Segmenter::default();
        assert!(feed(&mut s, &[(true, 0.06), (false, 2.0)]).is_empty());
        assert!(s.flush().is_none());
    }

    #[test]
    fn speech_without_a_pause_is_cut_at_its_quietest_recent_frame() {
        let mut s = Segmenter::default();
        // 13 s of speech, a 0.1 s dip (too short to be a pause), then more speech.
        let out = feed(&mut s, &[(true, 13.0), (false, 0.1), (true, 3.0)]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(close(out[0], 13.0), "cut in the dip: {out:?}");
        // The rest carries on as the next phrase, nothing lost.
        let rest = feed(&mut s, &[(false, 1.0)]);
        assert!(rest.len() == 1 && close(rest[0], 3.3), "{rest:?}");
    }

    #[test]
    fn no_audio_is_lost_or_repeated_across_a_forced_cut() {
        let mut s = Segmenter::default();
        let mut got = 0;
        let total = frames(40.0);
        for _ in 0..total {
            if let Some(p) = s.push(&speech()) {
                got += p.len();
            }
        }
        got += s.flush().map_or(0, |p| p.len());
        assert_eq!(got, total * FRAME);
    }
}
