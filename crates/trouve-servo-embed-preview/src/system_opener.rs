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

pub fn open(path: impl AsRef<OsStr>) {
    let path: OsString = path.as_ref().to_owned();
    let Some(sender) = WORKER.get_or_init(start_worker) else {
        tracing::warn!(path = %path.to_string_lossy(), "system opener worker is unavailable");
        return;
    };
    if let Err(error) = sender.try_send(path) {
        let path = match &error {
            TrySendError::Full(path) | TrySendError::Disconnected(path) => path,
        };
        tracing::warn!(%error, path = %path.to_string_lossy(), "could not queue system handler");
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
