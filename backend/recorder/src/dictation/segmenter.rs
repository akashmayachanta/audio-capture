//! Cuts the live microphone audio into phrases at pauses, so each phrase can be
//! transcribed while the user is still talking. Pure: 20 ms frames in, finished
//! phrases (48 kHz mono samples) out.

use crate::audio::FRAME;

/// A frame louder than this (on `level_of`'s 0..1 scale; about -44 dBFS) counts as speech.
const SPEECH_LEVEL: f32 = 0.12;
/// This much silence after speech ends a phrase (0.6 s).
const PAUSE_FRAMES: usize = 30;
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

#[derive(Default)]
pub struct Segmenter {
    /// The phrase being collected, whole frames.
    samples: Vec<f32>,
    /// `level_of` each frame in `samples`.
    levels: Vec<f32>,
    speech_frames: usize,
    /// Silent frames at the end of `samples`.
    silence_run: usize,
}

impl Segmenter {
    /// Adds one frame (`FRAME` samples); returns a phrase when this frame completes one.
    pub fn push(&mut self, frame: &[f32]) -> Option<Vec<f32>> {
        debug_assert_eq!(frame.len(), FRAME);
        let level = level_of(frame);
        self.samples.extend_from_slice(frame);
        self.levels.push(level);
        if level > SPEECH_LEVEL {
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
        if self.silence_run >= PAUSE_FRAMES {
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
        self.speech_frames = self.levels.iter().filter(|l| **l > SPEECH_LEVEL).count();
        self.silence_run = self.levels.iter().rev().take_while(|l| **l <= SPEECH_LEVEL).count();
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
