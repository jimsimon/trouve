//! Kimi Code subscription-allowance query.
//!
//! Kimi Code's open-source CLI implements `/usage` with a bearer-authenticated
//! `GET {base_url}/usages`. The endpoint is not part of Kimi's documented
//! OpenAI-compatible surface, so callers must restrict it to the canonical
//! Kimi Code base URL and treat failures as unavailable status.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use trouve_protocol::{SubscriptionHealth, SubscriptionWindow};

pub const KIMI_CODE_BASE_URL: &str = "https://api.kimi.com/coding/v1";

pub fn is_kimi_code_base_url(base_url: Option<&str>) -> bool {
    let Some(base_url) = base_url else {
        return false;
    };
    let Ok(candidate) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Ok(canonical) = reqwest::Url::parse(KIMI_CODE_BASE_URL) else {
        return false;
    };
    candidate.scheme() == canonical.scheme()
        && candidate.host_str() == canonical.host_str()
        && candidate.port_or_known_default() == canonical.port_or_known_default()
        && candidate.path().trim_end_matches('/') == canonical.path()
}

pub async fn subscription_health(
    provider_id: &str,
    base_url: &str,
    api_key: &str,
) -> SubscriptionHealth {
    let unavailable = |note: String| SubscriptionHealth {
        provider_id: provider_id.into(),
        status: "unavailable".into(),
        plan: String::new(),
        windows: Vec::new(),
        credits: String::new(),
        note,
    };
    let request = reqwest::Client::new()
        .get(format!("{}/usages", base_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(8));
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return unavailable(format!("could not read Kimi Code usage: {error}")),
    };
    let status = response.status();
    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            return unavailable(format!(
                "Kimi Code usage returned {status} with an unreadable response: {error}"
            ));
        }
    };
    if !status.is_success() {
        let message = payload
            .pointer("/error/message")
            .or_else(|| payload.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("usage request failed");
        return unavailable(format!("Kimi Code usage returned {status}: {message}"));
    }
    parse_usage_health(provider_id, &payload)
}

pub fn parse_usage_health(provider_id: &str, payload: &Value) -> SubscriptionHealth {
    let mut windows = Vec::new();
    if let Some(summary) = payload.get("usage")
        && let Some(window) = parse_usage_row(summary, "Weekly limit")
    {
        windows.push(window);
    }
    if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
        for (index, limit) in limits.iter().enumerate() {
            let detail = limit.get("detail").unwrap_or(limit);
            let label = limit
                .get("name")
                .or_else(|| limit.get("title"))
                .or_else(|| limit.get("scope"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| limit_label(limit, detail, index));
            if let Some(window) = parse_usage_row(detail, &label) {
                windows.push(window);
            }
        }
    }

    let credits = parse_extra_usage(payload).unwrap_or_default();
    if windows.is_empty() && credits.is_empty() {
        SubscriptionHealth {
            provider_id: provider_id.into(),
            status: "unavailable".into(),
            plan: String::new(),
            windows,
            credits,
            note: "the Kimi Code usage endpoint returned no allowance data".into(),
        }
    } else {
        SubscriptionHealth {
            provider_id: provider_id.into(),
            status: "ok".into(),
            plan: String::new(),
            windows,
            credits,
            note: String::new(),
        }
    }
}

