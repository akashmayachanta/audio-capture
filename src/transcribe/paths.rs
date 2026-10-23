//! The transcription cache folder (`%LOCALAPPDATA%\MeetRec` by default):
//!
//! ```text
//! <root>\venv\                      Python environment (tools\transcribe\setup.ps1)
//! <root>\models\<model>\            downloaded weights + manifest.json
//! <root>\ov_cache\                  OpenVINO compiled-model cache + cache_manifest.json
//! <root>\tmp\<job-id>\              per-job WAVs and progress file
//! ```
//!
//! Deliberately not `%APPDATA%` (which roams and holds config + logs): these
//! are large, machine-specific and re-creatable.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Result};

pub const MODEL_NAME: &str = "parakeet-tdt-0.6b-v3-ov";

/// Overrides the cache location (like `MEETREC_CONFIG` does for the config), so
/// scratch runs never touch the real cache.
pub const ENV_VAR: &str = "MEETREC_CACHE";

/// What `--clear-cache` removes. The venv is never touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearKind {
    /// Compiled OpenVINO blobs.
    Ov,
    /// Per-job scratch folders.
    Tmp,
    /// Compiled blobs, scratch and the downloaded model.
    All,
}

impl FromStr for ClearKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ov" => Ok(Self::Ov),
            "tmp" => Ok(Self::Tmp),
            "all" => Ok(Self::All),
            other => bail!("unknown cache kind {other:?} (use ov, tmp or all)"),
        }
    }
}

/// `%LOCALAPPDATA%\MeetRec`.
pub fn default_root() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("MeetRec")
}

/// Environment override first, then the configured folder, then the default.
pub fn resolve_root(env: Option<OsString>, configured: &Path) -> PathBuf {
    match env.filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None if configured.as_os_str().is_empty() => default_root(),
        None => configured.to_path_buf(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheDirs {
    root: PathBuf,
}

/// Bytes used per part of the cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sizes {
    pub venv: u64,
    pub models: u64,
    pub ov_cache: u64,
    pub tmp: u64,
}

impl Sizes {
    pub fn total(&self) -> u64 {
        self.venv + self.models + self.ov_cache + self.tmp
    }
}

impl CacheDirs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The cache for a configured folder (empty = default), honouring `MEETREC_CACHE`.
    pub fn from_config(configured: &Path) -> Self {
        Self::new(resolve_root(std::env::var_os(ENV_VAR), configured))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn venv_dir(&self) -> PathBuf {
        self.root.join("venv")
    }

    pub fn venv_python(&self) -> PathBuf {
        self.venv_dir().join("Scripts").join("python.exe")
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    pub fn model_dir(&self) -> PathBuf {
        self.models_dir().join(MODEL_NAME)
    }

    pub fn ov_cache_dir(&self) -> PathBuf {
        self.root.join("ov_cache")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn tmp_job_dir(&self, id: &str) -> PathBuf {
        self.tmp_dir().join(id)
    }

    pub fn sizes(&self) -> Sizes {
        Sizes {
            venv: dir_size(&self.venv_dir()),
            models: dir_size(&self.models_dir()),
            ov_cache: dir_size(&self.ov_cache_dir()),
            tmp: dir_size(&self.tmp_dir()),
        }
    }

    /// Removes what `kind` names; returns the bytes freed. Missing folders are fine.
    pub fn clear(&self, kind: ClearKind) -> Result<u64> {
        let dirs = match kind {
            ClearKind::Ov => vec![self.ov_cache_dir()],
            ClearKind::Tmp => vec![self.tmp_dir()],
            ClearKind::All => vec![self.ov_cache_dir(), self.tmp_dir(), self.models_dir()],
        };
        let mut freed = 0;
        for dir in dirs {
            let size = dir_size(&dir);
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => freed += size,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(anyhow::Error::new(e).context(format!("remove {}", dir.display()))),
            }
        }
        Ok(freed)
    }

    /// Deletes job folders under `tmp` untouched for `max_age` (left by a crash).
    /// Returns how many were removed.
    pub fn sweep_stale_tmp(&self, max_age: Duration) -> usize {
        self.sweep_stale_tmp_at(SystemTime::now(), max_age)
    }

    pub fn sweep_stale_tmp_at(&self, now: SystemTime, max_age: Duration) -> usize {
        let Ok(entries) = std::fs::read_dir(self.tmp_dir()) else { return 0 };
        let mut removed = 0;
        for e in entries.flatten() {
            let path = e.path();
            let age = newest_mtime(&path).and_then(|t| now.duration_since(t).ok());
            if age.is_some_and(|a| a >= max_age) && remove_any(&path) {
                removed += 1;
            }
        }
        removed
    }
}

fn remove_any(path: &Path) -> bool {
    if path.is_dir() { std::fs::remove_dir_all(path).is_ok() } else { std::fs::remove_file(path).is_ok() }
}

/// Newest modification time of `path` and its direct children (a job folder's
/// own mtime only moves when files are created in it).
fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let own = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let children = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().and_then(|m| m.modified()).ok());
    own.into_iter().chain(children).max()
}

