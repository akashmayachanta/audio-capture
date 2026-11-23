//! "Start with Windows" via the per-user Run key.

use anyhow::Result;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "AudCap";

/// Makes the Run entry match `enabled` for the running exe, also when it
/// exists but names another copy (e.g. an old build folder). True if it changed.
pub fn sync(enabled: bool) -> Result<bool> {
    let exe = std::env::current_exe()?;
    let command = format!("\"{}\"", exe.display());
    sync_entry(RUN_KEY, VALUE, enabled.then_some(command.as_str()))
}

/// Writes `wanted` under HKCU\`key` only when it differs from what is there.
pub fn sync_entry(key: &str, name: &str, wanted: Option<&str>) -> Result<bool> {
    if run_entry(key, name).as_deref() == wanted {
        return Ok(false);
    }
    set_run_entry(key, name, wanted)?;
    Ok(true)
}

/// Writes (`Some`) or deletes (`None`) a value under HKCU\`key`.
pub fn set_run_entry(key: &str, name: &str, command: Option<&str>) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match command {
        Some(cmd) => {
            let (run, _) = hkcu.create_subkey(key)?;
            run.set_value(name, &cmd)?;
        }
        None => {
            if let Ok(run) = hkcu.open_subkey_with_flags(key, KEY_SET_VALUE) {
                match run.delete_value(name) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

pub fn run_entry(key: &str, name: &str) -> Option<String> {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(key, KEY_READ).ok()?.get_value(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch key under HKCU, deleted on drop.
    struct ScratchKey(String);

    impl Drop for ScratchKey {
        fn drop(&mut self) {
            let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
        }
    }

    fn scratch() -> ScratchKey {
        ScratchKey(format!(r"Software\AudCapTest-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn entry_can_be_added_and_removed() {
        let key = scratch();
        assert_eq!(run_entry(&key.0, "AudCap"), None);
        set_run_entry(&key.0, "AudCap", Some(r#""C:\Apps\audcap.exe""#)).unwrap();
        assert_eq!(run_entry(&key.0, "AudCap").as_deref(), Some(r#""C:\Apps\audcap.exe""#));
        set_run_entry(&key.0, "AudCap", None).unwrap();
        assert_eq!(run_entry(&key.0, "AudCap"), None);
    }

    #[test]
    fn sync_replaces_a_stale_path_and_leaves_a_current_one() {
        let key = scratch();
        let old = r#""C:\Old\target\release\audcap.exe""#;
        let new = r#""C:\New\backend\recorder\target\release\audcap.exe""#;
        set_run_entry(&key.0, "AudCap", Some(old)).unwrap();
        assert!(sync_entry(&key.0, "AudCap", Some(new)).unwrap(), "stale path is rewritten");
        assert_eq!(run_entry(&key.0, "AudCap").as_deref(), Some(new));
        assert!(!sync_entry(&key.0, "AudCap", Some(new)).unwrap(), "already current");
        assert!(sync_entry(&key.0, "AudCap", None).unwrap());
        assert_eq!(run_entry(&key.0, "AudCap"), None);
        assert!(!sync_entry(&key.0, "AudCap", None).unwrap());
    }

    #[test]
    fn removing_a_missing_entry_is_fine() {
        let key = scratch();
        set_run_entry(&key.0, "AudCap", None).unwrap();
    }
}
