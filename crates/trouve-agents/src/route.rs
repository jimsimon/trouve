//! Bounded, nonblocking routes for multiplexed vendor transports.
//!
//! Codex and Cursor each have one stdout reader serving multiple turns. That
//! reader cannot await a slow consumer without blocking unrelated turns, but
//! an unbounded channel lets a stalled turn consume memory indefinitely.
//! These routes cap unread events and signal overload separately from the
//! event channel so the reader stays nonblocking and only the affected turn
//! fails.

use tokio::sync::{mpsc, watch};

pub(crate) const ROUTE_EVENT_BUDGET: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteSendError {
    Closed,
    Overloaded,
}

pub(crate) struct RouteSender<T> {
    events: mpsc::Sender<T>,
    overloaded: watch::Sender<bool>,
}

pub(crate) struct RouteReceiver<T> {
    events: mpsc::Receiver<T>,
    overloaded: watch::Receiver<bool>,
}

pub(crate) struct RouteOverload {
    overloaded: watch::Receiver<bool>,
}

pub(crate) fn route_channel<T>() -> (RouteSender<T>, RouteReceiver<T>) {
    let (events_tx, events_rx) = mpsc::channel(ROUTE_EVENT_BUDGET);
    let (overloaded_tx, overloaded_rx) = watch::channel(false);
    (
        RouteSender {
            events: events_tx,
            overloaded: overloaded_tx,
        },
        RouteReceiver {
            events: events_rx,
            overloaded: overloaded_rx,
        },
    )
}

impl<T> Clone for RouteSender<T> {
    fn clone(&self) -> Self {
        Self {
            events: self.events.clone(),
            overloaded: self.overloaded.clone(),
        }
    }
}

impl<T> RouteSender<T> {
    pub(crate) fn try_send(&self, event: T) -> Result<(), RouteSendError> {
        if *self.overloaded.borrow() {
            return Err(RouteSendError::Overloaded);
        }
        match self.events.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(RouteSendError::Closed),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.mark_overloaded();
                Err(RouteSendError::Overloaded)
            }
        }
    }

    pub(crate) fn mark_overloaded(&self) {
        self.overloaded.send_replace(true);
    }

    pub(crate) fn same_channel(&self, other: &Self) -> bool {
        self.events.same_channel(&other.events)
    }
}

impl<T> RouteReceiver<T> {
    pub(crate) fn overload_signal(&self) -> RouteOverload {
        RouteOverload {
            overloaded: self.overloaded.clone(),
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<T> {
        self.events.recv().await
    }

    pub(crate) fn try_recv(&mut self) -> Result<T, mpsc::error::TryRecvError> {
        self.events.try_recv()
    }
}

impl RouteOverload {
    pub(crate) async fn wait(&mut self) {
        loop {
            if *self.overloaded.borrow() {
                return;
            }
            if self.overloaded.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_route_reports_overload_without_waiting_or_draining() {
        let (tx, mut rx) = route_channel();
        let mut overloaded = rx.overload_signal();
        for event in 0..ROUTE_EVENT_BUDGET {
            assert_eq!(tx.try_send(event), Ok(()));
        }
        assert_eq!(
            tx.try_send(ROUTE_EVENT_BUDGET),
            Err(RouteSendError::Overloaded)
        );
        overloaded.wait().await;
        assert_eq!(rx.recv().await, Some(0));
    }
}
