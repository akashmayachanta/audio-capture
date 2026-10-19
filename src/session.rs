//! One recording = one `.opus` file plus a `.json` sidecar.
//!
//! While recording, the audio goes to `<name>.opus.part` and the sidecar to
//! `<name>.json.part`; both are renamed on close, so a downstream watcher only
//! ever sees finished files. Leftover `.part` files from a crash are finalized on
//! the next start with `status: "recovered"`.

use std::fs::File;
use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::audio::SAMPLE_RATE;
use crate::encode::opus_ogg::OggOpusWriter;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Auto,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Recording,
    Complete,
    Recovered,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Devices {
    pub mic: Option<String>,
    pub system: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pause {
    /// Audio position (seconds) where the pause sits.
    pub at_secs: f64,
    pub start: DateTime<Local>,
    pub end: Option<DateTime<Local>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub at_secs: f64,
    pub time: DateTime<Local>,
}

/// The sidecar JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub file: String,
    pub start: DateTime<Local>,
    pub end: Option<DateTime<Local>>,
    pub duration_secs: f64,
    pub trigger: Trigger,
    pub apps: Vec<String>,
    pub devices: Devices,
    /// e.g. {"left": "mic", "right": "system"} or {"mono": "mic"}.
    pub channels: BTreeMap<String, String>,
    pub pauses: Vec<Pause>,
    pub markers: Vec<Marker>,
    pub status: Status,
}

/// What a recording captures; decides channel count and the channel map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    Mic,
    System,
    Both,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub trigger: Trigger,
    pub app: Option<String>,
    pub mode: CaptureMode,
    pub devices: Devices,
    pub bitrate_kbps: u32,
}

pub enum Closed {
    Saved(PathBuf),
    Discarded,
}

type Writer = OggOpusWriter<BufWriter<File>>;

pub struct Session {
    meta: Meta,
    part: PathBuf,
    json_part: PathBuf,
    final_path: PathBuf,
    writer: Writer,
    paused: bool,
}

impl Session {
    pub fn open(output_dir: &Path, start: DateTime<Local>, info: SessionInfo) -> Result<Self> {
        let app = info.app.clone().unwrap_or_else(|| "Manual".into());
        let final_path = session_path(output_dir, start, &app);
        std::fs::create_dir_all(final_path.parent().unwrap())?;
        let part = append_ext(&final_path, ".part");
        let json_part = final_path.with_extension("json.part");

        let mut channels = BTreeMap::new();
        let n = match info.mode {
            CaptureMode::Both => {
                channels.insert("left".into(), "mic".into());
                channels.insert("right".into(), "system".into());
                2
            }
            CaptureMode::Mic => {
                channels.insert("mono".into(), "mic".into());
                1
            }
            CaptureMode::System => {
                channels.insert("mono".into(), "system".into());
                1
            }
        };
        let meta = Meta {
            id: uuid::Uuid::new_v4().to_string(),
            file: file_name(&final_path),
            start,
            end: None,
            duration_secs: 0.0,
            trigger: info.trigger,
            apps: info.app.into_iter().collect(),
            devices: info.devices,
            channels,
            pauses: Vec::new(),
            markers: Vec::new(),
            status: Status::Recording,
        };

        let file = File::create(&part).with_context(|| format!("create {}", part.display()))?;
        let channel_tag = meta.channels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",");
        let date = start.to_rfc3339();
        let tags = [("MEETREC_CHANNELS", channel_tag.as_str()), ("DATE", date.as_str())];
        let writer = OggOpusWriter::new(BufWriter::new(file), n, info.bitrate_kbps as i32 * 1000, &tags)?;
        let session = Self { meta, part, json_part, final_path, writer, paused: false };
        session.save_sidecar()?;
        Ok(session)
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn part_path(&self) -> &Path {
        &self.part
    }

