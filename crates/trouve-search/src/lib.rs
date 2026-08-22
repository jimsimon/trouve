//! Trouve: fast and accurate code search for agents.
//!
//! Rust port of [MinishLab/semble](https://github.com/MinishLab/semble) with a
//! content-addressed chunk store that makes indexing incremental, multithreaded,
//! and shared across git branches and worktrees.

pub mod bm25;
pub mod chunk;
pub mod cli;
pub mod daemon;
pub mod dense;
pub mod embed;
pub mod index;
pub mod languages;
pub mod manifest;
pub mod mcp;
pub mod ranking;
pub mod search;
pub mod snapshot;
pub mod stats;
pub mod store;
pub mod tokens;
pub mod types;
pub mod utils;
pub mod walker;

/// Ask glibc to return completely free allocator pages to the operating
/// system after a memory-intensive index build or eviction.
///
/// Other allocators and platforms either release memory by their own policy
/// or do not expose an equivalent stable API, so this is a no-op there.
pub fn release_unused_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }

        // SAFETY: malloc_trim is process-global and thread-safe in glibc.
        // A zero pad requests release of every completely free page.
        let _ = unsafe { malloc_trim(0) };
    }
}

/// Schedule allocator-page release without adding its potentially expensive
/// glibc arena scan to a foreground search request. Concurrent requests
/// coalesce behind one process-wide trim worker.
pub fn release_unused_memory_in_background() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::sync::atomic::{AtomicBool, Ordering};

        static TRIM_RUNNING: AtomicBool = AtomicBool::new(false);
        if TRIM_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("trouve-memory-trim".into())
            .spawn(|| {
                release_unused_memory();
                TRIM_RUNNING.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            TRIM_RUNNING.store(false, Ordering::Release);
        }
    }
}
