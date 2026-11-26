//! Dictation: hold or double-tap a key to speak into the focused field.
//! The pure parts (`keys`, `gesture`, `controller`, `service`, `overlay_scene`)
//! are unit-tested; the rest talks to Win32 and the transcriber. `Dictation`
//! (below) is the glue the tray's app loop drives.
//!
//! Transcription runs while the user is still talking: the capture thread cuts
//! the audio into phrases at pauses (`segmenter`) and a second thread sends them
//! to the transcriber one by one, in order. On release only the last phrase is
//! left to do. The speech API engine does the same (`api_phrases`, on by default). `paste_mode` decides whether hands-free dictation pastes each
//! phrase as it arrives or everything at the end.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Local;

use crate::config::{self, Config, MicReady, PasteMode};
use capture::Capture;
use warm_mic::WarmMic;
use client::{Client, DictateError};
use controller::{Action, Controller, Input, State};
use gesture::{Detector, Gesture};
use keys::KeyId;
use overlay::Overlay;
use overlay_scene::Phase;
use paste::PasteOutcome;
use rawinput::{RawKeys, Stamped};
use service::{ServiceState, SpeechService};

pub mod capture;
pub mod client;
pub mod controller;
pub mod gesture;
pub mod keys;
pub mod mic_test;
pub mod overlay;
pub mod overlay_scene;
pub mod paste;
pub mod rawinput;
pub mod segmenter;
pub mod service;
pub mod warm_mic;

/// How long a finished dictation waits for the speech service to start and load its model.
const SERVICE_WAIT: Duration = Duration::from_secs(90);

/// The hold and double-tap keys the config asks for; an empty or unknown name turns that gesture off.
pub fn keys_from_config(d: &config::Dictation) -> (Option<KeyId>, Option<KeyId>) {
    if !d.enabled {
        return (None, None);
    }
    (KeyId::parse(&d.hold_key), KeyId::parse(&d.toggle_key))
}

/// Alt and Win do something when tapped alone (menu bar, Start menu); see `paste::send_menu_mask_key`.
fn masks_menu(key: &KeyId) -> bool {
    ["LeftAlt", "RightAlt", "LeftWin", "RightWin"].iter().any(|n| KeyId::parse(n).as_ref() == Some(key))
}

/// What the app loop knows that dictation needs.
pub struct Env<'a> {
    pub speech: &'a SpeechService,
    pub service: ServiceState,
    pub transcriber_dir: &'a Path,
}

enum WorkerMsg {
    Phase(Phase),
    /// The text of the next phrase.
    Piece(String),
    /// Nothing more is coming: how many phrases were transcribed, or why it stopped.
    Done(Result<usize, String>),
}

/// The thread transcribing this dictation's phrases.
struct Work {
    cancel: Arc<AtomicBool>,
    rx: Receiver<WorkerMsg>,
}

/// `mic_ready = "recent"`: how long the mic stays open after a dictation.
const RECENT_WARM: Duration = Duration::from_secs(5 * 60);

/// True when a phrase is pasted as soon as its text arrives. Never during
/// hold-to-talk: the held key (Right Alt) would turn Ctrl+V into Ctrl+Alt+V.
fn pastes_live(mode: PasteMode, hands_free: bool) -> bool {
    mode == PasteMode::Live && hands_free
}

