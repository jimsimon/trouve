//! Non-blocking system opener that still reaps its launcher process.

use std::ffi::{OsStr, OsString};
use std::sync::OnceLock;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

const QUEUE_CAPACITY: usize = 16;
static WORKER: OnceLock<Option<SyncSender<OsString>>> = OnceLock::new();

/// Open a URL or path with the system handler without blocking the caller.
///
/// `open::that_detached` double-forks on Unix and leaves its intermediate child
/// for this long-lived process to reap. A bounded worker queue runs the
/// ordinary, waiting opener off the UI thread while ensuring the launcher is
/// collected when it exits.
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
        .name("trouve-opener".into())
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