fn parse_usage_row(value: &Value, default_label: &str) -> Option<SubscriptionWindow> {
    let limit = int_field(value, "limit")?;
    let used = int_field(value, "used").or_else(|| {
        int_field(value, "remaining").map(|remaining| limit.saturating_sub(remaining))
    })?;
    let label = value
        .get("name")
        .or_else(|| value.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(default_label)
        .to_string();
    let used_percent = if limit <= 0 {
        0
    } else {
        ((used.saturating_mul(100) / limit).clamp(0, 100)) as i64
    };
    Some(SubscriptionWindow {
        label,
        used_percent,
        resets: reset_hint(value),
    })
}

fn limit_label(item: &Value, detail: &Value, index: usize) -> String {
    let window = item.get("window").unwrap_or(&Value::Null);
    let duration = int_field(window, "duration")
        .or_else(|| int_field(item, "duration"))
        .or_else(|| int_field(detail, "duration"));
    let unit = window
        .get("timeUnit")
        .or_else(|| item.get("timeUnit"))
        .or_else(|| detail.get("timeUnit"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match (duration, unit) {
        (Some(duration), unit)
            if unit.contains("MINUTE") && duration >= 60 && duration % 60 == 0 =>
        {
            format!("{}h limit", duration / 60)
        }
        (Some(duration), unit) if unit.contains("MINUTE") => format!("{duration}m limit"),
        (Some(duration), unit) if unit.contains("HOUR") => format!("{duration}h limit"),
        (Some(duration), unit) if unit.contains("DAY") => format!("{duration}d limit"),
        _ => format!("Limit #{}", index + 1),
    }
}

fn reset_hint(value: &Value) -> String {
    for key in ["reset_at", "resetAt", "reset_time", "resetTime"] {
        if let Some(raw) = value.get(key).and_then(Value::as_str) {
            if let Ok(reset) = DateTime::parse_from_rfc3339(raw) {
                return format_reset(reset.with_timezone(&Utc));
            }
            return format!("resets at {raw}");
        }
    }
    for key in ["reset_in", "resetIn", "ttl", "window"] {
        if let Some(seconds) = int_field(value, key).filter(|seconds| *seconds > 0) {
            return format!("resets in {}", format_duration(seconds as u64));
        }
    }
    String::new()
}

fn format_reset(reset: DateTime<Utc>) -> String {
    let seconds = (reset - Utc::now()).num_seconds();
    if seconds <= 0 {
        "reset".into()
    } else {
        format!("resets in {}", format_duration(seconds as u64))
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{}m", minutes.max(1))
    }
}

fn parse_extra_usage(payload: &Value) -> Option<String> {
    let wallet = payload.get("boosterWallet")?;
    let balance = wallet.get("balance")?;
    if balance.get("type").and_then(Value::as_str) != Some("BOOSTER") {
        return None;
    }
    let fixed = int_field(balance, "amountLeft")?;
    let cents = if fixed > 0 && fixed < 1_000_000 {
        1
    } else {
        fixed / 1_000_000
    };
    let currency = wallet
        .pointer("/monthlyChargeLimit/currency")
        .or_else(|| wallet.pointer("/monthlyUsed/currency"))
        .and_then(Value::as_str)
        .unwrap_or("USD");
    Some(format!(
        "extra usage: {} {}.{:02}",
        currency,
        cents / 100,
        cents.unsigned_abs() % 100
    ))
}

fn int_field(value: &Value, key: &str) -> Option<i128> {
    let value = value.get(key)?;
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str()?.parse().ok())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_endpoint_check_is_strict() {
        assert!(is_kimi_code_base_url(Some(KIMI_CODE_BASE_URL)));
        assert!(is_kimi_code_base_url(Some(
            "https://api.kimi.com/coding/v1/"
        )));
        assert!(!is_kimi_code_base_url(Some(
            "https://api.kimi.com.evil.example/coding/v1"
        )));
        assert!(!is_kimi_code_base_url(Some(
            "https://api.kimi.com/coding/v2"
        )));
    }

    #[test]
    fn parses_summary_windows_and_extra_usage() {
        let health = parse_usage_health(
            "kimi-code",
            &json!({
                "usage": {"name": "Weekly limit", "remaining": 750, "limit": 1000, "resetIn": 3600},
                "limits": [{
                    "detail": {"used": 25, "limit": 100},
                    "window": {"duration": 300, "timeUnit": "MINUTE"}
                }],
                "boosterWallet": {
                    "balance": {"type": "BOOSTER", "amountLeft": "1234000000"},
                    "monthlyUsed": {"currency": "USD"}
                }
            }),
        );
        assert_eq!(health.status, "ok");
        assert_eq!(health.windows.len(), 2);
        assert_eq!(health.windows[0].label, "Weekly limit");
        assert_eq!(health.windows[0].used_percent, 25);
        assert_eq!(health.windows[1].label, "5h limit");
        assert_eq!(health.windows[1].used_percent, 25);
        assert_eq!(health.credits, "extra usage: USD 12.34");
    }

    #[test]
    fn empty_payload_is_unavailable() {
        let health = parse_usage_health("kimi-code", &json!({}));
        assert_eq!(health.status, "unavailable");
        assert!(health.windows.is_empty());
    }
}
