//! Transcription through an OpenAI-compatible speech API
//! (`POST {base_url}/audio/transcriptions`, config `[speech_api]`), an
//! alternative to the local transcriber. Jobs run in the tray process
//! (`web_api::server::ServerHost`), one at a time; the window and other local
//! apps (daily-driver) reach them over `/api/speech/*`.
//!
//! A job reads each speaker's channel as 16 kHz WAV pieces of 10 minutes
//! (`audio::extract`, under the API's 25 MB upload limit), skips silent ones,
//! and writes `<recording>.md` in the transcriber's layout. `verbose_json` gives
//! timed segments; a model that only answers `json` (gpt-4o-transcribe) gets
//! 30-second slices, one transcript line each.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::audio::extract::{self, OUT_RATE};
use crate::config::SpeechApi;
use crate::web_api::files::{self, RecordingJson};

const PIECE_SECS: f64 = 600.0;
const SLICE_SECS: usize = 30;
/// 16-bit peak below this (about -36 dBFS) = nothing said; not sent.
const SILENCE_PEAK: i16 = 500;
/// Segments the model itself rates as not speech are dropped (Whisper invents text on noise).
const NO_SPEECH: f64 = 0.8;
const API_TIMEOUT: Duration = Duration::from_secs(600);
const TEST_TIMEOUT: Duration = Duration::from_secs(60);
const KEEP_FINISHED: usize = 50;
/// Longer than `[dictation] max_secs` can be (600).
const DICTATION_MAX_SECS: f64 = 900.0;
const DICTATION_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a meeting job's progress is asked for.
#[cfg(not(test))]
const MEETING_POLL: Duration = Duration::from_secs(2);
#[cfg(test)]
const MEETING_POLL: Duration = Duration::from_millis(5);

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    /// Seconds from the start of the piece sent.
    pub start: f64,
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub enum ApiError {
    /// The model rejected `verbose_json`: ask for plain `json`.
    NoSegments(String),
    Failed(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NoSegments(m) | ApiError::Failed(m) => f.write_str(m),
        }
    }
}

/// `multipart/form-data` body: text fields, then the file.
pub fn multipart(boundary: &str, fields: &[(&str, &str)], file_name: &str, content_type: &str, file: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(file.len() + 512);
    for (name, value) in fields {
        out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: {content_type}\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(file);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

/// The API's reply -> segments. Without `segments` (or none in the reply) the
/// whole text is one segment at 0.
pub fn parse_reply(body: &str, segments: bool) -> Vec<Segment> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        let t = body.trim();
        return if t.is_empty() { Vec::new() } else { vec![Segment { start: 0.0, text: t.to_string() }] };
    };
    if segments {
        if let Some(segs) = v["segments"].as_array().filter(|s| !s.is_empty()) {
            return segs
                .iter()
                .filter(|s| s["no_speech_prob"].as_f64().unwrap_or(0.0) < NO_SPEECH)
                .filter_map(|s| {
                    let text = s["text"].as_str().unwrap_or("").trim();
                    (!text.is_empty()).then(|| Segment { start: s["start"].as_f64().unwrap_or(0.0), text: text.to_string() })
                })
                .collect();
        }
    }
    let t = v["text"].as_str().unwrap_or("").trim();
    if t.is_empty() { Vec::new() } else { vec![Segment { start: 0.0, text: t.to_string() }] }
}

/// The message in an error reply (`{"error": {"message"}}`, `{"error": "…"}`, `{"detail"}`), else status + body.
pub fn error_message(status: u16, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let m = v["error"]["message"].as_str().or(v["error"].as_str()).or(v["detail"].as_str()).or(v["message"].as_str());
        if let Some(m) = m {
            return m.to_string();
        }
    }
    let snippet: String = body.chars().take(300).collect();
    format!("HTTP {status}: {}", snippet.trim())
}

/// Sends one WAV file.
pub fn transcribe_wav(cfg: &SpeechApi, wav: &[u8], segments: bool, timeout: Duration) -> Result<Vec<Segment>, ApiError> {
    transcribe_wav_with(cfg, wav, segments, timeout, &[])
}

/// `transcribe_wav` with extra form fields (empty values are left out).
fn transcribe_wav_with(cfg: &SpeechApi, wav: &[u8], segments: bool, timeout: Duration, extra: &[(&str, &str)]) -> Result<Vec<Segment>, ApiError> {
    let boundary = format!("audcap-{}", uuid::Uuid::new_v4().simple());
    let mut fields = vec![("model", cfg.model.as_str()), ("response_format", if segments { "verbose_json" } else { "json" })];
    if segments {
        fields.push(("timestamp_granularities[]", "segment"));
    }
    fields.extend(extra.iter().copied().filter(|(_, v)| !v.is_empty()));
    let body = multipart(&boundary, &fields, "audio.wav", "audio/wav", wav);
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into();
    let mut req = agent
        .post(format!("{}/audio/transcriptions", cfg.base_url.trim_end_matches('/')))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"));
    if !cfg.api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", cfg.api_key));
    }
    let mut resp = req.send(&body[..]).map_err(|e| ApiError::Failed(format!("Couldn't reach {}: {e}", cfg.base_url)))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string().map_err(|e| ApiError::Failed(format!("reading the reply: {e}")))?;
    if status >= 400 {
        let m = error_message(status, &text);
        if segments && (status == 400 || status == 422) && (m.contains("response_format") || m.contains("verbose_json")) {
            return Err(ApiError::NoSegments(m));
        }
        return Err(ApiError::Failed(m));
    }
    Ok(parse_reply(&text, segments))
}

