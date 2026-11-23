//! A minimal HTTP/1.1 client for the local transcriber (`127.0.0.1:8765`):
//! `GET /api/health`, `POST /api/load`, `POST /api/dictate`, `POST /api/shutdown`. Loopback only,
//! one request per connection (`Connection: close`), so no library is needed.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Where the transcriber listens (its `settings.toml` default; the window's CSP uses it too).
pub const TRANSCRIBER_ADDR: &str = "127.0.0.1:8765";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DictateError {
    /// Nothing is listening: the speech service is not running.
    Unreachable,
    /// The model is still loading; try again shortly.
    Loading,
    /// No model has been chosen.
    NoModel,
    /// Anything else, as a message for the user.
    Failed(String),
    /// The caller set the cancel flag.
    Cancelled,
}

/// What `GET /api/health` says about the model (all empty from servers older than this).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// The model chosen last (loaded or loading); `None` = none chosen yet.
    pub model: Option<String>,
    pub loading: bool,
    /// Loading `model` failed.
    pub failed: bool,
}

fn local_addr(overridden: Option<String>) -> String {
    overridden.map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).unwrap_or_else(|| TRANSCRIBER_ADDR.to_string())
}

enum Failure {
    Unreachable,
    Cancelled,
    TimedOut,
    Io(String),
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

pub struct Client {
    addr: String,
    dictate_timeout: Duration,
}

impl Client {
    pub fn new(addr: &str) -> Self {
        // A Bark piece of 20 s takes ~5 s; a whole 5-minute dictation a couple of minutes.
        Self { addr: addr.to_string(), dictate_timeout: Duration::from_secs(600) }
    }

    /// The transcriber on this machine. `AUDCAP_TRANSCRIBER_ADDR` points scratch
    /// runs at another one, like `AUDCAP_CONFIG` does for the config.
    pub fn local() -> Self {
        Self::new(&local_addr(std::env::var("AUDCAP_TRANSCRIBER_ADDR").ok()))
    }

    /// The API answers `GET /api/health`.
    pub fn health(&self) -> bool {
        matches!(self.request("GET", "/api/health", None, &[], Duration::from_secs(2), None), Ok(r) if r.status == 200)
    }

    /// `GET /api/health` with what it says about the model; `None` = not up.
    pub fn status(&self) -> Option<Health> {
        let reply = self.request("GET", "/api/health", None, &[], Duration::from_secs(2), None).ok()?;
        if reply.status != 200 {
            return None;
        }
        let json: serde_json::Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        Some(Health {
            model: json["model"].as_str().map(str::to_string),
            loading: json["loading"].as_bool().unwrap_or(false),
            failed: json["failed"].as_bool().unwrap_or(false),
        })
    }

    /// `POST /api/load`: switches the running server to `model` (it loads in the background).
    pub fn load(&self, model: &str) -> Result<(), String> {
        let body = serde_json::json!({ "model": model }).to_string();
        let reply = match self.request("POST", "/api/load", Some("application/json"), body.as_bytes(), Duration::from_secs(5), None) {
            Ok(r) => r,
            Err(Failure::Unreachable) => return Err("the speech service is not running".into()),
            Err(Failure::TimedOut) => return Err("the speech service did not answer".into()),
            Err(Failure::Io(m)) => return Err(m),
            Err(Failure::Cancelled) => return Err("cancelled".into()),
        };
        if reply.status == 200 {
            return Ok(());
        }
        let json: Option<serde_json::Value> = serde_json::from_slice(&reply.body).ok();
        Err(json.and_then(|j| j["detail"].as_str().map(str::to_string)).unwrap_or_else(|| format!("HTTP {}", reply.status)))
    }

    /// `POST /api/shutdown`: true when the server said it is stopping.
    pub fn shutdown(&self) -> bool {
        matches!(self.request("POST", "/api/shutdown", None, &[], Duration::from_secs(3), None), Ok(r) if r.status == 200)
    }

