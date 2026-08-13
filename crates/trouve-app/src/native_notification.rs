//! Bounded native-notification delivery for the Wry desktop host.
//!
//! `notify-rust` keeps a D-Bus connection alive while waiting for an action.
//! Its synchronous Linux waiter also enters `zbus::block_on`, which lazily
//! creates a second per-core Tokio runtime. Linux listeners therefore stay on
//! the app runtime and expire after five minutes. Windows and macOS expose only
//! synchronous response waiters; those workers can live until the OS responds,
//! so the shared four-listener budget bounds their thread count.

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

fn action_listener_budget() -> &'static ActionListenerBudget {
    static BUDGET: OnceLock<ActionListenerBudget> = OnceLock::new();
    BUDGET.get_or_init(|| ActionListenerBudget::new(MAX_PENDING_ACTION_LISTENERS))
}

fn escape_freedesktop_markup(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn dispatch_activation_response(
    response: &notify_rust::NotificationResponse,
    on_activate: impl FnOnce(),
) {
    if matches!(response, notify_rust::NotificationResponse::Default) {
        on_activate();
    }
}

fn request(
    summary: &str,
    body: &str,
    sound: bool,
    track_activation: bool,
) -> notify_rust::Notification {
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let _ = track_activation;
    let mut request = notify_rust::Notification::new();
    request.appname("Trouve").summary(summary);
    #[cfg(all(unix, not(target_os = "macos")))]
    request.body(&escape_freedesktop_markup(body));
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    request.body(body);
    request.icon("trouve");
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

/// Display a native notification without allowing click tracking to retain
/// unbounded platform resources. Linux click tracking expires after five
/// minutes. Windows and macOS tracking lasts until the OS responds, with at
/// most four response-waiter threads alive across the process.
pub(crate) fn show(
    summary: String,
    body: String,
    sound: bool,
    on_activate: impl FnOnce() + Send + 'static,
) {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Notification callbacks normally run on the controller/gateway
            // runtime. Preserve delivery if a future caller does not, but do
            // not fall back to an unbounded blocking action waiter.
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

        let slot = action_listener_budget().try_acquire();
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
                    dispatch_activation_response(response, on_activate);
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
        let slot = action_listener_budget().try_acquire();
        let _ = std::thread::Builder::new()
            .name("trouve-notify".into())
            .spawn(move || {
                let handle = match request(&summary, &body, sound, slot.is_some()).show() {
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
                if let Err(error) = handle.wait_for_response(
                    move |response: &notify_rust::NotificationResponse| {
                        dispatch_activation_response(response, on_activate);
                    },
                ) {
                    tracing::debug!(%error, "waiting for native notification response failed");
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
        let first = budget.try_acquire().expect("first listener has a slot");
        let second = budget.try_acquire().expect("second listener has a slot");
        assert!(budget.try_acquire().is_none());

        drop(first);
        let replacement = budget
            .try_acquire()
            .expect("dropping a listener releases its slot");
        assert!(budget.try_acquire().is_none());

        drop((second, replacement));
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_body_cannot_inject_freedesktop_markup() {
        assert_eq!(
            escape_freedesktop_markup("<b>model</b> & user"),
            "&lt;b&gt;model&lt;/b&gt; &amp; user"
        );
    }

    #[test]
    fn only_default_notification_response_dispatches_activation() {
        let activations = Arc::new(AtomicUsize::new(0));
        let clicked = activations.clone();
        dispatch_activation_response(&notify_rust::NotificationResponse::Default, move || {
            clicked.fetch_add(1, Ordering::Release);
        });
        let action = activations.clone();
        dispatch_activation_response(
            &notify_rust::NotificationResponse::Action("secondary".into()),
            move || {
                action.fetch_add(1, Ordering::Release);
            },
        );
        assert_eq!(activations.load(Ordering::Acquire), 1);
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
