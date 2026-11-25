//! Loopback HTTP for other apps on this machine (daily-driver's Recordings page):
//! the window's `web_api::handle` routes for recordings (read, delete, a WAV
//! piece of one channel, writing a transcript) and starting the transcriber. Settings, devices and the desktop actions (folder
//! picker, Explorer, key detection) stay window-only. Runs in the tray process on
//! `127.0.0.1:<web_port>`; one request per connection (`Connection: close`), so,
//! like `dictation::client`, no library is needed.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http::{header, Request, Response, StatusCode};
use serde_json::json;

use super::{handle, Host, LaunchStatus};
use crate::audio::device::DeviceInfo;

/// `api/<route>` paths served over HTTP; everything else is 404.
pub const ROUTES: &[&str] = &[
    "recordings",
    "transcript",
    "recording",
    "audio",
    "pcm",
    "transcriber",
    "transcriber/start",
    "speech/settings",
    "speech/test",
    "speech/jobs",
    "speech/transcribe",
    "speech/cancel",
];

const MAX_HEAD: usize = 16 * 1024;
/// A transcript (`PUT /api/transcript`) is the largest body.
const MAX_BODY: usize = 4 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(15);

type Resp = Response<Vec<u8>>;

/// The `Host` for HTTP callers: config and the transcriber launcher. The desktop
/// actions are never routed here (see `ROUTES`), so they do nothing.
pub struct ServerHost {
    config_path: PathBuf,
    transcriber: crate::transcriber::Launcher,
    speech: crate::speech_api::Jobs,
}

impl ServerHost {
    pub fn new(config_path: PathBuf, transcriber_log: PathBuf) -> Self {
        Self { config_path, transcriber: crate::transcriber::Launcher::new(transcriber_log), speech: crate::speech_api::Jobs::new() }
    }
}

impl Host for ServerHost {
    fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }
    fn devices(&self) -> (Vec<DeviceInfo>, Vec<DeviceInfo>) {
        (Vec::new(), Vec::new())
    }
    fn pick_folder(&self, _start: &Path) -> Option<PathBuf> {
        None
    }
    fn open_folder(&self, _dir: &Path) {}
    fn exe_path(&self) -> PathBuf {
        std::env::current_exe().unwrap_or_default()
    }
    fn start_transcriber(&self, dir: &Path) -> Result<(), String> {
        self.transcriber.start(dir, None)
    }
    fn transcriber_status(&self) -> LaunchStatus {
        self.transcriber.status()
    }
    fn detect_key(&self, _timeout: Duration) -> Option<(String, String)> {
        None
    }
    fn speech_jobs(&self) -> Option<&crate::speech_api::Jobs> {
        Some(&self.speech)
    }
}

/// Sends `req` to this server in the tray (`127.0.0.1:port`) and returns its
/// reply: how the window reaches the tray's speech-API jobs.
pub fn forward(port: u16, req: &Request<Vec<u8>>) -> Resp {
    let unreachable = || error(StatusCode::SERVICE_UNAVAILABLE, "The AudCap tray is not running, or its web_port is 0 (config.toml).");
    if port == 0 {
        return unreachable();
    }
    let Ok(mut s) = TcpStream::connect_timeout(&(std::net::Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2)) else {
        return unreachable();
    };
    let _ = s.set_read_timeout(Some(IO_TIMEOUT));
    let target = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let head = format!(
        "{} {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        req.method(),
        req.body().len()
    );
    let mut reply = Vec::new();
    let sent = s.write_all(head.as_bytes()).and_then(|()| s.write_all(req.body())).and_then(|()| s.read_to_end(&mut reply));
    if sent.is_err() {
        return unreachable();
    }
    parse_response(&reply).unwrap_or_else(|| error(StatusCode::BAD_GATEWAY, "bad reply from the tray"))
}

/// A `Connection: close` reply -> status, content type and body.
fn parse_response(raw: &[u8]) -> Option<Resp> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let content_type = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_else(|| "application/json".into());
    Response::builder().status(status).header(header::CONTENT_TYPE, content_type).body(raw[split + 4..].to_vec()).ok()
}

