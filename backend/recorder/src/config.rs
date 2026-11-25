//! `%APPDATA%\AudCap\config.toml`. Every field has a default, so a partial (or
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

/// When a dictation's text is put into the focused field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasteMode {
    /// Hands-free: each phrase as soon as it is transcribed. Hold-to-talk still
    /// pastes on release (the held key would turn the paste into a shortcut).
    #[default]
    Live,
    /// Everything at once when the dictation ends.
    End,
}

/// Which service turns dictation into text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DictationEngine {
    /// The local transcriber, phrase by phrase while you talk.
    #[default]
    Local,
    /// The `[speech_api]` endpoint: the whole clip in one request when you stop.
    Api,
}

/// Dictation: hold a key to talk, or double-tap it for hands-free. Key names
/// are `dictation::keys::KeyId` names (`RightAlt`, `F13`, ...); empty = off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dictation {
    pub enabled: bool,
    pub hold_key: String,
    pub toggle_key: String,
    /// Keep each dictation's audio under `<output_dir>/dictation/`.
    pub keep_audio: bool,
    /// Listening stops by itself after this long.
    pub max_secs: u64,
    pub paste_mode: PasteMode,
    pub engine: DictationEngine,
    /// Speech must be this many dB above the room's noise floor (3–30).
    pub noise_margin_db: u32,
}

impl Default for Dictation {
    fn default() -> Self {
        Self { enabled: true, hold_key: "RightAlt".into(), toggle_key: "RightAlt".into(), keep_audio: true, max_secs: 300, paste_mode: PasteMode::Live, engine: DictationEngine::Local, noise_margin_db: 10 }
    }
}

/// An OpenAI-compatible speech-to-text API (`POST {base_url}/audio/transcriptions`),
/// an alternative to the local transcriber; see `speech_api`. Empty = not set up.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeechApi {
    /// e.g. `https://api.openai.com/v1`
    pub base_url: String,
    /// Sent as `Authorization: Bearer`; empty = none.
    pub api_key: String,
    /// e.g. `whisper-1`
    pub model: String,
}

impl SpeechApi {
    pub fn configured(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }

    pub fn sanitized(mut self) -> Self {
        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();
        self.api_key = self.api_key.trim().to_string();
        self.model = self.model.trim().to_string();
        self
    }
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
    /// The Python transcriber's folder (with `pyproject.toml`) for the window's
    /// Start button; empty = found next to the exe or in the repo.
    pub transcriber_dir: PathBuf,
    /// Loopback port of the recordings API for other local apps
    /// (`web_api::server`, `http://127.0.0.1:<port>/api/`); 0 = off. Read at tray start.
    pub web_port: u16,
    pub dictation: Dictation,
    pub speech_api: SpeechApi,
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
            output_dir: documents.join("AudCap"),
            bitrate_kbps: 24,
            auto_record: true,
            trigger_apps: KNOWN_APPS.iter().map(|s| s.to_string()).collect(),
            release_grace_secs: 10,
            min_duration_secs: 30,
            hotkeys: Hotkeys::default(),
            start_with_windows: false,
            transcriber_dir: PathBuf::new(),
            web_port: 8766,
            dictation: Dictation::default(),
            speech_api: SpeechApi::default(),
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
        self.dictation.max_secs = self.dictation.max_secs.clamp(5, 600);
        self.dictation.noise_margin_db = self.dictation.noise_margin_db.clamp(3, 30);
        self.speech_api = self.speech_api.sanitized();
        if self.output_dir.as_os_str().to_string_lossy().trim().is_empty() {
            self.output_dir = Config::default().output_dir;
        }
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

/// `%APPDATA%\AudCap`.
pub fn app_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join("AudCap")
}

/// `AUDCAP_CONFIG` overrides the location (handy for testing).
pub fn config_path() -> PathBuf {
    std::env::var_os("AUDCAP_CONFIG").map(PathBuf::from).unwrap_or_else(|| app_dir().join("config.toml"))
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
        assert!(c.output_dir.ends_with("AudCap"));
    }

    #[test]
    fn empty_output_dir_falls_back_to_the_default() {
        // Otherwise recordings land in the tray's working directory (System32 at sign-in).
        let mut c = Config::default();
        c.output_dir = PathBuf::from("  ");
        assert_eq!(c.sanitized().output_dir, Config::default().output_dir);
        let c = Config::parse("output_dir = \"\"\n").unwrap();
        assert_eq!(c.output_dir, Config::default().output_dir);
    }

    #[test]
    fn old_transcription_section_is_ignored() {
        let c = Config::parse("auto_record = false\n[transcription]\ndevice = \"npu\"\n").unwrap();
        assert!(!c.auto_record);
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
    fn dictation_defaults() {
        let d = Config::default().dictation;
        assert!(d.enabled);
        assert_eq!(d.hold_key, "RightAlt");
        assert_eq!(d.toggle_key, "RightAlt");
        assert!(d.keep_audio);
        assert_eq!(d.max_secs, 300);
        assert_eq!(d.paste_mode, PasteMode::Live);
    }

    #[test]
    fn dictation_engine_is_local_unless_api_is_chosen() {
        let engine = |toml: &str| Config::parse(toml).unwrap().dictation.engine;
        assert_eq!(engine(""), DictationEngine::Local, "configs from before the setting");
        assert_eq!(engine("[dictation]
engine = \"api\"
"), DictationEngine::Api);
        assert_eq!(engine("[dictation]
engine = \"local\"
"), DictationEngine::Local);
        assert!(Config::default().to_toml().contains("engine = \"local\""));
    }

    #[test]
    fn dictation_paste_mode_is_live_or_end() {
        let mode = |toml: &str| Config::parse(toml).unwrap().dictation.paste_mode;
        assert_eq!(mode("[dictation]\npaste_mode = \"end\"\n"), PasteMode::End);
        assert_eq!(mode("[dictation]\npaste_mode = \"live\"\n"), PasteMode::Live);
        assert_eq!(mode("[dictation]\nkeep_audio = false\n"), PasteMode::Live, "configs from before the setting");
        assert!(Config::default().to_toml().contains("paste_mode = \"live\""));
    }

    #[test]
    fn old_config_without_dictation_section_parses() {
        assert_eq!(Config::parse("").unwrap().dictation, Dictation::default());
    }

    #[test]
    fn partial_dictation_section_keeps_other_defaults() {
        let d = Config::parse("[dictation]\nkeep_audio = false\n").unwrap().dictation;
        assert!(!d.keep_audio);
        assert_eq!(d.hold_key, "RightAlt");
        assert_eq!(d.max_secs, 300);
    }

    #[test]
    fn dictation_noise_margin_defaults_to_10_db_and_is_clamped() {
        let m = |toml: &str| Config::parse(toml).unwrap().dictation.noise_margin_db;
        assert_eq!(m(""), 10);
        assert_eq!(m("[dictation]
noise_margin_db = 0
"), 3);
        assert_eq!(m("[dictation]
noise_margin_db = 99
"), 30);
        assert_eq!(m("[dictation]
noise_margin_db = 14
"), 14);
    }

    #[test]
    fn dictation_max_secs_is_clamped() {
        assert_eq!(Config::parse("[dictation]\nmax_secs = 0\n").unwrap().dictation.max_secs, 5);
        assert_eq!(Config::parse("[dictation]\nmax_secs = 99999\n").unwrap().dictation.max_secs, 600);
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
