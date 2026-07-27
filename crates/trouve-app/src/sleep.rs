//! Platform sleep inhibition for active agent runs.
//!
//! The assertion is scoped to an RAII guard: acquiring it on the first active
//! session and dropping it when the last session goes idle also guarantees
//! cleanup if the controller or app exits unexpectedly.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    None,
    Acquire,
    Release,
}

#[derive(Default)]
struct ActivityState {
    active: bool,
}

impl ActivityState {
    fn update(&mut self, active: bool) -> Transition {
        if self.active == active {
            return Transition::None;
        }
        self.active = active;
        if active {
            Transition::Acquire
        } else {
            Transition::Release
        }
    }
}

/// Owns the operating system's sleep-inhibition assertion.
///
/// Failed acquisitions are not retried for every event in the same busy
/// period. A later idle → active transition tries again, which avoids log
/// spam while still recovering from a transient platform failure.
#[derive(Default)]
pub(crate) struct SleepInhibitor {
    activity: ActivityState,
    guard: Option<keepawake::KeepAwake>,
}

impl SleepInhibitor {
    pub(crate) fn set_active(&mut self, active: bool) {
        match self.activity.update(active) {
            Transition::None => {}
            Transition::Acquire => {
                match keepawake::Builder::default()
                    .idle(true)
                    .reason("Trouve agents are running")
                    .app_name("Trouve")
                    .app_reverse_domain("io.github.jimsimon.trouve")
                    .create()
                {
                    Ok(guard) => {
                        self.guard = Some(guard);
                        tracing::debug!("preventing automatic system sleep while agents run");
                    }
                    Err(error) => {
                        tracing::warn!(
                            "could not prevent automatic system sleep while agents run: {error}"
                        );
                    }
                }
            }
            Transition::Release => {
                if self.guard.take().is_some() {
                    tracing::debug!("released automatic system sleep inhibition");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityState, Transition};

    #[test]
    fn acquires_and_releases_only_at_busy_boundaries() {
        let mut state = ActivityState::default();

        assert_eq!(state.update(false), Transition::None);
        assert_eq!(state.update(true), Transition::Acquire);
        assert_eq!(state.update(true), Transition::None);
        assert_eq!(state.update(false), Transition::Release);
        assert_eq!(state.update(false), Transition::None);
        assert_eq!(state.update(true), Transition::Acquire);
    }
}
