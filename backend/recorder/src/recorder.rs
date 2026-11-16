//! A running recording: capture streams → mixer → session, on an engine thread.
//!
//! The recorder lives from Start to Stop. Split closes the current file and opens
//! the next one without touching the capture streams; device restarts keep the
//! same file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Local;

use ringbuf::traits::Split;
use ringbuf::HeapRb;

use crate::audio::capture::{CaptureStream, Source};
use crate::audio::mixer::Mixer;
use crate::audio::SAMPLE_RATE;
use crate::config::Config;
use crate::session::{CaptureMode, Closed, Devices, Session, SessionInfo, Trigger};

enum Cmd {
    Split,
    Pause,
    Resume,
    Marker,
    AddApp(String),
    RestartCapture,
    Stop { min_secs: f64 },
}

pub struct Recorder {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<Result<Closed>>>,
    /// Recorded milliseconds in the current file (for the tooltip).
    elapsed_ms: Arc<AtomicU64>,
}

impl Recorder {
    /// Opens the capture streams `config.capture_mode` asks for and starts recording.
    pub fn start(config: &Config, trigger: Trigger, app: Option<String>) -> Result<Self> {
        let ring = || HeapRb::<f32>::new(SAMPLE_RATE as usize * 2).split();
        let mic_id = Some(config.mic_device.clone()).filter(|s| !s.is_empty());
        let sys_id = Some(config.system_device.clone()).filter(|s| !s.is_empty());
        let mut captures = Vec::new();
        let mut mic = None;
        let mut sys = None;
        if matches!(config.capture_mode, CaptureMode::Mic | CaptureMode::Both) {
            let (p, c) = ring();
            captures.push(CaptureStream::spawn(Source::Mic, mic_id, p));
            mic = Some(c);
        }
        if matches!(config.capture_mode, CaptureMode::System | CaptureMode::Both) {
            let (p, c) = ring();
            captures.push(CaptureStream::spawn(Source::System, sys_id, p));
            sys = Some(c);
        }
        let info = SessionInfo {
            trigger,
            app,
            mode: config.capture_mode,
            devices: Devices::default(),
            bitrate_kbps: config.bitrate_kbps,
        };
        Self::with_sources(Mixer::new(mic, sys), captures, config.output_dir.clone(), info)
    }

    /// Runs `mixer` into a new session under `output_dir`. `captures` are kept
    /// alive (and restartable) for the life of the recorder.
    pub fn with_sources(
        mixer: Mixer,
        captures: Vec<CaptureStream>,
        output_dir: PathBuf,
        info: SessionInfo,
    ) -> Result<Self> {
        let session = Session::open(&output_dir, Local::now(), info.clone())?;
        tracing::info!("recording to {}", session.part_path().display());
        let (tx, rx) = mpsc::channel();
        let elapsed_ms = Arc::new(AtomicU64::new(0));
        let engine = Engine { mixer, captures, output_dir, info, session, rx, elapsed_ms: elapsed_ms.clone() };
        let thread = std::thread::Builder::new().name("engine".into()).spawn(move || engine.run())?;
        Ok(Self { tx, thread: Some(thread), elapsed_ms })
    }

    pub fn split(&self) {
        self.send(Cmd::Split);
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    pub fn resume(&self) {
        self.send(Cmd::Resume);
    }

    pub fn marker(&self) {
        self.send(Cmd::Marker);
    }

    pub fn add_app(&self, app: &str) {
        self.send(Cmd::AddApp(app.to_string()));
    }

    pub fn restart_capture(&self) {
        self.send(Cmd::RestartCapture);
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::Relaxed))
    }

    /// False once the engine thread has exited on its own (e.g. disk full).
    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Closes the current file; discards it if shorter than `min_secs`.
    pub fn stop(mut self, min_secs: f64) -> Result<Closed> {
        self.send(Cmd::Stop { min_secs });
        let thread = self.thread.take().expect("engine thread");
        thread.join().map_err(|_| anyhow!("engine thread panicked"))?
    }

    fn send(&self, cmd: Cmd) {
        // A dead engine is reported through is_alive/stop.
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = self.tx.send(Cmd::Stop { min_secs: 0.0 });
            let _ = t.join();
        }
    }
}

