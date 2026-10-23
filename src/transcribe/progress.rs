//! The progress file of a running `--transcribe` process
//! (`<cache>\tmp\<job-id>\progress.json`). The Transcripts window polls it, and
//! finds jobs that an earlier window started, so a job can outlive its window.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use super::paths::CacheDirs;

pub const FILE_NAME: &str = "progress.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobState {
    pub recording: PathBuf,
    /// The `--transcribe` process.
    pub pid: u32,
    pub phase: Phase,
    /// 0..=1 while running.
    pub progress: f32,
    /// Why it failed.
    #[serde(default)]
    pub message: String,
}

pub fn state_path(cache: &CacheDirs, job_id: &str) -> PathBuf {
    cache.tmp_job_dir(job_id).join(FILE_NAME)
}

/// Written to a temp file and renamed, so a poller never reads half of it.
pub fn write(path: &Path, state: &JobState) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, serde_json::to_vec(state)?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read(path: &Path) -> Option<JobState> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// What the window should show for a state file, given whether its process is alive.
/// A job whose process is gone without saying so has crashed.
pub fn effective(state: JobState, alive: bool) -> JobState {
    if state.phase == Phase::Running && !alive {
        JobState { phase: Phase::Failed, message: "the transcription process stopped unexpectedly".into(), ..state }
    } else {
        state
    }
}

/// Every job record under `tmp`, with crashed jobs shown as failed.
pub fn scan(cache: &CacheDirs) -> Vec<(String, JobState)> {
    let Ok(entries) = std::fs::read_dir(cache.tmp_dir()) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let state = read(&e.path().join(FILE_NAME))?;
            let alive = pid_alive(state.pid);
            Some((e.file_name().to_string_lossy().into_owned(), effective(state, alive)))
        })
        .collect()
}

pub fn pid_alive(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code).is_ok();
        let _ = CloseHandle(handle);
        ok && code == STILL_ACTIVE.0 as u32
    }
}

/// Stops a process and everything it started (the Python helper).
pub fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: Phase, pid: u32) -> JobState {
        JobState { recording: "D:\\rec\\a.opus".into(), pid, phase, progress: 0.4, message: String::new() }
    }

    #[test]
    fn state_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("job").join(FILE_NAME);
        let s = state(Phase::Running, 42);
        write(&path, &s).unwrap();
        assert_eq!(read(&path), Some(s));
        assert!(!dir.path().join("job").join("progress.json.tmp").exists());
    }

    #[test]
    fn a_missing_or_torn_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(&dir.path().join("nope.json")), None);
        std::fs::write(dir.path().join("torn.json"), b"{\"recording\": \"x").unwrap();
        assert_eq!(read(&dir.path().join("torn.json")), None);
    }

    #[test]
    fn a_running_job_whose_process_is_gone_has_failed() {
        let gone = effective(state(Phase::Running, 1), false);
        assert_eq!(gone.phase, Phase::Failed);
        assert!(gone.message.contains("stopped"));
        assert_eq!(effective(state(Phase::Running, 1), true).phase, Phase::Running);
        let mut failed = state(Phase::Failed, 1);
        failed.message = "bad wav".into();
        assert_eq!(effective(failed.clone(), false), failed, "a recorded failure keeps its message");
    }

    #[test]
    fn this_process_is_alive_and_a_bogus_pid_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(u32::MAX - 1));
    }

    #[test]
    fn scan_finds_jobs_and_marks_dead_ones_failed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDirs::new(dir.path());
        write(&state_path(&cache, "live"), &state(Phase::Running, std::process::id())).unwrap();
        write(&state_path(&cache, "dead"), &state(Phase::Running, u32::MAX - 1)).unwrap();
        std::fs::create_dir_all(cache.tmp_job_dir("empty")).unwrap();
        let mut found = scan(&cache);
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<_> = found.iter().map(|(n, s)| (n.as_str(), s.phase)).collect();
        assert_eq!(names, [("dead", Phase::Failed), ("live", Phase::Running)]);
    }

    #[test]
    fn scanning_without_a_tmp_folder_finds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(&CacheDirs::new(dir.path().join("none"))).is_empty());
    }
}