/// One dictation clip (mono Ogg Opus) → its text, in a single request. A
/// silent clip gives "" without asking (Whisper invents text for silence).
pub fn dictate(cfg: &SpeechApi, opus: &[u8]) -> Result<String, String> {
    dictate_phrase(cfg, opus, "")
}

/// `dictate` for one phrase of a dictation sent phrase by phrase. `session` is
/// one id for all the phrases of a dictation, sent as `audcap_session`: a
/// Whisper Max server groups them for its review page (other APIs ignore it).
/// The text so far is deliberately not sent as `prompt`: tested on real
/// recordings, Whisper copied the earlier phrases' style (one lowercase phrase
/// made the rest lowercase) and sometimes skipped speech, with no gain.
pub fn dictate_phrase(cfg: &SpeechApi, opus: &[u8], session: &str) -> Result<String, String> {
    if !cfg.configured() {
        return Err("The speech API is not set up. Open Settings → Speech API.".into());
    }
    let piece = extract::extract(std::io::Cursor::new(opus), 0, 0.0, DICTATION_MAX_SECS).map_err(|e| format!("Could not read the dictation: {e:#}"))?;
    if piece.samples.is_empty() || silent(&piece.samples) {
        return Ok(String::new());
    }
    let wav = extract::wav(&piece.samples, OUT_RATE);
    let extra = [("audcap_session", session)];
    let segs = match transcribe_wav_with(cfg, &wav, true, DICTATION_TIMEOUT, &extra) {
        Err(ApiError::NoSegments(_)) => transcribe_wav_with(cfg, &wav, false, DICTATION_TIMEOUT, &extra),
        r => r,
    }
    .map_err(|e| e.to_string())?;
    Ok(segs.iter().map(|s| s.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" "))
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u64,
}

/// One second of silence: checks the URL, key and model.
pub fn test(cfg: &SpeechApi) -> TestResult {
    let cfg = cfg.clone().sanitized();
    if !cfg.configured() {
        return TestResult { ok: false, message: "Fill in the base URL and the model name.".into(), latency_ms: 0 };
    }
    let t0 = Instant::now();
    let r = transcribe_wav(&cfg, &extract::wav(&vec![0; OUT_RATE as usize], OUT_RATE), false, TEST_TIMEOUT);
    let latency_ms = t0.elapsed().as_millis() as u64;
    match r {
        Ok(_) => TestResult { ok: true, message: format!("Connected to {}", cfg.model), latency_ms },
        Err(e) => TestResult { ok: false, message: e.to_string(), latency_ms },
    }
}

// ── Transcript ───────────────────────────────────────────────────────────────

/// `h:mm:ss` from an hour up, else `m:ss` (the transcriber's `duration`).
fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn label(speaker: &str) -> &'static str {
    if speaker == "others" { "Others" } else { "Me" }
}

/// A line of the transcript: (start secs, speaker label as shown: "Me", "Others", "Speaker 2", ...,
/// text).
pub type Line = (f64, String, String);

fn host_of(cfg: &SpeechApi) -> &str {
    cfg.base_url.split("://").nth(1).unwrap_or(&cfg.base_url).split('/').next().unwrap_or("")
}

/// Same layout as the transcriber's `app/markdown.py` (the frontend's `transcript.ts` reads it).
pub fn to_markdown(rec: &RecordingJson, duration_secs: f64, cfg: &SpeechApi, lines: &[Line]) -> String {
    let engine = format!("{} on {} (speech API)", cfg.model, host_of(cfg));
    to_markdown_with(rec, duration_secs, &engine, &[], lines)
}

/// `to_markdown` with the engine named by the caller and extra header fields after it.
fn to_markdown_with(rec: &RecordingJson, duration_secs: f64, engine: &str, fields: &[(&str, String)], lines: &[Line]) -> String {
    let stem = Path::new(&rec.file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = vec![format!("# Transcript: {stem}"), String::new(), format!("- **Recording:** `{}`", rec.file)];
    if !rec.started.is_empty() {
        out.push(format!("- **Date:** {}", rec.started));
    }
    if !rec.apps.is_empty() {
        out.push(format!("- **Apps:** {}", rec.apps.join(", ")));
    }
    out.push(format!("- **Duration:** {}", duration(duration_secs)));
    out.push(format!("- **Transcribed with:** {engine}"));
    out.extend(fields.iter().map(|(k, v)| format!("- **{k}:** {v}")));
    out.extend(["".into(), "---".into(), "".into()]);
    if lines.is_empty() {
        out.extend(["_No speech detected._".into(), "".into()]);
    }
    for (start, who, text) in lines {
        out.push(format!("**[{}] {}:** {}", clock(*start), who, text.split_whitespace().collect::<Vec<_>>().join(" ")));
        out.push(String::new());
    }
    out.join("\n")
}

fn silent(samples: &[i16]) -> bool {
    samples.iter().step_by(8).all(|s| s.unsigned_abs() < SILENCE_PEAK as u16)
}

/// `(offset secs, samples)` slices of a piece.
fn slices(samples: &[i16], secs: usize) -> Vec<(f64, &[i16])> {
    samples.chunks(secs * OUT_RATE as usize).enumerate().map(|(i, c)| ((i * secs) as f64, c)).collect()
}

// ── Jobs ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct JobView {
    pub id: u64,
    pub rec_id: String,
    /// queued | running | done | error | cancelled
    pub state: String,
    pub phase: String,
    pub progress: f64,
    pub error: String,
    /// Seconds since the epoch.
    pub created: f64,
    pub source: &'static str,
}