struct Engine {
    mixer: Mixer,
    captures: Vec<CaptureStream>,
    output_dir: PathBuf,
    info: SessionInfo,
    session: Session,
    rx: Receiver<Cmd>,
    elapsed_ms: Arc<AtomicU64>,
}

impl Engine {
    fn run(mut self) -> Result<Closed> {
        let mut frame = Vec::new();
        let mut last_devices = Instant::now() - Duration::from_secs(10);
        loop {
            let cmd = self.rx.recv_timeout(Duration::from_millis(10));
            // Always drain what is buffered first, so a command applies at the
            // point in the audio where it was issued.
            if let Err(e) = self.drain(&mut frame) {
                tracing::error!("write failed: {e:#}; closing recording");
                return self.session.close(Local::now(), 0.0);
            }
            match cmd {
                Ok(Cmd::Stop { min_secs }) => return self.session.close(Local::now(), min_secs),
                Err(RecvTimeoutError::Disconnected) => return self.session.close(Local::now(), 0.0),
                Ok(Cmd::Split) => self.split(),
                Ok(Cmd::Pause) => self.session.pause(Local::now()),
                Ok(Cmd::Resume) => self.session.resume(Local::now()),
                Ok(Cmd::Marker) => self.session.marker(Local::now()),
                Ok(Cmd::AddApp(app)) => self.session.add_app(&app),
                Ok(Cmd::RestartCapture) => self.captures.iter().for_each(CaptureStream::request_restart),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_devices.elapsed() >= Duration::from_secs(1) {
                last_devices = Instant::now();
                self.update_devices();
            }
        }
    }

    fn drain(&mut self, frame: &mut Vec<f32>) -> Result<()> {
        while self.mixer.next_frame(frame) {
            self.session.write_frame(frame)?;
        }
        self.elapsed_ms.store((self.session.duration_secs() * 1000.0) as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Never ends the recording: if the next file can't be opened we keep
    /// writing the current one, and a file that fails to finalize is left as
    /// `.part` for startup recovery.
    fn split(&mut self) {
        let now = Local::now();
        let mut info = self.info.clone();
        info.devices = self.session.meta().devices.clone();
        let mut next = match Session::open(&self.output_dir, now, info) {
            Ok(next) => next,
            Err(e) => {
                tracing::error!("split: could not open the next file ({e:#}); continuing in the current one");
                return;
            }
        };
        if self.session.is_paused() {
            next.pause(now);
        }
        let prev = std::mem::replace(&mut self.session, next);
        let part = prev.part_path().to_path_buf();
        match prev.close(now, 0.0) {
            Ok(Closed::Saved(p)) => tracing::info!("split: saved {}", p.display()),
            Ok(Closed::Discarded) => {}
            Err(e) => tracing::error!("split: could not finalize {} ({e:#}); left for recovery", part.display()),
        }
    }

    fn update_devices(&mut self) {
        let mut devices = self.session.meta().devices.clone();
        for c in &self.captures {
            let name = Some(c.device_name()).filter(|n| !n.is_empty());
            match c.source() {
                Source::Mic if name.is_some() => devices.mic = name,
                Source::System if name.is_some() => devices.system = name,
                _ => {}
            }
        }
        self.session.set_devices(devices);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use crate::session::Meta;
    use ringbuf::traits::{Observer, Producer};
    use ringbuf::{HeapProd, HeapRb};

    fn setup(dir: &std::path::Path) -> (HeapProd<f32>, Recorder) {
        let (p, c) = HeapRb::<f32>::new(48_000 * 10).split();
        let info = SessionInfo {
            trigger: Trigger::Manual,
            app: Some("Teams".into()),
            mode: CaptureMode::Mic,
            devices: Devices::default(),
            bitrate_kbps: 24,
        };
        let r = Recorder::with_sources(Mixer::new(Some(c), None), Vec::new(), dir.to_path_buf(), info).unwrap();
        (p, r)
    }

    fn push_secs(p: &mut HeapProd<f32>, secs: usize) {
        assert_eq!(p.push_slice(&vec![0.1; FRAME * 50 * secs]), FRAME * 50 * secs);
    }

    fn meta(path: &std::path::Path) -> Meta {
        serde_json::from_str(&std::fs::read_to_string(path.with_extension("json")).unwrap()).unwrap()
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < until, "engine did not catch up");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for(r: &Recorder, d: Duration) {
        wait_until(|| r.elapsed() >= d);
    }

    fn files(dir: &std::path::Path, suffix: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.to_string_lossy().ends_with(suffix) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// The in-progress sidecar, once it satisfies `cond`.
    fn wait_sidecar(dir: &std::path::Path, cond: impl Fn(&Meta) -> bool) {
        wait_until(|| {
            files(dir, ".json.part").iter().any(|p| {
                std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Meta>(&t).ok()).is_some_and(|m| cond(&m))
            })
        });
    }

    #[test]
    fn records_until_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 2);
        wait_for(&r, Duration::from_secs(2));
        let Closed::Saved(path) = r.stop(0.0).unwrap() else { panic!() };
        assert!((meta(&path).duration_secs - 2.0).abs() < 0.001);
    }

    #[test]
    fn split_starts_a_new_file_with_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 2);
        wait_for(&r, Duration::from_secs(2));
        r.split();
        wait_until(|| files(dir.path(), ".opus").len() == 1);
        push_secs(&mut p, 1);
        r.marker();
        let Closed::Saved(second) = r.stop(0.0).unwrap() else { panic!() };
        let m2 = meta(&second);
        assert!((m2.duration_secs - 1.0).abs() < 0.001, "{}", m2.duration_secs);
        assert_eq!(m2.markers.len(), 1);
        let first = second.with_file_name(second.file_name().unwrap().to_string_lossy().replace("_2.opus", ".opus"));
        assert!((meta(&first).duration_secs - 2.0).abs() < 0.001);
        assert_eq!(meta(&first).apps, vec!["Teams".to_string()]);
    }

