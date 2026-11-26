//! The dictation mic kept open between dictations (`[dictation] mic_ready`).
//!
//! Opening the mic on the key press loses the first words: a laptop mic gives
//! 0.5–1.2 s of nothing while Windows starts it. Kept open, its audio flows
//! into a short rolling buffer (`PREROLL_SECS`, in memory only, overwritten all
//! the time); a dictation `tap`s it and gets that moment before the press, then
//! the live audio, until the tap is dropped. The room's noise floor is learned
//! all along, so phrases are cut right from the first word.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::audio::capture::{CaptureStream, Source};
use crate::audio::{FRAME, SAMPLE_RATE};

use super::segmenter::{level_of, NoiseFloor};

/// How much audio from before the key press a dictation starts with. Short, so
/// the end of a conversation held before pressing is not picked up; the gesture
/// detector sees the press within a few ms.
pub const PREROLL_SECS: f32 = 0.3;
const PREROLL: usize = (SAMPLE_RATE as f32 * PREROLL_SECS) as usize;
/// A tap's ring: the dictation thread drains it every 10 ms.
const TAP_RING: usize = SAMPLE_RATE as usize * 2;
const POLL: Duration = Duration::from_millis(10);
const NO_FLOOR: u32 = u32::MAX;

/// Where the mic's audio goes: the rolling pre-roll while nobody listens, the
/// tap while a dictation does. Pure, for the tests.
struct Fanout {
    preroll: VecDeque<f32>,
    tap: Option<HeapProd<f32>>,
}

impl Fanout {
    fn new() -> Self {
        Self { preroll: VecDeque::with_capacity(PREROLL), tap: None }
    }

    fn feed(&mut self, samples: &[f32]) {
        if let Some(tap) = &mut self.tap {
            tap.push_slice(samples); // a full ring (a stalled dictation thread) drops audio, as a mic ring does
            return;
        }
        let keep = samples.len().min(PREROLL);
        let overflow = (self.preroll.len() + keep).saturating_sub(PREROLL);
        self.preroll.drain(..overflow);
        self.preroll.extend(&samples[samples.len() - keep..]);
    }

    /// Starts a tap: the pre-roll first, then everything that follows.
    fn attach(&mut self) -> HeapCons<f32> {
        let (mut prod, cons) = HeapRb::<f32>::new(TAP_RING).split();
        let (a, b) = self.preroll.as_slices();
        prod.push_slice(a);
        prod.push_slice(b);
        self.preroll.clear();
        self.tap = Some(prod);
        cons
    }

    fn detach(&mut self) {
        self.tap = None;
    }
}

struct Shared {
    stop: AtomicBool,
    fanout: Mutex<Fanout>,
    /// The learned noise floor (f32 bits); `NO_FLOOR` while unknown.
    floor: AtomicU32,
}

/// A dictation's share of the warm mic; the audio stops when it is dropped.
pub struct Tap {
    shared: Arc<Shared>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.shared.fanout.lock().unwrap().detach();
    }
}

pub struct WarmMic {
    stream: Option<CaptureStream>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    device: String,
}

impl WarmMic {
    /// Opens `device` (`""` = the Windows default) and keeps it open until dropped.
    pub fn open(device: &str) -> Self {
        let (prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 2).split();
        let id = Some(device.to_string()).filter(|s| !s.is_empty());
        let stream = CaptureStream::spawn(Source::Mic, id, prod);
        tracing::info!("dictation mic kept open ({} ms before each press)", (PREROLL_SECS * 1000.0) as u32);
        Self::with_ring(cons, Some(stream), device)
    }

    /// A warm mic fed from a ring instead of the microphone, for tests.
    #[cfg(test)]
    pub(super) fn from_ring(cons: HeapCons<f32>) -> Self {
        Self::with_ring(cons, None, "")
    }

    fn with_ring(cons: HeapCons<f32>, stream: Option<CaptureStream>, device: &str) -> Self {
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), fanout: Mutex::new(Fanout::new()), floor: AtomicU32::new(NO_FLOOR) });
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("dictation-warm-mic".into()).spawn(move || pump(cons, &s)).expect("spawn the warm mic thread");
        Self { stream, shared, thread: Some(thread), device: device.to_string() }
    }

    /// The configured device this was opened for (`""` = default).
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The audio for one dictation: the moment before now, then live, until the
    /// `Tap` is dropped. Also: how many samples of it are from before now.
    pub fn tap(&self) -> (HeapCons<f32>, Tap, usize) {
        let mut fanout = self.shared.fanout.lock().unwrap();
        let lead = fanout.preroll.len();
        let cons = fanout.attach();
        (cons, Tap { shared: self.shared.clone() }, lead)
    }

    /// The room's noise floor as learned so far; `None` until it is known.
    pub fn noise_floor(&self) -> Option<f32> {
        Some(self.shared.floor.load(Ordering::Relaxed)).filter(|b| *b != NO_FLOOR).map(f32::from_bits)
    }

    /// Reopens the device: the default mic changed, or the PC woke from sleep.
    pub fn restart(&self) {
        if let Some(s) = &self.stream {
            s.request_restart();
        }
    }
}

impl Drop for WarmMic {
    fn drop(&mut self) {
        self.stream.take();
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        tracing::info!("dictation mic closed");
    }
}