/// Listens on `127.0.0.1:port` on a background thread (a thread per connection).
/// `port` 0 = off.
pub fn spawn(port: u16, host: Arc<dyn Host + Send + Sync>) -> io::Result<()> {
    if port == 0 {
        return Ok(());
    }
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    tracing::info!("recordings API on http://127.0.0.1:{port}/api/");
    std::thread::Builder::new().name("web-api".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                if let Err(e) = serve(stream, port, host.as_ref()) {
                    tracing::debug!("web api connection: {e}");
                }
            });
        }
    })?;
    Ok(())
}

fn serve(stream: TcpStream, port: u16, host: &dyn Host) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let resp = match read_request(&mut reader) {
        Ok(req) => route(host, port, &req),
        Err(e) => error(StatusCode::BAD_REQUEST, &e),
    };
    let mut out = stream;
    out.write_all(&encode(&resp))?;
    out.flush()
}

/// Serves `req` if it is for an exposed route on this server's own address.
pub fn route(host: &dyn Host, port: u16, req: &Request<Vec<u8>>) -> Resp {
    // Blocks DNS rebinding: a web page must not reach the recordings through a name that resolves here.
    let host_header = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if host_header != format!("127.0.0.1:{port}") && host_header != format!("localhost:{port}") {
        return error(StatusCode::FORBIDDEN, "forbidden host");
    }
    match req.uri().path().strip_prefix("/api/") {
        Some(r) if ROUTES.contains(&r) => handle(host, req),
        _ => error(StatusCode::NOT_FOUND, "not found"),
    }
}

/// One HTTP/1.x request: request line, headers, and a `Content-Length` body.
pub fn read_request(r: &mut impl BufRead) -> Result<Request<Vec<u8>>, String> {
    let mut head_len = 0;
    let mut line = String::new();
    let mut next_line = |line: &mut String| -> Result<(), String> {
        line.clear();
        let n = r.by_ref().take((MAX_HEAD - head_len) as u64).read_line(line).map_err(|e| e.to_string())?;
        head_len += n;
        if n == 0 || !line.ends_with('\n') {
            return Err("incomplete request head".into());
        }
        Ok(())
    };
    next_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("bad request line".into());
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err("bad request line".into());
    }
    let mut builder = Request::builder().method(method).uri(target);
    let mut content_length = 0usize;
    loop {
        next_line(&mut line)?;
        let l = line.trim_end_matches(['\r', '\n']);
        if l.is_empty() {
            break;
        }
        let (name, value) = l.split_once(':').ok_or("bad header")?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().map_err(|_| "bad content-length")?;
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("chunked bodies are not supported".into());
        }
        builder = builder.header(name, value);
    }
    if content_length > MAX_BODY {
        return Err("body too large".into());
    }
    let mut body = vec![0; content_length];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    builder.body(body).map_err(|e| e.to_string())
}

