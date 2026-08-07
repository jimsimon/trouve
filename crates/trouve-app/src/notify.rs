//! Desktop notifications for agent activity the user would otherwise miss:
//! a turn finishing, failing, or blocking on approval/questions while the
//! window is unfocused or the thread isn't the one on screen.
//!
//! Click tracking uses the bounded asynchronous dispatcher in
//! [`crate::native_notification`], avoiding one indefinitely blocked D-Bus
//! thread per notification. Preferences live in
//! [`crate::winstate::Notifications`].

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
    crate::native_notification::show(toast.summary, toast.body, toast.sound, move || {
        let _ = tx.send(UiCommand::NotificationActivated {
            session_id: toast.session_id,
            thread_id: toast.thread_id,
        });
    });
}
