//! Desktop notifications for agent activity the user would otherwise miss:
//! a turn finishing, failing, or blocking on approval/questions while the
//! window is unfocused or the thread isn't the one on screen.
//!
//! Each notification runs on its own detached thread because the Linux
//! (D-Bus) backend blocks while waiting for the click that jumps back to
//! the session. Preferences live in [`crate::winstate::Notifications`].

use crate::controller::UiCommand;

/// One notification to pop, plus where a click should land.
pub struct Toast {
    pub summary: String,
    pub body: String,
    pub sound: bool,
    /// Session/thread a click reveals (Linux only; other platforms show
    /// the notification without an action).
    pub session_id: String,
    pub thread_id: String,
}

/// Show `toast` without blocking the caller. On Linux, clicking it sends
/// [`UiCommand::NotificationActivated`] so the controller can raise the
/// window and open the thread.
pub fn show(toast: Toast, tx: tokio::sync::mpsc::UnboundedSender<UiCommand>) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Trouve")
            .summary(&toast.summary)
            .body(&toast.body)
            .icon("trouve");
        if toast.sound {
            // Freedesktop sound-naming name on Linux; macOS/Windows take
            // their platform defaults.
            #[cfg(all(unix, not(target_os = "macos")))]
            n.sound_name("message-new-instant");
            #[cfg(target_os = "macos")]
            n.sound_name("Ping");
        }

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Lets KDE/GNOME group the toast under the app and reuse its
            // desktop-entry icon.
            n.hint(notify_rust::Hint::DesktopEntry("trouve".into()));
            n.action("default", "Open");
            match n.show() {
                Ok(handle) => handle.wait_for_action(|action| {
                    if action == "default" {
                        let _ = tx.send(UiCommand::NotificationActivated {
                            session_id: toast.session_id,
                            thread_id: toast.thread_id,
                        });
                    }
                }),
                Err(e) => tracing::debug!("notification failed: {e}"),
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            let _ = tx; // click-through is Linux-only
            if let Err(e) = n.show() {
                tracing::debug!("notification failed: {e}");
            }
        }
    });
}