struct Job {
    view: JobView,
    cfg: SpeechApi,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct State {
    jobs: VecDeque<Job>,
    next_id: u64,
}

/// The tray's queue of speech-API jobs (one worker thread).
pub struct Jobs {
    state: Arc<Mutex<State>>,
    worker: Mutex<Option<Sender<u64>>>,
}

impl Default for Jobs {
    fn default() -> Self {
        Self::new()
    }
}

impl Jobs {
    pub fn new() -> Self {
        Self { state: Arc::new(Mutex::new(State { jobs: VecDeque::new(), next_id: 1 })), worker: Mutex::new(None) }
    }

    pub fn list(&self) -> Vec<JobView> {
        self.state.lock().unwrap().jobs.iter().map(|j| j.view.clone()).collect()
    }

    /// Queues `rec_id` (under `root`); an already queued/running job for it is returned instead.
    pub fn submit(&self, root: &Path, rec_id: &str, cfg: &SpeechApi) -> Result<JobView, String> {
        if !cfg.configured() {
            return Err("Set up the speech API in Settings first.".into());
        }
        if files::resolve_recording(root, rec_id).is_none() {
            return Err("no such recording".into());
        }
        let mut st = self.state.lock().unwrap();
        if let Some(j) = st.jobs.iter().find(|j| j.view.rec_id == rec_id && matches!(j.view.state.as_str(), "queued" | "running")) {
            return Ok(j.view.clone());
        }
        let id = st.next_id;
        st.next_id += 1;
        let created = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let view = JobView { id, rec_id: rec_id.into(), state: "queued".into(), phase: "Queued".into(), progress: 0.0, error: String::new(), created, source: "api" };
        st.jobs.push_back(Job { view: view.clone(), cfg: cfg.clone(), root: root.to_path_buf(), cancel: Arc::new(AtomicBool::new(false)) });
        while st.jobs.iter().filter(|j| !matches!(j.view.state.as_str(), "queued" | "running")).count() > KEEP_FINISHED {
            if let Some(i) = st.jobs.iter().position(|j| !matches!(j.view.state.as_str(), "queued" | "running")) {
                st.jobs.remove(i);
            }
        }
        drop(st);
        self.sender().send(id).map_err(|_| "the speech worker stopped".to_string())?;
        Ok(view)
    }

    pub fn cancel(&self, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        let Some(j) = st.jobs.iter_mut().find(|j| j.view.id == id) else { return false };
        match j.view.state.as_str() {
            "queued" => {
                j.cancel.store(true, Ordering::Relaxed);
                (j.view.state, j.view.phase) = ("cancelled".into(), "Cancelled".into());
                true
            }
            "running" => {
                j.cancel.store(true, Ordering::Relaxed);
                j.view.phase = "Cancelling".into();
                true
            }
            _ => false,
        }
    }

    fn sender(&self) -> Sender<u64> {
        let mut w = self.worker.lock().unwrap();
        if let Some(tx) = w.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel::<u64>();
        let state = Arc::clone(&self.state);
        std::thread::Builder::new()
            .name("speech-api".into())
            .spawn(move || {
                for id in rx {
                    work(&state, id);
                }
            })
            .expect("spawn speech worker");
        *w = Some(tx.clone());
        tx
    }
}

fn work(state: &Arc<Mutex<State>>, id: u64) {
    let (rec_id, cfg, root, cancel) = {
        let mut st = state.lock().unwrap();
        let Some(j) = st.jobs.iter_mut().find(|j| j.view.id == id) else { return };
        if j.view.state != "queued" {
            return;
        }
        (j.view.state, j.view.phase) = ("running".into(), "Starting".into());
        (j.view.rec_id.clone(), j.cfg.clone(), j.root.clone(), Arc::clone(&j.cancel))
    };
    let update = |phase: &str, progress: Option<f64>| {
        let mut st = state.lock().unwrap();
        if let Some(j) = st.jobs.iter_mut().find(|j| j.view.id == id) {
            if j.view.phase != "Cancelling" {
                j.view.phase = phase.to_string();
            }
            if let Some(p) = progress {
                j.view.progress = p;
            }
        }
    };
    let result = run(&root, &rec_id, &cfg, &cancel, &update);
    let mut st = state.lock().unwrap();
    if let Some(j) = st.jobs.iter_mut().find(|j| j.view.id == id) {
        match result {
            Ok(()) => (j.view.state, j.view.phase, j.view.progress) = ("done".into(), "Done".into(), 1.0),
            Err(_) if cancel.load(Ordering::Relaxed) => (j.view.state, j.view.phase) = ("cancelled".into(), "Cancelled".into()),
            Err(e) => {
                tracing::warn!("speech API transcription of {rec_id} failed: {e}");
                (j.view.state, j.view.phase, j.view.error) = ("error".into(), "Failed".into(), e);
            }
        }
    }
}

/// Transcribes one recording and writes its `.md`. `update(phase, progress)`.
///
/// A Whisper Max server (its `/health` says `"meetings": true`) gets the whole recording as one
/// meeting job: it cuts the speech only at pauses, decides each chunk's language, drops the echo of
/// laptop speakers and fills in what Whisper skipped, and gives back lines labelled Me / Others
/// (`run_meeting`). Any other OpenAI-compatible API gets each channel in 10-minute pieces (`run_pieces`).
pub fn run(root: &Path, rec_id: &str, cfg: &SpeechApi, cancel: &AtomicBool, update: &dyn Fn(&str, Option<f64>)) -> Result<(), String> {
    let path = files::resolve_recording(root, rec_id).ok_or("The recording is gone.")?;
    let rec = crate::recordings::list(root)
        .iter()
        .map(|r| files::recording_json(root, r))
        .find(|r| r.id == rec_id)
        .ok_or("The recording is gone.")?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    update("Connecting to the speech API", None);
    if meeting_server(cfg)? {
        run_meeting(&path, &rec, cfg, cancel, update)
    } else {
        run_pieces(&path, &rec, cfg, cancel, update)
    }
}

/// `{base_url without /v1}/health`: true when it is a Whisper Max server that takes meeting jobs.
/// A server that can't be reached is an error (nothing is retried: click Transcribe again).
fn meeting_server(cfg: &SpeechApi) -> Result<bool, String> {
    let base = cfg.base_url.trim_end_matches('/');
    let origin = base.strip_suffix("/v1").unwrap_or(base);
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(HEALTH_TIMEOUT)).http_status_as_error(false).build().into();
    let mut resp = agent
        .get(format!("{origin}/health"))
        .call()
        .map_err(|e| format!("Couldn't reach the speech API at {}: {e}. Is the server running? Start it and click Transcribe again.", cfg.base_url))?;
    if resp.status().as_u16() != 200 {
        return Ok(false);
    }
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    Ok(serde_json::from_str::<Value>(&body).map(|v| v["meetings"] == Value::Bool(true)).unwrap_or(false))
}

