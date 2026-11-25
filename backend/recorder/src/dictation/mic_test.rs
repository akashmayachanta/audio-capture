//! Settings' **Test microphone**: the dictation phrase cutter (`Segmenter`) run
//! on the live mic in the window process, so the user can see the level, the
//! learned noise floor, the speech threshold and where phrases end. Nothing is
//! recorded or transcribed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::HeapRb;
use serde::Serialize;

use super::segmenter::{level_of, Segmenter};
use crate::audio::capture::{CaptureStream, Source};
use crate::audio::{FRAME, SAMPLE_RATE};

/// The page polls; when it stops (window closed, tab hidden) the mic is released.
const IDLE_STOP: Duration = Duration::from_secs(5);
/// A forgotten test doesn't hold the mic forever.
const MAX_RUN: Duration = Duration::from_secs(300);

/// What the page shows; levels on `level_of`'s 0..1 scale (-50..0 dBFS).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MicView {
    pub level: f32,
    /// `None` while still being measured (the first second).
    pub floor: Option<f32>,
    pub threshold: f32,
    /// The last frame counted as speech.
    pub speech: bool,
    /// Phrases cut so far (each would be sent to the transcriber on its own).
    pub phrases: u32,
    pub last_phrase_secs: Option<f64>,
    pub secs: f64,
}

/// Pure: frames in, `MicView` out.
pub struct MicTest {
    seg: Segmenter,
    view: MicView,
    frames: u64,
}

impl MicTest {
    pub fn new(margin_db: f32) -> Self {
        let seg = Segmenter::new(margin_db, None);
        let view = MicView { threshold: seg.threshold(), ..Default::default() };
        Self { seg, view, frames: 0 }
    }

    pub fn push(&mut self, frame: &[f32]) {
        let level = level_of(frame);
        if let Some(phrase) = self.seg.push(frame) {
            self.view.phrases += 1;
            self.view.last_phrase_secs = Some(phrase.len() as f64 / SAMPLE_RATE as f64);
        }
        self.frames += 1;
        self.view.level = level;
        self.view.threshold = self.seg.threshold();
        self.view.floor = self.seg.learned_floor();
        self.view.speech = level > self.view.threshold;
        self.view.secs = (self.frames * FRAME as u64) as f64 / SAMPLE_RATE as f64;
    }

    pub fn view(&self) -> MicView {
        self.view.clone()
    }
}

/// A running test: the mic, and a thread feeding `MicTest`.
pub struct MicTestRun {
    stop: Arc<AtomicBool>,
    view: Arc<Mutex<MicView>>,
    polled: Arc<Mutex<Instant>>,
    thread: Option<JoinHandle<()>>,
}

impl MicTestRun {
    /// `device`: a capture endpoint id; `None` = the Windows default.
    pub fn start(device: Option<String>, margin_db: f32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let view = Arc::new(Mutex::new(MicView::default()));
        let polled = Arc::new(Mutex::new(Instant::now()));
        let (s, v, p) = (stop.clone(), view.clone(), polled.clone());
        let thread = std::thread::Builder::new()
            .name("mic-test".into())
            .spawn(move || {
                let (prod, mut cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize).split();
                let _stream = CaptureStream::spawn(Source::Mic, device, prod);
                let mut test = MicTest::new(margin_db);
                let started = Instant::now();
                let mut frame = [0f32; FRAME];
                while !s.load(Ordering::Relaxed) && started.elapsed() < MAX_RUN && p.lock().map(|t| t.elapsed() < IDLE_STOP).unwrap_or(false) {
                    let mut fed = false;
                    while cons.occupied_len() >= FRAME {
                        cons.pop_slice(&mut frame);
                        test.push(&frame);
                        fed = true;
                    }
                    if fed {
                        if let Ok(mut v) = v.lock() {
                            *v = test.view();
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                tracing::info!("mic test stopped after {:.0} s", started.elapsed().as_secs_f64());
            })
            .ok();
        Self { stop, view, polled, thread }
    }

    /// The latest view; also tells the thread the page is still watching.
    pub fn view(&self) -> Option<MicView> {
        if self.is_finished() {
            return None;
        }
        if let Ok(mut t) = self.polled.lock() {
            *t = Instant::now();
        }
        self.view.lock().ok().map(|v| v.clone())
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for MicTestRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32) -> Vec<f32> {
        (0..FRAME).map(|i| amp * (i as f32 * 0.05).sin()).collect()
    }

    fn run(t: &mut MicTest, amp: f32, secs: f64) {
        for _ in 0..(secs * 50.0) as usize {
            t.push(&tone(amp));
        }
    }

    #[test]
    fn the_view_follows_the_room_and_the_voice() {
        let mut t = MicTest::new(10.0);
        assert_eq!(t.view().floor, None);
        run(&mut t, 0.025, 2.0); // hum
        let v = t.view();
        assert!(v.floor.is_some_and(|f| (f - v.level).abs() < 0.02), "{v:?}");
        assert!(!v.speech && v.threshold > v.level, "{v:?}");
        run(&mut t, 0.3, 1.0); // a voice
        assert!(t.view().speech);
        run(&mut t, 0.025, 1.0);
        let v = t.view();
        assert_eq!(v.phrases, 1, "{v:?}");
        assert!(v.last_phrase_secs.is_some_and(|s| (s - 1.4).abs() < 0.05), "{v:?}");
        assert!((v.secs - 4.0).abs() < 0.01);
    }

    #[test]
    fn a_bigger_margin_raises_the_threshold() {
        let mut quiet = MicTest::new(6.0);
        let mut strict = MicTest::new(20.0);
        // About -45 dBFS: neither threshold reaches the cap.
        run(&mut quiet, 0.008, 2.0);
        run(&mut strict, 0.008, 2.0);
        assert!(strict.view().threshold > quiet.view().threshold + 0.2);
    }

    #[test]
    #[ignore = "needs a microphone"]
    fn the_live_test_reports_the_mic() {
        let run = MicTestRun::start(None, 10.0);
        std::thread::sleep(Duration::from_millis(1500));
        let v = run.view().expect("running");
        assert!(v.secs > 1.0 && v.floor.is_some(), "{v:?}");
    }
}
