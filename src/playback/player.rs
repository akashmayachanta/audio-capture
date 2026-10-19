//! Plays one recording: decode → mix → WASAPI render, on its own thread.
//!
//! `Transport` is the hardware-free part (file, position, listen mode) and is
//! what the tests drive; `Player` owns the thread and the output device, which
//! is only held while actually playing.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::mix::{to_stereo, Listen};
use super::reader::OpusFileReader;
use crate::audio::{device, SAMPLE_RATE};

const POLL: Duration = Duration::from_millis(10);
/// 100 ms device buffer: plenty of slack for a 10 ms poll, short enough that
/// pause and seek feel immediate.
const BUFFER_HNS: i64 = 1_000_000;

/// A recording opened for playback, rendered as stereo frames on demand.
pub struct Transport {
    reader: OpusFileReader<BufReader<File>>,
    pub listen: Listen,
    pub volume: f32,
    /// Mixed stereo samples not yet rendered.
    pending: VecDeque<f32>,
    mixed: Vec<f32>,
}

impl Transport {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            reader: OpusFileReader::open(path)?,
            listen: Listen::default(),
            volume: 1.0,
            pending: VecDeque::new(),
            mixed: Vec::new(),
        })
    }

    pub fn is_mono(&self) -> bool {
        self.reader.channels() == 1
    }

    pub fn duration_secs(&self) -> f64 {
        self.reader.duration_secs()
    }

    /// Time of the next frame `render` produces.
    pub fn position_secs(&self) -> f64 {
        (self.reader.position_samples() - self.pending.len() as u64 / 2) as f64 / SAMPLE_RATE as f64
    }

    pub fn seek(&mut self, secs: f64) -> Result<()> {
        self.pending.clear();
        self.reader.seek(secs)
    }

    /// Everything has been rendered.
    pub fn at_end(&self) -> bool {
        self.position_secs() >= self.duration_secs()
    }

    /// Appends exactly `frames` stereo frames to `out`, silence past the end.
    /// Returns false once the end of the file has been reached.
    pub fn render(&mut self, frames: usize, out: &mut Vec<f32>) -> bool {
        let want = frames * 2;
        let channels = self.reader.channels();
        while self.pending.len() < want {
            let Some(chunk) = self.reader.next_chunk() else { break };
            to_stereo(chunk, channels, self.listen, self.volume, &mut self.mixed);
            self.pending.extend(self.mixed.drain(..));
        }
        let n = want.min(self.pending.len());
        out.extend(self.pending.drain(..n));
        out.resize(out.len() + want - n, 0.0);
        n == want
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// What the UI shows; refreshed by the player thread after every step.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub state: PlayState,
    pub file: Option<PathBuf>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub mono: bool,
    pub error: Option<String>,
}

enum Cmd {
    Load(PathBuf),
    Play,
    Pause,
    Stop,
    Seek(f64),
    Listen(Listen),
    Volume(f32),
}

/// Handle to the player thread. Dropping it stops playback.
pub struct Player {
    tx: Option<Sender<Cmd>>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let s = status.clone();
        let thread = std::thread::Builder::new()
            .name("player".into())
            .spawn(move || player_thread(rx, &s))
            .expect("spawn player thread");
        Self { tx: Some(tx), status, thread: Some(thread) }
    }

    /// Opens `path`, stopped at the start.
    pub fn load(&self, path: PathBuf) {
        self.send(Cmd::Load(path));
    }

    pub fn play(&self) {
        self.send(Cmd::Play);
    }

    pub fn pause(&self) {
        self.send(Cmd::Pause);
    }

    pub fn stop(&self) {
        self.send(Cmd::Stop);
    }

    pub fn seek(&self, secs: f64) {
        self.send(Cmd::Seek(secs));
    }

    pub fn set_listen(&self, listen: Listen) {
        self.send(Cmd::Listen(listen));
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Cmd::Volume(volume));
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.tx.take(); // disconnects: the thread exits
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn player_thread(rx: Receiver<Cmd>, status: &Mutex<Status>) {
    if wasapi::initialize_mta().is_err() {
        status.lock().unwrap().error = Some("Audio output unavailable (COM init failed)".into());
        return;
    }
    let mut p = Playback::default();
    loop {
        let cmd = if p.state == PlayState::Playing {
            match rx.recv_timeout(POLL) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => break,
            }
        };
        for cmd in cmd.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            p.handle(cmd);
        }
        if p.state == PlayState::Playing {
            p.feed();
        }
        *status.lock().unwrap() = p.status();
    }
    drop(p); // release the device before COM goes away
    wasapi::deinitialize();
}

#[derive(Default)]
struct Playback {
    transport: Option<Transport>,
    output: Option<Output>,
    state: PlayState,
    file: Option<PathBuf>,
    listen: Listen,
    volume: Option<f32>,
    error: Option<String>,
    buf: Vec<f32>,
}