fn write_md(path: &Path, md: String) -> Result<(), String> {
    let (dest, tmp) = (files::sibling(path, "md"), files::sibling(path, "md.tmp"));
    std::fs::write(&tmp, md).and_then(|()| std::fs::rename(&tmp, &dest)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write the transcript: {e}")
    })
}

fn language_name(code: &str) -> String {
    match code {
        "en" => "English".into(),
        "te" => "Telugu".into(),
        "hi" => "Hindi".into(),
        other => other.into(),
    }
}

/// One request to a meeting job: (status, JSON body).
fn meeting_call(agent: &ureq::Agent, method: &str, url: &str, auth: &str) -> Result<(u16, Value), String> {
    let lost = |e: ureq::Error| format!("Lost the connection to the speech API: {e}. Start it and click Transcribe again; it carries on where it stopped.");
    let mut resp = match method {
        "DELETE" => agent.delete(url).header("Authorization", auth).call(),
        _ => agent.get(url).header("Authorization", auth).call(),
    }
    .map_err(lost)?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().with_config().limit(64 * 1024 * 1024).read_to_string().map_err(lost)?;
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// The whole recording to Whisper Max's `/v1/meetings`; follows the job, writes the `.md` from its
/// segments, then deletes the job (and with it the server's copy of the audio).
fn run_meeting(path: &Path, rec: &RecordingJson, cfg: &SpeechApi, cancel: &AtomicBool, update: &dyn Fn(&str, Option<f64>)) -> Result<(), String> {
    let base = cfg.base_url.trim_end_matches('/');
    let auth = format!("Bearer {}", cfg.api_key);
    let bytes = std::fs::read(path).map_err(|e| format!("could not open the recording: {e}"))?;
    let channels = match rec.speakers.as_slice() {
        ["me"] => "me",
        ["others"] => "others",
        _ => "stereo",
    };
    let stem = Path::new(&rec.file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let boundary = format!("audcap-{}", uuid::Uuid::new_v4().simple());
    let fields = [("title", stem.as_str()), ("recording", rec.id.as_str()), ("channels", channels), ("language", "auto"), ("started", rec.started.as_str())];
    let body = multipart(&boundary, &fields, &rec.file, "audio/ogg", &bytes);
    update("Sending the recording", None);
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(API_TIMEOUT)).http_status_as_error(false).build().into();
    let mut resp = agent
        .post(format!("{base}/meetings"))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .header("Authorization", &auth)
        .send(&body[..])
        .map_err(|e| format!("Couldn't send the recording to {}: {e}", cfg.base_url))?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| format!("reading the reply: {e}"))?;
    if status >= 400 {
        return Err(error_message(status, &text));
    }
    let job: Value = serde_json::from_str(&text).map_err(|_| format!("The speech API answered something unexpected: {}", text.chars().take(200).collect::<String>()))?;
    let id = job["id"].as_str().ok_or("The speech API gave no job id.")?.to_string();
    let url = format!("{base}/meetings/{id}");
    let poll: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).http_status_as_error(false).build().into();
    let forget = || {
        let _ = meeting_call(&poll, "DELETE", &url, &auth);
    };
    let job = loop {
        if cancel.load(Ordering::Relaxed) {
            forget();
            return Err("cancelled".into());
        }
        let (status, job) = meeting_call(&poll, "GET", &url, &auth)?;
        if status == 404 {
            return Err("The speech API no longer has this job. Click Transcribe again.".into());
        }
        if status >= 400 {
            return Err(error_message(status, &job.to_string()));
        }
        match job["state"].as_str().unwrap_or("") {
            "done" => break job,
            "error" => {
                forget();
                return Err(job["error"].as_str().filter(|e| !e.is_empty()).unwrap_or("The transcription failed.").to_string());
            }
            "cancelled" => return Err("The job was cancelled on the speech API.".into()),
            _ => update(job["phase"].as_str().unwrap_or("Transcribing"), job["progress"].as_f64().map(|p| p.min(0.99))),
        }
        std::thread::sleep(MEETING_POLL);
    };
    update("Saving", Some(0.99));
    let segments = job["segments"].as_array().cloned().unwrap_or_default();
    let lines: Vec<Line> = segments
        .iter()
        .filter_map(|s| {
            let text = s["text"].as_str()?.trim();
            (!text.is_empty()).then(|| (s["start"].as_f64().unwrap_or(0.0), s["label"].as_str().unwrap_or("Others").to_string(), text.to_string()))
        })
        .collect();
    let mut fields: Vec<(&str, String)> = Vec::new();
    let speakers: Vec<&str> = job["speakers"].as_array().map(|a| a.iter().filter_map(|s| s["label"].as_str()).collect()).unwrap_or_default();
    if !speakers.is_empty() {
        fields.push(("Speakers", speakers.join(", ")));
    }
    // Languages: only worth a line when something was not English.
    let mut langs: Vec<String> = Vec::new();
    for s in job["speakers"].as_array().into_iter().flatten() {
        let (Some(id), Some(name)) = (s["id"].as_str(), s["label"].as_str()) else { continue };
        let Some(counts) = job["languages"][id].as_object() else { continue };
        if counts.keys().any(|l| l != "en") {
            let mut parts: Vec<(String, u64)> = counts.iter().map(|(l, n)| (language_name(l), n.as_u64().unwrap_or(0))).collect();
            parts.sort_by(|a, b| b.1.cmp(&a.1));
            langs.push(format!("{name}: {}", parts.iter().map(|(l, n)| format!("{l} ({n})")).collect::<Vec<_>>().join(", ")));
        }
    }
    if !langs.is_empty() {
        fields.push(("Languages", langs.join("; ")));
    }
    let total = job["duration"].as_f64().filter(|d| *d > 0.0).or(rec.duration_secs).unwrap_or(0.0);
    let engine = format!("{} on {} (meeting job)", cfg.model, host_of(cfg));
    write_md(path, to_markdown_with(rec, total, &engine, &fields, &lines))?;
    forget();
    Ok(())
}