    #[test]
    fn pause_drops_audio_until_resume() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 1);
        wait_for(&r, Duration::from_secs(1));
        r.pause();
        wait_sidecar(dir.path(), |m| m.pauses.len() == 1);
        push_secs(&mut p, 3);
        wait_until(|| p.is_empty());
        r.resume();
        wait_sidecar(dir.path(), |m| m.pauses[0].end.is_some());
        push_secs(&mut p, 1);
        let Closed::Saved(path) = r.stop(0.0).unwrap() else { panic!() };
        let m = meta(&path);
        assert!((m.duration_secs - 2.0).abs() < 0.001, "{}", m.duration_secs);
        assert_eq!(m.pauses.len(), 1);
    }

    #[test]
    fn split_while_paused_stays_paused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 1);
        wait_for(&r, Duration::from_secs(1));
        r.pause();
        wait_sidecar(dir.path(), |m| m.pauses.len() == 1);
        r.split();
        wait_until(|| files(dir.path(), ".opus").len() == 1);
        push_secs(&mut p, 2); // must not be recorded: the user paused
        wait_until(|| p.is_empty());
        let Closed::Saved(second) = r.stop(0.0).unwrap() else { panic!() };
        let m = meta(&second);
        assert_eq!(m.duration_secs, 0.0, "audio leaked into the file while paused");
        assert_eq!(m.pauses.len(), 1);
    }

    #[test]
    fn failed_close_on_split_keeps_recording() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 1);
        wait_for(&r, Duration::from_secs(1));
        // Block the rename target of the first file so finalizing it fails.
        let first_part = files(dir.path(), ".opus.part").pop().unwrap();
        let first_final = first_part.with_extension("");
        std::fs::create_dir(&first_final).unwrap();
        r.split();
        wait_until(|| files(dir.path(), ".opus.part").len() == 2);
        push_secs(&mut p, 1);
        assert!(r.is_alive(), "engine died on a failed close");
        let Closed::Saved(second) = r.stop(0.0).unwrap() else { panic!() };
        assert!((meta(&second).duration_secs - 1.0).abs() < 0.001);
        assert!(first_part.exists(), "unfinalized file is left for recovery");
    }

    #[test]
    fn stop_applies_minimum_duration() {
        let dir = tempfile::tempdir().unwrap();
        let (mut p, r) = setup(dir.path());
        push_secs(&mut p, 1);
        r.add_app("Chrome");
        assert!(matches!(r.stop(30.0).unwrap(), Closed::Discarded));
    }

    #[test]
    fn alive_while_running() {
        let dir = tempfile::tempdir().unwrap();
        let (_p, r) = setup(dir.path());
        assert!(r.is_alive());
        r.stop(0.0).unwrap();
    }
}
