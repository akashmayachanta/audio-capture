//! Sleep / wake notifications (no window needed).

use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{bail, Result};
use windows::Win32::Foundation::{HANDLE, WIN32_ERROR};
use windows::Win32::System::Power::{PowerRegisterSuspendResumeNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS};
use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    Suspend,
    Resume,
}

/// Maps a `PBT_*` power broadcast code to the events we care about.
pub fn classify(pbt: u32) -> Option<PowerEvent> {
    match pbt {
        PBT_APMSUSPEND => Some(PowerEvent::Suspend),
        PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resume),
        _ => None,
    }
}

/// A power event plus, for Suspend, a channel to acknowledge once the
/// recording is safely closed. The system waits (briefly) for that ack.
pub type Notification = (PowerEvent, Option<SyncSender<()>>);

/// How long the suspend notification may hold up sleep while we close files.
const SUSPEND_ACK_TIMEOUT: Duration = Duration::from_millis(1500);

static SINK: OnceLock<Mutex<Sender<Notification>>> = OnceLock::new();

unsafe extern "system" fn on_power(_context: *const core::ffi::c_void, kind: u32, _setting: *const core::ffi::c_void) -> u32 {
    let Some(event) = classify(kind) else { return 0 };
    let Some(sink) = SINK.get() else { return 0 };
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    let ack = (event == PowerEvent::Suspend).then_some(ack_tx);
    let sent = sink.lock().map(|tx| tx.send((event, ack)).is_ok()).unwrap_or(false);
    if sent && event == PowerEvent::Suspend {
        let _ = ack_rx.recv_timeout(SUSPEND_ACK_TIMEOUT);
    }
    0
}

/// Registers for suspend/resume notifications for the life of the process.
pub fn watch(tx: Sender<Notification>) -> Result<()> {
    if SINK.set(Mutex::new(tx)).is_err() {
        bail!("power watcher already registered");
    }
    // The system keeps a pointer to the parameters; they must outlive the registration.
    let params: &'static mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS =
        Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(on_power), Context: std::ptr::null_mut() }));
    let mut handle = std::ptr::null_mut();
    let err = unsafe {
        PowerRegisterSuspendResumeNotification(
            DEVICE_NOTIFY_CALLBACK,
            HANDLE(params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut core::ffi::c_void),
            &mut handle,
        )
    };
    if err != WIN32_ERROR(0) {
        bail!("PowerRegisterSuspendResumeNotification failed: {err:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_and_both_resume_codes_are_recognised() {
        assert_eq!(classify(4), Some(PowerEvent::Suspend)); // PBT_APMSUSPEND
        assert_eq!(classify(18), Some(PowerEvent::Resume)); // PBT_APMRESUMEAUTOMATIC
        assert_eq!(classify(7), None, "RESUMESUSPEND always follows RESUMEAUTOMATIC");
        assert_eq!(classify(10), None); // PBT_APMPOWERSTATUSCHANGE
    }
}
