//! Who is using the microphone, from the Windows privacy "consent store".
//!
//! Every app that touches the mic gets a key under
//! `HKCU\…\CapabilityAccessManager\ConsentStore\microphone`: packaged (Store)
//! apps by package family name, desktop apps under `NonPackaged\<exe path with
//! '\' replaced by '#'>`. While an app holds the mic its `LastUsedTimeStart` is
//! set and `LastUsedTimeStop` is 0.

use std::collections::BTreeSet;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::controller::Input;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsentEntry {
    /// Package family name, or exe path with '#' separators.
    pub key: String,
    pub packaged: bool,
    pub start: u64,
    pub stop: u64,
}

//// Lower-case exe file name → app.
const KNOWN_EXES: &[(&str, &str)] = &[
    ("ms-teams.exe", "Teams"),
    ("teams.exe", "Teams"),
    ("zoom.exe", "Zoom"),
    ("webex.exe", "Webex"),
    ("webexhost.exe", "Webex"),
    ("ciscocollabhost.exe", "Webex"),
    ("atmgr.exe", "Webex"),
    ("slack.exe", "Slack"),
    ("chrome.exe", "Chrome"),
    ("msedge.exe", "Edge"),
    ("firefox.exe", "Firefox"),
];

/// Lower-case package name (before the `_publisherid`) → app.
const KNOWN_PACKAGES: &[(&str, &str)] = &[
    ("msteams", "Teams"),
    ("microsoftteams", "Teams"),
    ("microsoft.teams", "Teams"),
    ("91750d7e.slack", "Slack"),
];

fn exe_file_name(key: &str) -> &str {
    key.rsplit(['#', '\\', '/']).next().unwrap_or(key)
}

/// Display name for a consent-store key: a known meeting app, else the exe stem
/// or the package name.
pub fn friendly_name(key: &str, packaged: bool) -> String {
    if packaged {
        let name = key.split('_').next().unwrap_or(key);
        let lower = name.to_ascii_lowercase();
        if let Some((_, app)) = KNOWN_PACKAGES.iter().find(|(p, _)| *p == lower) {
            return app.to_string();
        }
        return name.rsplit('.').next().unwrap_or(name).to_string();
    }
    let file = exe_file_name(key);
    let lower = file.to_ascii_lowercase();
    if let Some((_, app)) = KNOWN_EXES.iter().find(|(e, _)| *e == lower) {
        return app.to_string();
    }
    file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_string()
}

/// Friendly names of apps holding the mic now, excluding `own_exe` (file name).
pub fn active_holders(entries: &[ConsentEntry], own_exe: &str) -> BTreeSet<String> {
    entries
        .iter()
        .filter(|e| e.start > 0 && e.stop == 0)
        .filter(|e| e.packaged || !exe_file_name(&e.key).eq_ignore_ascii_case(own_exe))
        .map(|e| friendly_name(&e.key, e.packaged))
        .collect()
}

/// The holders that are on the (case-insensitive) allow-list.
pub fn filter_allowed(holders: &BTreeSet<String>, allow: &[String]) -> BTreeSet<String> {
    holders.iter().filter(|h| allow.iter().any(|a| a.eq_ignore_ascii_case(h))).cloned().collect()
}

/// Turns successive snapshots of allow-listed holders into controller inputs.
#[derive(Default)]
pub struct UsageTracker {
    active: BTreeSet<String>,
}

impl UsageTracker {
    pub fn update(&mut self, now: BTreeSet<String>) -> Vec<Input> {
        let mut inputs: Vec<Input> = now.difference(&self.active).cloned().map(Input::MicInUse).collect();
        if now.is_empty() && !self.active.is_empty() {
            inputs.push(Input::MicReleased);
        }
        self.active = now;
        inputs
    }

    /// Forget the last snapshot so the next one is reported in full (after resume).
    pub fn reset(&mut self) {
        self.active.clear();
    }
}

const CONSENT_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

/// Reads every microphone consent entry for the current user.
pub fn read_consent_store() -> std::io::Result<Vec<ConsentEntry>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    fn read_entry(parent: &RegKey, name: String, packaged: bool) -> Option<ConsentEntry> {
        let key = parent.open_subkey(&name).ok()?;
        let start: u64 = key.get_value("LastUsedTimeStart").unwrap_or(0);
        let stop: u64 = key.get_value("LastUsedTimeStop").unwrap_or(0);
        Some(ConsentEntry { key: name, packaged, start, stop })
    }

    let root = RegKey::predef(HKEY_CURRENT_USER).open_subkey(CONSENT_KEY)?;
    let mut entries = Vec::new();
    for name in root.enum_keys().flatten() {
        if name == "NonPackaged" {
            if let Ok(np) = root.open_subkey(&name) {
                entries.extend(np.enum_keys().flatten().filter_map(|n| read_entry(&np, n, false)));
            }
        } else {
            entries.extend(read_entry(&root, name, true));
        }
    }
    Ok(entries)
}