    /// Sends an Ogg Opus clip; returns the text. Waits up to ten minutes; `cancel` aborts early.
    pub fn dictate(&self, opus: &[u8], cancel: &AtomicBool) -> Result<String, DictateError> {
        let reply = match self.request("POST", "/api/dictate", Some("audio/ogg"), opus, self.dictate_timeout, Some(cancel)) {
            Ok(reply) => reply,
            Err(Failure::Unreachable) => return Err(DictateError::Unreachable),
            Err(Failure::Cancelled) => return Err(DictateError::Cancelled),
            Err(Failure::TimedOut) => return Err(DictateError::Failed("the speech service did not answer".into())),
            Err(Failure::Io(m)) => return Err(DictateError::Failed(m)),
        };
        let json: Option<serde_json::Value> = serde_json::from_slice(&reply.body).ok();
        match reply.status {
            200 => json
                .and_then(|j| j["text"].as_str().map(str::to_string))
                .ok_or_else(|| DictateError::Failed("unexpected reply from the speech service".into())),
            503 => Err(DictateError::Loading),
            409 => Err(DictateError::NoModel),
            status => Err(DictateError::Failed(
                json.and_then(|j| j["detail"].as_str().map(str::to_string)).unwrap_or_else(|| format!("HTTP {status}")),
            )),
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<Reply, Failure> {
        let addr = self.addr.to_socket_addrs().ok().and_then(|mut a| a.next()).ok_or(Failure::Unreachable)?;
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| Failure::Unreachable)?;
        let deadline = Instant::now() + timeout;
        let io = |e: std::io::Error| Failure::Io(e.to_string());
        stream.set_write_timeout(Some(Duration::from_secs(10))).map_err(io)?;
        // Short reads, so the cancel flag and the deadline are looked at often.
        stream.set_read_timeout(Some(Duration::from_millis(50))).map_err(io)?;
        let content_type = content_type.map(|t| format!("Content-Type: {t}\r\n")).unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{content_type}Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            body.len()
        );
        stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body)).map_err(io)?;

        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(Failure::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(Failure::TimedOut);
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
                Err(e) => return Err(Failure::Io(e.to_string())),
            }
        }
        parse_reply(&buf)
    }
}

