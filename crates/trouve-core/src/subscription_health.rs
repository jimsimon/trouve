//! Per-provider cache and backoff for subscription-usage probes.
//!
//! Every probe is expensive on the vendor side: Claude Code spawns a CLI
//! process that calls Anthropic's `/api/oauth/usage`, Codex round-trips its
//! app-server, Cursor exchanges a key for a token. Nothing bounded how often
//! that happened — each open trouve client refreshed on its own 30 s TTL and
//! the engine forwarded every request — and Anthropic's usage endpoint
//! answers 429 once enough pollers pile up, at which point the panel showed
//! nothing at all. This cache makes the engine the single rate limiter and
//! keeps the last good reading on screen through a transient failure.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use trouve_protocol::SubscriptionHealth;

/// How long a successful probe is served without re-querying the vendor.
pub const FRESH_TTL: Duration = Duration::from_secs(60);
/// First pause after a failed probe; doubles per consecutive failure.
pub const FAILURE_BACKOFF_MIN: Duration = Duration::from_secs(60);
/// Cap on the failure backoff so a recovered endpoint is noticed promptly.
pub const FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(10 * 60);
/// How long a stale good reading keeps standing in for a failed probe before
/// the failure itself is shown. Reset labels are pre-rendered ("resets in
/// 2h 10m"), so showing very old windows would mislead.
pub const LAST_GOOD_MAX_AGE: Duration = Duration::from_secs(15 * 60);

struct Entry {
    /// What the last probe returned, as shown to clients.
    displayed: SubscriptionHealth,
    /// Earliest instant a new probe may run for this provider.
    next_probe_at: Instant,
    /// Consecutive failed probes, driving the backoff.
    failures: u32,
    /// Most recent `status == "ok"` reading and when it was taken.
    last_good: Option<(SubscriptionHealth, Instant)>,
}

/// One entry per provider id. Providers that disappear from the registry
/// are dropped by [`SubscriptionHealthCache::retain`].
#[derive(Default)]
pub struct SubscriptionHealthCache {
    entries: HashMap<String, Entry>,
}

impl SubscriptionHealthCache {
    /// The cached reading for `provider_id` if it is still inside its fresh
    /// window or failure backoff; `None` means a probe should run now.
    pub fn lookup(&self, provider_id: &str, now: Instant) -> Option<SubscriptionHealth> {
        let entry = self.entries.get(provider_id)?;
        (now < entry.next_probe_at).then(|| entry.displayed.clone())
    }

    /// Record a fresh probe result and return what clients should see. A
    /// failed probe with a recent good reading in hand shows that reading
    /// with the failure explained in its note, so the meters do not blink
    /// out every time the vendor throttles the usage endpoint.
    pub fn record(
        &mut self,
        provider_id: &str,
        health: SubscriptionHealth,
        now: Instant,
    ) -> SubscriptionHealth {
        let previous = self.entries.remove(provider_id);
        let entry = if health.status == "ok" {
            Entry {
                displayed: health.clone(),
                next_probe_at: now + FRESH_TTL,
                failures: 0,
                last_good: Some((health, now)),
            }
        } else {
            let failures = previous.as_ref().map_or(0, |e| e.failures) + 1;
            let last_good = previous.and_then(|e| e.last_good);
            let displayed = match &last_good {
                Some((good, taken_at)) if now.duration_since(*taken_at) <= LAST_GOOD_MAX_AGE => {
                    SubscriptionHealth {
                        note: stale_note(&health.note, now.duration_since(*taken_at)),
                        ..good.clone()
                    }
                }
                _ => health,
            };
            Entry {
                displayed,
                next_probe_at: now + failure_backoff(failures),
                failures,
                last_good,
            }
        };
        let displayed = entry.displayed.clone();
        self.entries.insert(provider_id.to_string(), entry);
        displayed
    }

    /// Drop entries for providers no longer registered.
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.entries.retain(|id, _| keep(id));
    }
}

fn failure_backoff(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(8);
    (FAILURE_BACKOFF_MIN * factor).min(FAILURE_BACKOFF_MAX)
}

