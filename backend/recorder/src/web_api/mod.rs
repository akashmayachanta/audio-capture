//! The settings/recordings window's local API, served over the WebView's
//! custom protocol. Pure request -> response so it is testable without a window.

pub mod assets;
pub mod files;

use std::path::{Path, PathBuf};

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
}

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
        "transcript" => only(Method::GET, &|| transcript(host, req)),
        "audio" => only(Method::GET, &|| audio(host, req)),
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
                "The transcriber folder was not found next to meetrec.exe. Set transcriber_dir in config.toml.",
            ),
            Some(dir) => match host.start_transcriber(&dir) {
                Ok(()) => ok_json(json!({ "dir": dir })),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            },
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
            self.dir.path().join("app").join("meetrec.exe")
        }
        fn start_transcriber(&self, dir: &Path) -> Result<(), String> {
            self.started.borrow_mut().push(dir.to_path_buf());
            self.start_error.clone().map_or(Ok(()), Err)
        }
        fn transcriber_status(&self) -> LaunchStatus {
            LaunchStatus { running: !self.started.borrow().is_empty(), ..Default::default() }
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

    const BASE: &str = "http://meetrec.localhost";

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
