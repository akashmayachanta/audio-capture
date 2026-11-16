//! Finished recordings under `output_dir`, newest first, for the window's API.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

use crate::session::Meta;

#[derive(Clone, Debug)]
pub struct Recording {
    pub path: PathBuf,
    /// The `.json` sidecar, when it exists and parses.
    pub meta: Option<Meta>,
    /// Start time (from the sidecar, else the file's modification time); sort key.
    pub when: DateTime<Local>,
    /// A `<same name>.md` transcript sits next to the recording.
    pub transcribed: bool,
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
            let transcribed = path.with_extension("md").is_file();
            Recording { path, meta, when, transcribed }
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Devices, Status, Trigger};
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
        assert!(found.iter().any(|r| r.path == bare));
    }

    #[test]
    fn missing_output_dir_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list(&dir.path().join("nope")).is_empty());
    }




    #[test]
    fn listing_notices_a_transcript_next_to_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let done = recording(dir.path(), "2026/10/done", None);
        let todo = recording(dir.path(), "2026/10/todo", None);
        std::fs::write(done.with_extension("md"), "# Transcript").unwrap();
        let found = list(dir.path());
        assert!(found.iter().find(|r| r.path == done).unwrap().transcribed);
        assert!(!found.iter().find(|r| r.path == todo).unwrap().transcribed);
        assert_eq!(found.len(), 2, "the .md is not a recording");
    }

}
