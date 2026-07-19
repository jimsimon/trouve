//! Non-blocking system opener that still reaps its launcher process.

use std::ffi::{OsStr, OsString};

/// Open a URL or path with the system handler without blocking the caller.
///
/// `open::that_detached` double-forks on Unix and leaves its intermediate
/// child for this long-lived process to reap. Running the ordinary, waiting
/// opener on a short-lived thread keeps the UI responsive while ensuring the
/// launcher is collected when it exits.
pub fn open(path: impl AsRef<OsStr>) {
    let path: OsString = path.as_ref().to_owned();
    std::thread::spawn(move || {
        if let Err(error) = open::that(&path) {
            tracing::warn!(%error, path = %path.to_string_lossy(), "could not open system handler");
        }
    });
}
