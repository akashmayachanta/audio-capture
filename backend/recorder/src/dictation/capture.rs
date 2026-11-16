//! Records the mic for one dictation: a thread drains the capture ring into a
//! mono Ogg Opus file, publishes the level for the overlay and, when asked,
//! hands out each phrase (cut at pauses) as soon as it is complete.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::{HeapCons, HeapRb};

use crate::audio::capture::{CaptureStream, Source};
use crate::audio::{FRAME, SAMPLE_RATE};
use crate::config::Config;
use crate::encode::opus_ogg::OggOpusWriter;
use crate::recordings::DICTATION_DIR;
use crate::session::{append_ext, rename_with_retry};

use super::segmenter::{level_of, Segmenter};

/// Shorter than this is a blip (a tap that got through, a cough), not speech.
const MIN_CLIP_SECS: f64 = 0.3;

type Writer = OggOpusWriter<BufWriter<File>>;

/// A finished dictation.
pub struct Clip {
    /// Where it was saved (deleted again when audio is not kept).
    pub path: PathBuf,
    /// The whole Ogg Opus file, for the transcriber.
    pub bytes: Vec<u8>,
    pub secs: f64,
}

/// `<output_dir>/dictation/YYYY/MM/YYYY-MM-DD_HHMMSS.opus`, `_2`, `_3`, ... if taken.
pub fn clip_path(output_dir: &Path, start: DateTime<Local>) -> PathBuf {
    let dir = output_dir
        .join(DICTATION_DIR)
        .join(start.format("%Y").to_string())
        .join(start.format("%m").to_string());
    let stem = start.format("%Y-%m-%d_%H%M%S").to_string();
    let taken = |p: &Path| p.exists() || append_ext(p, ".part").exists();
    let mut path = dir.join(format!("{stem}.opus"));
    let mut n = 2;
    while taken(&path) {
        path = dir.join(format!("{stem}_{n}.opus"));
        n += 1;
    }
    path
}

/// A phrase (48 kHz mono samples) as a small Ogg Opus clip of its own, for the transcriber.
pub fn encode_clip(samples: &[f32], bitrate_bps: i32) -> Result<Vec<u8>> {
    let mut writer = OggOpusWriter::new(Vec::new(), 1, bitrate_bps, &[])?;
    let mut frame = [0f32; FRAME];
    for chunk in samples.chunks(FRAME) {
        frame[..chunk.len()].copy_from_slice(chunk);
        frame[chunk.len()..].fill(0.0);
        writer.write_frame(&frame)?;
    }
    writer.finish()
}

/// Encodes every whole frame waiting in `cons`, showing each to `on_frame`; returns how many.
fn pump<W: Write>(
    cons: &mut HeapCons<f32>,
    writer: &mut OggOpusWriter<W>,
    level: &AtomicU32,
    on_frame: &mut dyn FnMut(&[f32]),
) -> Result<usize> {
    let mut frame = [0f32; FRAME];
    let mut frames = 0;
    while cons.occupied_len() >= FRAME {
        cons.pop_slice(&mut frame);
        level.store(level_of(&frame).to_bits(), Ordering::Relaxed);
        writer.write_frame(&frame)?;
        on_frame(&frame);
        frames += 1;
    }
    Ok(frames)
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    /// The encoder or the disk failed; nothing more will be written.
    dead: AtomicBool,
    level: AtomicU32,
    samples: AtomicU64,
}