/// The phrases of one dictation as one text.
fn join_pieces(pieces: &[String]) -> String {
    pieces.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Sends one clip to the transcriber, waiting (up to `SERVICE_WAIT`) for the
/// service to come up. `None` when cancelled.
fn transcribe_bytes(client: &Client, bytes: &[u8], cancel: &AtomicBool, tx: &Sender<WorkerMsg>) -> Option<Result<String, String>> {
    let started = Instant::now();
    let mut phase: Option<Phase> = None;
    let mut show = |p: Phase| {
        if phase != Some(p) {
            phase = Some(p);
            let _ = tx.send(WorkerMsg::Phase(p));
        }
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if client.health() {
            show(Phase::Transcribing);
            match client.dictate(bytes, cancel) {
                Ok(text) => return Some(Ok(text)),
                Err(DictateError::Cancelled) => return None,
                Err(DictateError::NoModel) => {
                    return Some(Err("No speech model is loaded. Open Settings → Transcription and load one.".into()));
                }
                Err(DictateError::Failed(m)) => return Some(Err(m)),
                Err(DictateError::Loading | DictateError::Unreachable) => {}
            }
        }
        show(Phase::Starting);
        if started.elapsed() > SERVICE_WAIT {
            return Some(Err("The speech service did not start. Is the transcriber set up (uv)?".into()));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Transcribes the phrases of one dictation as the capture hands them out, in
/// order, until the capture closes the channel (the recording ended).
fn stream_worker(phrases: Receiver<Vec<f32>>, bitrate_bps: i32, cancel: Arc<AtomicBool>, tx: Sender<WorkerMsg>) {
    let client = Client::local();
    let mut sent = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match phrases.recv_timeout(Duration::from_millis(50)) {
            Ok(_) if cancel.load(Ordering::Relaxed) => return, // see `api_stream_worker`
            Ok(samples) => {
                let result = match capture::encode_clip(&samples, bitrate_bps) {
                    Ok(bytes) => transcribe_bytes(&client, &bytes, &cancel, &tx),
                    Err(e) => Some(Err(format!("Could not encode the dictation: {e}"))),
                };
                match result {
                    None => return,
                    Some(Ok(text)) => {
                        sent += 1;
                        let _ = tx.send(WorkerMsg::Piece(text));
                    }
                    Some(Err(e)) => {
                        let _ = tx.send(WorkerMsg::Done(Err(e)));
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = tx.send(WorkerMsg::Done(Ok(sent)));
                return;
            }
        }
    }
}

/// Transcribes a whole saved clip in one go: the fallback when no phrase was
/// found in it while recording (very quiet speech).
fn clip_worker(bytes: Vec<u8>, cancel: Arc<AtomicBool>, tx: Sender<WorkerMsg>) {
    match transcribe_bytes(&Client::local(), &bytes, &cancel, &tx) {
        None => {}
        Some(Ok(text)) => {
            let _ = tx.send(WorkerMsg::Piece(text));
            let _ = tx.send(WorkerMsg::Done(Ok(1)));
        }
        Some(Err(e)) => {
            let _ = tx.send(WorkerMsg::Done(Err(e)));
        }
    }
}

/// `[dictation] engine = "api"` with `api_phrases`: each phrase to the speech API
/// as the capture hands it out, in order, with one id for the whole dictation.
fn api_stream_worker(api: config::SpeechApi, phrases: Receiver<Vec<f32>>, bitrate_bps: i32, cancel: Arc<AtomicBool>, tx: Sender<WorkerMsg>) {
    let session = uuid::Uuid::new_v4().simple().to_string();
    let mut sent = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        match phrases.recv_timeout(Duration::from_millis(50)) {
            // A cancelled capture still hands out its last phrase as it stops (a Ctrl tap
            // flushes the warm mic's pre-roll): never send that.
            Ok(_) if cancel.load(Ordering::Relaxed) => return,
            Ok(samples) => {
                let started = Instant::now();
                let result = capture::encode_clip(&samples, bitrate_bps)
                    .map_err(|e| format!("Could not encode the dictation: {e}"))
                    .and_then(|bytes| crate::speech_api::dictate_phrase(&api, &bytes, &session));
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                match result {
                    Ok(text) => {
                        sent += 1;
                        tracing::info!("dictation phrase {sent}: {:.1} s of audio, transcribed in {} ms", samples.len() as f32 / 48_000.0, started.elapsed().as_millis());
                        let _ = tx.send(WorkerMsg::Piece(text));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Done(Err(e)));
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = tx.send(WorkerMsg::Done(Ok(sent)));
                return;
            }
        }
    }
}

/// `[dictation] engine = "api"`: the whole clip to the speech API in one request.
fn api_worker(api: config::SpeechApi, bytes: Vec<u8>, cancel: Arc<AtomicBool>, tx: Sender<WorkerMsg>) {
    let _ = tx.send(WorkerMsg::Phase(Phase::Transcribing));
    let result = crate::speech_api::dictate(&api, &bytes);
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    match result {
        Ok(text) => {
            let _ = tx.send(WorkerMsg::Piece(text));
            let _ = tx.send(WorkerMsg::Done(Ok(1)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Done(Err(e)));
        }
    }
}

/// Hold-to-talk and hands-free dictation: keys in, text pasted out. Driven by `poll`.
pub struct Dictation {
    cfg: Config,
    raw: Option<(RawKeys, Receiver<Stamped>)>,
    detector: Detector,
    controller: Controller,
    overlay: Option<Overlay>,
    capture: Option<Capture>,
    work: Option<Work>,
    base: Instant,
    last_second: Instant,
    queue: VecDeque<Input>,
    notices: Vec<String>,
    /// Texts for the paste thread, which pastes them one after another.
    paste_jobs: Sender<String>,
    /// Messages back from the paste thread.
    paste_rx: Receiver<String>,
    /// Phrases transcribed so far that are waiting to be pasted together at the end.
    pieces: Vec<String>,
    /// Something of this dictation has already been pasted.
    live_pasted: bool,
    /// This dictation is (or became) hands-free: no key is held down.
    hands_free: bool,
    /// The whole clip, sent if the recording produced no phrase by itself.
    fallback: Option<Vec<u8>>,
    /// The room's noise floor from the last dictation: the next one starts with it.
    noise_floor: Option<f32>,
    /// The mic kept open between dictations (`mic_ready`), when it is.
    warm: Option<WarmMic>,
    /// When the last dictation ended (`mic_ready = "recent"`); a tap (Ctrl+C) is not one.
    last_dictation: Option<Instant>,
}

impl Dictation {
    pub fn new(cfg: &Config) -> Dictation {
        let (paste_tx, paste_rx) = mpsc::channel();
        let (paste_jobs, jobs) = mpsc::channel::<String>();
        // Pasting waits for the keys to come up and restores the clipboard afterwards: off the
        // UI thread, and one text at a time so phrases land in order.
        std::thread::Builder::new()
            .name("dictation-paste".into())
            .spawn(move || {
                for text in jobs {
                    let message = match paste::paste_text(&text) {
                        Ok(PasteOutcome::Pasted) => None,
                        Ok(PasteOutcome::LeftOnClipboard) => Some("The dictation is on the clipboard; press Ctrl+V to paste it.".to_string()),
                        Err(e) => Some(format!("Could not paste the dictation: {e:#}")),
                    };
                    if let Some(m) = message {
                        let _ = paste_tx.send(m);
                    }
                }
            })
            .expect("spawn the paste thread");
        let now = Instant::now();
        let mut d = Dictation {
            cfg: cfg.clone(),
            raw: None,
            detector: Detector::new(None, None),
            controller: Controller::new(cfg.dictation.max_secs),
            overlay: None,
            capture: None,
            work: None,
            base: now,
            last_second: now,
            queue: VecDeque::new(),
            notices: Vec::new(),
            paste_jobs,
            paste_rx,
            pieces: Vec::new(),
            live_pasted: false,
            hands_free: false,
            fallback: None,
            noise_floor: None,
            warm: None,
            last_dictation: None,
        };
        match Overlay::new() {
            Ok(o) => d.overlay = Some(o),
            Err(e) => tracing::warn!("dictation overlay unavailable: {e:#}"),
        }
        d.apply_config(cfg);
        d
    }

    /// New settings: keys, limits, where audio goes. Anything in progress is cancelled when the
    /// keys change or dictation is turned off.
    pub fn apply_config(&mut self, cfg: &Config) {
        let (hold, toggle) = keys_from_config(&cfg.dictation);
        let keys_changed = keys_from_config(&self.cfg.dictation) != (hold.clone(), toggle.clone());
        if keys_changed || (hold.is_none() && toggle.is_none()) {
            self.cancel_all();
        }
        self.cfg = cfg.clone();
        self.controller.set_max_secs(cfg.dictation.max_secs);
        self.detector.set_keys(hold.clone(), toggle.clone());
        let wanted = hold.is_some() || toggle.is_some();
        if wanted && self.raw.is_none() {
            match RawKeys::start() {
                Ok(r) => self.raw = Some(r),
                Err(e) => {
                    tracing::warn!("dictation keys unavailable: {e:#}");
                    self.notices.push(format!("Dictation is unavailable: {e}"));
                }
            }
        } else if !wanted {
            self.raw = None;
        }
    }

    /// Sleep/resume: a dictation in progress is dropped.
    pub fn suspend(&mut self) {
        self.queue.push_back(Input::Suspend);
    }

    /// True while something is on screen or being recorded: the app loop should wake often.
    pub fn wants_fast_loop(&self) -> bool {
        self.capture.is_some() || self.overlay.as_ref().is_some_and(Overlay::is_animating)
    }

    fn ms(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.base).as_millis() as u64
    }

    fn cancel_all(&mut self) {
        self.queue.clear();
        if let Some(c) = self.take_capture() {
            c.cancel(false);
        }
        self.cancel_work();
        self.controller = Controller::new(self.cfg.dictation.max_secs);
        if let Some(o) = &mut self.overlay {
            o.hide();
        }
    }

    /// Stops the transcription thread and forgets what it produced so far.
    fn cancel_work(&mut self) {
        if let Some(w) = self.work.take() {
            w.cancel.store(true, Ordering::Relaxed);
        }
        self.pieces.clear();
        self.fallback = None;
    }

    fn spawn_work(&mut self, name: &str, run: impl FnOnce(Arc<AtomicBool>, Sender<WorkerMsg>) + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::Builder::new().name(name.into()).spawn(move || run(flag, tx)).expect("spawn the transcription thread");
        self.work = Some(Work { cancel, rx });
    }

    /// Reads keys, advances the state machine and carries out what it asks.
    /// Returns messages to show the user (toasts).
    pub fn poll(&mut self, now: Instant, env: &Env) -> Vec<String> {
        let keys: Vec<Stamped> = self.raw.as_ref().map(|(_, rx)| rx.try_iter().collect()).unwrap_or_default();
        for stamped in keys {
            let ms = self.ms(stamped.at);
            if stamped.event.down && stamped.event.key.to_string() == "Escape" {
                self.queue.push_back(Input::Esc);
            }
            for g in self.detector.key(&stamped.event, ms) {
                self.on_gesture(g);
            }
        }
        for g in self.detector.tick(self.ms(now)) {
            self.on_gesture(g);
        }

        let mut messages = Vec::new();
        if let Some(work) = &self.work {
            messages.extend(work.rx.try_iter());
        }
        for m in messages {
            match m {
                WorkerMsg::Phase(p) if self.controller.state() == State::Transcribing => {
                    if let Some(o) = &mut self.overlay {
                        o.show(p);
                    }
                }
                WorkerMsg::Phase(_) => {}
                WorkerMsg::Piece(text) => {
                    let text = text.trim();
                    if text.is_empty() {
                        // A phrase of noise.
                    } else if pastes_live(self.cfg.dictation.paste_mode, self.hands_free) {
                        self.paste(format!("{text} "));
                        self.live_pasted = true;
                    } else {
                        self.pieces.push(text.to_string());
                    }
                }
                WorkerMsg::Done(Ok(0)) if self.fallback.is_some() => {
                    // No phrase was found while recording: let the transcriber judge the whole clip.
                    let bytes = self.fallback.take().unwrap_or_default();
                    if self.uses_api() {
                        let api = self.cfg.speech_api.clone();
                        self.spawn_work("dictation-api", move |cancel, tx| api_worker(api, bytes, cancel, tx));
                    } else {
                        self.spawn_work("dictation-transcribe", move |cancel, tx| clip_worker(bytes, cancel, tx));
                    }
                }
                WorkerMsg::Done(Ok(_)) => {
                    self.work = None;
                    self.fallback = None;
                    let text = join_pieces(&std::mem::take(&mut self.pieces));
                    self.queue.push_back(if text.is_empty() && self.live_pasted { Input::Finished } else { Input::TranscriptReady(text) });
                }
                WorkerMsg::Done(Err(e)) => {
                    self.cancel_work();
                    if let Some(c) = self.take_capture() {
                        // Still listening: there is no point in going on.
                        c.cancel(self.cfg.dictation.keep_audio);
                        self.queue.push_back(Input::CaptureFailed(e));
                    } else {
                        self.queue.push_back(Input::TranscriptFailed(e));
                    }
                }
            }
        }

        if let Some(c) = &self.capture {
            if let Some(o) = &mut self.overlay {
                o.set_level(c.level());
            }
            if !c.is_alive() {
                self.queue.push_back(Input::CaptureFailed("The microphone recording stopped because of an error.".into()));
            }
            if now.duration_since(self.last_second) >= Duration::from_secs(1) {
                self.last_second = now;
                self.queue.push_back(Input::Tick { listening_secs: c.elapsed_secs() as u64 });
            }
        }

        while let Some(input) = self.queue.pop_front() {
            for action in self.controller.handle(input) {
                self.execute(action, env);
            }
            if self.controller.state() == State::Toggle {
                self.hands_free = true;
            }
        }
        self.notices.extend(self.paste_rx.try_iter());
        if let Some(o) = &mut self.overlay {
            o.tick();
        }
        self.sync_warm(now);
        std::mem::take(&mut self.notices)
    }

    /// Opens or closes the mic kept between dictations, as `mic_ready` asks;
    /// reopens it when another mic is chosen. Never while a dictation uses it.
    fn sync_warm(&mut self, now: Instant) {
        if self.capture.is_some() {
            return;
        }
        let wanted = self.raw.is_some()
            && match self.cfg.dictation.mic_ready {
                MicReady::Off => false,
                MicReady::Always => true,
                MicReady::Recent => self.last_dictation.is_some_and(|t| now.duration_since(t) < RECENT_WARM),
            };
        let stale = self.warm.as_ref().is_some_and(|w| w.device() != self.cfg.mic_device);
        if self.warm.is_some() && (!wanted || stale) {
            self.warm = None;
        }
        if wanted && self.warm.is_none() {
            self.warm = Some(WarmMic::open(&self.cfg.mic_device));
        }
    }

    /// The default mic changed or the PC woke up: the kept-open mic reopens on the current device.
    pub fn device_changed(&self) {
        if let Some(w) = &self.warm {
            w.restart();
        }
    }

    fn on_gesture(&mut self, g: Gesture) {
        if g == Gesture::ShowOverlay {
            // The hold key is down and it is a hold: keep Alt/Win's release from opening a menu.
            let (hold, _) = keys_from_config(&self.cfg.dictation);
            if hold.is_some_and(|k| masks_menu(&k)) {
                paste::send_menu_mask_key();
            }
        }
        self.queue.push_back(Input::Gesture(g));
    }

    fn uses_api(&self) -> bool {
        self.cfg.dictation.engine == config::DictationEngine::Api
    }

    fn ensure_server(&mut self, env: &Env) {
        if !self.uses_api() && env.service == ServiceState::Down {
            if let Err(e) = env.speech.start(env.transcriber_dir, None) {
                self.notices.push(e);
            }
        }
    }

    fn execute(&mut self, action: Action, env: &Env) {
        tracing::debug!("dictation action {action:?}");
        let keep_audio = self.cfg.dictation.keep_audio;
        match action {
            Action::StartCapture => {
                self.cancel_work();
                self.live_pasted = false;
                self.hands_free = false;
                // Without `api_phrases` the API gets the whole clip at the end: no phrases, no stream worker.
                let (phrases_tx, phrases) = mpsc::channel();
                let phrases_tx = (!self.uses_api() || self.cfg.dictation.api_phrases).then_some(phrases_tx);
                let streaming = phrases_tx.is_some();
                if self.cfg.dictation.mic_ready != MicReady::Off && self.warm.is_none() && self.raw.is_some() {
                    // "recent" after a quiet spell: opened now (this dictation may still lose
                    // its first moment) and kept for the next ones.
                    self.warm = Some(WarmMic::open(&self.cfg.mic_device));
                }
                let started = match &self.warm {
                    Some(w) => Capture::start_warm(&self.cfg, Local::now(), w, phrases_tx, self.noise_floor),
                    None => Capture::start(&self.cfg, Local::now(), phrases_tx, self.noise_floor),
                };
                match started {
                    Ok(c) => {
                        self.capture = Some(c);
                        if streaming {
                            let bitrate_bps = self.cfg.bitrate_kbps as i32 * 1000;
                            if self.uses_api() {
                                let api = self.cfg.speech_api.clone();
                                self.spawn_work("dictation-api-stream", move |cancel, tx| api_stream_worker(api, phrases, bitrate_bps, cancel, tx));
                            } else {
                                self.spawn_work("dictation-stream", move |cancel, tx| stream_worker(phrases, bitrate_bps, cancel, tx));
                            }
                        }
                    }
                    Err(e) => self.queue.push_back(Input::CaptureFailed(format!("Could not start the microphone: {e}"))),
                }
            }
            Action::EnsureServer => self.ensure_server(env),
            Action::ShowListening => {
                if let Some(o) = &mut self.overlay {
                    o.show(Phase::Listening);
                }
            }
            Action::HideOverlay => {
                if let Some(o) = &mut self.overlay {
                    o.hide();
                }
            }
            Action::DiscardCapture => {
                self.cancel_work();
                if let Some(c) = self.take_capture() {
                    c.cancel(false);
                }
            }
            Action::CancelCapture => {
                self.cancel_work();
                if let Some(c) = self.take_capture() {
                    c.cancel(keep_audio);
                }
            }
            Action::FinishCapture => self.finish(env),
            Action::CancelTranscription => self.cancel_work(),
            Action::Paste(text) => self.paste(text),
            Action::Notify(message) => self.notices.push(message),
        }
    }

    /// Stops the mic. The phrases spoken so far are already transcribed (or on
    /// their way); the capture hands out the last one as it stops.
    fn finish(&mut self, env: &Env) {
        self.last_dictation = Some(Instant::now());
        let Some(capture) = self.take_capture() else {
            self.cancel_work();
            self.queue.push_back(Input::TranscriptFailed("There was no recording to transcribe.".into()));
            return;
        };
        self.ensure_server(env);
        match capture.finish(self.cfg.dictation.keep_audio) {
            Ok(Some(clip)) => {
                if self.work.is_some() {
                    // Phrases are on their way; the whole clip is only for when none was found.
                    self.fallback = Some(clip.bytes);
                } else if self.uses_api() {
                    let api = self.cfg.speech_api.clone();
                    self.spawn_work("dictation-api", move |cancel, tx| api_worker(api, clip.bytes, cancel, tx));
                } else {
                    self.spawn_work("dictation-transcribe", move |cancel, tx| clip_worker(clip.bytes, cancel, tx));
                }
                let phase = if self.uses_api() || env.service == ServiceState::Up { Phase::Transcribing } else { Phase::Starting };
                if let Some(o) = &mut self.overlay {
                    o.show(phase);
                }
            }
            Ok(None) => {
                self.cancel_work();
                self.queue.push_back(Input::TranscriptReady(String::new()));
            }
            Err(e) => {
                self.cancel_work();
                self.queue.push_back(Input::TranscriptFailed(format!("Could not save the dictation: {e}")));
            }
        }
    }

    /// Takes the capture out, remembering the noise floor it learned for the next dictation.
    fn take_capture(&mut self) -> Option<Capture> {
        let c = self.capture.take()?;
        self.noise_floor = c.noise_floor().or(self.noise_floor);
        Some(c)
    }

    /// Queues `text` for the paste thread.
    fn paste(&self, text: String) {
        let _ = self.paste_jobs.send(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dictation as DictationConfig;

    fn cfg(enabled: bool, hold: &str, toggle: &str) -> DictationConfig {
        DictationConfig { enabled, hold_key: hold.into(), toggle_key: toggle.into(), ..DictationConfig::default() }
    }

    fn k(s: &str) -> Option<KeyId> {
        KeyId::parse(s)
    }

    #[test]
    fn the_default_is_right_alt_for_both() {
        assert_eq!(keys_from_config(&DictationConfig::default()), (k("RightAlt"), k("RightAlt")));
    }

    #[test]
    fn disabled_means_no_keys() {
        assert_eq!(keys_from_config(&cfg(false, "RightAlt", "RightAlt")), (None, None));
    }

    #[test]
    fn empty_and_unknown_names_turn_that_gesture_off() {
        assert_eq!(keys_from_config(&cfg(true, "", "RightAlt")), (None, k("RightAlt")));
        assert_eq!(keys_from_config(&cfg(true, "nope", "f13")), (None, k("F13")));
    }

    #[test]
    fn only_hands_free_dictation_pastes_while_speaking() {
        assert!(pastes_live(PasteMode::Live, true));
        assert!(!pastes_live(PasteMode::Live, false), "hold-to-talk: the key is still down");
        assert!(!pastes_live(PasteMode::End, true));
        assert!(!pastes_live(PasteMode::End, false));
    }

    #[test]
    fn the_api_worker_reports_a_missing_setup_as_the_error() {
        let (tx, rx) = mpsc::channel();
        api_worker(config::SpeechApi::default(), vec![1, 2, 3], Arc::new(AtomicBool::new(false)), tx);
        let msgs: Vec<WorkerMsg> = rx.try_iter().collect();
        assert!(matches!(&msgs[0], WorkerMsg::Phase(Phase::Transcribing)));
        assert!(matches!(&msgs[1], WorkerMsg::Done(Err(e)) if e.contains("Settings")), "{}", msgs.len());
    }

    /// A speech API that counts the requests it gets (and answers each with "hi").
    fn counting_api() -> (config::SpeechApi, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                h.fetch_add(1, Ordering::SeqCst);
                let _ = crate::web_api::server::read_request(&mut BufReader::new(s.try_clone().unwrap()));
                let body = r#"{"text":"hi"}"#;
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let api = config::SpeechApi { base_url: format!("http://127.0.0.1:{port}/v1"), api_key: String::new(), model: "m".into() };
        (api, hits)
    }

    fn tone(secs: f32) -> Vec<f32> {
        (0..(secs * 48_000.0) as usize).map(|n| 0.5 * (n as f32 * 0.04).sin()).collect()
    }

    #[test]
    fn a_phrase_that_arrives_after_a_cancel_is_never_sent() {
        // A Ctrl tap (Ctrl+C) starts a capture and cancels it at once; stopping the capture hands
        // out its last phrase (with the warm mic: the moment before the press). It must not be
        // transcribed: it may be someone talking, and nobody asked for it.
        let (api, hits) = counting_api();
        for _ in 0..20 {
            let (phrases_tx, phrases) = mpsc::channel::<Vec<f32>>();
            let (tx, rx) = mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let c = cancel.clone();
            let a = api.clone();
            let worker = std::thread::spawn(move || api_stream_worker(a, phrases, 32_000, c, tx));
            std::thread::sleep(Duration::from_millis(15)); // the worker is waiting for a phrase
            cancel.store(true, Ordering::Relaxed); // DiscardCapture: cancel_work() first...
            let _ = phrases_tx.send(tone(0.5)); // ...then the capture stops and flushes its phrase
            drop(phrases_tx);
            worker.join().unwrap();
            assert!(rx.try_iter().next().is_none(), "a cancelled worker reports nothing");
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "no request for a cancelled dictation");
    }

    #[test]
    fn phrases_of_one_dictation_are_sent_in_order_and_all_reported() {
        let (api, hits) = counting_api();
        let (phrases_tx, phrases) = mpsc::channel::<Vec<f32>>();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || api_stream_worker(api, phrases, 32_000, Arc::new(AtomicBool::new(false)), tx));
        for _ in 0..5 {
            phrases_tx.send(tone(0.5)).unwrap();
        }
        drop(phrases_tx);
        worker.join().unwrap();
        let msgs: Vec<WorkerMsg> = rx.try_iter().collect();
        let pieces = msgs.iter().filter(|m| matches!(m, WorkerMsg::Piece(t) if t == "hi")).count();
        assert_eq!(pieces, 5);
        assert!(matches!(msgs.last(), Some(WorkerMsg::Done(Ok(5)))));
        assert_eq!(hits.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn phrases_are_joined_with_single_spaces() {
        let pieces = |p: &[&str]| join_pieces(&p.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(pieces(&["Hello there.", " How are you? ", "", "Fine."]), "Hello there. How are you? Fine.");
        assert_eq!(pieces(&[]), "");
        assert_eq!(pieces(&["  "]), "");
    }

    #[test]
    fn the_stream_worker_reports_done_when_the_recording_ends_without_phrases() {
        let (phrases_tx, phrases) = mpsc::channel::<Vec<f32>>();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn(move || stream_worker(phrases, 24_000, cancel, tx));
        drop(phrases_tx);
        assert!(matches!(rx.recv_timeout(Duration::from_secs(3)), Ok(WorkerMsg::Done(Ok(0)))));
        worker.join().unwrap();
    }

    #[test]
    fn a_cancelled_stream_worker_stops_without_a_word() {
        let (_phrases_tx, phrases) = mpsc::channel::<Vec<f32>>();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true));
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || stream_worker(phrases, 24_000, flag, tx));
        worker.join().unwrap();
        assert!(rx.try_recv().is_err());
    }

    /// Plays a 48 kHz mono 16-bit WAV (`AUDCAP_TEST_WAV`) through the capture at
    /// real-time speed and prints each phrase's text as the service returns it.
    #[test]
    #[ignore = "needs the speech service with a model loaded, and AUDCAP_TEST_WAV"]
    fn phrases_of_a_wav_are_transcribed_while_it_plays() {
        use ringbuf::traits::{Producer, Split};
        let wav = std::fs::read(std::env::var("AUDCAP_TEST_WAV").expect("AUDCAP_TEST_WAV")).unwrap();
        let data = wav.windows(4).position(|w| w == b"data").expect("a data chunk") + 8;
        let samples: Vec<f32> = wav[data..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.output_dir = dir.path().to_path_buf();
        let (mut prod, cons) = ringbuf::HeapRb::<f32>::new(48_000 * 2).split();
        let (phrases_tx, phrases) = mpsc::channel();
        let cap = Capture::from_ring(&cfg, Local::now(), cons, Some(phrases_tx)).unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = std::thread::spawn(move || stream_worker(phrases, 24_000, cancel, tx));
        let start = Instant::now();
        let mut texts = Vec::new();
        let drain = |texts: &mut Vec<String>, wait: Duration| -> bool {
            match rx.recv_timeout(wait) {
                Ok(WorkerMsg::Piece(t)) => {
                    println!("{:6.2}s  PIECE {t:?}", start.elapsed().as_secs_f32());
                    texts.push(t);
                    false
                }
                Ok(WorkerMsg::Done(r)) => {
                    println!("{:6.2}s  DONE {r:?}", start.elapsed().as_secs_f32());
                    assert!(r.is_ok());
                    true
                }
                Ok(WorkerMsg::Phase(_)) | Err(_) => false,
            }
        };
        for chunk in samples.chunks(480) {
            prod.push_slice(chunk);
            drain(&mut texts, Duration::from_millis(10));
        }
        println!("{:6.2}s  RELEASE (audio is {:.2}s)", start.elapsed().as_secs_f32(), samples.len() as f32 / 48_000.0);
        cap.finish(false).unwrap().expect("a clip");
        while !drain(&mut texts, Duration::from_secs(30)) {}
        worker.join().unwrap();
        println!("TEXT {:?}", join_pieces(&texts));
        assert!(!join_pieces(&texts).is_empty());
    }

    #[test]
    fn menu_masking_applies_to_alt_and_win_only() {
        for key in ["LeftAlt", "RightAlt", "LeftWin", "RightWin"] {
            assert!(masks_menu(&KeyId::parse(key).unwrap()), "{key}");
        }
        for key in ["RightCtrl", "F13", "CapsLock", "vk:0x45"] {
            assert!(!masks_menu(&KeyId::parse(key).unwrap()), "{key}");
        }
    }
}
