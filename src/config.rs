//! `%APPDATA%\MeetRec\config.toml`. Every field has a default, so a partial (or
//! missing) file is fine.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub use crate::session::CaptureMode;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    pub start_stop: String,
    pub pause: String,
    pub split: String,
    pub marker: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub capture_mode: CaptureMode,
    /// Endpoint id of the mic; empty = Windows default.
    pub mic_device: String,
    /// Endpoint id of the speakers/headset whose output is recorded as
    /// "others"; empty = Windows default. Meeting apps can play to a device
    /// other than the default, which would leave that channel silent.
    pub system_device: String,
    pub output_dir: PathBuf,
    /// Per channel.
    pub bitrate_kbps: u32,
    pub auto_record: bool,
    pub trigger_apps: Vec<String>,
    pub release_grace_secs: u64,
    pub min_duration_secs: u64,
    pub hotkeys: Hotkeys,
    pub start_with_windows: bool,
}

/// Apps the detector knows by name; also the default allow-list.
pub const KNOWN_APPS: &[&str] = &["Teams", "Zoom", "Webex", "Slack", "Chrome", "Edge", "Firefox"];

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            start_stop: "ctrl+alt+R".into(),
            pause: "ctrl+alt+P".into(),
            split: "ctrl+alt+S".into(),
            marker: "ctrl+alt+M".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let documents = directories::UserDirs::new()
            .and_then(|u| u.document_dir().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        Self {
            capture_mode: CaptureMode::Both,
            mic_device: String::new(),
            system_device: String::new(),
            output_dir: documents.join("MeetRec"),
            bitrate_kbps: 24,
            auto_record: true,
            trigger_apps: KNOWN_APPS.iter().map(|s| s.to_string()).collect(),
            release_grace_secs: 10,
            min_duration_secs: 30,
            hotkeys: Hotkeys::default(),
            start_with_windows: false,
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str::<Self>(text)?.sanitized())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config serializes")
    }

    /// Reads `path`; a missing file is created with defaults. A broken file is
    /// left alone (the user may be mid-edit) and defaults are used.
    pub fn load_or_create(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|e| {
                tracing::warn!("{} is invalid ({e:#}); using defaults", path.display());
                Self::default()
            }),
            Err(_) => {
                let c = Self::default();
                if let Err(e) = c.save(path) {
                    tracing::warn!("could not write {}: {e:#}", path.display());
                }
                c
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_toml())?;
        Ok(())
    }

    /// Clamps numbers into ranges the recorder can work with.
    pub fn sanitized(mut self) -> Self {
        self.bitrate_kbps = self.bitrate_kbps.clamp(6, 128);
        self.release_grace_secs = self.release_grace_secs.clamp(1, 600);
        self.min_duration_secs = self.min_duration_secs.min(3600);
        self
    }
}

/// Notices edits to the config file (by the settings window or by hand).
pub struct ConfigWatcher {
    path: PathBuf,
    modified: Option<SystemTime>,
}

impl ConfigWatcher {
    /// Starts from the file's current state; only later changes are reported.
    pub fn new(path: &Path) -> Self {
        Self { path: path.to_path_buf(), modified: modified(path) }
    }

    /// `Some` when the file changed since the last poll: the new config, or the
    /// parse error (callers keep their current config then).
    pub fn poll(&mut self) -> Option<Result<Config>> {
        let now = modified(&self.path);
        if now == self.modified || now.is_none() {
            return None;
        }
        self.modified = now;
        Some(std::fs::read_to_string(&self.path).map_err(Into::into).and_then(|t| Config::parse(&t)))
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// `%APPDATA%\MeetRec`.
pub fn app_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("MeetRec")
}

/// `MEETREC_CONFIG` overrides the location (handy for testing).
pub fn config_path() -> PathBuf {
    std::env::var_os("MEETREC_CONFIG").map(PathBuf::from).unwrap_or_else(|| app_dir().join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_plan() {
        let c = Config::default();
        assert_eq!(c.capture_mode, CaptureMode::Both);
        assert!(c.auto_record);
        assert_eq!(c.release_grace_secs, 10);
        assert_eq!(c.min_duration_secs, 30);
        assert_eq!(c.bitrate_kbps, 24);
        assert!(!c.start_with_windows);
        assert_eq!(c.mic_device, "");
        assert_eq!(c.system_device, "");
        assert_eq!(c.trigger_apps, KNOWN_APPS.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(c.hotkeys.start_stop, "ctrl+alt+R");
        assert_eq!(c.hotkeys.pause, "ctrl+alt+P");
        assert_eq!(c.hotkeys.split, "ctrl+alt+S");
        assert_eq!(c.hotkeys.marker, "ctrl+alt+M");
        assert!(c.output_dir.ends_with("MeetRec"));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut c = Config::default();
        c.capture_mode = CaptureMode::Mic;
        c.system_device = "{headset}".into();
        c.trigger_apps = vec!["Zoom".into()];
        c.hotkeys.split = "ctrl+shift+X".into();
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let c = Config::parse("capture_mode = \"system\"\n[hotkeys]\npause = \"F9\"\n").unwrap();
        assert_eq!(c.capture_mode, CaptureMode::System);
        assert_eq!(c.hotkeys.pause, "F9");
        assert_eq!(c.hotkeys.start_stop, "ctrl+alt+R");
        assert_eq!(c.release_grace_secs, 10);
    }

    #[test]
    fn broken_file_is_an_error() {
        assert!(Config::parse("capture_mode = 7").is_err());
    }

    #[test]
    fn missing_file_is_created_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let c = Config::load_or_create(&path);
        assert_eq!(c, Config::default());
        assert_eq!(Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap(), c);
    }

    #[test]
    fn out_of_range_numbers_are_clamped() {
        let c = Config::parse("bitrate_kbps = 1000\nrelease_grace_secs = 0\nmin_duration_secs = 99999").unwrap();
        assert_eq!(c.bitrate_kbps, 128);
        assert_eq!(c.release_grace_secs, 1);
        assert_eq!(c.min_duration_secs, 3600);
        assert_eq!(Config::parse("bitrate_kbps = 1").unwrap().bitrate_kbps, 6);
    }

    fn touch(path: &Path, text: &str, secs_later: u64) {
        std::fs::write(path, text).unwrap();
        let t = SystemTime::now() + std::time::Duration::from_secs(secs_later);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    #[test]
    fn watcher_reports_changes_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save(&path).unwrap();
        let mut w = ConfigWatcher::new(&path);
        assert!(w.poll().is_none(), "unchanged");
        touch(&path, "auto_record = false", 5);
        let c = w.poll().expect("changed").unwrap();
        assert!(!c.auto_record);
        assert!(w.poll().is_none(), "reported once");
    }

    #[test]
    fn watcher_reports_broken_edits_as_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save(&path).unwrap();
        let mut w = ConfigWatcher::new(&path);
        touch(&path, "auto_record = ", 5);
        assert!(w.poll().expect("changed").is_err());
    }

    #[test]
    fn watcher_tolerates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut w = ConfigWatcher::new(&path);
        assert!(w.poll().is_none());
        touch(&path, "auto_record = false", 0);
        assert!(w.poll().is_some());
    }

    #[test]
    fn broken_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "auto_record = maybe").unwrap();
        assert_eq!(Config::load_or_create(&path), Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "auto_record = maybe");
    }
}