/// Moves the mic's audio to the fanout every 10 ms and learns the noise floor.
fn pump(mut cons: HeapCons<f32>, shared: &Shared) {
    let mut buf = vec![0f32; SAMPLE_RATE as usize / 10];
    let mut frame: Vec<f32> = Vec::with_capacity(FRAME);
    let mut floor = NoiseFloor::new(None);
    while !shared.stop.load(Ordering::Relaxed) {
        while cons.occupied_len() > 0 {
            let n = cons.pop_slice(&mut buf);
            shared.fanout.lock().unwrap().feed(&buf[..n]);
            for &s in &buf[..n] {
                frame.push(s);
                if frame.len() == FRAME {
                    floor.push(level_of(&frame));
                    frame.clear();
                }
            }
            if let Some(f) = floor.known() {
                shared.floor.store(f.to_bits(), Ordering::Relaxed);
            }
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: usize, n: usize) -> Vec<f32> {
        (from..from + n).map(|i| i as f32).collect()
    }

    fn drain(cons: &mut HeapCons<f32>) -> Vec<f32> {
        let mut out = vec![0f32; cons.occupied_len()];
        let n = cons.pop_slice(&mut out);
        out.truncate(n);
        out
    }

    #[test]
    fn the_preroll_keeps_only_the_last_moment() {
        let mut f = Fanout::new();
        f.feed(&ramp(0, PREROLL / 2));
        f.feed(&ramp(PREROLL / 2, PREROLL * 3));
        assert_eq!(f.preroll.len(), PREROLL);
        assert_eq!(*f.preroll.back().unwrap() as usize, PREROLL / 2 + PREROLL * 3 - 1);
        assert_eq!(*f.preroll.front().unwrap() as usize, PREROLL / 2 + PREROLL * 2);
    }

    #[test]
    fn a_tap_gets_the_preroll_then_the_live_audio_in_order() {
        let mut f = Fanout::new();
        f.feed(&ramp(0, PREROLL * 2));
        let mut cons = f.attach();
        f.feed(&ramp(PREROLL * 2, 500));
        let got = drain(&mut cons);
        assert_eq!(got, ramp(PREROLL, PREROLL + 500), "the last PREROLL samples, then live, nothing missing or repeated");
    }

    #[test]
    fn nothing_reaches_a_finished_tap_and_the_preroll_starts_again() {
        let mut f = Fanout::new();
        let mut cons = f.attach();
        f.feed(&ramp(0, 100));
        f.detach();
        f.feed(&ramp(100, 50));
        assert_eq!(drain(&mut cons), ramp(0, 100));
        assert_eq!(f.preroll.iter().map(|s| *s as usize).collect::<Vec<_>>(), (100..150).collect::<Vec<_>>());
    }

    #[test]
    fn before_any_audio_a_tap_starts_empty() {
        let mut f = Fanout::new();
        let mut cons = f.attach();
        assert!(drain(&mut cons).is_empty());
    }

    #[test]
    fn hundreds_of_dictations_in_a_row_each_get_an_unbroken_slice_of_the_audio() {
        // The mic keeps delivering (a counter, so gaps and repeats show) while dictations
        // start and stop as fast as Ctrl taps: each must get consecutive samples only.
        let (mut prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize).split();
        let mic = WarmMic::with_ring(cons, None, "");
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let feeder = std::thread::spawn(move || {
            let mut n = 0u32;
            while !s.load(Ordering::Relaxed) {
                let chunk: Vec<f32> = (0..480).map(|i| (n + i) as f32).collect(); // 10 ms
                n += prod.push_slice(&chunk) as u32;
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        let mut last_end = -1f32;
        for i in 0..300 {
            let (mut cons, tap, lead) = mic.tap();
            assert!(lead <= PREROLL);
            std::thread::sleep(Duration::from_millis(if i % 10 == 0 { 30 } else { 1 }));
            drop(tap);
            std::thread::sleep(Duration::from_millis(15));
            let got = drain(&mut cons);
            for w in got.windows(2) {
                assert_eq!(w[1], w[0] + 1.0, "dictation {i}: a gap or a repeat at {}", w[0]);
            }
            if let Some(first) = got.first() {
                assert!(*first > last_end - PREROLL as f32 - 1.0, "dictation {i} starts no earlier than its pre-roll");
                last_end = *got.last().unwrap();
            }
        }
        stop.store(true, Ordering::Relaxed);
        feeder.join().unwrap();
    }

    #[test]
    fn a_warm_mic_hands_its_audio_to_one_dictation_at_a_time() {
        let (mut prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize).split();
        let mic = WarmMic::with_ring(cons, None, "");
        for _ in 0..3 {
            prod.push_slice(&vec![0.01f32; SAMPLE_RATE as usize / 2]);
            std::thread::sleep(Duration::from_millis(60));
        }
        let (mut tap_cons, tap, lead) = mic.tap();
        assert_eq!(lead, PREROLL);
        prod.push_slice(&[0.5f32; 480]);
        std::thread::sleep(Duration::from_millis(100));
        let got = drain(&mut tap_cons);
        assert_eq!(got.len(), PREROLL + 480);
        assert!(got[..PREROLL].iter().all(|&s| s == 0.01) && got[PREROLL..].iter().all(|&s| s == 0.5));
        drop(tap);
        prod.push_slice(&[0.7f32; 480]);
        std::thread::sleep(Duration::from_millis(100));
        assert!(drain(&mut tap_cons).is_empty(), "a dropped tap gets nothing more");
        assert!(mic.noise_floor().is_some(), "the floor is learned from 1.5 s of quiet");
    }
}
