//! Bounded, process-reaping system opener for the isolated Servo harness.
//!
//! This intentionally mirrors `trouve-app`'s native opener. The preview is an
//! excluded workspace, so keeping the tiny adapter local avoids creating a
//! reusable crate solely for qualification code.

use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

const QUEUE_CAPACITY: usize = 16;
static WORKER: OnceLock<Option<SyncSender<OsString>>> = OnceLock::new();

pub fn open(path: impl AsRef<OsStr>) -> Result<(), String> {
    enqueue(path.as_ref().to_owned())
}

fn enqueue(request: OsString) -> Result<(), String> {
    let Some(sender) = WORKER.get_or_init(start_worker) else {
        return Err("system opener worker is unavailable".into());
    };
    match sender.try_send(request) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err("system opener queue is full".into()),
        Err(TrySendError::Disconnected(_)) => Err("system opener worker disconnected".into()),
    }
}

fn start_worker() -> Option<SyncSender<OsString>> {
    let (sender, receiver) = sync_channel::<OsString>(QUEUE_CAPACITY);
    match std::thread::Builder::new()
        .name("trouve-servo-opener".into())
        .spawn(move || {
            while let Ok(path) = receiver.recv() {
                if let Err(error) = open::that(&path) {
                    tracing::warn!(
                        %error,
                        path = %path.to_string_lossy(),
                        "could not open system handler"
                    );
                }
            }
        }) {
        Ok(_) => Some(sender),
        Err(error) => {
            tracing::warn!(%error, "could not start system opener worker");
            None
        }
    }
}