/// Status and body of a `Connection: close` reply read to the end.
fn parse_reply(buf: &[u8]) -> Result<Reply, Failure> {
    let bad = || Failure::Io("unexpected reply from the speech service".into());
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(bad)?;
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        return Err(bad());
    }
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
    Ok(Reply { status, body: buf[end + 4..].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{mpsc, Arc};

    /// Reads one request (head + Content-Length body) from `s`.
    fn read_request(s: &mut TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    return buf;
                }
            }
            if n == 0 {
                return buf;
            }
        }
    }

    /// Answers one request with `status` and a JSON `body`; the request it saw comes back on the channel.
    fn stub(status: &str, body: &str) -> (String, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        let (status, body) = (status.to_string(), body.to_string());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = tx.send(read_request(&mut s)); // a test that doesn't care drops the receiver
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(reply.as_bytes());
        });
        (addr, rx)
    }

    /// Accepts a connection and never answers.
    fn silent() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let _held = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(5));
        });
        addr
    }

    fn closed_port() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn the_local_address_can_be_overridden_for_scratch_runs() {
        assert_eq!(local_addr(None), "127.0.0.1:8765");
        assert_eq!(local_addr(Some("127.0.0.1:8799".into())), "127.0.0.1:8799");
        assert_eq!(local_addr(Some("  ".into())), "127.0.0.1:8765", "blank means unset");
    }

    #[test]
    fn dictate_posts_the_clip_and_returns_the_text() {
        let (addr, seen) = stub("200 OK", r#"{"text":"hello world","model":"m","secs":0.2}"#);
        let text = Client::new(&addr).dictate(b"OggSclip", &no_cancel()).unwrap();
        assert_eq!(text, "hello world");
        let req = seen.recv().unwrap();
        let head = String::from_utf8_lossy(&req).to_string();
        assert!(head.starts_with("POST /api/dictate HTTP/1.1\r\n"), "{head}");
        assert!(head.to_ascii_lowercase().contains("content-length: 8\r\n"), "{head}");
        assert!(req.ends_with(b"OggSclip"));
    }

    #[test]
    fn loading_model_is_loading() {
        let (addr, _) = stub("503 Service Unavailable", r#"{"detail":"the bark model is still loading"}"#);
        assert_eq!(Client::new(&addr).dictate(b"x", &no_cancel()), Err(DictateError::Loading));
    }

    #[test]
    fn no_model_is_no_model() {
        let (addr, _) = stub("409 Conflict", r#"{"detail":"no model selected"}"#);
        assert_eq!(Client::new(&addr).dictate(b"x", &no_cancel()), Err(DictateError::NoModel));
    }

    #[test]
    fn server_errors_carry_the_detail() {
        let (addr, _) = stub("500 Internal Server Error", r#"{"detail":"boom"}"#);
        assert_eq!(Client::new(&addr).dictate(b"x", &no_cancel()), Err(DictateError::Failed("boom".into())));
        let (addr, _) = stub("502 Bad Gateway", "not json");
        assert_eq!(Client::new(&addr).dictate(b"x", &no_cancel()), Err(DictateError::Failed("HTTP 502".into())));
    }

    #[test]
    fn a_closed_port_is_unreachable() {
        assert_eq!(Client::new(&closed_port()).dictate(b"x", &no_cancel()), Err(DictateError::Unreachable));
    }

    #[test]
    fn health_is_true_only_for_a_200() {
        let (addr, seen) = stub("200 OK", r#"{"ok":true}"#);
        assert!(Client::new(&addr).health());
        assert!(String::from_utf8_lossy(&seen.recv().unwrap()).starts_with("GET /api/health HTTP/1.1\r\n"));
        let (addr, _) = stub("404 Not Found", "{}");
        assert!(!Client::new(&addr).health());
        assert!(!Client::new(&closed_port()).health());
    }

    #[test]
    fn status_reads_the_active_model_from_health() {
        let (addr, _) = stub("200 OK", r#"{"ok":true,"model":"bark","loading":true,"failed":false}"#);
        assert_eq!(
            Client::new(&addr).status(),
            Some(Health { model: Some("bark".into()), loading: true, failed: false })
        );
        let (addr, _) = stub("200 OK", r#"{"ok":true}"#);
        assert_eq!(Client::new(&addr).status(), Some(Health::default()), "an older server");
        let (addr, _) = stub("200 OK", r#"{"ok":true,"model":null,"loading":false,"failed":true}"#);
        assert_eq!(Client::new(&addr).status(), Some(Health { model: None, loading: false, failed: true }));
        let (addr, _) = stub("500 Internal Server Error", "{}");
        assert_eq!(Client::new(&addr).status(), None);
        assert_eq!(Client::new(&closed_port()).status(), None);
    }

    #[test]
    fn load_posts_the_model_and_reports_the_detail() {
        let (addr, seen) = stub("200 OK", r#"{"active":"bark"}"#);
        assert_eq!(Client::new(&addr).load("bark"), Ok(()));
        let req = String::from_utf8_lossy(&seen.recv().unwrap()).to_string();
        assert!(req.starts_with("POST /api/load HTTP/1.1\r\n"), "{req}");
        assert!(req.to_ascii_lowercase().contains("content-type: application/json\r\n"), "{req}");
        assert!(req.ends_with(r#"{"model":"bark"}"#), "{req}");
        let (addr, _) = stub("409 Conflict", r#"{"detail":"a transcription is running"}"#);
        assert_eq!(Client::new(&addr).load("bark"), Err("a transcription is running".into()));
        assert!(Client::new(&closed_port()).load("bark").is_err());
    }

    #[test]
    fn shutdown_posts_and_reports_success() {
        let (addr, seen) = stub("200 OK", r#"{"stopping":true}"#);
        assert!(Client::new(&addr).shutdown());
        assert!(String::from_utf8_lossy(&seen.recv().unwrap()).starts_with("POST /api/shutdown HTTP/1.1\r\n"));
        assert!(!Client::new(&closed_port()).shutdown());
    }

    #[test]
    fn the_cancel_flag_aborts_a_slow_request() {
        let addr = silent();
        let cancel = Arc::new(no_cancel());
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::Relaxed);
        });
        let t = Instant::now();
        assert_eq!(Client::new(&addr).dictate(b"x", &cancel), Err(DictateError::Cancelled));
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn a_server_that_never_answers_times_out_as_failed() {
        let addr = silent();
        let mut c = Client::new(&addr);
        c.dictate_timeout = Duration::from_millis(300);
        let t = Instant::now();
        assert!(matches!(c.dictate(b"x", &no_cancel()), Err(DictateError::Failed(m)) if m.contains("did not answer")));
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}
