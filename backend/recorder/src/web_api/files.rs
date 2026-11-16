//! Recording files for the window's API: ids (paths relative to `output_dir`),
//! byte ranges for seeking, and the JSON listing.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::recordings::Recording;
use crate::session::{Status, Trigger};

/// Open-ended ranges (`bytes=N-`) are answered with at most this many bytes;
/// the browser asks for the rest as it plays.
pub const MAX_CHUNK: u64 = 2 * 1024 * 1024;

/// The `.opus` file `id` names under `root`, or `None` for anything that is not
/// a plain relative path to an existing recording inside `root`.
pub fn resolve_recording(root: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains('\\') || id.contains(':') {
        return None;
    }
    let rel = Path::new(id);
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    if rel.extension().is_none_or(|e| e != "opus") {
        return None;
    }
    // Component by component, so the path has native separators (Explorer needs them).
    let path = rel.components().fold(root.to_path_buf(), |p, c| p.join(c));
    if !path.is_file() {
        return None;
    }
    let (root, canon) = (root.canonicalize().ok()?, path.canonicalize().ok()?);
    canon.starts_with(&root).then_some(path)
}

/// `opus` with another extension (`md` for the transcript).
pub fn sibling(opus: &Path, ext: &str) -> PathBuf {
    opus.with_extension(ext)
}