    /// Encodes one frame, unless paused.
    pub fn write_frame(&mut self, pcm: &[f32]) -> Result<()> {
        if !self.paused {
            self.writer.write_frame(pcm)?;
        }
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn pause(&mut self, now: DateTime<Local>) {
        if !self.paused {
            self.paused = true;
            let at_secs = self.duration_secs();
            self.meta.pauses.push(Pause { at_secs, start: now, end: None });
            self.save_sidecar_logged();
        }
    }

    pub fn resume(&mut self, now: DateTime<Local>) {
        if self.paused {
            self.paused = false;
            if let Some(p) = self.meta.pauses.last_mut() {
                p.end = Some(now);
            }
            self.save_sidecar_logged();
        }
    }

    pub fn marker(&mut self, now: DateTime<Local>) {
        let at_secs = self.duration_secs();
        self.meta.markers.push(Marker { at_secs, time: now });
        self.save_sidecar_logged();
    }

    pub fn add_app(&mut self, app: &str) {
        if !self.meta.apps.iter().any(|a| a == app) {
            self.meta.apps.push(app.to_string());
            self.save_sidecar_logged();
        }
    }

    pub fn set_devices(&mut self, devices: Devices) {
        if self.meta.devices != devices {
            self.meta.devices = devices;
            self.save_sidecar_logged();
        }
    }

    /// Recorded audio so far, excluding pauses.
    pub fn duration_secs(&self) -> f64 {
        self.writer.samples_written() as f64 / SAMPLE_RATE as f64
    }

    /// Finalizes the file, or deletes it when shorter than `min_secs`.
    pub fn close(mut self, now: DateTime<Local>, min_secs: f64) -> Result<Closed> {
        self.resume(now);
        self.meta.duration_secs = self.duration_secs();
        self.meta.end = Some(now);
        self.meta.status = Status::Complete;
        drop(self.writer.finish()?);
        if self.meta.duration_secs < min_secs {
            std::fs::remove_file(&self.part)?;
            let _ = std::fs::remove_file(&self.json_part);
            return Ok(Closed::Discarded);
        }
        rename_with_retry(&self.part, &self.final_path)?;
        write_json(&self.final_path.with_extension("json"), &self.meta)?;
        let _ = std::fs::remove_file(&self.json_part);
        Ok(Closed::Saved(self.final_path))
    }

    fn save_sidecar(&self) -> Result<()> {
        write_json(&self.json_part, &self.meta)
    }

    fn save_sidecar_logged(&self) {
        if let Err(e) = self.save_sidecar() {
            tracing::warn!("sidecar update failed: {e:#}");
        }
    }
}

/// Written to a temp file and renamed into place, so a crash mid-write never
/// leaves a torn sidecar.
fn write_json(path: &Path, meta: &Meta) -> Result<()> {
    let tmp = append_ext(path, ".tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(meta)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Antivirus, the search indexer or OneDrive may hold a just-closed file for a
/// moment; give them about a second before giving up.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    const ATTEMPTS: u32 = 5;
    for attempt in 1.. {
        match std::fs::rename(from, to) {
            Err(e) if attempt < ATTEMPTS => {
                tracing::debug!("rename {} failed ({e}), retrying", from.display());
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            result => return result,
        }
    }
    unreachable!()
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    PathBuf::from(s)
}

fn sanitize(app: &str) -> String {
    let mut out = String::new();
    for c in app.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "Recording".into() } else { out.to_string() }
}

/// `<dir>/YYYY/MM/YYYY-MM-DD_HHMM_<App>.opus`, made unique with `_2`, `_3`, …
pub fn session_path(output_dir: &Path, start: DateTime<Local>, app: &str) -> PathBuf {
    let dir = output_dir.join(start.format("%Y").to_string()).join(start.format("%m").to_string());
    let stem = format!("{}_{}", start.format("%Y-%m-%d_%H%M"), sanitize(app));
    let taken = |p: &Path| p.exists() || append_ext(p, ".part").exists();
    let mut path = dir.join(format!("{stem}.opus"));
    let mut n = 2;
    while taken(&path) {
        path = dir.join(format!("{stem}_{n}.opus"));
        n += 1;
    }
    path
}

/// Finalizes every `*.opus.part` under `output_dir`; returns the recovered files.
pub fn recover_parts(output_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut parts = Vec::new();
    find_parts(output_dir, &mut parts);
    let mut recovered = Vec::new();
    for part in parts {
        match recover_one(&part) {
            Ok(p) => recovered.push(p),
            Err(e) => tracing::warn!("could not recover {}: {e:#}", part.display()),
        }
    }
    Ok(recovered)
}

fn find_parts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_parts(&p, out);
        } else if p.to_string_lossy().ends_with(".opus.part") {
            out.push(p);
        }
    }
}