/// `phrases` gets every phrase as it completes and the last one when the recording
/// stops; it is dropped (closing the channel) when this returns.
fn run(mut cons: HeapCons<f32>, mut writer: Writer, shared: &Shared, phrases: Option<Sender<Vec<f32>>>) -> Result<Writer> {
    let mut segmenter = Segmenter::default();
    loop {
        // Read the flag first, so one more drain always follows the stop request.
        let stop = shared.stop.load(Ordering::Acquire);
        let mut on_frame = |frame: &[f32]| {
            if let (Some(tx), Some(phrase)) = (&phrases, segmenter.push(frame)) {
                let _ = tx.send(phrase);
            }
        };
        match pump(&mut cons, &mut writer, &shared.level, &mut on_frame) {
            Ok(_) => shared.samples.store(writer.samples_written(), Ordering::Relaxed),
            Err(e) => {
                shared.dead.store(true, Ordering::Relaxed);
                return Err(e);
            }
        }
        if stop {
            if let (Some(tx), Some(phrase)) = (&phrases, segmenter.flush()) {
                let _ = tx.send(phrase);
            }
            return Ok(writer);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub struct Capture {
    stream: Option<CaptureStream>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Result<Writer>>>,
    part: PathBuf,
    path: PathBuf,
}

impl Capture {
    /// Opens the configured mic (the Windows default when none is chosen) and starts recording.
    /// `phrases` receives each phrase as it is spoken; the channel closes when the recording ends.
    pub fn start(cfg: &Config, now: DateTime<Local>, phrases: Option<Sender<Vec<f32>>>) -> Result<Capture> {
        let (prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 2).split();
        let mic_id = Some(cfg.mic_device.clone()).filter(|s| !s.is_empty());
        let stream = CaptureStream::spawn(Source::Mic, mic_id, prod);
        Self::with_ring(clip_path(&cfg.output_dir, now), cons, cfg.bitrate_kbps as i32 * 1000, Some(stream), phrases)
    }

    /// A capture fed from a ring instead of the microphone, for live tests.
    #[cfg(test)]
    pub(super) fn from_ring(cfg: &Config, now: DateTime<Local>, cons: HeapCons<f32>, phrases: Option<Sender<Vec<f32>>>) -> Result<Capture> {
        Self::with_ring(clip_path(&cfg.output_dir, now), cons, cfg.bitrate_kbps as i32 * 1000, None, phrases)
    }

    /// The part everything but the microphone: audio arrives through `cons`.
    fn with_ring(
        path: PathBuf,
        cons: HeapCons<f32>,
        bitrate_bps: i32,
        stream: Option<CaptureStream>,
        phrases: Option<Sender<Vec<f32>>>,
    ) -> Result<Capture> {
        std::fs::create_dir_all(path.parent().context("clip path has no folder")?)?;
        let part = append_ext(&path, ".part");
        let file = File::create(&part).with_context(|| format!("create {}", part.display()))?;
        let date = Local::now().to_rfc3339();
        let tags = [("MEETREC_CHANNELS", "mono=mic"), ("DATE", date.as_str())];
        let writer = OggOpusWriter::new(BufWriter::new(file), 1, bitrate_bps, &tags)?;
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("dictation-capture".into())
            .spawn(move || run(cons, writer, &s, phrases))
            .context("spawn the dictation thread")?;
        Ok(Capture { stream, shared, thread: Some(thread), part, path })
    }

    /// Mic level for the overlay, 0..1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.shared.samples.load(Ordering::Relaxed) as f64 / SAMPLE_RATE as f64
    }

    /// False once the encoder or the disk has failed.
    pub fn is_alive(&self) -> bool {
        !self.shared.dead.load(Ordering::Relaxed)
    }

    /// Stops the mic and the thread; gives back the writer (not yet finished).
    fn stop(&mut self) -> Result<Writer> {
        self.stream.take(); // no more audio
        self.shared.stop.store(true, Ordering::Release);
        let thread = self.thread.take().ok_or_else(|| anyhow!("already stopped"))?;
        thread.join().map_err(|_| anyhow!("the dictation thread panicked"))?
    }

    /// Ends the recording. `None` when it was too short to be speech. The file is
    /// kept under `dictation/` when `keep_audio`, either way the bytes come back.
    pub fn finish(mut self, keep_audio: bool) -> Result<Option<Clip>> {
        let writer = self.stop().inspect_err(|_| {
            let _ = std::fs::remove_file(&self.part);
        })?;
        let secs = writer.samples_written() as f64 / SAMPLE_RATE as f64;
        drop(writer.finish()?);
        if secs < MIN_CLIP_SECS {
            let _ = std::fs::remove_file(&self.part);
            return Ok(None);
        }
        rename_with_retry(&self.part, &self.path)?;
        let bytes = std::fs::read(&self.path)?;
        if !keep_audio {
            let _ = std::fs::remove_file(&self.path);
        }
        Ok(Some(Clip { path: self.path.clone(), bytes, secs }))
    }

    /// Ends the recording without transcribing it. A real clip is still saved when `keep_audio`.
    pub fn cancel(mut self, keep_audio: bool) {
        let secs = match self.stop() {
            Ok(writer) => {
                let secs = writer.samples_written() as f64 / SAMPLE_RATE as f64;
                drop(writer.finish());
                secs
            }
            Err(_) => 0.0,
        };
        if keep_audio && secs >= MIN_CLIP_SECS && rename_with_retry(&self.part, &self.path).is_ok() {
            return;
        }
        let _ = std::fs::remove_file(&self.part);
    }
}

impl Drop for Capture {
    /// Dropped without `finish`/`cancel` (the app is quitting): leave no half file behind.
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop();
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ringbuf::traits::{Observer, Producer, Split};
    use ringbuf::{HeapProd, HeapRb};
    use std::sync::atomic::AtomicU32;
    use std::time::{Duration, Instant};

    fn at() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 16, 9, 5, 7).unwrap()
    }

    fn sine(samples: usize, amp: f32) -> Vec<f32> {
        (0..samples).map(|i| amp * (i as f32 * 0.1).sin()).collect()
    }

    fn secs(s: f64) -> usize {
        (s * SAMPLE_RATE as f64) as usize
    }

    /// A capture fed from the returned producer instead of a microphone.
    fn capture_in(dir: &Path) -> (Capture, HeapProd<f32>) {
        let (prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 4).split();
        let cap = Capture::with_ring(clip_path(dir, at()), cons, 24_000, None, None).unwrap();
        (cap, prod)
    }

    #[test]
    fn phrases_are_handed_out_while_recording_and_the_tail_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let (prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 6).split();
        let mut prod: HeapProd<f32> = prod;
        let (tx, rx) = std::sync::mpsc::channel();
        let cap = Capture::with_ring(clip_path(dir.path(), at()), cons, 24_000, None, Some(tx)).unwrap();
        // A phrase, a pause, then words that are still being spoken at the release.
        prod.push_slice(&sine(secs(1.0), 0.3));
        prod.push_slice(&vec![0.0; secs(1.0)]);
        prod.push_slice(&sine(secs(0.5), 0.3));
        let first = rx.recv_timeout(Duration::from_secs(3)).expect("the first phrase, before the recording ends");
        assert!((first.len() as f64 / SAMPLE_RATE as f64 - 1.2).abs() < 0.1, "{}", first.len());
        let clip = cap.finish(true).unwrap().expect("the whole clip is still saved");
        assert!((clip.secs - 2.5).abs() < 0.05, "{}", clip.secs);
        let tail = rx.recv_timeout(Duration::from_secs(3)).expect("the tail");
        assert!(tail.len() >= secs(0.5), "{}", tail.len());
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_err(), "then the channel closes");
    }

    #[test]
    fn a_phrase_encodes_to_an_ogg_opus_clip() {
        let bytes = encode_clip(&sine(secs(0.5) + 17, 0.3), 24_000).unwrap();
        assert!(bytes.starts_with(b"OggS"));
        assert!(bytes.len() > 500, "{}", bytes.len());
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                if e.path().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.push(e.path());
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, &mut out);
        out
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let t = Instant::now();
        while !ok() {
            assert!(t.elapsed() < Duration::from_secs(3), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn clip_path_has_date_folders_under_dictation() {
        let p = clip_path(Path::new("C:/rec"), at());
        assert_eq!(p, Path::new("C:/rec/dictation/2026/11/2026-11-16_090507.opus"));
    }

    #[test]
    fn clip_path_gets_a_suffix_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        let first = clip_path(dir.path(), at());
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = clip_path(dir.path(), at());
        assert!(second.to_string_lossy().ends_with("2026-11-16_090507_2.opus"), "{second:?}");
        std::fs::write(append_ext(&second, ".part"), b"x").unwrap();
        assert!(clip_path(dir.path(), at()).to_string_lossy().ends_with("_3.opus"));
    }

    #[test]
    fn pump_encodes_whole_frames_only() {
        let (mut prod, mut cons) = HeapRb::<f32>::new(FRAME * 4).split();
        prod.push_slice(&sine(FRAME * 2 + FRAME / 2, 0.3));
        let mut writer = OggOpusWriter::new(Vec::new(), 1, 24_000, &[]).unwrap();
        let level = AtomicU32::new(0);
        let mut seen = 0;
        assert_eq!(pump(&mut cons, &mut writer, &level, &mut |f| seen += f.len()).unwrap(), 2);
        assert_eq!(seen, FRAME * 2);
        assert_eq!(cons.occupied_len(), FRAME / 2, "the half frame waits for the rest");
        assert_eq!(writer.samples_written(), FRAME as u64 * 2);
    }

    #[test]
    fn level_and_elapsed_follow_the_audio() {
        let dir = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(dir.path());
        assert!(cap.is_alive());
        prod.push_slice(&sine(secs(1.0), 0.5));
        wait_for("the level", || cap.level() > 0.2);
        wait_for("one second of audio", || cap.elapsed_secs() >= 0.99);
        cap.cancel(false);
    }

    #[test]
    fn a_blip_is_not_a_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(dir.path());
        prod.push_slice(&sine(secs(0.1), 0.5));
        assert!(cap.finish(true).unwrap().is_none());
        assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
    }

    #[test]
    fn finish_renames_the_part_and_returns_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(dir.path());
        prod.push_slice(&sine(secs(1.0), 0.5));
        let clip = cap.finish(true).unwrap().expect("a clip");
        assert_eq!(files(dir.path()), vec![clip.path.clone()]);
        assert_eq!(clip.path.extension().unwrap(), "opus");
        assert_eq!(clip.bytes, std::fs::read(&clip.path).unwrap());
        assert!(clip.bytes.starts_with(b"OggS"));
        assert!((clip.secs - 1.0).abs() < 0.03, "{}", clip.secs);
    }

    #[test]
    fn not_keeping_the_audio_deletes_the_file_but_returns_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(dir.path());
        prod.push_slice(&sine(secs(1.0), 0.5));
        let clip = cap.finish(false).unwrap().expect("a clip");
        assert!(clip.bytes.starts_with(b"OggS"));
        assert!(files(dir.path()).is_empty(), "{:?}", files(dir.path()));
    }

    #[test]
    fn cancel_keeps_a_real_clip_only_when_asked() {
        let kept = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(kept.path());
        prod.push_slice(&sine(secs(1.0), 0.5));
        cap.cancel(true);
        let found = files(kept.path());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].extension().unwrap(), "opus");

        let dropped = tempfile::tempdir().unwrap();
        let (cap, mut prod) = capture_in(dropped.path());
        prod.push_slice(&sine(secs(1.0), 0.5));
        cap.cancel(false);
        assert!(files(dropped.path()).is_empty());
    }

    #[test]
    #[ignore = "needs a microphone"]
    fn records_a_second_from_the_mic() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.output_dir = dir.path().to_path_buf();
        let cap = Capture::start(&cfg, Local::now(), None).unwrap();
        std::thread::sleep(Duration::from_millis(1300));
        let clip = cap.finish(true).unwrap().expect("a clip");
        assert!(clip.secs > 0.8, "{}", clip.secs);
    }
}
