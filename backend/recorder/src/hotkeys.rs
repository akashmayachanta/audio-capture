//! Global hotkeys from the config. They are delivered through the same thread's
//! message loop as the tray, and map onto the same commands as the tray menu.

use anyhow::Result;
use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;

use crate::config;
use crate::tray::MenuCommand;

/// The configured bindings, parsed. An empty string disables that binding.
pub fn parse_bindings(cfg: &config::Hotkeys) -> Vec<(MenuCommand, Result<HotKey>)> {
    [
        (MenuCommand::StartStop, &cfg.start_stop),
        (MenuCommand::PauseResume, &cfg.pause),
        (MenuCommand::Split, &cfg.split),
        (MenuCommand::Marker, &cfg.marker),
    ]
    .into_iter()
    .filter(|(_, s)| !s.trim().is_empty())
    .map(|(cmd, s)| (cmd, s.trim().parse::<HotKey>().map_err(|e| anyhow::anyhow!("{s:?}: {e}"))))
    .collect()
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    bound: Vec<(HotKey, MenuCommand)>,
}

impl Hotkeys {
    pub fn new() -> Result<Self> {
        Ok(Self { manager: GlobalHotKeyManager::new()?, bound: Vec::new() })
    }

    /// Replaces all registrations. Bad or already-taken bindings are logged and skipped.
    pub fn apply(&mut self, cfg: &config::Hotkeys) {
        for (hk, _) in self.bound.drain(..) {
            let _ = self.manager.unregister(hk);
        }
        for (cmd, parsed) in parse_bindings(cfg) {
            match parsed.and_then(|hk| Ok(self.manager.register(hk).map(|_| hk)?)) {
                Ok(hk) => {
                    tracing::debug!("hotkey {cmd:?} registered as {hk} (id {})", hk.id());
                    self.bound.push((hk, cmd));
                }
                Err(e) => tracing::warn!("hotkey for {cmd:?} not registered: {e:#}"),
            }
        }
    }

    pub fn command_for(&self, id: u32) -> Option<MenuCommand> {
        self.bound.iter().find(|(hk, _)| hk.id() == id).map(|(_, c)| *c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::{Code, Modifiers};

    #[test]
    fn default_bindings_parse() {
        let b = parse_bindings(&config::Hotkeys::default());
        assert_eq!(b.len(), 4);
        let (cmd, hk) = &b[0];
        assert_eq!(*cmd, MenuCommand::StartStop);
        assert_eq!(*hk.as_ref().unwrap(), HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyR));
        let cmds: Vec<_> = b.iter().map(|(c, _)| *c).collect();
        assert_eq!(cmds, vec![MenuCommand::StartStop, MenuCommand::PauseResume, MenuCommand::Split, MenuCommand::Marker]);
        assert!(b.iter().all(|(_, hk)| hk.is_ok()));
    }

    #[test]
    fn empty_binding_is_skipped_and_garbage_is_an_error() {
        let cfg = config::Hotkeys {
            start_stop: String::new(),
            pause: "ctrl+alt+nonsense".into(),
            split: "F9".into(),
            marker: " ".into(),
        };
        let b = parse_bindings(&cfg);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].0, MenuCommand::PauseResume);
        assert!(b[0].1.is_err());
        assert_eq!(b[1].0, MenuCommand::Split);
        assert!(b[1].1.is_ok());
    }
}
