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

#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
const TRIM_RUNNING: u8 = 1;
#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
const TRIM_DIRTY: u8 = 2;

#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn drain_memory_trim_requests(state: &std::sync::atomic::AtomicU8, mut trim: impl FnMut()) {
    use std::sync::atomic::Ordering;

    loop {
        state.fetch_and(!TRIM_DIRTY, Ordering::AcqRel);
        trim();

        loop {
            let current = state.load(Ordering::Acquire);
            if current & TRIM_DIRTY != 0 {
                break;
            }
            if state
                .compare_exchange(
                    current,
                    current & !TRIM_RUNNING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return;
            }
        }
    }
}

#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn schedule_memory_trim(
    state: &'static std::sync::atomic::AtomicU8,
    pool: Option<&rayon::ThreadPool>,
    trim: fn(),
) {
    use std::sync::atomic::Ordering;

    let Some(pool) = pool else {
        return;
    };
    let mut current = state.fetch_or(TRIM_DIRTY, Ordering::AcqRel) | TRIM_DIRTY;
    loop {
        if current & TRIM_RUNNING != 0 {
            return;
        }
        match state.compare_exchange(
            current,
            current | TRIM_RUNNING,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
    pool.spawn(move || drain_memory_trim_requests(state, trim));
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn memory_trim_pool() -> Option<&'static rayon::ThreadPool> {
    use std::sync::{Mutex, OnceLock};

    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    static INITIALIZING: Mutex<()> = Mutex::new(());
    if let Some(pool) = POOL.get() {
        return Some(pool);
    }

    let _initializing = INITIALIZING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pool) = POOL.get() {
        return Some(pool);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .thread_name(|_| "trouve-memory-trim".into())
        .build()
        .ok()?;
    let _ = POOL.set(pool);
    POOL.get()
}

/// Schedule allocator-page release without adding its potentially expensive
/// glibc arena scan to a foreground search request. Concurrent requests
/// coalesce behind one process-wide trim worker.
pub fn release_unused_memory_in_background() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::sync::atomic::AtomicU8;

        static TRIM_STATE: AtomicU8 = AtomicU8::new(0);
        // Initialize the fallible executor before claiming pending work. A
        // resource-constrained caller can then return without panicking or
        // leaving a permanently running scheduler state; later calls retry.
        schedule_memory_trim(&TRIM_STATE, memory_trim_pool(), release_unused_memory);
    }
}

#[cfg(test)]
mod tests {
    use super::{TRIM_DIRTY, TRIM_RUNNING, drain_memory_trim_requests, schedule_memory_trim};
    use std::sync::atomic::{AtomicU8, Ordering};

    #[test]
    fn background_trim_repeats_when_requested_during_a_scan() {
        let state = AtomicU8::new(TRIM_RUNNING | TRIM_DIRTY);
        let mut trims = 0;

        drain_memory_trim_requests(&state, || {
            trims += 1;
            if trims == 1 {
                state.fetch_or(TRIM_DIRTY, Ordering::Release);
            }
        });

        assert_eq!(trims, 2);
        assert_eq!(state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unavailable_trim_executor_does_not_claim_or_run_work() {
        static STATE: AtomicU8 = AtomicU8::new(0);
        STATE.store(0, Ordering::Release);

        schedule_memory_trim(&STATE, None, || panic!("trim ran without an executor"));

        assert_eq!(STATE.load(Ordering::Acquire), 0);
    }
}
