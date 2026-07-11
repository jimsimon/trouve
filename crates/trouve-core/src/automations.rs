//! Automations: prompts that fire on a schedule.
//!
//! The schedule model is deliberately small — "hourly at minute M",
//! "daily at HH:MM", or "weekly on these days at HH:MM" — which covers the
//! common presets (hourly/daily/weekly) and exact day-of-week + time-of-day
//! combinations without dragging in cron syntax. Times are the server's
//! local time zone (the machine the user is looking at).
//!
//! The scheduler itself lives in the engine ([`crate::engine::Engine`]
//! spawns it when serving); this module owns the pure parts: validation
//! and next-occurrence math, which is where the edge cases (DST, month
//! boundaries) live and what the tests pin down.

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike};
use trouve_protocol::AutomationSchedule;

/// Check a schedule coming in from a client. Returns a human-readable
/// complaint, or None when it is usable.
pub fn validate(schedule: &AutomationSchedule) -> Option<String> {
    match schedule.kind.as_str() {
        "hourly" => {
            if schedule.minute > 59 {
                return Some("hourly minute must be 0-59".into());
            }
        }
        "daily" => {
            if parse_time(&schedule.time).is_none() {
                return Some("daily schedules need a time like \"09:30\"".into());
            }
        }
        "weekly" => {
            if parse_time(&schedule.time).is_none() {
                return Some("weekly schedules need a time like \"09:30\"".into());
            }
            if schedule.days.is_empty() {
                return Some("weekly schedules need at least one day".into());
            }
            if schedule.days.iter().any(|d| *d > 6) {
                return Some("days are 0 (Monday) through 6 (Sunday)".into());
            }
        }
        other => return Some(format!("unknown schedule kind \"{other}\"")),
    }
    None
}