/// Polls the consent store every 2 s and sends the set of mic holders (all apps,
/// not yet allow-list filtered) whenever it changes. Stops when the receiver is gone.
pub fn spawn_detector(tx: Sender<BTreeSet<String>>) {
    let own_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "meetrec.exe".into());
    let spawned = std::thread::Builder::new().name("mic-detector".into()).spawn(move || {
        let mut last: Option<BTreeSet<String>> = None;
        loop {
            match read_consent_store() {
                Ok(entries) => {
                    let holders = active_holders(&entries, &own_exe);
                    if last.as_ref() != Some(&holders) {
                        tracing::debug!("mic holders: {holders:?}");
                        if tx.send(holders.clone()).is_err() {
                            return;
                        }
                        last = Some(holders);
                    }
                }
                Err(e) => tracing::warn!("reading mic consent store: {e}"),
            }
            std::thread::sleep(POLL);
        }
    });
    if let Err(e) = spawned {
        tracing::error!("could not start mic detector: {e}");
    }
}

const POLL: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests {
    use super::*;

    fn np(path: &str, start: u64, stop: u64) -> ConsentEntry {
        ConsentEntry { key: path.into(), packaged: false, start, stop }
    }

    fn pk(name: &str, start: u64, stop: u64) -> ConsentEntry {
        ConsentEntry { key: name.into(), packaged: true, start, stop }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn known_apps_get_friendly_names() {
        assert_eq!(friendly_name("C:#Program Files#Google#Chrome#Application#chrome.exe", false), "Chrome");
        assert_eq!(friendly_name("C:#Program Files (x86)#Microsoft#Edge#Application#msedge.exe", false), "Edge");
        assert_eq!(friendly_name("C:#Program Files#Mozilla Firefox#firefox.exe", false), "Firefox");
        assert_eq!(friendly_name("C:#Users#me#AppData#Roaming#Zoom#bin#Zoom.exe", false), "Zoom");
        assert_eq!(friendly_name("C:#Users#me#AppData#Local#Microsoft#Teams#current#Teams.exe", false), "Teams");
        assert_eq!(friendly_name("C:#Program Files#Cisco Spark#CiscoCollabHost.exe", false), "Webex");
        assert_eq!(friendly_name("C:#Users#me#AppData#Local#slack#app-4.41.0#slack.exe", false), "Slack");
        assert_eq!(friendly_name("MSTeams_8wekyb3d8bbwe", true), "Teams");
        assert_eq!(friendly_name("91750D7E.Slack_8she8kybcnzg4", true), "Slack");
    }

    #[test]
    fn unknown_apps_fall_back_to_exe_or_package_name() {
        assert_eq!(friendly_name("C:#Users#me#AppData#Local#Discord#app-1.0#Discord.exe", false), "Discord");
        assert_eq!(friendly_name("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe", true), "WindowsSoundRecorder");
    }

    #[test]
    fn holder_is_an_entry_with_start_and_no_stop() {
        let entries = vec![
            np("C:#Program Files#Google#Chrome#Application#chrome.exe", 134367984624333222, 134368074014870522),
            np("C:#Users#me#AppData#Roaming#Zoom#bin#Zoom.exe", 134368317567963836, 0),
            pk("MSTeams_8wekyb3d8bbwe", 0, 0),
            pk("91750D7E.Slack_8she8kybcnzg4", 134368317567963836, 0),
        ];
        assert_eq!(active_holders(&entries, "meetrec.exe"), set(&["Slack", "Zoom"]));
    }

    #[test]
    fn our_own_exe_is_ignored() {
        let entries = vec![np("C:#Drive#Vault#audio-capture#target#release#MeetRec.exe", 134368317567963836, 0)];
        assert!(active_holders(&entries, "meetrec.exe").is_empty());
    }

    #[test]
    fn allow_list_is_case_insensitive() {
        let holders = set(&["Zoom", "Discord", "Chrome"]);
        let allow = vec!["zoom".to_string(), "Chrome".to_string()];
        assert_eq!(filter_allowed(&holders, &allow), set(&["Chrome", "Zoom"]));
    }

    #[test]
    fn tracker_reports_new_holders_and_release() {
        let mut t = UsageTracker::default();
        assert!(t.update(set(&[])).is_empty());
        assert_eq!(t.update(set(&["Teams"])), vec![Input::MicInUse("Teams".into())]);
        assert!(t.update(set(&["Teams"])).is_empty(), "no change, no input");
        assert_eq!(t.update(set(&["Chrome", "Teams"])), vec![Input::MicInUse("Chrome".into())]);
        assert!(t.update(set(&["Chrome"])).is_empty(), "someone still holds it");
        assert_eq!(t.update(set(&[])), vec![Input::MicReleased]);
    }

    #[test]
    fn tracker_reset_reports_current_holders_again() {
        let mut t = UsageTracker::default();
        t.update(set(&["Teams"]));
        t.reset();
        assert_eq!(t.update(set(&["Teams"])), vec![Input::MicInUse("Teams".into())]);
    }

    #[test]
    fn live_consent_store_is_readable() {
        // Every Windows user profile has this key once any app has asked for the mic.
        let entries = read_consent_store().expect("consent store");
        assert!(entries.iter().all(|e| !e.key.is_empty()));
    }
}