impl Playback {
    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load(path) => {
                self.output = None;
                self.state = PlayState::Stopped;
                self.error = None;
                self.transport = match Transport::open(&path) {
                    Ok(mut t) => {
                        t.listen = self.listen;
                        t.volume = self.volume.unwrap_or(1.0);
                        Some(t)
                    }
                    Err(e) => {
                        self.error = Some(format!("Could not open: {e:#}"));
                        None
                    }
                };
                self.file = Some(path);
            }
            Cmd::Play => self.play(),
            Cmd::Pause if self.state == PlayState::Playing => {
                // Resume from what was heard, not from what was buffered.
                let heard = self.heard_secs();
                self.output = None;
                self.seek(heard);
                self.state = PlayState::Paused;
            }
            Cmd::Pause => {}
            Cmd::Stop => {
                self.output = None;
                self.seek(0.0);
                self.state = PlayState::Stopped;
            }
            Cmd::Seek(secs) => {
                self.seek(secs);
                if let Some(o) = &self.output {
                    o.discard_buffered();
                }
            }
            Cmd::Listen(listen) => {
                self.listen = listen;
                if let Some(t) = &mut self.transport {
                    t.listen = listen;
                }
            }
            Cmd::Volume(volume) => {
                self.volume = Some(volume);
                if let Some(t) = &mut self.transport {
                    t.volume = volume;
                }
            }
        }
    }

    fn play(&mut self) {
        let Some(t) = &mut self.transport else { return };
        if t.position_secs() >= t.duration_secs() {
            let _ = t.seek(0.0); // finished: play again from the start
        }
        if self.output.is_none() {
            match Output::open() {
                Ok(o) => self.output = Some(o),
                Err(e) => {
                    self.error = Some(format!("No audio output: {e:#}"));
                    return;
                }
            }
        }
        self.error = None;
        self.state = PlayState::Playing;
    }

    fn seek(&mut self, secs: f64) {
        if let Some(t) = &mut self.transport
            && let Err(e) = t.seek(secs)
        {
            self.error = Some(format!("Seek failed: {e:#}"));
        }
    }

    /// Tops up the device buffer; stops once the file has played out.
    fn feed(&mut self) {
        let (Some(t), Some(o)) = (&mut self.transport, &mut self.output) else { return };
        let step = (|| -> Result<bool> {
            // Past the end, stop topping up so the buffer can drain.
            let frames = if t.at_end() { 0 } else { o.available()? };
            self.buf.clear();
            t.render(frames, &mut self.buf);
            o.write(frames, &self.buf)?;
            Ok(!t.at_end() || o.buffered()? > 0)
        })();
        match step {
            Ok(true) => {}
            Ok(false) => {
                self.output = None;
                self.seek(0.0);
                self.state = PlayState::Stopped;
            }
            Err(e) => {
                tracing::warn!("playback failed: {e:#}");
                let heard = self.heard_secs();
                self.output = None;
                self.seek(heard);
                self.error = Some(format!("Playback stopped: {e:#}"));
                self.state = PlayState::Paused;
            }
        }
    }

    /// Position of the audio leaving the speakers: decoded minus still buffered.
    fn heard_secs(&self) -> f64 {
        let Some(t) = &self.transport else { return 0.0 };
        let buffered = self.output.as_ref().and_then(|o| o.buffered().ok()).unwrap_or(0);
        (t.position_secs() - buffered as f64 / SAMPLE_RATE as f64).max(0.0)
    }

    fn status(&self) -> Status {
        let t = self.transport.as_ref();
        Status {
            state: self.state,
            file: self.file.clone(),
            position_secs: if self.state == PlayState::Playing {
                self.heard_secs()
            } else {
                t.map_or(0.0, Transport::position_secs)
            },
            duration_secs: t.map_or(0.0, Transport::duration_secs),
            mono: t.is_some_and(Transport::is_mono),
            error: self.error.clone(),
        }
    }
}

/// The default speakers, as 48 kHz stereo float (Windows converts as needed).
struct Output {
    client: wasapi::AudioClient,
    render: wasapi::AudioRenderClient,
    bytes: Vec<u8>,
}