fn recover_one(part: &Path) -> Result<PathBuf> {
    let json_part = part.with_extension("").with_extension("json.part");
    let mut final_path = part.with_extension(""); // strip ".part"
    if final_path.exists() {
        final_path = append_ext(&final_path.with_extension(""), "_recovered.opus");
    }
    let file_meta = std::fs::metadata(part)?;
    let modified: DateTime<Local> = file_meta.modified()?.into();
    let duration_secs = ogg_duration_secs(part).unwrap_or(0.0);

    let parsed = std::fs::read(&json_part).ok().and_then(|bytes| match serde_json::from_slice::<Meta>(&bytes) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!("{} is unreadable ({e}); recovering the audio without it", json_part.display());
            None
        }
    });
    let mut meta = match parsed {
        Some(m) => m,
        None => Meta {
            id: uuid::Uuid::new_v4().to_string(),
            file: String::new(),
            start: file_meta.created().map(Into::into).unwrap_or(modified),
            end: None,
            duration_secs: 0.0,
            trigger: Trigger::Manual,
            apps: Vec::new(),
            devices: Devices::default(),
            channels: BTreeMap::new(),
            pauses: Vec::new(),
            markers: Vec::new(),
            status: Status::Recording,
        },
    };
    meta.file = file_name(&final_path);
    meta.duration_secs = duration_secs;
    meta.end = Some(modified);
    meta.status = Status::Recovered;
    for p in meta.pauses.iter_mut().filter(|p| p.end.is_none()) {
        p.end = Some(modified);
    }

    rename_with_retry(part, &final_path)?;
    write_json(&final_path.with_extension("json"), &meta)?;
    let _ = std::fs::remove_file(&json_part);
    tracing::info!("recovered {} ({duration_secs:.1} s)", final_path.display());
    Ok(final_path)
}

