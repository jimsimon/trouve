//! Bounded native notification delivery for the isolated Servo preview. The
//! synchronous `notify-rust` action waiter creates a second per-core Tokio
//! runtime through zbus and can retain one thread/connection per notification,
//! so this copy follows the app host's bounded asynchronous design.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

const MAX_PENDING_ACTION_LISTENERS: usize = 4;
const ACTION_LISTENER_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
struct ActionListenerBudget {
    active: Arc<AtomicUsize>,
    maximum: usize,
}

impl ActionListenerBudget {
    fn new(maximum: usize) -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            maximum,
        }
    }

    fn try_acquire(&self) -> Option<ActionListenerSlot> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.maximum).then_some(active + 1)
            })
            .ok()
            .map(|_| ActionListenerSlot {
                active: self.active.clone(),
            })
    }
}

struct ActionListenerSlot {
    active: Arc<AtomicUsize>,
}

impl Drop for ActionListenerSlot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn budget() -> &'static ActionListenerBudget {
    static BUDGET: OnceLock<ActionListenerBudget> = OnceLock::new();
    BUDGET.get_or_init(|| ActionListenerBudget::new(MAX_PENDING_ACTION_LISTENERS))
}

fn request(
    summary: &str,
    body: &str,
    sound: bool,
    track_activation: bool,
) -> notify_rust::Notification {
    let mut request = notify_rust::Notification::new();
    request
        .appname("Trouve")
        .summary(summary)
        .body(body)
        .icon("trouve");
    if sound {
        #[cfg(all(unix, not(target_os = "macos")))]
        request.sound_name("message-new-instant");
        #[cfg(target_os = "macos")]
        request.sound_name("Ping");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        request.hint(notify_rust::Hint::DesktopEntry("trouve".into()));
        if track_activation {
            request.action("default", "Open");
        }
    }
    request
}

pub(crate) fn show(
    summary: String,
    body: String,
    sound: bool,
    on_activate: impl FnOnce() + Send + 'static,
) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            let _ = on_activate;
            let _ = std::thread::Builder::new()
                .name("trouve-notify".into())
                .spawn(move || {
                    if let Err(error) = request(&summary, &body, sound, false).show() {
                        tracing::debug!(%error, "notification failed");
                    }
                });
            return;
        };
        let slot = budget().try_acquire();
        runtime.spawn(async move {
            let handle = match request(&summary, &body, sound, slot.is_some())
                .show_async()
                .await
            {
                Ok(handle) => handle,
                Err(error) => {
                    tracing::debug!(%error, "notification failed");
                    return;
                }
            };
            let Some(_slot) = slot else {
                tracing::debug!(
                    maximum = MAX_PENDING_ACTION_LISTENERS,
                    "notification shown without click tracking because the bounded action-listener budget is full"
                );
                return;
            };
            let wait = handle.wait_for_action_async(
                move |response: &notify_rust::NotificationResponse| {
                    if matches!(response, notify_rust::NotificationResponse::Default) {
                        on_activate();
                    }
                },
            );
            if tokio::time::timeout(ACTION_LISTENER_LIFETIME, wait)
                .await
                .is_err()
            {
                tracing::debug!("native notification click-tracking window expired");
            }
        });
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = on_activate;
        let _ = std::thread::Builder::new()
            .name("trouve-notify".into())
            .spawn(move || {
                if let Err(error) = request(&summary, &body, sound, false).show() {
                    tracing::debug!(%error, "notification failed");
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_listener_budget_is_bounded_and_reusable() {
        let budget = ActionListenerBudget::new(2);
        let first = budget.try_acquire().unwrap();
        let second = budget.try_acquire().unwrap();
        assert!(budget.try_acquire().is_none());
        drop(first);
        let replacement = budget.try_acquire().unwrap();
        assert!(budget.try_acquire().is_none());
        drop((second, replacement));
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn untracked_notifications_do_not_offer_a_dead_open_action() {
        assert!(request("summary", "body", false, false).actions.is_empty());
        assert_eq!(
            request("summary", "body", false, true).actions,
            ["default", "Open"]
        );
    }
}