fn stale_note(failure_note: &str, age: Duration) -> String {
    let age = match age.as_secs() {
        s if s < 90 => "a minute ago".to_string(),
        s => format!("{} minutes ago", (s + 30) / 60),
    };
    let reason = failure_note.trim_end_matches('.');
    if reason.is_empty() {
        format!("Showing usage from {age}; the latest refresh failed.")
    } else {
        format!("Showing usage from {age}: {reason}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(percent: i64) -> SubscriptionHealth {
        SubscriptionHealth {
            provider_id: "claude-code".into(),
            status: "ok".into(),
            plan: "max".into(),
            windows: vec![trouve_protocol::SubscriptionWindow {
                label: "5h window".into(),
                used_percent: percent,
                resets: "resets in 2h".into(),
            }],
            credits: String::new(),
            note: String::new(),
        }
    }

    fn throttled() -> SubscriptionHealth {
        SubscriptionHealth {
            provider_id: "claude-code".into(),
            status: "unavailable".into(),
            plan: "max".into(),
            windows: Vec::new(),
            credits: String::new(),
            note: "Anthropic's usage endpoint is temporarily rate-limiting requests".into(),
        }
    }

    #[test]
    fn fresh_result_is_served_until_ttl_expires() {
        let mut cache = SubscriptionHealthCache::default();
        let t0 = Instant::now();
        cache.record("claude-code", ok(10), t0);
        assert_eq!(
            cache
                .lookup("claude-code", t0 + Duration::from_secs(59))
                .unwrap()
                .status,
            "ok"
        );
        assert!(cache.lookup("claude-code", t0 + FRESH_TTL).is_none());
        assert!(
            cache.lookup("codex", t0).is_none(),
            "unknown providers always probe"
        );
    }

    #[test]
    fn failure_keeps_last_good_reading_with_explanation() {
        let mut cache = SubscriptionHealthCache::default();
        let t0 = Instant::now();
        cache.record("claude-code", ok(10), t0);
        let shown = cache.record("claude-code", throttled(), t0 + Duration::from_secs(120));
        assert_eq!(shown.status, "ok");
        assert_eq!(shown.windows[0].used_percent, 10);
        assert_eq!(
            shown.note,
            "Showing usage from 2 minutes ago: Anthropic's usage endpoint is temporarily rate-limiting requests."
        );
        // The stale reading stays visible through the backoff window.
        let cached = cache
            .lookup("claude-code", t0 + Duration::from_secs(150))
            .unwrap();
        assert_eq!(cached.status, "ok");
    }

    #[test]
    fn failure_without_history_is_shown_as_is() {
        let mut cache = SubscriptionHealthCache::default();
        let t0 = Instant::now();
        let shown = cache.record("claude-code", throttled(), t0);
        assert_eq!(shown.status, "unavailable");
        assert!(shown.note.contains("rate-limiting"));
    }

    #[test]
    fn failure_backoff_doubles_and_caps() {
        let mut cache = SubscriptionHealthCache::default();
        let mut now = Instant::now();
        for secs in [60u64, 120, 240, 480, 600, 600] {
            cache.record("claude-code", throttled(), now);
            let pause = Duration::from_secs(secs);
            assert!(
                cache
                    .lookup("claude-code", now + pause - Duration::from_secs(1))
                    .is_some()
            );
            assert!(cache.lookup("claude-code", now + pause).is_none());
            now += pause;
        }
        // Recovery resets the backoff.
        cache.record("claude-code", ok(3), now);
        cache.record("claude-code", throttled(), now + FRESH_TTL);
        assert!(
            cache
                .lookup("claude-code", now + FRESH_TTL + Duration::from_secs(59))
                .is_some()
        );
        assert!(
            cache
                .lookup("claude-code", now + FRESH_TTL + Duration::from_secs(60))
                .is_none()
        );
    }

    #[test]
    fn stale_reading_expires_after_max_age() {
        let mut cache = SubscriptionHealthCache::default();
        let t0 = Instant::now();
        cache.record("claude-code", ok(10), t0);
        let later = t0 + LAST_GOOD_MAX_AGE + Duration::from_secs(1);
        let shown = cache.record("claude-code", throttled(), later);
        assert_eq!(shown.status, "unavailable");
        assert!(shown.windows.is_empty());
    }

    #[test]
    fn retain_drops_unregistered_providers() {
        let mut cache = SubscriptionHealthCache::default();
        let t0 = Instant::now();
        cache.record("claude-code", ok(10), t0);
        cache.record("codex", ok(20), t0);
        cache.retain(|id| id == "codex");
        assert!(cache.lookup("claude-code", t0).is_none());
        assert!(cache.lookup("codex", t0).is_some());
    }
}
