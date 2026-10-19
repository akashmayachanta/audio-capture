//! Finished recordings under `output_dir`, newest first, for the Recordings tab.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::session::{Meta, Status, Trigger};

#[derive(Clone, Debug)]
pub struct Recording {
    pub path: PathBuf,
    /// The `.json` sidecar, when it exists and parses.
    pub meta: Option<Meta>,
    /// Start time (from the sidecar, else the file's modification time); sort key.
    pub when: DateTime<Local>,
}

impl Recording {
    /// One line for the list, e.g. `2026-10-19 10:15 · Chrome · 0:38`.
    pub fn label(&self) -> String {
        let Some(m) = &self.meta else {
            return self.path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        };
        let who = match (m.apps.is_empty(), m.trigger) {
            (false, _) => m.apps.join(", "),
            (true, Trigger::Manual) => "Manual".into(),
            (true, Trigger::Auto) => "Auto".into(),
        };
        let mut label = format!("{} · {who} · {}", m.start.format("%Y-%m-%d %H:%M"), format_time(m.duration_secs));
        if m.status == Status::Recovered {
            label.push_str(" · recovered");
        }
        label
    }
}

/// Every `*.opus` under `output_dir` (not `.part` files still being written).
pub fn list(output_dir: &Path) -> Vec<Recording> {
    let mut paths = Vec::new();
    find_opus(output_dir, &mut paths);
    let mut out: Vec<Recording> = paths
        .into_iter()
        .map(|path| {
            let meta = std::fs::read(path.with_extension("json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Meta>(&bytes).ok());
            let when = meta.as_ref().map(|m| m.start).unwrap_or_else(|| {
                std::fs::metadata(&path).and_then(|m| m.modified()).map_or_else(|_| Local::now(), Into::into)
            });
            Recording { path, meta, when }
        })
        .collect();
    out.sort_by(|a, b| b.when.cmp(&a.when).then_with(|| b.path.cmp(&a.path)));
    out
}

fn find_opus(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_opus(&p, out);
        } else if p.extension().is_some_and(|x| x == "opus") {
            out.push(p);
        }
    }
}

/// `m:ss`, or `h:mm:ss` from an hour up.
pub fn format_time(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Devices;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    fn meta(start: DateTime<Local>, apps: &[&str], trigger: Trigger, status: Status, secs: f64) -> Meta {
        Meta {
            id: "id".into(),
            file: String::new(),
            start,
            end: None,
            duration_secs: secs,
            trigger,
            apps: apps.iter().map(|a| a.to_string()).collect(),
            devices: Devices::default(),
            channels: BTreeMap::new(),
            pauses: Vec::new(),
            markers: Vec::new(),
            status,
        }
    }

    fn at(month: u32, day: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, month, day, h, m, 0).unwrap()
    }

    /// Writes `<dir>/<rel>.opus` (a stand-in, never decoded) and, if given, its sidecar.
    fn recording(dir: &Path, rel: &str, meta: Option<&Meta>) -> PathBuf {
        let path = dir.join(format!("{rel}.opus"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"OggS").unwrap();
        if let Some(m) = meta {
            std::fs::write(path.with_extension("json"), serde_json::to_vec(m).unwrap()).unwrap();
        }
        path
    }

    fn rec(meta: Option<Meta>) -> Recording {
        let when = meta.as_ref().map_or(at(10, 19, 9, 0), |m| m.start);
        Recording { path: PathBuf::from("x/2026-10-19_1015_Chrome.opus"), meta, when }
    }

    #[test]
    fn lists_finished_recordings_newest_first_and_skips_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let a = recording(d, "2026/10/a", Some(&meta(at(10, 19, 10, 0), &["Chrome"], Trigger::Auto, Status::Complete, 60.0)));
        let b = recording(d, "2026/10/b", Some(&meta(at(10, 19, 11, 0), &[], Trigger::Manual, Status::Complete, 5.0)));
        let c = recording(d, "2026/09/c", Some(&meta(at(9, 1, 8, 0), &["Teams"], Trigger::Auto, Status::Recovered, 9.0)));
        std::fs::write(d.join("2026/10/live.opus.part"), b"OggS").unwrap();
        std::fs::write(d.join("2026/10/live.json.part"), b"{}").unwrap();
        std::fs::write(d.join("2026/10/notes.txt"), b"hi").unwrap();
        let paths: Vec<PathBuf> = list(d).into_iter().map(|r| r.path).collect();
        assert_eq!(paths, vec![b, a, c]);
    }

    #[test]
    fn recording_without_a_readable_sidecar_is_still_listed() {
        let dir = tempfile::tempdir().unwrap();
        let bare = recording(dir.path(), "2026/10/2026-10-19_1015_Manual", None);
        let torn = recording(dir.path(), "2026/10/2026-10-19_1016_Zoom", None);
        std::fs::write(torn.with_extension("json"), b"{\"id\": \"tru").unwrap();
        let found = list(dir.path());
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|r| r.meta.is_none()));
        let labels: Vec<String> = found.iter().map(Recording::label).collect();
        assert!(labels.contains(&"2026-10-19_1015_Manual".to_string()), "{labels:?}");
        assert!(found.iter().any(|r| r.path == bare));
    }

    #[test]
    fn missing_output_dir_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn label_shows_start_apps_and_duration() {
        let r = rec(Some(meta(at(10, 19, 10, 15), &["Chrome", "Teams"], Trigger::Auto, Status::Complete, 38.13)));
        assert_eq!(r.label(), "2026-10-19 10:15 · Chrome, Teams · 0:38");
    }

    #[test]
    fn label_names_manual_recordings_and_flags_recovered_ones() {
        let r = rec(Some(meta(at(10, 19, 10, 4), &[], Trigger::Manual, Status::Recovered, 3725.0)));
        assert_eq!(r.label(), "2026-10-19 10:04 · Manual · 1:02:05 · recovered");
    }

    #[test]
    fn times_are_minutes_and_seconds_or_hours() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(65.9), "1:05");
        assert_eq!(format_time(3723.0), "1:02:03");
        assert_eq!(format_time(-1.0), "0:00");
    }
}