/// One `Range: bytes=…` spec -> inclusive `(start, end)` within `len`.
pub fn parse_range(header: &str, len: u64) -> Result<(u64, u64), ()> {
    let spec = header.trim().strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || len == 0 {
        return Err(());
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let num = |s: &str| s.trim().parse::<u64>().map_err(|_| ());
    let (start, end) = match (a.trim().is_empty(), b.trim().is_empty()) {
        (true, false) => {
            let n = num(b)?;
            if n == 0 {
                return Err(());
            }
            (len.saturating_sub(n), len - 1)
        }
        (false, true) => (num(a)?, len - 1),
        (false, false) => (num(a)?, num(b)?.min(len - 1)),
        (true, true) => return Err(()),
    };
    if start >= len || start > end {
        return Err(());
    }
    Ok((start, end))
}

pub fn read_range(path: &Path, start: u64, end: u64) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((end - start + 1) as usize);
    f.take(end - start + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Speaker per decoded channel, from the sidecar's channel map.
pub fn speakers(channels: Option<&BTreeMap<String, String>>) -> Vec<&'static str> {
    let who = |source: Option<&String>, default: &'static str| match source.map(String::as_str) {
        Some("mic") => "me",
        Some("system") => "others",
        _ => default,
    };
    match channels {
        Some(c) if c.contains_key("mono") => vec![who(c.get("mono"), "me")],
        Some(c) => vec![who(c.get("left"), "me"), who(c.get("right"), "others")],
        None => vec!["me", "others"],
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordingJson {
    pub id: String,
    pub file: String,
    /// Local start, `YYYY-MM-DD HH:MM`.
    pub started: String,
    pub started_iso: String,
    pub duration_secs: Option<f64>,
    pub apps: Vec<String>,
    pub trigger: Option<String>,
    pub status: String,
    pub speakers: Vec<&'static str>,
    pub transcribed: bool,
    /// Seconds since the epoch of the `.md`, so the page reloads a redone transcript.
    pub transcript_mtime: Option<f64>,
}

fn md_mtime(opus: &Path) -> Option<f64> {
    let modified = std::fs::metadata(sibling(opus, "md")).and_then(|m| m.modified()).ok()?;
    Some(modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64())
}

pub fn recording_json(root: &Path, r: &Recording) -> RecordingJson {
    let rel = r.path.strip_prefix(root).unwrap_or(&r.path);
    let id = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    let m = r.meta.as_ref();
    RecordingJson {
        id,
        file: r.path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        started: r.when.format("%Y-%m-%d %H:%M").to_string(),
        started_iso: r.when.to_rfc3339(),
        duration_secs: m.map(|m| m.duration_secs),
        apps: m.map(|m| m.apps.clone()).unwrap_or_default(),
        trigger: m.map(|m| match m.trigger {
            Trigger::Auto => "auto".into(),
            Trigger::Manual => "manual".into(),
        }),
        status: match m.map(|m| m.status) {
            Some(Status::Complete) => "complete".into(),
            Some(Status::Recovered) => "recovered".into(),
            Some(Status::Recording) => "recording".into(),
            None => String::new(),
        },
        speakers: speakers(m.map(|m| &m.channels)),
        transcribed: r.transcribed,
        transcript_mtime: r.transcribed.then(|| md_mtime(&r.path)).flatten(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recordings;
    use crate::session::{Devices, Meta, Status, Trigger};
    use chrono::{Local, TimeZone};

    fn touch(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn channels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn meta() -> Meta {
        Meta {
            id: "id".into(),
            file: "a.opus".into(),
            start: Local.with_ymd_and_hms(2026, 10, 19, 10, 15, 0).unwrap(),
            end: None,
            duration_secs: 38.0,
            trigger: Trigger::Auto,
            apps: vec!["Chrome".into()],
            devices: Devices::default(),
            channels: channels(&[("left", "mic"), ("right", "system")]),
            pauses: Vec::new(),
            markers: Vec::new(),
            status: Status::Complete,
        }
    }

    #[test]
    fn resolves_a_recording_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = touch(dir.path(), "2026/10/a.opus", b"OggS");
        let got = resolve_recording(dir.path(), "2026/10/a.opus").unwrap();
        assert_eq!(got.canonicalize().unwrap(), p.canonicalize().unwrap());
    }

    #[test]
    fn resolved_path_uses_native_separators() {
        // Explorer opens its default folder for paths with '/' in them.
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "2026/10/a.opus", b"OggS");
        let got = resolve_recording(dir.path(), "2026/10/a.opus").unwrap();
        assert!(!got.to_string_lossy().contains('/'), "{got:?}");
    }

    #[test]
    fn recording_json_carries_the_transcript_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let p = touch(dir.path(), "a.opus", b"OggS");
        let r = &recordings::list(dir.path())[0];
        assert_eq!(recording_json(dir.path(), r).transcript_mtime, None);
        std::fs::write(p.with_extension("md"), "# Transcript").unwrap();
        let r = &recordings::list(dir.path())[0];
        assert!(recording_json(dir.path(), r).transcript_mtime.unwrap() > 0.0);
    }

    #[test]
    fn rejects_ids_that_escape_or_are_not_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("rec");
        touch(&root, "2026/10/a.opus", b"OggS");
        touch(&root, "2026/10/a.json", b"{}");
        touch(dir.path(), "x.opus", b"OggS");
        for id in [
            "../x.opus",
            "2026/../../x.opus",
            "C:/x.opus",
            "/x.opus",
            "2026\\10\\a.opus",
            "2026/10/a.json",
            "2026/10/missing.opus",
            "",
        ] {
            assert_eq!(resolve_recording(&root, id), None, "{id:?}");
        }
    }

    #[test]
    fn range_forms() {
        assert_eq!(parse_range("bytes=0-99", 1000), Ok((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Ok((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Ok((900, 999)));
        assert_eq!(parse_range("bytes=0-5000", 1000), Ok((0, 999)));
    }

    #[test]
    fn bad_ranges_are_errors() {
        for h in ["bytes=1000-", "bytes=5-2", "items=0-1", "bytes=0-1,4-5", "bytes=abc", "bytes=-0"] {
            assert_eq!(parse_range(h, 1000), Err(()), "{h}");
        }
    }

    #[test]
    fn read_range_returns_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = touch(dir.path(), "a.opus", b"0123456789");
        assert_eq!(read_range(&p, 2, 5).unwrap(), b"2345");
    }

    #[test]
    fn speakers_follow_the_channel_map() {
        assert_eq!(speakers(Some(&channels(&[("left", "mic"), ("right", "system")]))), vec!["me", "others"]);
        assert_eq!(speakers(Some(&channels(&[("mono", "mic")]))), vec!["me"]);
        assert_eq!(speakers(Some(&channels(&[("mono", "system")]))), vec!["others"]);
        assert_eq!(speakers(None), vec!["me", "others"]);
    }

    #[test]
    fn recording_json_uses_relative_posix_id_and_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let p = touch(dir.path(), "2026/10/a.opus", b"OggS");
        std::fs::write(p.with_extension("json"), serde_json::to_vec(&meta()).unwrap()).unwrap();
        std::fs::write(p.with_extension("md"), "# Transcript").unwrap();
        let r = &recordings::list(dir.path())[0];
        let j = recording_json(dir.path(), r);
        assert_eq!(j.id, "2026/10/a.opus");
        assert_eq!(j.file, "a.opus");
        assert_eq!(j.started, "2026-10-19 10:15");
        assert_eq!(j.duration_secs, Some(38.0));
        assert_eq!(j.apps, vec!["Chrome".to_string()]);
        assert_eq!(j.trigger.as_deref(), Some("auto"));
        assert_eq!(j.status, "complete");
        assert_eq!(j.speakers, vec!["me", "others"]);
        assert!(j.transcribed);
    }

    #[test]
    fn recording_json_without_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "2026/10/2026-10-19_1015_Zoom.opus", b"OggS");
        let r = &recordings::list(dir.path())[0];
        let j = recording_json(dir.path(), r);
        assert_eq!(j.duration_secs, None);
        assert_eq!(j.status, "");
        assert_eq!(j.trigger, None);
        assert_eq!(j.speakers, vec!["me", "others"]);
        assert!(!j.transcribed);
        assert!(!j.started_iso.is_empty());
    }
}
