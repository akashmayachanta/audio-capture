//! Audio endpoint lookup.

use anyhow::{Context, Result};
use wasapi::{Device, DeviceEnumerator, DeviceEventCallbacks, DeviceState, Direction, Role};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

/// Active endpoints for a direction (capture = microphones, render = speakers).
/// COM must be initialized on the calling thread.
pub fn list(direction: Direction) -> Result<Vec<DeviceInfo>> {
    let collection = DeviceEnumerator::new()?.get_device_collection(&direction)?;
    let mut out = Vec::new();
    for i in 0..collection.get_nbr_devices()? {
        let dev = collection.get_device_at_index(i)?;
        out.push(DeviceInfo { id: dev.get_id()?, name: dev.get_friendlyname()? });
    }
    Ok(out)
}

/// The device with `id` if it is present, otherwise the default device.
pub fn open(direction: Direction, id: Option<&str>) -> Result<Device> {
    let enumerator = DeviceEnumerator::new()?;
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        match enumerator.get_device(id) {
            Ok(dev) => match dev.get_state() {
                Ok(state) if usable(&state) => return Ok(dev),
                Ok(state) => tracing::warn!("device {id} is {state:?}, using default"),
                Err(e) => tracing::warn!("device {id} state unknown ({e}), using default"),
            },
            Err(e) => tracing::warn!("device {id} unavailable ({e}), using default"),
        }
    }
    enumerator
        .get_default_device(&direction)
        .with_context(|| format!("no default {direction:?} device"))
}

/// `GetDevice` also returns unplugged/disabled endpoints; capturing from those
/// fails forever, so only an active one counts as available.
fn usable(state: &DeviceState) -> bool {
    matches!(state, DeviceState::Active)
}

/// Whether a default-device change affects the endpoints we capture from
/// (`open` follows the Console-role default).
pub fn is_relevant_default_change(_direction: Direction, role: Role) -> bool {
    role == Role::Console
}

/// Calls `on_change` (from an audio-system thread) whenever the default mic or
/// speakers change. Registration lives on its own MTA thread for the process lifetime.
pub fn watch_default_devices(on_change: impl Fn() + Send + Sync + 'static) {
    let spawned = std::thread::Builder::new().name("device-watch".into()).spawn(move || {
        if wasapi::initialize_mta().is_err() {
            tracing::warn!("device watcher: COM init failed");
            return;
        }
        let mut callbacks = DeviceEventCallbacks::new();
        callbacks.set_default_device_callback(move |direction, role, id| {
            if is_relevant_default_change(direction, role) {
                tracing::info!("default {direction:?} device changed to {id:?}");
                on_change();
            }
        });
        let registration = DeviceEnumerator::new().and_then(|e| e.register_notification_callback(callbacks));
        match registration {
            Ok(_registration) => loop {
                std::thread::park();
            },
            Err(e) => tracing::warn!("device watcher: {e}"),
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("device watcher thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_active_endpoint_is_used_as_the_chosen_mic() {
        assert!(usable(&DeviceState::Active));
        for state in [DeviceState::Disabled, DeviceState::NotPresent, DeviceState::Unplugged] {
            assert!(!usable(&state), "{state:?}");
        }
    }

    #[test]
    fn only_console_role_changes_matter() {
        assert!(is_relevant_default_change(Direction::Capture, Role::Console));
        assert!(is_relevant_default_change(Direction::Render, Role::Console));
        assert!(!is_relevant_default_change(Direction::Capture, Role::Communications));
        assert!(!is_relevant_default_change(Direction::Render, Role::Multimedia));
    }
}