/// Audio length from the last readable granule position (minus pre-skip).
/// A crashed file ends without an EOS page; reading stops at the first damaged page.
fn ogg_duration_secs(path: &Path) -> Result<f64> {
    let mut reader = ogg::PacketReader::new(BufReader::new(File::open(path)?));
    let mut pre_skip = 0u64;
    let mut last = 0u64;
    let mut first = true;
    while let Ok(Some(p)) = reader.read_packet() {
        if first {
            if p.data.len() >= 12 && &p.data[..8] == b"OpusHead" {
                pre_skip = u16::from_le_bytes([p.data[10], p.data[11]]) as u64;
            }
            first = false;
        } else if p.last_in_page() {
            last = last.max(p.absgp_page());
        }
    }
    Ok(last.saturating_sub(pre_skip) as f64 / SAMPLE_RATE as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use chrono::TimeZone;

    fn at(h: u32, m: u32, s: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 19, h, m, s).unwrap()
    }

    fn info(trigger: Trigger, app: Option<&str>, mode: CaptureMode) -> SessionInfo {
        SessionInfo {
            trigger,
            app: app.map(String::from),
            mode,
            devices: Devices { mic: Some("Headset Mic".into()), system: Some("Speakers".into()) },
            bitrate_kbps: 24,
        }
    }

    fn write_secs(s: &mut Session, secs: usize, channels: usize) {
        let frame = vec![0.0f32; FRAME * channels];
        for _ in 0..secs * 50 {
            s.write_frame(&frame).unwrap();
        }
    }

    fn read_meta(path: &Path) -> Meta {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn path_is_year_month_date_time_app() {
        let p = session_path(Path::new("C:/rec"), at(14, 30, 5), "Teams");
        assert_eq!(p, Path::new("C:/rec/2026/10/2026-10-19_1430_Teams.opus"));
    }

    #[test]
    fn path_sanitizes_app_name() {
        let p = session_path(Path::new("C:/rec"), at(9, 5, 0), "Microsoft Teams: (new)");
        assert_eq!(p.file_name().unwrap(), "2026-10-19_0905_Microsoft-Teams-new.opus");
    }

    #[test]
    fn path_is_unique_when_name_taken() {
        let dir = tempfile::tempdir().unwrap();
        let first = session_path(dir.path(), at(14, 30, 0), "Zoom");
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = session_path(dir.path(), at(14, 30, 0), "Zoom");
        assert_eq!(second.file_name().unwrap(), "2026-10-19_1430_Zoom_2.opus");
        std::fs::write(second.with_extension("opus.part"), b"x").unwrap();
        let third = session_path(dir.path(), at(14, 30, 0), "Zoom");
        assert_eq!(third.file_name().unwrap(), "2026-10-19_1430_Zoom_3.opus");
    }

    #[test]
    fn recording_writes_part_files_then_renames_on_close() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(14, 30, 0), info(Trigger::Auto, Some("Teams"), CaptureMode::Both)).unwrap();
        let part = s.part_path().to_path_buf();
        assert!(part.to_string_lossy().ends_with(".opus.part"));
        assert!(part.exists());
        assert!(part.with_extension("").with_extension("json.part").exists(), "sidecar part");
        write_secs(&mut s, 2, 2);
        let Closed::Saved(path) = s.close(at(14, 30, 2), 1.0).unwrap() else { panic!("discarded") };
        assert!(!part.exists());
        assert_eq!(path.extension().unwrap(), "opus");
        assert!(path.exists());
        let meta = read_meta(&path.with_extension("json"));
        assert_eq!(meta.status, Status::Complete);
        assert_eq!(meta.trigger, Trigger::Auto);
        assert_eq!(meta.apps, vec!["Teams".to_string()]);
        assert!((meta.duration_secs - 2.0).abs() < 0.001);
        assert_eq!(meta.end, Some(at(14, 30, 2)));
        assert_eq!(meta.channels.get("left").unwrap(), "mic");
        assert_eq!(meta.channels.get("right").unwrap(), "system");
        assert_eq!(meta.devices.mic.as_deref(), Some("Headset Mic"));
        assert_eq!(meta.file, "2026-10-19_1430_Teams.opus");
        assert!(!dir.path().join("2026/10/2026-10-19_1430_Teams.json.part").exists());
    }

    #[test]
    fn manual_without_app_is_named_manual_and_mono_map_for_mic_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(8, 0, 0), info(Trigger::Manual, None, CaptureMode::Mic)).unwrap();
        write_secs(&mut s, 1, 1);
        let Closed::Saved(path) = s.close(at(8, 0, 1), 0.0).unwrap() else { panic!() };
        assert!(path.ends_with("2026-10-19_0800_Manual.opus"));
        let meta = read_meta(&path.with_extension("json"));
        assert_eq!(meta.channels.len(), 1);
        assert_eq!(meta.channels.get("mono").unwrap(), "mic");
    }

    #[test]
    fn short_session_is_discarded_and_files_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(10, 0, 0), info(Trigger::Auto, Some("Zoom"), CaptureMode::Both)).unwrap();
        let part = s.part_path().to_path_buf();
        write_secs(&mut s, 1, 2);
        assert!(matches!(s.close(at(10, 0, 1), 30.0).unwrap(), Closed::Discarded));
        assert!(!part.exists());
        let leftovers: Vec<_> = walk(dir.path());
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    fn pause_skips_audio_and_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(11, 0, 0), info(Trigger::Manual, None, CaptureMode::Both)).unwrap();
        write_secs(&mut s, 1, 2);
        s.pause(at(11, 0, 1));
        assert!(s.is_paused());
        write_secs(&mut s, 5, 2); // dropped
        s.resume(at(11, 0, 6));
        write_secs(&mut s, 1, 2);
        s.marker(at(11, 0, 7));
        assert!((s.duration_secs() - 2.0).abs() < 0.001);
        let Closed::Saved(path) = s.close(at(11, 0, 7), 0.0).unwrap() else { panic!() };
        let meta = read_meta(&path.with_extension("json"));
        assert_eq!(meta.pauses.len(), 1);
        assert!((meta.pauses[0].at_secs - 1.0).abs() < 0.001);
        assert_eq!(meta.pauses[0].start, at(11, 0, 1));
        assert_eq!(meta.pauses[0].end, Some(at(11, 0, 6)));
        assert_eq!(meta.markers.len(), 1);
        assert!((meta.markers[0].at_secs - 2.0).abs() < 0.001);
    }

    #[test]
    fn close_while_paused_ends_the_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(11, 0, 0), info(Trigger::Manual, None, CaptureMode::Mic)).unwrap();
        write_secs(&mut s, 1, 1);
        s.pause(at(11, 0, 1));
        let Closed::Saved(path) = s.close(at(11, 0, 9), 0.0).unwrap() else { panic!() };
        let meta = read_meta(&path.with_extension("json"));
        assert_eq!(meta.pauses[0].end, Some(at(11, 0, 9)));
    }

    #[test]
    fn apps_are_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(12, 0, 0), info(Trigger::Auto, Some("Teams"), CaptureMode::Both)).unwrap();
        s.add_app("Teams");
        s.add_app("Chrome");
        assert_eq!(s.meta().apps, vec!["Teams".to_string(), "Chrome".to_string()]);
        drop(s.close(at(12, 0, 0), 0.0).unwrap());
    }

    #[test]
    fn crashed_session_is_recovered_with_audio_duration() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(13, 0, 0), info(Trigger::Auto, Some("Teams"), CaptureMode::Both)).unwrap();
        let part = s.part_path().to_path_buf();
        write_secs(&mut s, 3, 2);
        s.marker(at(13, 0, 2));
        drop(s); // crash: no close, no EOS page

        let recovered = recover_parts(dir.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        let path = &recovered[0];
        assert_eq!(path.extension().unwrap(), "opus");
        assert!(path.exists());
        assert!(!part.exists());
        let meta = read_meta(&path.with_extension("json"));
        assert_eq!(meta.status, Status::Recovered);
        assert_eq!(meta.apps, vec!["Teams".to_string()]);
        // Pages are flushed every second, so at least 2 of the 3 seconds survive.
        assert!(meta.duration_secs >= 2.0 && meta.duration_secs <= 3.0, "{}", meta.duration_secs);
        assert!(meta.end.is_some());
        assert!(walk(dir.path()).iter().all(|p| !p.to_string_lossy().ends_with(".part")));
    }

    #[test]
    fn close_retries_a_briefly_blocked_rename() {
        // Antivirus, the indexer or OneDrive can hold a just-written file for a moment.
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(16, 0, 0), info(Trigger::Manual, None, CaptureMode::Mic)).unwrap();
        write_secs(&mut s, 1, 1);
        let target = s.part_path().with_extension("");
        std::fs::create_dir(&target).unwrap();
        let unblock = std::thread::spawn({
            let target = target.clone();
            move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                std::fs::remove_dir(target).unwrap();
            }
        });
        let closed = s.close(at(16, 0, 1), 0.0);
        unblock.join().unwrap();
        let Closed::Saved(path) = closed.unwrap() else { panic!() };
        assert!(path.is_file());
    }

    #[test]
    fn corrupt_sidecar_does_not_block_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(17, 0, 0), info(Trigger::Auto, Some("Zoom"), CaptureMode::Mic)).unwrap();
        write_secs(&mut s, 2, 1);
        let sidecar = s.part_path().with_extension("").with_extension("json.part");
        drop(s);
        std::fs::write(&sidecar, "{\"id\": \"trunc").unwrap(); // torn write
        let recovered = recover_parts(dir.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        let meta = read_meta(&recovered[0].with_extension("json"));
        assert_eq!(meta.status, Status::Recovered);
        assert!(meta.duration_secs >= 1.0);
    }

    #[test]
    fn orphan_audio_part_without_sidecar_is_still_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::open(dir.path(), at(15, 0, 0), info(Trigger::Manual, None, CaptureMode::Mic)).unwrap();
        write_secs(&mut s, 2, 1);
        let sidecar = s.part_path().with_extension("").with_extension("json.part");
        drop(s);
        std::fs::remove_file(sidecar).unwrap();
        let recovered = recover_parts(dir.path()).unwrap();
        let meta = read_meta(&recovered[0].with_extension("json"));
        assert_eq!(meta.status, Status::Recovered);
        assert!(meta.duration_secs >= 1.0);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
