//! Starts the optional Python transcriber (`backend/transcriber`, `uv run python -m app`)
//! for the window's Start button. The server outlives the window process; the
//! window stops it through the transcriber's own `POST /api/shutdown`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;

/// The transcriber folder: `configured` when set, else the first ancestor of the
/// exe with `transcriber/` or `backend/transcriber/` holding a `pyproject.toml`
/// (a dev build in `backend/recorder/target/*/`, or a copy next to the exe).
pub fn find_dir(configured: &Path, exe: &Path) -> Option<PathBuf> {
    let is_project = |d: &Path| d.join("pyproject.toml").is_file();
    if !configured.as_os_str().is_empty() {
        return is_project(configured).then(|| configured.to_path_buf());
    }
    exe.ancestors()
        .skip(1)
        .flat_map(|a| [a.join("transcriber"), a.join("backend").join("transcriber")])
        .find(|d| is_project(d))
}

/// What the window knows about a server it started (it can't know about one
/// started elsewhere; the page checks the API itself for that).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LaunchStatus {
    /// The process this window started is still running.
    pub running: bool,
    /// Exit code when it has stopped.
    pub exit_code: Option<i32>,
    /// The end of its log when it stopped with an error.
    pub log_tail: String,
}

pub struct Launcher {
    log_path: PathBuf,
    child: Mutex<Option<Child>>,
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

impl Launcher {
    pub fn new(log_path: PathBuf) -> Self {
        Self { log_path, child: Mutex::new(None) }
    }

    /// Runs `uv run python -m app` in `dir`, hidden, output to the log file.
    /// A second Start while the first process still runs does nothing.
    pub fn start(&self, dir: &Path) -> Result<(), String> {
        let mut child = self.child.lock().unwrap();
        if let Some(c) = child.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok(());
            }
        }
        if let Some(parent) = self.log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log = File::create(&self.log_path).map_err(|e| format!("could not write {}: {e}", self.log_path.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        use std::os::windows::process::CommandExt;
        let spawned = Command::new("uv")
            .args(["run", "python", "-m", "app"])
            .current_dir(dir)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
            .spawn();
        match spawned {
            Ok(c) => {
                tracing::info!("started the transcriber in {} (pid {})", dir.display(), c.id());
                *child = Some(c);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err("uv was not found on PATH. Install it from https://docs.astral.sh/uv/ and reopen AudCap.".into())
            }
            Err(e) => Err(format!("could not start the transcriber: {e}")),
        }
    }

    /// Ends the process this launcher started together with its children (uv → python).
    /// False when nothing of ours is running or `taskkill` failed.
    pub fn kill_tree(&self) -> bool {
        let mut child = self.child.lock().unwrap();
        let Some(c) = child.as_mut() else { return false };
        if !matches!(c.try_wait(), Ok(None)) {
            return false;
        }
        use std::os::windows::process::CommandExt;
        Command::new("taskkill")
            .args(["/PID", &c.id().to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn status(&self) -> LaunchStatus {
        let mut child = self.child.lock().unwrap();
        let Some(c) = child.as_mut() else { return LaunchStatus::default() };
        match c.try_wait() {
            Ok(None) => LaunchStatus { running: true, ..Default::default() },
            Ok(Some(st)) => {
                let code = st.code();
                let log_tail = if code == Some(0) { String::new() } else { tail(&self.log_path, 12) };
                LaunchStatus { running: false, exit_code: code, log_tail }
            }
            Err(_) => LaunchStatus::default(),
        }
    }
}

/// The last `lines` lines of a text file ("" if unreadable).
pub fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[project]\n").unwrap();
    }

    #[test]
    fn finds_the_repo_transcriber_from_a_dev_build() {
        let root = tempfile::tempdir().unwrap();
        project(&root.path().join("backend/transcriber"));
        let exe = root.path().join("backend/recorder/target/release/audcap.exe");
        assert_eq!(find_dir(Path::new(""), &exe), Some(root.path().join("backend").join("transcriber")));
    }

    #[test]
    fn finds_a_transcriber_folder_next_to_the_exe() {
        let root = tempfile::tempdir().unwrap();
        project(&root.path().join("transcriber"));
        let exe = root.path().join("audcap.exe");
        assert_eq!(find_dir(Path::new(""), &exe), Some(root.path().join("transcriber")));
    }

    #[test]
    fn configured_folder_wins_and_must_be_a_project() {
        let root = tempfile::tempdir().unwrap();
        project(&root.path().join("transcriber"));
        let exe = root.path().join("audcap.exe");
        let other = root.path().join("elsewhere");
        assert_eq!(find_dir(&other, &exe), None);
        project(&other);
        assert_eq!(find_dir(&other, &exe), Some(other));
    }

    #[test]
    fn nothing_found_is_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(find_dir(Path::new(""), &root.path().join("a/b/audcap.exe")), None);
    }

    #[test]
    fn tail_keeps_the_last_non_empty_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.log");
        std::fs::write(&p, "a\n\nb\nc\nd\n\n").unwrap();
        assert_eq!(tail(&p, 2), "c\nd");
        assert_eq!(tail(&p, 10), "a\nb\nc\nd");
        assert_eq!(tail(&dir.path().join("missing"), 3), "");
    }
}
