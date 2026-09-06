//! Ownership for explicitly managed background work.
//!
//! A managed task is different from a descendant that happens to daemonize:
//! its caller deliberately transfers the work to this registry, supplies a
//! cancellation path, and keeps any resource lease inside the registered
//! future. Equal keys coalesce to one running task plus at most one rerun.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio_util::sync::CancellationToken;

type ManagedFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type ManagedWork = dyn Fn(CancellationToken) -> ManagedFuture + Send + Sync + 'static;

#[derive(Clone, Default)]
pub(crate) struct ManagedBackgroundTasks {
    inner: Arc<ManagedBackgroundInner>,
}

#[derive(Default)]
struct ManagedBackgroundInner {
    tasks: Mutex<HashMap<String, ManagedTaskState>>,
    shutdown: CancellationToken,
}

struct ManagedTaskState {
    id: u64,
    rerun_requested: bool,
    cancel: CancellationToken,
}

static MANAGED_TASK_SEQUENCE: AtomicU64 = AtomicU64::new(1);

impl Drop for ManagedBackgroundInner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl ManagedBackgroundTasks {
    /// Start one managed task, or request one rerun when the same key is
    /// already active. Returns whether this call started a new owner.
    pub(crate) fn schedule<F, Fut>(&self, key: String, work: F) -> bool
    where
        F: Fn(CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut tasks = self.inner.tasks.lock().unwrap();
        if let Some(task) = tasks.get_mut(&key) {
            task.rerun_requested = true;
            return false;
        }

        let cancel = self.inner.shutdown.child_token();
        let id = MANAGED_TASK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        tasks.insert(
            key.clone(),
            ManagedTaskState {
                id,
                rerun_requested: false,
                cancel: cancel.clone(),
            },
        );
        drop(tasks);

        let work: Arc<ManagedWork> = Arc::new(move |cancel| Box::pin(work(cancel)));
        let owner = Arc::downgrade(&self.inner);
        tokio::spawn(run_managed_task(key, id, owner, cancel, work));
        true
    }

    #[cfg(test)]
    pub(crate) fn is_running(&self, key: &str) -> bool {
        self.inner.tasks.lock().unwrap().contains_key(key)
    }
}

struct TaskRegistration {
    key: String,
    id: u64,
    owner: Weak<ManagedBackgroundInner>,
}

impl Drop for TaskRegistration {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut tasks = owner.tasks.lock().unwrap();
            if tasks.get(&self.key).is_some_and(|task| task.id == self.id) {
                tasks.remove(&self.key);
            }
        }
    }
}

async fn run_managed_task(
    key: String,
    id: u64,
    owner: Weak<ManagedBackgroundInner>,
    cancel: CancellationToken,
    work: Arc<ManagedWork>,
) {
    let registration = TaskRegistration {
        key: key.clone(),
        id,
        owner: owner.clone(),
    };
    loop {
        work(cancel.clone()).await;

        let Some(owner) = owner.upgrade() else {
            break;
        };
        let mut tasks = owner.tasks.lock().unwrap();
        let rerun = tasks.get(&key).is_some_and(|task| {
            task.id == id && task.rerun_requested && !task.cancel.is_cancelled()
        });
        if rerun {
            tasks
                .get_mut(&key)
                .expect("this managed task remains registered")
                .rerun_requested = false;
        } else if tasks.get(&key).is_some_and(|task| task.id == id) {
            tasks.remove(&key);
        }
        drop(tasks);
        drop(owner);
        if !rerun {
            break;
        }
    }
    drop(registration);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn duplicate_schedules_coalesce_to_one_rerun() {
        let tasks = ManagedBackgroundTasks::default();
        let starts = Arc::new(AtomicUsize::new(0));
        let permits = Arc::new(tokio::sync::Semaphore::new(0));
        let work = {
            let starts = starts.clone();
            let permits = permits.clone();
            move |cancel: CancellationToken| {
                let starts = starts.clone();
                let permits = permits.clone();
                async move {
                    starts.fetch_add(1, Ordering::SeqCst);
                    tokio::select! {
                        _ = cancel.cancelled() => {}
                        permit = permits.acquire() => drop(permit),
                    }
                }
            }
        };

        assert!(tasks.schedule("repository".into(), work.clone()));
        wait_for_count(&starts, 1).await;
        assert!(!tasks.schedule("repository".into(), work.clone()));
        assert!(!tasks.schedule("repository".into(), work));

        permits.add_permits(1);
        wait_for_count(&starts, 2).await;
        permits.add_permits(1);
        wait_until(|| !tasks.is_running("repository")).await;
        assert_eq!(starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dropping_the_owner_cancels_managed_work() {
        let tasks = ManagedBackgroundTasks::default();
        let cancelled = Arc::new(AtomicUsize::new(0));
        let observed = cancelled.clone();
        assert!(tasks.schedule("repository".into(), move |cancel| {
            let observed = observed.clone();
            async move {
                cancel.cancelled().await;
                observed.fetch_add(1, Ordering::SeqCst);
            }
        }));
        assert!(tasks.is_running("repository"));

        drop(tasks);
        wait_for_count(&cancelled, 1).await;
    }

    async fn wait_for_count(value: &AtomicUsize, expected: usize) {
        wait_until(|| value.load(Ordering::SeqCst) == expected).await;
    }

    async fn wait_until(mut predicate: impl FnMut() -> bool) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
