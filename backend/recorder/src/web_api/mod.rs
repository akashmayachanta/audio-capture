//! The settings/recordings window's local API, served over the WebView's
//! custom protocol. Pure request -> response so it is testable without a window.

pub mod assets;
pub mod files;
pub mod server;

use std::path::{Path, PathBuf};
use std::time::Duration;

use http::{header, Method, Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::audio::device::DeviceInfo;
use crate::config::{Config, KNOWN_APPS};
pub use crate::transcriber::LaunchStatus;

/// What the API needs from the window process (devices, dialogs, Explorer,
/// starting the transcriber).
pub trait Host {
    fn config_path(&self) -> PathBuf;
    /// (microphones, speakers)
    fn devices(&self) -> (Vec<DeviceInfo>, Vec<DeviceInfo>);
    fn pick_folder(&self, start: &Path) -> Option<PathBuf>;
    fn open_folder(&self, dir: &Path);
    fn exe_path(&self) -> PathBuf;
    /// Starts `uv run python -m app` in `dir`.
    fn start_transcriber(&self, dir: &Path) -> Result<(), String>;
    fn transcriber_status(&self) -> LaunchStatus;
    /// Waits for the next key press: (`KeyId` name, display name). `None` when
    /// nothing was pressed in time.
    fn detect_key(&self, timeout: Duration) -> Option<(String, String)>;
    /// The speech-API job queue, when it lives in this process (the tray).
    fn speech_jobs(&self) -> Option<&crate::speech_api::Jobs> {
        None
    }
    /// Answers a `/api/speech/*` job request elsewhere (the window: the tray's
    /// loopback server), when `speech_jobs` is `None`.
    fn forward_speech(&self, _req: &Request<Vec<u8>>) -> Resp {
        error(StatusCode::SERVICE_UNAVAILABLE, "speech-API jobs are not available here")
    }
    /// Settings' mic test (the window only): opens `device` (`None` = default),
    /// replacing a running test.
    fn mic_test_start(&self, _device: Option<String>, _margin_db: f32) -> Result<(), String> {
        Err("The microphone test runs in the AudCap window.".into())
    }
    /// The running test's latest numbers; `None` when none runs.
    fn mic_test_view(&self) -> Option<crate::dictation::mic_test::MicView> {
        None
    }
    fn mic_test_stop(&self) {}
}

/// How long Settings' "Detect" waits for a key.
const DETECT_KEY_TIMEOUT: Duration = Duration::from_secs(5);

type Resp = Response<Vec<u8>>;

pub fn handle(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let path = req.uri().path();
    let Some(route) = path.strip_prefix("/api/") else {
        return assets::serve(path);
    };
    let m = req.method();
    let allowed = |ok: &Method| if m == ok { None } else { Some(error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")) };
    let only = |ok: Method, f: &dyn Fn() -> Resp| allowed(&ok).unwrap_or_else(f);
    match route {
        "config" => match *m {
            Method::GET => config_response(&Config::load_or_create(&host.config_path())),
            Method::PUT => put_config(host, req.body()),
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        },
        "devices" => only(Method::GET, &|| {
            let (mics, speakers) = host.devices();
            ok_json(json!({ "mics": mics, "speakers": speakers }))
        }),
        "recordings" => only(Method::GET, &|| {
            let out = output_dir(host);
            let list: Vec<_> = crate::recordings::list(&out).iter().map(|r| files::recording_json(&out, r)).collect();
            ok_json(json!({ "output_dir": out, "recordings": list }))
        }),
        "transcript" => match *m {
            Method::GET => transcript(host, req),
            Method::PUT => put_transcript(host, req),
            Method::DELETE => delete_transcript(host, req),
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        },
        "recording" => only(Method::DELETE, &|| delete_recording(host, req)),
        "audio" => only(Method::GET, &|| audio(host, req)),
        "pcm" => only(Method::GET, &|| pcm(host, req)),
        "open-folder" => only(Method::POST, &|| open_folder(host, req.body())),
        "pick-folder" => only(Method::POST, &|| {
            let picked = host.pick_folder(&output_dir(host));
            ok_json(json!({ "path": picked }))
        }),
        "transcriber" => only(Method::GET, &|| {
            let mut v = serde_json::to_value(host.transcriber_status()).unwrap();
            v["dir"] = json!(transcriber_dir(host));
            ok_json(v)
        }),
        "transcriber/start" => only(Method::POST, &|| match transcriber_dir(host) {
            None => error(
                StatusCode::NOT_FOUND,
                "The transcriber folder was not found next to audcap.exe. Set transcriber_dir in config.toml.",
            ),
            Some(dir) => match host.start_transcriber(&dir) {
                Ok(()) => ok_json(json!({ "dir": dir })),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            },
        }),
        "speech/settings" => match *m {
            Method::GET => speech_settings(&Config::load_or_create(&host.config_path())),
            Method::PUT => put_speech_settings(host, req.body()),
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        },
        "speech/test" => only(Method::POST, &|| match serde_json::from_slice::<crate::config::SpeechApi>(req.body()) {
            Ok(cfg) => ok_json(serde_json::to_value(crate::speech_api::test(&cfg)).unwrap()),
            Err(e) => error(StatusCode::BAD_REQUEST, &format!("invalid settings: {e}")),
        }),
        "speech/jobs" | "speech/transcribe" | "speech/cancel" => {
            let want = if route == "speech/jobs" { Method::GET } else { Method::POST };
            if *m != want {
                return error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            }
            match host.speech_jobs() {
                Some(jobs) => speech_jobs(host, jobs, route, req.body()),
                None => host.forward_speech(req),
            }
        }
        "mic-test" => match *m {
            Method::GET => ok_json(json!({ "test": host.mic_test_view() })),
            Method::POST => {
                #[derive(serde::Deserialize, Default)]
                #[serde(default)]
                struct Start {
                    device: String,
                    noise_margin_db: Option<u32>,
                }
                let Ok(b) = serde_json::from_slice::<Start>(req.body()) else {
                    return error(StatusCode::BAD_REQUEST, "bad mic test request");
                };
                let margin = b.noise_margin_db.unwrap_or(Config::default().dictation.noise_margin_db).clamp(3, 30);
                match host.mic_test_start(Some(b.device).filter(|d| !d.is_empty()), margin as f32) {
                    Ok(()) => ok_json(json!({ "ok": true })),
                    Err(e) => error(StatusCode::SERVICE_UNAVAILABLE, &e),
                }
            }
            Method::DELETE => {
                host.mic_test_stop();
                ok_json(json!({ "ok": true }))
            }
            _ => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        },
        "detect-key" => only(Method::POST, &|| {
            // Esc cancels a dictation, so it cannot be a dictation key: pressing it cancels Detect.
            let key = host.detect_key(DETECT_KEY_TIMEOUT).filter(|(id, _)| id.as_str() != "Escape");
            ok_json(json!({ "key": key.map(|(id, name)| json!({ "id": id, "name": name })) }))
        }),
        _ => not_found(),
    }
}

fn transcriber_dir(host: &dyn Host) -> Option<PathBuf> {
    let c = Config::load_or_create(&host.config_path());
    crate::transcriber::find_dir(&c.transcriber_dir, &host.exe_path())
}

fn output_dir(host: &dyn Host) -> PathBuf {
    Config::load_or_create(&host.config_path()).output_dir
}

fn config_response(c: &Config) -> Resp {
    ok_json(json!({ "config": c, "known_apps": KNOWN_APPS }))
}

fn put_config(host: &dyn Host, body: &[u8]) -> Resp {
    let config = match serde_json::from_slice::<Config>(body) {
        Ok(c) => c.sanitized(),
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid settings: {e}")),
    };
    match config.save(&host.config_path()) {
        Ok(()) => config_response(&config),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not save: {e}")),
    }
}

/// `[speech_api]` with `configured`.
fn speech_settings(c: &Config) -> Resp {
    let mut v = serde_json::to_value(&c.speech_api).unwrap();
    v["configured"] = json!(c.speech_api.configured());
    ok_json(v)
}

/// Saves `[speech_api]` only; the rest of config.toml stays as it is.
fn put_speech_settings(host: &dyn Host, body: &[u8]) -> Resp {
    let api = match serde_json::from_slice::<crate::config::SpeechApi>(body) {
        Ok(a) => a.sanitized(),
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid settings: {e}")),
    };
    let mut c = Config::load_or_create(&host.config_path());
    c.speech_api = api;
    match c.save(&host.config_path()) {
        Ok(()) => speech_settings(&c),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not save: {e}")),
    }
}

/// `speech/jobs` -> `{jobs, configured, model}`; `speech/transcribe` `{id}` -> the job;
/// `speech/cancel` `{job_id}` -> `{cancelled}`.
fn speech_jobs(host: &dyn Host, jobs: &crate::speech_api::Jobs, route: &str, body: &[u8]) -> Resp {
    #[derive(Deserialize)]
    struct Transcribe {
        id: String,
    }
    #[derive(Deserialize)]
    struct Cancel {
        job_id: u64,
    }
    let c = Config::load_or_create(&host.config_path());
    match route {
        "speech/jobs" => ok_json(json!({ "jobs": jobs.list(), "configured": c.speech_api.configured(), "model": c.speech_api.model })),
        "speech/transcribe" => match serde_json::from_slice::<Transcribe>(body) {
            Ok(t) => match jobs.submit(&c.output_dir, &t.id, &c.speech_api) {
                Ok(job) => ok_json(serde_json::to_value(job).unwrap()),
                Err(e) if e == "no such recording" => error(StatusCode::NOT_FOUND, &e),
                Err(e) => error(StatusCode::CONFLICT, &e),
            },
            Err(e) => error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
        },
        _ => match serde_json::from_slice::<Cancel>(body) {
            Ok(b) => ok_json(json!({ "cancelled": jobs.cancel(b.job_id) })),
            Err(e) => error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
        },
    }
}

/// The recording `?id=` names, if it resolves inside `output_dir`.
fn recording(host: &dyn Host, req: &Request<Vec<u8>>) -> Option<PathBuf> {
    let id = query_param(req.uri().query().unwrap_or(""), "id")?;
    files::resolve_recording(&output_dir(host), &id)
}

fn transcript(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    match std::fs::read(files::sibling(&opus, "md")) {
        Ok(bytes) => response(StatusCode::OK, "text/markdown; charset=utf-8", bytes),
        Err(_) => error(StatusCode::NOT_FOUND, "not transcribed yet"),
    }
}

/// Writes the recording's `.md` (a transcript made elsewhere, e.g. by a speech
/// API): UTF-8 Markdown in the body, written as temp + rename.
fn put_transcript(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    if std::str::from_utf8(req.body()).is_err() {
        return error(StatusCode::BAD_REQUEST, "the transcript must be UTF-8 text");
    }
    let md = files::sibling(&opus, "md");
    let tmp = files::sibling(&opus, "md.tmp");
    let written = std::fs::write(&tmp, req.body()).and_then(|()| std::fs::rename(&tmp, &md));
    match written {
        Ok(()) => ok_json(json!({})),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not write the transcript: {e}"))
        }
    }
}

/// Longest piece `GET /api/pcm` returns (16 kHz 16-bit: 15 min = 29 MB).
const MAX_PCM_SECS: f64 = 900.0;

/// `?id=&ch=&start=&secs=`: channel `ch` of the recording from `start` for
/// `secs` seconds as 16 kHz mono WAV; `X-Audio-Secs` = the whole recording's
/// length, `X-Audio-Channels` = its channel count. Past the end: an empty WAV.
fn pcm(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    let q = req.uri().query().unwrap_or("");
    let num = |name: &str, default: f64| query_param(q, name).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(default);
    let ch = num("ch", 0.0).max(0.0) as usize;
    let (start, secs) = (num("start", 0.0).max(0.0), num("secs", MAX_PCM_SECS).clamp(0.0, MAX_PCM_SECS));
    let file = match std::fs::File::open(&opus) {
        Ok(f) => std::io::BufReader::new(f),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not open the recording: {e}")),
    };
    match crate::audio::extract::extract(file, ch, start, secs) {
        Ok(piece) => {
            let mut r = response(StatusCode::OK, "audio/wav", crate::audio::extract::wav(&piece.samples, crate::audio::extract::OUT_RATE));
            r.headers_mut().insert("x-audio-secs", format!("{:.3}", piece.total_secs).parse().unwrap());
            r.headers_mut().insert("x-audio-channels", piece.channels.to_string().parse().unwrap());
            r
        }
        Err(e) => error(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    }
}

/// Deletes the recording's `.md`; the recording and its sidecar stay.
fn delete_transcript(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    match std::fs::remove_file(files::sibling(&opus, "md")) {
        Ok(()) => ok_json(json!({})),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => error(StatusCode::NOT_FOUND, "not transcribed yet"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not delete the transcript: {e}")),
    }
}

/// Deletes the recording with its sidecar and transcript. The `.opus` goes
/// first: if it is in use nothing is removed.
fn delete_recording(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    if let Err(e) = std::fs::remove_file(&opus) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not delete the recording: {e}"));
    }
    for ext in ["json", "md"] {
        let _ = std::fs::remove_file(files::sibling(&opus, ext));
    }
    ok_json(json!({}))
}

fn audio(host: &dyn Host, req: &Request<Vec<u8>>) -> Resp {
    let Some(opus) = recording(host, req) else { return not_found() };
    let Ok(len) = std::fs::metadata(&opus).map(|m| m.len()) else { return not_found() };
    let range = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match range {
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
        Some(h) => match files::parse_range(h, len) {
            Ok((s, e)) => {
                let open_ended = h.trim().ends_with('-');
                let e = if open_ended { e.min(s + files::MAX_CHUNK - 1) } else { e };
                (StatusCode::PARTIAL_CONTENT, s, e)
            }
            Err(()) => {
                let mut r = error(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable");
                r.headers_mut().insert(header::CONTENT_RANGE, format!("bytes */{len}").parse().unwrap());
                return r;
            }
        },
    };
    let body = if len == 0 { Ok(Vec::new()) } else { files::read_range(&opus, start, end) };
    let Ok(body) = body else { return error(StatusCode::INTERNAL_SERVER_ERROR, "could not read the recording") };
    let mut r = response(status, "audio/ogg", body);
    r.headers_mut().insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    if status == StatusCode::PARTIAL_CONTENT {
        r.headers_mut().insert(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}").parse().unwrap());
    }
    r
}

fn open_folder(host: &dyn Host, body: &[u8]) -> Resp {
    #[derive(Deserialize, Default)]
    struct Body {
        id: Option<String>,
    }
    let b: Body = serde_json::from_slice(body).unwrap_or_default();
    let out = output_dir(host);
    let dir = b
        .id
        .and_then(|id| files::resolve_recording(&out, &id))
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or(out);
    host.open_folder(&dir);
    ok_json(json!({}))
}

/// Percent-decoded value of `name` in a query string (`+` is a space).
fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == name).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Resp {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap()
}

fn ok_json(v: serde_json::Value) -> Resp {
    response(StatusCode::OK, "application/json", v.to_string().into_bytes())
}

fn error(status: StatusCode, message: &str) -> Resp {
    response(status, "application/json", json!({ "error": message }).to_string().into_bytes())
}

fn not_found() -> Resp {
    error(StatusCode::NOT_FOUND, "not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::device::DeviceInfo;
    use crate::config::Config;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    struct FakeHost {
        dir: tempfile::TempDir,
        opened: RefCell<Vec<PathBuf>>,
        picked: Option<PathBuf>,
        started: RefCell<Vec<PathBuf>>,
        start_error: Option<String>,
        detected: Option<(String, String)>,
        detect_timeouts: RefCell<Vec<Duration>>,
        mic_tests: RefCell<Vec<(Option<String>, f32)>>,
    }

    impl FakeHost {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let host = Self {
                dir,
                opened: RefCell::new(Vec::new()),
                picked: None,
                started: RefCell::new(Vec::new()),
                start_error: None,
                detected: None,
                detect_timeouts: RefCell::new(Vec::new()),
                mic_tests: RefCell::new(Vec::new()),
            };
            // Never point a test at the real recordings folder.
            let mut c = Config::default();
            c.output_dir = host.out();
            c.save(&host.config_path()).unwrap();
            host
        }
        fn out(&self) -> PathBuf {
            self.dir.path().join("rec")
        }
        fn recording(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.out().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Host for FakeHost {
        fn config_path(&self) -> PathBuf {
            self.dir.path().join("config.toml")
        }
        fn devices(&self) -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
            (
                vec![DeviceInfo { id: "{mic-1}".into(), name: "Headset Mic".into() }],
                vec![DeviceInfo { id: "{out-1}".into(), name: "Headset Earphone".into() }],
            )
        }
        fn pick_folder(&self, _start: &Path) -> Option<PathBuf> {
            self.picked.clone()
        }
        fn open_folder(&self, dir: &Path) {
            self.opened.borrow_mut().push(dir.to_path_buf());
        }
        fn exe_path(&self) -> PathBuf {
            self.dir.path().join("app").join("audcap.exe")
        }
        fn start_transcriber(&self, dir: &Path) -> Result<(), String> {
            self.started.borrow_mut().push(dir.to_path_buf());
            self.start_error.clone().map_or(Ok(()), Err)
        }
        fn transcriber_status(&self) -> LaunchStatus {
            LaunchStatus { running: !self.started.borrow().is_empty(), ..Default::default() }
        }
        fn detect_key(&self, timeout: Duration) -> Option<(String, String)> {
            self.detect_timeouts.borrow_mut().push(timeout);
            self.detected.clone()
        }
        fn mic_test_start(&self, device: Option<String>, margin_db: f32) -> Result<(), String> {
            self.mic_tests.borrow_mut().push((device, margin_db));
            Ok(())
        }
        fn mic_test_view(&self) -> Option<crate::dictation::mic_test::MicView> {
            let running = self.mic_tests.borrow().last().is_some_and(|(d, _)| d.as_deref() != Some("stopped"));
            running.then(|| crate::dictation::mic_test::MicView { level: 0.5, threshold: 0.3, speech: true, ..Default::default() })
        }
        fn mic_test_stop(&self) {
            self.mic_tests.borrow_mut().push((Some("stopped".into()), 0.0));
        }
    }

    fn transcriber_project(host: &FakeHost) -> PathBuf {
        let d = host.dir.path().join("app").join("transcriber");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pyproject.toml"), "").unwrap();
        d
    }

    fn req(method: &str, uri: &str, body: &str) -> http::Request<Vec<u8>> {
        http::Request::builder().method(method).uri(uri).body(body.as_bytes().to_vec()).unwrap()
    }

    fn json(r: &http::Response<Vec<u8>>) -> serde_json::Value {
        serde_json::from_slice(r.body()).unwrap()
    }

    fn header<'a>(r: &'a http::Response<Vec<u8>>, name: &str) -> &'a str {
        r.headers().get(name).map(|v| v.to_str().unwrap()).unwrap_or("")
    }

    const BASE: &str = "http://audcap.localhost";

    #[test]
    fn get_config_creates_defaults_and_lists_known_apps() {
        let host = FakeHost::new();
        std::fs::remove_file(host.config_path()).unwrap();
        let r = handle(&host, &req("GET", &format!("{BASE}/api/config"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(header(&r, "content-type"), "application/json");
        let v = json(&r);
        assert_eq!(v["config"]["bitrate_kbps"], 24);
        assert_eq!(v["known_apps"][0], "Teams");
        assert!(host.config_path().is_file());
    }

    #[test]
    fn put_config_saves_sanitized_values() {
        let host = FakeHost::new();
        let mut c = serde_json::to_value(Config::load_or_create(&host.config_path())).unwrap();
        c["bitrate_kbps"] = 999.into();
        c["release_grace_secs"] = 0.into();
        let r = handle(&host, &req("PUT", &format!("{BASE}/api/config"), &c.to_string()));
        assert_eq!(r.status(), 200);
        assert_eq!(json(&r)["config"]["bitrate_kbps"], 128);
        assert_eq!(json(&r)["config"]["release_grace_secs"], 1);
        let saved = Config::load_or_create(&host.config_path());
        assert_eq!((saved.bitrate_kbps, saved.release_grace_secs), (128, 1));
    }

    #[test]
    fn put_config_rejects_bad_json() {
        let host = FakeHost::new();
        let r = handle(&host, &req("PUT", &format!("{BASE}/api/config"), "{nope"));
        assert_eq!(r.status(), 400);
        assert!(json(&r)["error"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn devices_are_listed() {
        let host = FakeHost::new();
        let v = json(&handle(&host, &req("GET", &format!("{BASE}/api/devices"), "")));
        assert_eq!(v["mics"][0]["name"], "Headset Mic");
        assert_eq!(v["speakers"][0]["id"], "{out-1}");
    }

    #[test]
    fn recordings_listing_uses_the_configured_output_dir() {
        let host = FakeHost::new();
        host.recording("2026/10/2026-10-19_1015_Zoom.opus", b"OggS");
        let v = json(&handle(&host, &req("GET", &format!("{BASE}/api/recordings"), "")));
        assert_eq!(v["output_dir"], host.out().to_string_lossy().as_ref());
        assert_eq!(v["recordings"][0]["id"], "2026/10/2026-10-19_1015_Zoom.opus");
    }

    #[test]
    fn audio_without_range_is_whole_file() {
        let host = FakeHost::new();
        host.recording("2026/10/a.opus", b"0123456789");
        let r = handle(&host, &req("GET", &format!("{BASE}/api/audio?id=2026%2F10%2Fa.opus"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(r.body().len(), 10);
        assert_eq!(header(&r, "accept-ranges"), "bytes");
        assert_eq!(header(&r, "content-type"), "audio/ogg");
    }

    #[test]
    fn audio_range_is_partial() {
        let host = FakeHost::new();
        host.recording("2026/10/a.opus", b"0123456789");
        let r = http::Request::builder()
            .uri(format!("{BASE}/api/audio?id=2026/10/a.opus"))
            .header("Range", "bytes=2-5")
            .body(Vec::new())
            .unwrap();
        let r = handle(&host, &r);
        assert_eq!(r.status(), 206);
        assert_eq!(header(&r, "content-range"), "bytes 2-5/10");
        assert_eq!(r.body(), b"2345");
    }

    #[test]
    fn audio_open_range_is_capped() {
        let host = FakeHost::new();
        host.recording("a.opus", &vec![7u8; 3 * 1024 * 1024]);
        let r = http::Request::builder()
            .uri(format!("{BASE}/api/audio?id=a.opus"))
            .header("Range", "bytes=0-")
            .body(Vec::new())
            .unwrap();
        let r = handle(&host, &r);
        assert_eq!(r.status(), 206);
        assert_eq!(r.body().len() as u64, files::MAX_CHUNK);
        assert_eq!(header(&r, "content-range"), "bytes 0-2097151/3145728");
    }

    #[test]
    fn audio_bad_range_is_416() {
        let host = FakeHost::new();
        host.recording("a.opus", b"0123456789");
        let r = http::Request::builder()
            .uri(format!("{BASE}/api/audio?id=a.opus"))
            .header("Range", "bytes=50-")
            .body(Vec::new())
            .unwrap();
        let r = handle(&host, &r);
        assert_eq!(r.status(), 416);
        assert_eq!(header(&r, "content-range"), "bytes */10");
    }

    #[test]
    fn audio_and_transcript_reject_escaping_ids() {
        let host = FakeHost::new();
        std::fs::write(host.dir.path().join("secret.opus"), b"x").unwrap();
        std::fs::write(host.dir.path().join("secret.md"), b"x").unwrap();
        for route in ["audio", "transcript"] {
            let r = handle(&host, &req("GET", &format!("{BASE}/api/{route}?id=..%2Fsecret.opus"), ""));
            assert_eq!(r.status(), 404, "{route}");
        }
    }

    #[test]
    fn transcript_is_served_or_404() {
        let host = FakeHost::new();
        let done = host.recording("2026/10/done.opus", b"OggS");
        host.recording("2026/10/todo.opus", b"OggS");
        std::fs::write(done.with_extension("md"), "# Transcript: x\n").unwrap();
        let r = handle(&host, &req("GET", &format!("{BASE}/api/transcript?id=2026/10/done.opus"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(header(&r, "content-type"), "text/markdown; charset=utf-8");
        assert_eq!(r.body(), b"# Transcript: x\n");
        let r = handle(&host, &req("GET", &format!("{BASE}/api/transcript?id=2026/10/todo.opus"), ""));
        assert_eq!(r.status(), 404);
    }

    #[test]
    fn delete_transcript_removes_only_the_md() {
        let host = FakeHost::new();
        let opus = host.recording("2026/10/done.opus", b"OggS");
        let md = opus.with_extension("md");
        std::fs::write(&md, "# Transcript: x\n").unwrap();
        std::fs::write(opus.with_extension("json"), "{}").unwrap();
        let uri = format!("{BASE}/api/transcript?id=2026/10/done.opus");
        assert_eq!(handle(&host, &req("DELETE", &uri, "")).status(), 200);
        assert!(!md.exists());
        assert!(opus.is_file() && opus.with_extension("json").is_file(), "the recording stays");
        assert_eq!(handle(&host, &req("DELETE", &uri, "")).status(), 404, "nothing left to delete");
        assert_eq!(handle(&host, &req("POST", &uri, "")).status(), 405);
    }

    #[test]
    fn speech_settings_round_trip_without_touching_the_rest() {
        let host = FakeHost::new();
        let uri = format!("{BASE}/api/speech/settings");
        assert_eq!(json(&handle(&host, &req("GET", &uri, "")))["configured"], false);
        let r = handle(&host, &req("PUT", &uri, r#"{"base_url":" https://api.openai.com/v1/ ","api_key":"k","model":"whisper-1"}"#));
        assert_eq!(r.status(), 200);
        let v = json(&r);
        assert_eq!((v["base_url"].as_str(), v["configured"].as_bool()), (Some("https://api.openai.com/v1"), Some(true)));
        let saved = Config::load_or_create(&host.config_path());
        assert_eq!(saved.speech_api.model, "whisper-1");
        assert_eq!(saved.output_dir, host.out(), "the rest is kept");
        assert_eq!(handle(&host, &req("PUT", &uri, "{nope")).status(), 400);
    }

    #[test]
    fn speech_test_checks_the_values_sent() {
        let host = FakeHost::new();
        let v = json(&handle(&host, &req("POST", &format!("{BASE}/api/speech/test"), r#"{"base_url":"","model":""}"#)));
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn speech_jobs_use_the_hosts_queue_or_are_forwarded() {
        struct JobHost(FakeHost, crate::speech_api::Jobs);
        impl Host for JobHost {
            fn config_path(&self) -> PathBuf {
                self.0.config_path()
            }
            fn devices(&self) -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
                self.0.devices()
            }
            fn pick_folder(&self, s: &Path) -> Option<PathBuf> {
                self.0.pick_folder(s)
            }
            fn open_folder(&self, d: &Path) {
                self.0.open_folder(d)
            }
            fn exe_path(&self) -> PathBuf {
                self.0.exe_path()
            }
            fn start_transcriber(&self, d: &Path) -> Result<(), String> {
                self.0.start_transcriber(d)
            }
            fn transcriber_status(&self) -> LaunchStatus {
                self.0.transcriber_status()
            }
            fn detect_key(&self, t: Duration) -> Option<(String, String)> {
                self.0.detect_key(t)
            }
            fn speech_jobs(&self) -> Option<&crate::speech_api::Jobs> {
                Some(&self.1)
            }
        }
        let host = JobHost(FakeHost::new(), crate::speech_api::Jobs::new());
        host.0.recording("a.opus", b"OggS");
        let v = json(&handle(&host, &req("GET", &format!("{BASE}/api/speech/jobs"), "")));
        assert_eq!((v["jobs"].as_array().unwrap().len(), v["configured"].as_bool()), (0, Some(false)));
        let r = handle(&host, &req("POST", &format!("{BASE}/api/speech/transcribe"), r#"{"id":"a.opus"}"#));
        assert_eq!(r.status(), 409, "not set up");
        assert_eq!(handle(&host, &req("GET", &format!("{BASE}/api/speech/transcribe"), "")).status(), 405);
        let v = json(&handle(&host, &req("POST", &format!("{BASE}/api/speech/cancel"), r#"{"job_id":7}"#)));
        assert_eq!(v["cancelled"], false);
        // Without a queue (the window), job requests go to forward_speech.
        let plain = FakeHost::new();
        assert_eq!(handle(&plain, &req("GET", &format!("{BASE}/api/speech/jobs"), "")).status(), 503);
    }

    #[test]
    fn put_transcript_writes_the_md() {
        let host = FakeHost::new();
        let opus = host.recording("2026/10/a.opus", b"OggS");
        let uri = format!("{BASE}/api/transcript?id=2026/10/a.opus");
        let r = handle(&host, &req("PUT", &uri, "# Transcript: a
"));
        assert_eq!(r.status(), 200);
        assert_eq!(std::fs::read_to_string(opus.with_extension("md")).unwrap(), "# Transcript: a
");
        assert!(!opus.with_extension("md.tmp").exists());
        let bad = http::Request::builder().method("PUT").uri(&uri).body(vec![0xff, 0xfe]).unwrap();
        assert_eq!(handle(&host, &bad).status(), 400);
        let r = handle(&host, &req("PUT", &format!("{BASE}/api/transcript?id=2026/10/missing.opus"), "x"));
        assert_eq!(r.status(), 404);
    }

    #[test]
    fn pcm_serves_a_wav_piece_or_an_error() {
        let host = FakeHost::new();
        let mut w = crate::encode::opus_ogg::OggOpusWriter::new(Vec::new(), 2, 24_000, &[]).unwrap();
        for _ in 0..50 {
            w.write_frame(&[0.0; 1920]).unwrap();
        }
        host.recording("2026/10/a.opus", &w.finish().unwrap());
        let r = handle(&host, &req("GET", &format!("{BASE}/api/pcm?id=2026/10/a.opus&ch=1&start=0&secs=0.5"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(header(&r, "content-type"), "audio/wav");
        assert_eq!(header(&r, "x-audio-channels"), "2");
        assert_eq!(r.body().len(), 44 + 8000 * 2);
        let secs: f64 = header(&r, "x-audio-secs").parse().unwrap();
        assert!((secs - 1.0).abs() < 0.05, "{secs}");
        let r = handle(&host, &req("GET", &format!("{BASE}/api/pcm?id=2026/10/a.opus&ch=5"), ""));
        assert_eq!(r.status(), 400);
        host.recording("2026/10/junk.opus", b"OggS");
        assert_eq!(handle(&host, &req("GET", &format!("{BASE}/api/pcm?id=2026/10/junk.opus"), "")).status(), 400);
    }

    #[test]
    fn delete_recording_removes_it_with_its_sidecar_and_transcript() {
        let host = FakeHost::new();
        let opus = host.recording("2026/10/gone.opus", b"OggS");
        std::fs::write(opus.with_extension("json"), "{}").unwrap();
        std::fs::write(opus.with_extension("md"), "# Transcript: x\n").unwrap();
        let other = host.recording("2026/10/kept.opus", b"OggS");
        std::fs::write(other.with_extension("md"), "# Transcript: y\n").unwrap();
        let uri = format!("{BASE}/api/recording?id=2026/10/gone.opus");
        assert_eq!(handle(&host, &req("GET", &uri, "")).status(), 405);
        assert_eq!(handle(&host, &req("DELETE", &uri, "")).status(), 200);
        for ext in ["opus", "json", "md"] {
            assert!(!opus.with_extension(ext).exists(), "{ext}");
        }
        assert!(other.is_file() && other.with_extension("md").is_file(), "other recordings stay");
        assert_eq!(handle(&host, &req("DELETE", &uri, "")).status(), 404, "nothing left to delete");
    }

    #[test]
    fn delete_recording_without_sidecar_or_transcript() {
        let host = FakeHost::new();
        let opus = host.recording("bare.opus", b"OggS");
        assert_eq!(handle(&host, &req("DELETE", &format!("{BASE}/api/recording?id=bare.opus"), "")).status(), 200);
        assert!(!opus.exists());
    }

    #[test]
    fn delete_recording_rejects_escaping_ids() {
        let host = FakeHost::new();
        let secret = host.dir.path().join("secret.opus");
        std::fs::write(&secret, b"x").unwrap();
        let r = handle(&host, &req("DELETE", &format!("{BASE}/api/recording?id=..%2Fsecret.opus"), ""));
        assert_eq!(r.status(), 404);
        assert!(secret.is_file());
    }

    #[test]
    fn delete_transcript_rejects_escaping_ids() {
        let host = FakeHost::new();
        std::fs::write(host.dir.path().join("secret.opus"), b"x").unwrap();
        let md = host.dir.path().join("secret.md");
        std::fs::write(&md, b"x").unwrap();
        let r = handle(&host, &req("DELETE", &format!("{BASE}/api/transcript?id=..%2Fsecret.opus"), ""));
        assert_eq!(r.status(), 404);
        assert!(md.is_file());
    }

    #[test]
    fn open_folder_opens_the_recordings_folder() {
        let host = FakeHost::new();
        let p = host.recording("2026/10/a.opus", b"OggS");
        let r = handle(&host, &req("POST", &format!("{BASE}/api/open-folder"), r#"{"id":"2026/10/a.opus"}"#));
        assert_eq!(r.status(), 200);
        handle(&host, &req("POST", &format!("{BASE}/api/open-folder"), r#"{"id":null}"#));
        assert_eq!(*host.opened.borrow(), vec![p.parent().unwrap().to_path_buf(), host.out()]);
    }

    #[test]
    fn pick_folder_returns_the_choice_or_null() {
        let mut host = FakeHost::new();
        let r = handle(&host, &req("POST", &format!("{BASE}/api/pick-folder"), ""));
        assert!(json(&r)["path"].is_null());
        host.picked = Some(PathBuf::from("D:/Rec"));
        let r = handle(&host, &req("POST", &format!("{BASE}/api/pick-folder"), ""));
        assert_eq!(json(&r)["path"], "D:/Rec");
    }

    #[test]
    fn transcriber_start_runs_the_found_folder() {
        let host = FakeHost::new();
        let d = transcriber_project(&host);
        let r = handle(&host, &req("POST", &format!("{BASE}/api/transcriber/start"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(*host.started.borrow(), vec![d.clone()]);
        let v = json(&handle(&host, &req("GET", &format!("{BASE}/api/transcriber"), "")));
        assert_eq!(v["dir"], d.to_string_lossy().as_ref());
        assert_eq!(v["running"], true);
    }

    #[test]
    fn transcriber_start_without_a_folder_is_an_error() {
        let host = FakeHost::new();
        let r = handle(&host, &req("POST", &format!("{BASE}/api/transcriber/start"), ""));
        assert_eq!(r.status(), 404);
        assert!(json(&r)["error"].as_str().unwrap().contains("transcriber_dir"));
        assert!(host.started.borrow().is_empty());
        let v = json(&handle(&host, &req("GET", &format!("{BASE}/api/transcriber"), "")));
        assert!(v["dir"].is_null());
        assert_eq!(v["running"], false);
    }

    #[test]
    fn transcriber_start_failure_is_reported() {
        let mut host = FakeHost::new();
        transcriber_project(&host);
        host.start_error = Some("uv was not found on PATH".into());
        let r = handle(&host, &req("POST", &format!("{BASE}/api/transcriber/start"), ""));
        assert_eq!(r.status(), 500);
        assert_eq!(json(&r)["error"], "uv was not found on PATH");
        assert_eq!(handle(&host, &req("GET", &format!("{BASE}/api/transcriber/start"), "")).status(), 405);
    }

    #[test]
    fn detect_key_returns_the_key_or_null() {
        let mut host = FakeHost::new();
        let uri = format!("{BASE}/api/detect-key");
        let r = handle(&host, &req("POST", &uri, ""));
        assert_eq!(r.status(), 200);
        assert!(json(&r)["key"].is_null(), "nothing pressed");
        host.detected = Some(("RightAlt".into(), "Right Alt".into()));
        let v = json(&handle(&host, &req("POST", &uri, "")));
        assert_eq!(v["key"]["id"], "RightAlt");
        assert_eq!(v["key"]["name"], "Right Alt");
        assert_eq!(*host.detect_timeouts.borrow(), vec![Duration::from_secs(5); 2]);
        assert_eq!(handle(&host, &req("GET", &uri, "")).status(), 405);
    }

    #[test]
    fn the_mic_test_starts_reports_and_stops() {
        let host = FakeHost::new();
        let uri = format!("{BASE}/api/mic-test");
        assert!(json(&handle(&host, &req("GET", &uri, "")))["test"].is_null(), "none running");
        let r = handle(&host, &req("POST", &uri, r#"{"device":"{mic-1}","noise_margin_db":99}"#));
        assert_eq!(r.status(), 200);
        handle(&host, &req("POST", &uri, r#"{"device":""}"#));
        assert_eq!(*host.mic_tests.borrow(), vec![(Some("{mic-1}".to_string()), 30.0), (None, 10.0)], "clamped; empty = default mic");
        let v = json(&handle(&host, &req("GET", &uri, "")));
        assert_eq!(v["test"]["speech"], true);
        assert_eq!(v["test"]["floor"], serde_json::Value::Null);
        handle(&host, &req("DELETE", &uri, ""));
        assert!(json(&handle(&host, &req("GET", &uri, "")))["test"].is_null());
        assert_eq!(handle(&host, &req("POST", &uri, "nope")).status(), 400);
    }

    #[test]
    fn detect_key_treats_escape_as_cancel() {
        let mut host = FakeHost::new();
        host.detected = Some(("Escape".into(), "Escape".into()));
        let v = json(&handle(&host, &req("POST", &format!("{BASE}/api/detect-key"), "")));
        assert!(v["key"].is_null());
    }

    #[test]
    fn non_api_paths_go_to_assets() {
        let host = FakeHost::new();
        let r = handle(&host, &req("GET", &format!("{BASE}/"), ""));
        assert_eq!(r.status(), 200);
        assert_eq!(header(&r, "content-type"), "text/html; charset=utf-8");
    }

    #[test]
    fn unknown_api_route_is_404_and_wrong_method_is_405() {
        let host = FakeHost::new();
        assert_eq!(handle(&host, &req("GET", &format!("{BASE}/api/nope"), "")).status(), 404);
        assert_eq!(handle(&host, &req("DELETE", &format!("{BASE}/api/config"), "")).status(), 405);
    }
}
