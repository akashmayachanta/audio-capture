//! "Start with Windows" via the per-user Run key.

use anyhow::Result;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "MeetRec";

/// Adds or removes the Run entry for the current exe.
pub fn set_enabled(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let command = format!("\"{}\"", exe.display());
    set_run_entry(RUN_KEY, VALUE, enabled.then_some(command.as_str()))
}

pub fn is_enabled() -> bool {
    run_entry(RUN_KEY, VALUE).is_some()
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
        ScratchKey(format!(r"Software\MeetRecTest-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn entry_can_be_added_and_removed() {
        let key = scratch();
        assert_eq!(run_entry(&key.0, "MeetRec"), None);
        set_run_entry(&key.0, "MeetRec", Some(r#""C:\Apps\meetrec.exe""#)).unwrap();
        assert_eq!(run_entry(&key.0, "MeetRec").as_deref(), Some(r#""C:\Apps\meetrec.exe""#));
        set_run_entry(&key.0, "MeetRec", None).unwrap();
        assert_eq!(run_entry(&key.0, "MeetRec"), None);
    }

    #[test]
    fn removing_a_missing_entry_is_fine() {
        let key = scratch();
        set_run_entry(&key.0, "MeetRec", None).unwrap();
    }
}