/// "HH:MM" (24h) → NaiveTime.
fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// The first fire time strictly after `after`, in the local time zone.
/// None only for invalid schedules (callers validate first).
pub fn next_run(schedule: &AutomationSchedule, after: DateTime<Local>) -> Option<DateTime<Local>> {
    match schedule.kind.as_str() {
        "hourly" => {
            let minute = u32::from(schedule.minute.min(59));
            let base = after
                .with_minute(minute)?
                .with_second(0)?
                .with_nanosecond(0)?;
            Some(if base > after {
                base
            } else {
                base + Duration::hours(1)
            })
        }
        "daily" => {
            let time = parse_time(&schedule.time)?;
            for offset in 0..3 {
                let day = after.date_naive() + Duration::days(offset);
                if let Some(at) = local_at(day, time) {
                    if at > after {
                        return Some(at);
                    }
                }
            }
            None
        }
        "weekly" => {
            let time = parse_time(&schedule.time)?;
            // Walk forward up to two weeks (one is enough except when the
            // only slot this week lands inside a DST spring-forward gap).
            for offset in 0..15 {
                let day = after.date_naive() + Duration::days(offset);
                let weekday = day.weekday().num_days_from_monday() as u8;
                if !schedule.days.contains(&weekday) {
                    continue;
                }
                if let Some(at) = local_at(day, time) {
                    if at > after {
                        return Some(at);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// A local wall-clock instant, skipping times that don't exist (DST
/// spring-forward) and taking the earlier reading of ambiguous ones
/// (fall-back).
fn local_at(day: chrono::NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&day.and_time(time)).earliest()
}

/// Human summary for lists: "Hourly at :15", "Daily at 09:00",
/// "Mon, Wed, Fri at 09:00".
pub fn summary(schedule: &AutomationSchedule) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    match schedule.kind.as_str() {
        "hourly" => format!("Hourly at :{:02}", schedule.minute),
        "daily" => format!("Daily at {}", schedule.time),
        "weekly" => {
            let mut days: Vec<u8> = schedule.days.clone();
            days.sort_unstable();
            days.dedup();
            if days.len() == 7 {
                return format!("Daily at {}", schedule.time);
            }
            let names: Vec<&str> = days
                .iter()
                .filter_map(|d| DAYS.get(*d as usize).copied())
                .collect();
            format!("{} at {}", names.join(", "), schedule.time)
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hourly(minute: u8) -> AutomationSchedule {
        AutomationSchedule {
            kind: "hourly".into(),
            minute,
            time: String::new(),
            days: vec![],
        }
    }

    fn daily(time: &str) -> AutomationSchedule {
        AutomationSchedule {
            kind: "daily".into(),
            minute: 0,
            time: time.into(),
            days: vec![],
        }
    }

    fn weekly(time: &str, days: &[u8]) -> AutomationSchedule {
        AutomationSchedule {
            kind: "weekly".into(),
            minute: 0,
            time: time.into(),
            days: days.to_vec(),
        }
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, 30)
            .single()
            .unwrap()
    }

    #[test]
    fn validation_catches_the_obvious() {
        assert!(validate(&hourly(15)).is_none());
        assert!(validate(&hourly(75)).is_some());
        assert!(validate(&daily("09:30")).is_none());
        assert!(validate(&daily("9am")).is_some());
        assert!(validate(&weekly("09:30", &[0, 4])).is_none());
        assert!(validate(&weekly("09:30", &[])).is_some());
        assert!(validate(&weekly("09:30", &[7])).is_some());
        assert!(validate(&AutomationSchedule {
            kind: "fortnightly".into(),
            minute: 0,
            time: String::new(),
            days: vec![],
        })
        .is_some());
    }

    #[test]
    fn hourly_rolls_to_the_next_hour() {
        // 10:05:30, fire at :15 → 10:15 today.
        let next = next_run(&hourly(15), at(2026, 7, 6, 10, 5)).unwrap();
        assert_eq!((next.hour(), next.minute(), next.second()), (10, 15, 0));
        // 10:20:30, fire at :15 → 11:15.
        let next = next_run(&hourly(15), at(2026, 7, 6, 10, 20)).unwrap();
        assert_eq!((next.hour(), next.minute()), (11, 15));
        // Exactly 10:15:30 → strictly after, so 11:15.
        let next = next_run(&hourly(15), at(2026, 7, 6, 10, 15)).unwrap();
        assert_eq!(next.hour(), 11);
    }

    #[test]
    fn daily_rolls_to_tomorrow() {
        let next = next_run(&daily("09:00"), at(2026, 7, 6, 8, 0)).unwrap();
        assert_eq!((next.day(), next.hour(), next.minute()), (6, 9, 0));
        let next = next_run(&daily("09:00"), at(2026, 7, 6, 12, 0)).unwrap();
        assert_eq!((next.day(), next.hour()), (7, 9));
    }

    #[test]
    fn weekly_picks_the_nearest_listed_day() {
        // 2026-07-06 is a Monday. Mon+Fri at 09:00, asked Monday noon →
        // Friday (day 10).
        let next = next_run(&weekly("09:00", &[0, 4]), at(2026, 7, 6, 12, 0)).unwrap();
        assert_eq!((next.day(), next.hour()), (10, 9));
        // Asked Monday 08:00 → same day 09:00.
        let next = next_run(&weekly("09:00", &[0, 4]), at(2026, 7, 6, 8, 0)).unwrap();
        assert_eq!((next.day(), next.hour()), (6, 9));
        // Sunday-only, asked Monday → next Sunday (day 12).
        let next = next_run(&weekly("21:30", &[6]), at(2026, 7, 6, 8, 0)).unwrap();
        assert_eq!((next.day(), next.hour(), next.minute()), (12, 21, 30));
    }

    #[test]
    fn summaries_read_naturally() {
        assert_eq!(summary(&hourly(5)), "Hourly at :05");
        assert_eq!(summary(&daily("09:00")), "Daily at 09:00");
        assert_eq!(
            summary(&weekly("09:00", &[0, 2, 4])),
            "Mon, Wed, Fri at 09:00"
        );
        assert_eq!(
            summary(&weekly("08:15", &[0, 1, 2, 3, 4, 5, 6])),
            "Daily at 08:15"
        );
    }
}