/// Status line, the response's headers, `Content-Length`, `Connection: close`, body.
pub fn encode(resp: &Resp) -> Vec<u8> {
    let status = resp.status();
    let mut out = format!("HTTP/1.1 {} {}\r\n", status.as_u16(), status.canonical_reason().unwrap_or(""));
    for (name, value) in resp.headers() {
        if name != header::CONTENT_LENGTH && name != header::CONNECTION {
            out.push_str(&format!("{}: {}\r\n", name, value.to_str().unwrap_or("")));
        }
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", resp.body().len()));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(resp.body());
    bytes
}

fn error(status: StatusCode, message: &str) -> Resp {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "error": message }).to_string().into_bytes())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::io::Cursor;

    const PORT: u16 = 8766;

    fn host() -> (tempfile::TempDir, ServerHost) {
        let dir = tempfile::tempdir().unwrap();
        let host = ServerHost::new(dir.path().join("config.toml"), dir.path().join("t.log"));
        // Never point a test at the real recordings folder.
        let mut c = Config::default();
        c.output_dir = dir.path().join("rec");
        c.save(&host.config_path()).unwrap();
        (dir, host)
    }

    fn parse(raw: &str) -> Result<Request<Vec<u8>>, String> {
        read_request(&mut Cursor::new(raw.as_bytes().to_vec()))
    }

    fn get(path: &str, host_header: &str) -> Request<Vec<u8>> {
        Request::builder().uri(path).header("Host", host_header).body(Vec::new()).unwrap()
    }

    fn json(r: &Resp) -> serde_json::Value {
        serde_json::from_slice(r.body()).unwrap()
    }

    #[test]
    fn reads_a_request_with_headers_and_body() {
        let req = parse("POST /api/transcriber/start?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8766\r\nContent-Length: 2\r\nRange: bytes=0-\r\n\r\n{}rest").unwrap();
        assert_eq!(req.method(), "POST");
        assert_eq!(req.uri().path(), "/api/transcriber/start");
        assert_eq!(req.uri().query(), Some("x=1"));
        assert_eq!(req.headers()["host"], "127.0.0.1:8766");
        assert_eq!(req.headers()["range"], "bytes=0-");
        assert_eq!(req.body(), b"{}");
    }

    #[test]
    fn reads_a_request_without_body_and_bare_newlines() {
        let req = parse("GET /api/recordings HTTP/1.0\nHost: localhost:8766\n\n").unwrap();
        assert_eq!(req.method(), "GET");
        assert!(req.body().is_empty());
    }

    #[test]
    fn rejects_broken_requests() {
        assert!(parse("").is_err());
        assert!(parse("GET /api/recordings HTTP/1.1\r\nHost: x").is_err(), "head never ends");
        assert!(parse("GET\r\n\r\n").is_err());
        assert!(parse("GET http://evil/ HTTP/1.1\r\n\r\n").is_err());
        assert!(parse("GET / SMTP\r\n\r\n").is_err());
        assert!(parse("GET / HTTP/1.1\r\nno colon\r\n\r\n").is_err());
        assert!(parse("POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort").is_err());
        assert!(parse(&format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1)).is_err());
        assert!(parse("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n").is_err());
        assert!(parse(&format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD))).is_err());
    }

    #[test]
    fn serves_exposed_routes() {
        let (_dir, host) = host();
        let r = route(&host, PORT, &get("/api/recordings", "127.0.0.1:8766"));
        assert_eq!(r.status(), 200);
        assert!(json(&r)["recordings"].is_array());
        let r = route(&host, PORT, &get("/api/recordings", "localhost:8766"));
        assert_eq!(r.status(), 200);
    }

    #[test]
    fn foreign_host_header_is_forbidden() {
        let (_dir, host) = host();
        for h in ["evil.example:8766", "127.0.0.1:9999", ""] {
            assert_eq!(route(&host, PORT, &get("/api/recordings", h)).status(), 403, "{h}");
        }
    }

    #[test]
    fn window_only_routes_and_assets_are_not_served() {
        let (_dir, host) = host();
        for path in ["/api/config", "/api/devices", "/api/open-folder", "/api/pick-folder", "/api/detect-key", "/", "/index.html"] {
            assert_eq!(route(&host, PORT, &get(path, "127.0.0.1:8766")).status(), 404, "{path}");
        }
    }

    #[test]
    fn encodes_status_headers_and_body() {
        let resp = Response::builder()
            .status(206)
            .header("Content-Type", "audio/ogg")
            .header("Content-Length", "999")
            .body(b"abc".to_vec())
            .unwrap();
        let text = String::from_utf8(encode(&resp)).unwrap();
        assert!(text.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(text.contains("content-type: audio/ogg\r\n"));
        assert!(text.contains("Content-Length: 3\r\n"));
        assert!(!text.contains("999"), "the body's own length wins");
        assert!(text.ends_with("Connection: close\r\n\r\nabc"));
    }

    #[test]
    fn forward_reaches_the_server_and_reports_a_missing_one() {
        let (_dir, host) = host();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        spawn(port, Arc::new(host)).unwrap();
        let r = forward(port, &Request::builder().uri("/api/speech/jobs").body(Vec::new()).unwrap());
        assert_eq!(r.status(), 200);
        assert_eq!(json(&r)["jobs"], serde_json::json!([]));
        let r = forward(port, &Request::builder().method("POST").uri("/api/speech/cancel").body(br#"{"job_id":1}"#.to_vec()).unwrap());
        assert_eq!(json(&r)["cancelled"], false);
        assert_eq!(forward(0, &get("/api/speech/jobs", "")).status(), 503);
    }

    #[test]
    fn serves_over_tcp() {
        let (_dir, host) = host();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        spawn(port, Arc::new(host)).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /api/recordings HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.contains("\"recordings\":[]"), "{reply}");
    }
}