impl Output {
    fn open() -> Result<Self> {
        let device = device::open(Direction::Render, None)?;
        let mut client = device.get_iaudioclient()?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 2, None);
        let mode = StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: BUFFER_HNS };
        client.initialize_client(&format, &Direction::Render, &mode)?;
        let render = client.get_audiorenderclient()?;
        client.start_stream()?;
        Ok(Self { client, render, bytes: Vec::new() })
    }

    /// Frames that fit in the device buffer right now.
    fn available(&self) -> Result<usize> {
        Ok(self.client.get_available_space_in_frames()? as usize)
    }

    /// Frames written but not yet played.
    fn buffered(&self) -> Result<u32> {
        Ok(self.client.get_current_padding()?)
    }

    fn write(&mut self, frames: usize, samples: &[f32]) -> Result<()> {
        self.bytes.clear();
        self.bytes.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        self.render.write_to_device(frames, &self.bytes, None)?;
        Ok(())
    }

    /// Drops what is queued so a seek is heard immediately.
    fn discard_buffered(&self) {
        let _ = self.client.stop_stream();
        let _ = self.client.reset_stream();
        let _ = self.client.start_stream();
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use crate::encode::opus_ogg::OggOpusWriter;

    /// One second: a 440 Hz tone on the left (mic), silence on the right.
    fn left_tone_file(dir: &Path) -> PathBuf {
        let path = dir.join("tone.opus");
        let mut w = OggOpusWriter::new(File::create(&path).unwrap(), 2, 48_000, &[]).unwrap();
        let mut n = 0;
        for _ in 0..50 {
            let mut pcm = Vec::new();
            for _ in 0..FRAME {
                pcm.push((n as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0).sin() * 0.5);
                pcm.push(0.0);
                n += 1;
            }
            w.write_frame(&pcm).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    #[test]
    fn renders_the_whole_file_then_silence() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Transport::open(&left_tone_file(dir.path())).unwrap();
        assert!(!t.is_mono());
        assert_eq!(t.duration_secs(), 1.0);
        let mut out = Vec::new();
        let mut chunks = 0;
        while t.render(4_800, &mut out) {
            chunks += 1;
            assert!(chunks <= 10, "a 1 s file must end within 10 chunks of 100 ms");
        }
        assert_eq!(out.len(), (chunks + 1) * 4_800 * 2, "every call renders exactly the frames asked for");
        assert_eq!(t.position_secs(), 1.0);
        assert!(t.at_end());
        let mut after = Vec::new();
        assert!(!t.render(4_800, &mut after));
        assert_eq!(after, vec![0.0; 4_800 * 2]);
    }

    #[test]
    fn position_counts_rendered_frames_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Transport::open(&left_tone_file(dir.path())).unwrap();
        let mut out = Vec::new();
        assert!(t.render(1_000, &mut out));
        assert_eq!(t.position_secs(), 1_000.0 / 48_000.0);
        assert!(!t.at_end());
    }

    #[test]
    fn seek_moves_the_position_and_drops_what_was_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = Transport::open(&left_tone_file(dir.path())).unwrap();
        let mut out = Vec::new();
        t.render(1_000, &mut out);
        t.seek(0.5).unwrap();
        assert_eq!(t.position_secs(), 0.5);
        t.render(4_800, &mut out);
        assert_eq!(t.position_secs(), 0.6);
    }

    #[test]
    fn listen_mode_picks_what_both_ears_hear() {
        let dir = tempfile::tempdir().unwrap();
        let path = left_tone_file(dir.path());
        let ears = |listen| {
            let mut t = Transport::open(&path).unwrap();
            t.listen = listen;
            let mut out = Vec::new();
            t.render(24_000, &mut out);
            let left: Vec<f32> = out.iter().step_by(2).copied().collect();
            let right: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
            assert_eq!(left, right, "{listen:?}: both ears hear the same");
            rms(&left)
        };
        assert!(ears(Listen::Both) > 0.3);
        assert!(ears(Listen::Me) > 0.3);
        assert!(ears(Listen::Others) < 0.001, "the tone is on the mic side only");
    }

    #[test]
    fn volume_scales_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = left_tone_file(dir.path());
        let level = |volume| {
            let mut t = Transport::open(&path).unwrap();
            t.volume = volume;
            let mut out = Vec::new();
            t.render(24_000, &mut out);
            rms(&out)
        };
        let ratio = level(0.5) / level(1.0);
        assert!((ratio - 0.5).abs() < 0.01, "ratio {ratio}");
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Transport::open(&dir.path().join("gone.opus")).is_err());
    }

    fn wait_for(player: &Player, what: &str, ok: impl Fn(&Status) -> bool) -> Status {
        for _ in 0..300 {
            let s = player.status();
            if ok(&s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}: {:?}", player.status());
    }

    /// Plays a quiet second through the default speakers: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn plays_through_the_default_output_and_stops_at_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let player = Player::spawn();
        player.set_volume(0.1);
        player.load(left_tone_file(dir.path()));
        let s = wait_for(&player, "load", |s| s.duration_secs > 0.0);
        assert_eq!(s.state, PlayState::Stopped);
        assert!(!s.mono);
        player.play();
        let s = wait_for(&player, "progress", |s| s.position_secs > 0.3);
        assert_eq!(s.state, PlayState::Playing, "{:?}", s.error);
        player.pause();
        let paused = wait_for(&player, "pause", |s| s.state == PlayState::Paused);
        assert!(paused.position_secs > 0.2 && paused.position_secs < 0.9, "{}", paused.position_secs);
        player.play();
        let s = wait_for(&player, "the end", |s| s.state == PlayState::Stopped);
        assert_eq!(s.position_secs, 0.0, "rewinds after finishing");
        assert!(s.error.is_none(), "{:?}", s.error);
    }
}