/// Total size of the files under `path` (0 when it does not exist).
pub fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// `1.2 GB`, `340 MB`, `12 KB`.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.0} MB", b / (KB * KB))
    } else {
        format!("{:.0} KB", b / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// Backdates a file or folder (Windows only lets you open a folder this way).
    fn age(path: &Path, ago: Duration) {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let t = SystemTime::now() - ago;
        std::fs::File::options()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn layout_hangs_off_the_root() {
        let c = CacheDirs::new("C:\\cache");
        assert_eq!(c.venv_python(), Path::new("C:\\cache\\venv\\Scripts\\python.exe"));
        assert_eq!(c.model_dir(), Path::new("C:\\cache\\models").join(MODEL_NAME));
        assert_eq!(c.ov_cache_dir(), Path::new("C:\\cache\\ov_cache"));
        assert_eq!(c.tmp_job_dir("abc"), Path::new("C:\\cache\\tmp\\abc"));
    }

    #[test]
    fn env_beats_config_beats_default() {
        let configured = Path::new("D:\\mine");
        assert_eq!(resolve_root(Some("E:\\scratch".into()), configured), Path::new("E:\\scratch"));
        assert_eq!(resolve_root(None, configured), configured);
        assert_eq!(resolve_root(Some("".into()), configured), configured, "empty env is unset");
        assert_eq!(resolve_root(None, Path::new("")), default_root());
        assert!(default_root().ends_with("MeetRec"));
    }

    #[test]
    fn clear_kinds_parse() {
        assert_eq!("ov".parse::<ClearKind>().unwrap(), ClearKind::Ov);
        assert_eq!("TMP".parse::<ClearKind>().unwrap(), ClearKind::Tmp);
        assert_eq!("all".parse::<ClearKind>().unwrap(), ClearKind::All);
        assert!("models".parse::<ClearKind>().is_err());
    }

    #[test]
    fn sizes_add_up_per_folder() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheDirs::new(dir.path());
        write(&c.model_dir().join("a.bin"), 1000);
        write(&c.model_dir().join("sub").join("b.bin"), 500);
        write(&c.ov_cache_dir().join("blob"), 200);
        write(&c.tmp_job_dir("j").join("me.wav"), 40);
        write(&c.venv_dir().join("x"), 7);
        let s = c.sizes();
        assert_eq!(s, Sizes { venv: 7, models: 1500, ov_cache: 200, tmp: 40 });
        assert_eq!(s.total(), 1747);
        assert_eq!(CacheDirs::new(dir.path().join("none")).sizes(), Sizes::default());
    }

    #[test]
    fn clearing_ov_or_tmp_leaves_models_and_venv_alone() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheDirs::new(dir.path());
        write(&c.model_dir().join("a.bin"), 1000);
        write(&c.ov_cache_dir().join("blob"), 200);
        write(&c.tmp_job_dir("j").join("me.wav"), 40);
        write(&c.venv_dir().join("x"), 7);

        assert_eq!(c.clear(ClearKind::Ov).unwrap(), 200);
        assert!(!c.ov_cache_dir().exists());
        assert!(c.tmp_dir().exists() && c.model_dir().exists());

        assert_eq!(c.clear(ClearKind::Tmp).unwrap(), 40);
        assert!(!c.tmp_dir().exists());
        assert!(c.model_dir().join("a.bin").exists() && c.venv_dir().join("x").exists());
    }

    #[test]
    fn clearing_all_removes_models_too_but_keeps_the_venv() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheDirs::new(dir.path());
        write(&c.model_dir().join("a.bin"), 1000);
        write(&c.ov_cache_dir().join("blob"), 200);
        write(&c.venv_dir().join("x"), 7);
        assert_eq!(c.clear(ClearKind::All).unwrap(), 1200);
        assert!(!c.models_dir().exists() && !c.ov_cache_dir().exists());
        assert!(c.venv_dir().join("x").exists());
    }

    #[test]
    fn clearing_an_empty_cache_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(CacheDirs::new(dir.path().join("nothing")).clear(ClearKind::All).unwrap(), 0);
    }

    #[test]
    fn only_stale_job_folders_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let c = CacheDirs::new(dir.path());
        let old = c.tmp_job_dir("old").join("me.wav");
        let fresh = c.tmp_job_dir("fresh").join("me.wav");
        let busy = c.tmp_job_dir("busy").join("progress.json");
        let busy_wav = c.tmp_job_dir("busy").join("me.wav");
        for p in [&old, &fresh, &busy, &busy_wav] {
            write(p, 10);
        }
        let day = Duration::from_secs(24 * 3600);
        age(&old, day * 3);
        age(&c.tmp_job_dir("old"), day * 3);
        age(&busy_wav, day * 3);
        age(&c.tmp_job_dir("busy"), day * 3); // dir is old, but progress.json is fresh
        assert_eq!(c.sweep_stale_tmp(day), 1);
        assert!(!c.tmp_job_dir("old").exists());
        assert!(c.tmp_job_dir("fresh").exists() && c.tmp_job_dir("busy").exists());
    }

    #[test]
    fn sweeping_without_a_tmp_folder_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(CacheDirs::new(dir.path()).sweep_stale_tmp(Duration::from_secs(1)), 0);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_size(0), "0 KB");
        assert_eq!(format_size(2048), "2 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(format_size(1_288_490_189), "1.2 GB");
    }
}