/// Each channel in 10-minute pieces to `/audio/transcriptions` (any OpenAI-compatible API).
fn run_pieces(path: &Path, rec: &RecordingJson, cfg: &SpeechApi, cancel: &AtomicBool, update: &dyn Fn(&str, Option<f64>)) -> Result<(), String> {
    let speakers = if rec.speakers.is_empty() { vec!["me", "others"] } else { rec.speakers.clone() };
    let check = || if cancel.load(Ordering::Relaxed) { Err("cancelled".to_string()) } else { Ok(()) };
    let mut use_segments = true;
    let mut total = rec.duration_secs.unwrap_or(0.0);
    let mut lines: Vec<Line> = Vec::new();
    for (ch, &speaker) in speakers.iter().enumerate() {
        let who = label(speaker);
        let mut start = 0.0;
        loop {
            check()?;
            update(&format!("{who}: reading audio"), None);
            let file = File::open(&path).map_err(|e| format!("could not open the recording: {e}"))?;
            let piece = extract::extract(BufReader::new(file), ch, start, PIECE_SECS).map_err(|e| format!("{e:#}"))?;
            total = piece.total_secs.max(total);
            if piece.samples.is_empty() {
                break;
            }
            let mut parts = if use_segments { vec![(0.0, &piece.samples[..])] } else { slices(&piece.samples, SLICE_SECS) };
            let mut i = 0;
            while i < parts.len() {
                let (offset, samples) = parts[i];
                i += 1;
                check()?;
                if silent(samples) {
                    continue;
                }
                update(&format!("{who}: transcribing {}", clock(start + offset)), None);
                match transcribe_wav(cfg, &extract::wav(samples, OUT_RATE), use_segments, API_TIMEOUT) {
                    Ok(segs) => lines.extend(segs.into_iter().map(|s| (start + offset + s.start, who.to_string(), s.text))),
                    Err(ApiError::NoSegments(_)) => {
                        use_segments = false;
                        parts = slices(&piece.samples, SLICE_SECS);
                        i = 0;
                        continue;
                    }
                    Err(e) => return Err(e.to_string()),
                }
                let done = (start + offset + samples.len() as f64 / OUT_RATE as f64) / total.max(1.0);
                update(&format!("{who}: transcribing"), Some(((ch as f64 + done.min(1.0)) / speakers.len() as f64).min(0.99)));
            }
            start += PIECE_SECS;
            if start >= piece.total_secs {
                break;
            }
        }
    }
    check()?;
    update("Saving", Some(0.99));
    let order = |w: &str| if w == "Me" { 0 } else { 1 };
    lines.sort_by(|a, b| a.0.total_cmp(&b.0).then(order(&a.1).cmp(&order(&b.1))));
    write_md(path, to_markdown(rec, total, cfg, &lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::opus_ogg::OggOpusWriter;
    use std::io::{BufReader, Write};
    use std::net::TcpListener;

    fn cfg(base: &str) -> SpeechApi {
        SpeechApi { base_url: base.into(), api_key: "sk-test".into(), model: "whisper-1".into() }
    }

    #[test]
    fn multipart_has_fields_then_the_file() {
        let body = multipart("B", &[("model", "m")], "a.wav", "audio/wav", b"RIFF");
        let text = String::from_utf8(body).unwrap();
        assert_eq!(
            text,
            "--B\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nm\r\n\
             --B\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFF\r\n--B--\r\n"
        );
    }

    #[test]
    fn replies_become_segments() {
        let verbose = r#"{"text":"hi there","segments":[{"start":1.5,"text":" hi ","no_speech_prob":0.1},{"start":3,"text":"noise","no_speech_prob":0.95},{"start":4,"text":" "}]}"#;
        assert_eq!(parse_reply(verbose, true), vec![Segment { start: 1.5, text: "hi".into() }]);
        assert_eq!(parse_reply(r#"{"text":" all of it "}"#, true), vec![Segment { start: 0.0, text: "all of it".into() }]);
        assert_eq!(parse_reply(verbose, false), vec![Segment { start: 0.0, text: "hi there".into() }]);
        assert_eq!(parse_reply("plain text", false), vec![Segment { start: 0.0, text: "plain text".into() }]);
        assert!(parse_reply(r#"{"text":""}"#, false).is_empty());
    }

    #[test]
    fn error_messages_are_read_from_common_shapes() {
        assert_eq!(error_message(401, r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(error_message(400, r#"{"error":"nope"}"#), "nope");
        assert_eq!(error_message(422, r#"{"detail":"x"}"#), "x");
        assert_eq!(error_message(502, "<html>gateway</html>"), "HTTP 502: <html>gateway</html>");
    }

    #[test]
    fn markdown_matches_the_transcriber_layout() {
        let rec = RecordingJson {
            id: "2026/11/2026-11-25_1015_Teams.opus".into(),
            file: "2026-11-25_1015_Teams.opus".into(),
            started: "2026-11-25 10:15".into(),
            started_iso: String::new(),
            duration_secs: Some(65.0),
            apps: vec!["Teams".into()],
            trigger: None,
            status: "complete".into(),
            speakers: vec!["me", "others"],
            transcribed: false,
            transcript_mtime: None,
        };
        let md = to_markdown(&rec, 65.0, &cfg("https://api.openai.com/v1"), &[(1.0, "Me".into(), "hello  there".into()), (3725.0, "Others".into(), "hi".into())]);
        assert_eq!(
            md,
            "# Transcript: 2026-11-25_1015_Teams\n\n- **Recording:** `2026-11-25_1015_Teams.opus`\n- **Date:** 2026-11-25 10:15\n\
             - **Apps:** Teams\n- **Duration:** 1:05\n- **Transcribed with:** whisper-1 on api.openai.com (speech API)\n\n---\n\n\
             **[00:00:01] Me:** hello there\n\n**[01:02:05] Others:** hi\n"
        );
        assert!(to_markdown(&rec, 0.0, &cfg("http://x"), &[]).contains("_No speech detected._"));
    }

    #[test]
    fn silence_and_slices() {
        assert!(silent(&[0, 10, -400]));
        assert!(!silent(&[0, 0, 0, 0, 0, 0, 0, 0, -3000]), "every 8th sample is checked");
        let s = vec![1i16; OUT_RATE as usize * 65];
        let parts = slices(&s, 30);
        assert_eq!(parts.iter().map(|p| (p.0, p.1.len())).collect::<Vec<_>>(), vec![(0.0, 480_000), (30.0, 480_000), (60.0, 80_000)]);
    }

    #[test]
    fn unconfigured_settings_fail_the_test_without_a_request() {
        let r = test(&SpeechApi::default());
        assert!(!r.ok && r.message.contains("base URL"));
    }

    /// `/health` of a server that is not Whisper Max (no meeting jobs).
    const NOT_WHISPER_MAX: (u16, &str) = (404, r#"{"detail":"Not Found"}"#);
    const WHISPER_MAX: (u16, &str) = (200, r#"{"ok":true,"meetings":true}"#);

    /// A fake API: answers each request with the next canned (status, body), and records the requests.
    fn fake_api(replies: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for (stream, (status, body)) in listener.incoming().flatten().zip(replies) {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let req = crate::web_api::server::read_request(&mut r).unwrap();
                log.lock().unwrap().push(format!(
                    "{} {} auth={} {}",
                    req.method(),
                    req.uri().path(),
                    req.headers().get("authorization").map(|v| v.to_str().unwrap()).unwrap_or(""),
                    String::from_utf8_lossy(req.body())
                        .lines()
                        .filter(|l| l.contains("json") || l.contains("audcap_session") || matches!(*l, "stereo" | "me" | "others"))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                let mut s = stream;
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (url, seen)
    }

    /// `<root>/a.opus`: `secs` of stereo audio, a tone on the left (me), silence on the right.
    fn recording(root: &Path, secs: usize) {
        let mut w = OggOpusWriter::new(Vec::new(), 2, 24_000, &[]).unwrap();
        let mut n = 0usize;
        for _ in 0..secs * 50 {
            let mut pcm = vec![0f32; 1920];
            for i in 0..960 {
                pcm[i * 2] = 0.5 * (2.0 * std::f32::consts::PI * 300.0 * (n + i) as f32 / 48_000.0).sin();
            }
            n += 960;
            w.write_frame(&pcm).unwrap();
        }
        std::fs::write(root.join("a.opus"), w.finish().unwrap()).unwrap();
    }

    #[test]
    fn a_job_transcribes_the_speaking_channel_and_writes_the_md() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 2);
        let (url, seen) = fake_api(vec![NOT_WHISPER_MAX, (200, r#"{"segments":[{"start":0.5,"text":"hello"}]}"#)]);
        run(dir.path(), "a.opus", &cfg(&url), &AtomicBool::new(false), &|_, _| {}).unwrap();
        let md = std::fs::read_to_string(dir.path().join("a.md")).unwrap();
        assert!(md.contains("**[00:00:00] Me:** hello"), "{md}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "health, then only the left channel (the right is silent): {seen:?}");
        assert!(seen[0].starts_with("GET /health"), "{}", seen[0]);
        assert!(seen[1].starts_with("POST /v1/audio/transcriptions auth=Bearer sk-test verbose_json"), "{}", seen[1]);
    }

    #[test]
    fn a_model_without_segments_gets_plain_json() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, seen) = fake_api(vec![
            NOT_WHISPER_MAX,
            (400, r#"{"error":{"message":"response_format 'verbose_json' is not compatible with model"}}"#),
            (200, r#"{"text":"hi"}"#),
        ]);
        run(dir.path(), "a.opus", &cfg(&url), &AtomicBool::new(false), &|_, _| {}).unwrap();
        assert!(std::fs::read_to_string(dir.path().join("a.md")).unwrap().contains("Me:** hi"));
        assert!(seen.lock().unwrap()[2].ends_with(" json"), "{:?}", seen.lock().unwrap());
    }

    #[test]
    fn api_errors_fail_the_job_and_write_nothing() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, _) = fake_api(vec![NOT_WHISPER_MAX, (401, r#"{"error":{"message":"Incorrect API key"}}"#)]);
        let e = run(dir.path(), "a.opus", &cfg(&url), &AtomicBool::new(false), &|_, _| {}).unwrap_err();
        assert_eq!(e, "Incorrect API key");
        assert!(!dir.path().join("a.md").exists());
    }

    /// A mono dictation clip: `secs` of a tone, or of silence.
    fn clip(secs: usize, tone: bool) -> Vec<u8> {
        let samples: Vec<f32> = (0..secs * 48_000)
            .map(|n| if tone { 0.5 * (2.0 * std::f32::consts::PI * 300.0 * n as f32 / 48_000.0).sin() } else { 0.0 })
            .collect();
        crate::dictation::capture::encode_clip(&samples, 32_000).unwrap()
    }

    #[test]
    fn a_dictation_clip_is_sent_whole_and_its_segments_joined() {
        let (url, seen) = fake_api(vec![(200, r#"{"segments":[{"start":0,"text":" Hello"},{"start":1,"text":"there. "}]}"#)]);
        assert_eq!(dictate(&cfg(&url), &clip(2, true)).unwrap(), "Hello there.");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("POST /v1/audio/transcriptions auth=Bearer sk-test verbose_json"), "{}", seen[0]);
    }

    #[test]
    fn a_phrase_carries_the_dictation_id() {
        let (url, seen) = fake_api(vec![(200, r#"{"text":"to Gachibowli"}"#)]);
        assert_eq!(dictate_phrase(&cfg(&url), &clip(1, true), "abc").unwrap(), "to Gachibowli");
        let seen = seen.lock().unwrap();
        assert!(seen[0].contains("name=\"audcap_session\""), "{}", seen[0]);
        assert!(!seen[0].contains("prompt"), "{}", seen[0]);
    }

    #[test]
    fn a_whole_dictation_sends_no_dictation_id() {
        let (url, seen) = fake_api(vec![(200, r#"{"text":"hi"}"#)]);
        dictate(&cfg(&url), &clip(1, true)).unwrap();
        assert!(!seen.lock().unwrap()[0].contains("audcap_session"), "empty fields are left out");
    }

    #[test]
    fn a_dictation_falls_back_to_plain_json() {
        let (url, _) = fake_api(vec![
            (400, r#"{"error":{"message":"response_format 'verbose_json' is not compatible with model"}}"#),
            (200, r#"{"text":"hi"}"#),
        ]);
        assert_eq!(dictate(&cfg(&url), &clip(1, true)).unwrap(), "hi");
    }

    #[test]
    fn a_silent_dictation_is_not_sent() {
        let (url, seen) = fake_api(vec![(200, r#"{"text":"Thank you."}"#)]);
        assert_eq!(dictate(&cfg(&url), &clip(1, false)).unwrap(), "");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_dictation_needs_the_api_set_up() {
        assert!(dictate(&SpeechApi::default(), &clip(1, true)).unwrap_err().contains("Settings"));
    }

    #[test]
    fn jobs_queue_run_and_report() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, _) = fake_api(vec![NOT_WHISPER_MAX, (200, r#"{"text":"ok"}"#)]);
        let jobs = Jobs::new();
        assert!(jobs.submit(dir.path(), "a.opus", &SpeechApi::default()).is_err(), "not set up");
        assert!(jobs.submit(dir.path(), "missing.opus", &cfg(&url)).is_err());
        let j = jobs.submit(dir.path(), "a.opus", &cfg(&url)).unwrap();
        assert_eq!((j.source, j.state.as_str()), ("api", "queued"));
        let t0 = Instant::now();
        while jobs.list()[0].state != "done" {
            assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", jobs.list());
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(dir.path().join("a.md").is_file());
        assert!(!jobs.cancel(j.id), "finished jobs can't be cancelled");
    }

    #[test]
    fn a_cancelled_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let e = run(dir.path(), "a.opus", &cfg("http://127.0.0.1:9"), &AtomicBool::new(true), &|_, _| {}).unwrap_err();
        assert_eq!(e, "cancelled");
        assert!(!dir.path().join("a.md").exists());
    }

    const MEETING_DONE: &str = r#"{"id":"J1","state":"done","progress":1,"duration":3800,
        "speakers":[{"id":"me","label":"Me"},{"id":"others","label":"Others"}],
        "languages":{"me":{"en":3},"others":{"en":5,"te":2}},
        "segments":[{"speaker":"me","label":"Me","start":1.0,"end":2,"text":" hello  all ","language":"en"},
                    {"speaker":"others","label":"Others","start":3725.0,"end":3730,"text":"namaskaram andi","language":"te"},
                    {"speaker":"others","label":"Others","start":3726.0,"end":3727,"text":"","language":"en"}]}"#;

    #[test]
    fn whisper_max_gets_the_whole_meeting_as_one_job() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, seen) = fake_api(vec![
            WHISPER_MAX,
            (200, r#"{"id":"J1","state":"queued","phase":"Waiting in line","progress":0}"#),
            (200, r#"{"id":"J1","state":"running","phase":"Transcribing 00:14:30 of 01:03:20","progress":0.5}"#),
            (200, MEETING_DONE),
            (200, r#"{"deleted":true}"#),
        ]);
        let phases = Mutex::new(Vec::new());
        run(dir.path(), "a.opus", &cfg(&url), &AtomicBool::new(false), &|p, f| phases.lock().unwrap().push((p.to_string(), f))).unwrap();
        let md = std::fs::read_to_string(dir.path().join("a.md")).unwrap();
        assert!(md.contains("**[00:00:01] Me:** hello all\n"), "{md}");
        assert!(md.contains("**[01:02:05] Others:** namaskaram andi\n"), "{md}");
        assert!(!md.contains("[01:02:06]"), "empty lines are left out: {md}");
        assert!(md.contains("- **Speakers:** Me, Others\n"), "{md}");
        assert!(md.contains("- **Languages:** Others: English (5), Telugu (2)\n"), "only where something was not English: {md}");
        assert!(md.contains("- **Duration:** 1:03:20\n"), "{md}");
        let seen = seen.lock().unwrap();
        let paths: Vec<&str> = seen.iter().map(|s| s.split(" auth=").next().unwrap()).collect();
        assert_eq!(paths, ["GET /health", "POST /v1/meetings", "GET /v1/meetings/J1", "GET /v1/meetings/J1", "DELETE /v1/meetings/J1"]);
        assert!(seen[1].contains("auth=Bearer sk-test") && seen[1].contains("stereo"), "{}", seen[1]);
        assert!(phases.lock().unwrap().contains(&("Transcribing 00:14:30 of 01:03:20".to_string(), Some(0.5))), "{:?}", phases.lock().unwrap());
    }

    #[test]
    fn a_failed_meeting_job_reports_its_error_and_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, seen) = fake_api(vec![
            WHISPER_MAX,
            (200, r#"{"id":"J2","state":"queued"}"#),
            (200, r#"{"id":"J2","state":"error","error":"Could not read the audio."}"#),
            (200, r#"{"deleted":true}"#),
        ]);
        let e = run(dir.path(), "a.opus", &cfg(&url), &AtomicBool::new(false), &|_, _| {}).unwrap_err();
        assert_eq!(e, "Could not read the audio.");
        assert!(!dir.path().join("a.md").exists());
        assert!(seen.lock().unwrap().last().unwrap().starts_with("DELETE /v1/meetings/J2"));
    }

    #[test]
    fn cancelling_a_meeting_job_deletes_it_on_the_server() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let (url, seen) = fake_api(vec![
            WHISPER_MAX,
            (200, r#"{"id":"J3","state":"queued"}"#),
            (200, r#"{"id":"J3","state":"running","phase":"Transcribing Me (00:00:01)","progress":0.1}"#),
            (200, r#"{"deleted":true}"#),
        ]);
        let cancel = AtomicBool::new(false);
        let e = run(dir.path(), "a.opus", &cfg(&url), &cancel, &|p, _| {
            if p.starts_with("Transcribing") {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert_eq!(e, "cancelled");
        assert!(!dir.path().join("a.md").exists());
        assert!(seen.lock().unwrap().last().unwrap().starts_with("DELETE /v1/meetings/J3"));
    }

    #[test]
    fn a_server_that_is_not_running_gives_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        recording(dir.path(), 1);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port(); // closed again at once
        let e = run(dir.path(), "a.opus", &cfg(&format!("http://127.0.0.1:{port}/v1")), &AtomicBool::new(false), &|_, _| {}).unwrap_err();
        assert!(e.contains("Couldn't reach") && e.contains("click Transcribe again"), "{e}");
        assert!(!dir.path().join("a.md").exists());
    }
}
