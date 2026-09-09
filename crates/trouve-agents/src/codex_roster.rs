//! Rebuild the trouve-owned `openai-codex` roster from the Codex app-server's
//! `model/list` response.
//!
//! The live list is an availability signal for the signed-in account: it
//! says which model slugs Codex will accept today and which reasoning
//! efforts each supports, but carries no context limits or pricing. Those
//! still come from the catalog, so each live model becomes an overlay patch
//! that inherits from its `openai/<slug>` public record when one exists.
//! Seed patches (bundled or previously refreshed) are kept for models that
//! remain available so hand-authored extras such as the Fast option survive;
//! models missing from the live list are dropped.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// Build the overlay patches for every non-hidden model in a `model/list`
/// result. `seed` supplies the current patches for the provider and
/// `has_base(slug)` reports whether `openai/<slug>` exists in the public
/// catalog. Fails when the response yields no models so a bad reply never
/// replaces a working roster.
pub fn rebuild_codex_roster(
    live: &Value,
    seed: &BTreeMap<String, Value>,
    has_base: impl Fn(&str) -> bool,
) -> Result<BTreeMap<String, Value>, String> {
    let Some(entries) = live.get("data").and_then(Value::as_array) else {
        return Err("model/list response has no `data` array".into());
    };
    let mut roster = BTreeMap::new();
    for entry in entries {
        let Some(slug) = entry.get("model").and_then(Value::as_str) else {
            continue;
        };
        if slug.is_empty() || entry.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let mut patch = match seed.get(slug) {
            Some(patch) => patch.clone(),
            None if has_base(slug) => json!({ "base_model": format!("openai/{slug}") }),
            None => synthesized_patch(entry),
        };
        let Some(object) = patch.as_object_mut() else {
            continue;
        };
        object.insert("id".into(), Value::String(slug.into()));
        if let Some(efforts) = live_reasoning_option(entry) {
            object.insert("reasoning_options".into(), Value::Array(vec![efforts]));
        }
        // The speed tier is live-derived: a previous roster's `fast` must not
        // outlive the vendor withdrawing it.
        let mut options = object
            .get("options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        options.remove("fast");
        if let Some(fast) = live_fast_option(entry) {
            options.insert("fast".into(), fast);
        }
        if options.is_empty() {
            object.remove("options");
        } else {
            object.insert("options".into(), Value::Object(options));
        }
        roster.insert(slug.to_string(), patch);
    }
    if roster.is_empty() {
        return Err("model/list response lists no visible models".into());
    }
    Ok(roster)
}

/// Minimal record for a model the public catalog does not know yet. It has
/// no limits or pricing, which the catalog surfaces as unknown rather than
/// guessed.
fn synthesized_patch(entry: &Value) -> Value {
    let display_name = entry
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty());
    let attachment = entry
        .get("inputModalities")
        .and_then(Value::as_array)
        .is_some_and(|modalities| modalities.iter().any(|m| m.as_str() == Some("image")));
    let mut object = Map::new();
    if let Some(name) = display_name {
        object.insert("name".into(), Value::String(name.into()));
    }
    object.insert("tool_call".into(), Value::Bool(true));
    object.insert("attachment".into(), Value::Bool(attachment));
    Value::Object(object)
}

/// The live effort levels are authoritative: they track the account's
/// entitlements more closely than any static catalog.
fn live_reasoning_option(entry: &Value) -> Option<Value> {
    let values: Vec<Value> = entry
        .get("supportedReasoningEfforts")?
        .as_array()?
        .iter()
        .filter_map(|effort| {
            effort
                .get("reasoningEffort")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .map(|value| Value::String(value.into()))
        .collect();
    if values.is_empty() {
        return None;
    }
    let mut option = Map::new();
    option.insert("type".into(), Value::String("effort".into()));
    option.insert("values".into(), Value::Array(values));
    if let Some(default) = entry
        .get("defaultReasoningEffort")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        option.insert("default".into(), Value::String(default.into()));
    }
    Some(Value::Object(option))
}

/// Codex advertises its priority service tier as `additionalSpeedTiers:
/// ["fast"]` (with a human description under `serviceTiers`). Expose it as
/// the boolean `fast` option the app-server adapter maps to `serviceTier`.
fn live_fast_option(entry: &Value) -> Option<Value> {
    let tiers = entry.get("additionalSpeedTiers")?.as_array()?;
    if !tiers.iter().any(|tier| tier.as_str() == Some("fast")) {
        return None;
    }
    let description = entry
        .get("serviceTiers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|tier| tier.get("id").and_then(Value::as_str) == Some("priority"))
        .and_then(|tier| tier.get("description"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .unwrap_or("Run faster with increased credit usage");
    Some(json!({
        "type": "boolean",
        "default": false,
        "description": description,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_entry(slug: &str, efforts: &[&str], default: &str) -> Value {
        json!({
            "model": slug,
            "displayName": slug.to_uppercase(),
            "hidden": false,
            "inputModalities": ["text", "image"],
            "supportedReasoningEfforts": efforts
                .iter()
                .map(|effort| json!({"reasoningEffort": effort, "description": ""}))
                .collect::<Vec<_>>(),
            "defaultReasoningEffort": default,
        })
    }

    fn seed() -> BTreeMap<String, Value> {
        let mut seed = BTreeMap::new();
        seed.insert(
            "gpt-5.6-sol".to_string(),
            json!({
                "id": "gpt-5.6-sol",
                "base_model": "openai/gpt-5.6-sol",
                "options": {"fast": {"type": "boolean", "default": false}},
                "reasoning_options": [{"type": "effort", "values": ["low", "medium"], "default": "medium"}],
                "limit": {"context": 500000}
            }),
        );
        seed.insert(
            "gpt-5.4-mini".to_string(),
            json!({"id": "gpt-5.4-mini", "base_model": "openai/gpt-5.4-mini"}),
        );
        seed
    }

    #[test]
    fn keeps_seed_extras_and_overrides_efforts_from_live() {
        let mut sol = live_entry("gpt-5.6-sol", &["low", "high", "ultra"], "low");
        sol["additionalSpeedTiers"] = json!(["fast"]);
        let live = json!({"data": [sol]});
        let roster = rebuild_codex_roster(&live, &seed(), |_| true).unwrap();
        let sol = &roster["gpt-5.6-sol"];
        assert_eq!(sol["base_model"], "openai/gpt-5.6-sol");
        assert_eq!(sol["options"]["fast"]["type"], "boolean");
        assert_eq!(sol["limit"]["context"], 500000);
        assert_eq!(
            sol["reasoning_options"],
            json!([{"type": "effort", "values": ["low", "high", "ultra"], "default": "low"}])
        );
    }

    #[test]
    fn drops_seed_models_missing_from_live() {
        let live = json!({"data": [live_entry("gpt-5.6-sol", &["low"], "low")]});
        let roster = rebuild_codex_roster(&live, &seed(), |_| true).unwrap();
        assert!(!roster.contains_key("gpt-5.4-mini"));
        assert_eq!(roster.len(), 1);
    }

    #[test]
    fn new_live_models_inherit_from_the_public_catalog_when_possible() {
        let live = json!({"data": [
            live_entry("gpt-5.6-luna", &["low", "medium"], "medium"),
            live_entry("gpt-7-nova", &["high"], "high"),
        ]});
        let roster = rebuild_codex_roster(&live, &seed(), |slug| slug == "gpt-5.6-luna").unwrap();
        assert_eq!(roster["gpt-5.6-luna"]["base_model"], "openai/gpt-5.6-luna");
        assert_eq!(roster["gpt-5.6-luna"]["id"], "gpt-5.6-luna");

        let nova = &roster["gpt-7-nova"];
        assert!(nova.get("base_model").is_none());
        assert_eq!(nova["name"], "GPT-7-NOVA");
        assert_eq!(nova["tool_call"], true);
        assert_eq!(nova["attachment"], true);
        assert_eq!(nova["reasoning_options"][0]["values"], json!(["high"]));
    }

    #[test]
    fn hidden_models_are_skipped_and_empty_responses_fail() {
        let mut hidden = live_entry("gpt-5.6-sol", &["low"], "low");
        hidden["hidden"] = json!(true);
        let live = json!({"data": [hidden]});
        assert!(rebuild_codex_roster(&live, &seed(), |_| true).is_err());
        assert!(rebuild_codex_roster(&json!({"data": []}), &seed(), |_| true).is_err());
        assert!(rebuild_codex_roster(&json!({}), &seed(), |_| true).is_err());
    }

    #[test]
    fn models_without_effort_metadata_keep_the_seed_efforts() {
        let live = json!({"data": [{"model": "gpt-5.6-sol", "hidden": false}]});
        let roster = rebuild_codex_roster(&live, &seed(), |_| true).unwrap();
        assert_eq!(
            roster["gpt-5.6-sol"]["reasoning_options"][0]["values"],
            json!(["low", "medium"])
        );
    }

    #[test]
    fn fast_option_comes_from_live_speed_tiers() {
        let mut entry = live_entry("gpt-5.6-luna", &["low"], "low");
        entry["additionalSpeedTiers"] = json!(["fast"]);
        entry["serviceTiers"] =
            json!([{"id": "priority", "name": "Fast", "description": "2x speed, increased usage"}]);
        let live = json!({"data": [entry, live_entry("gpt-5.3-codex-spark", &["low"], "low")]});
        let roster = rebuild_codex_roster(&live, &BTreeMap::new(), |_| true).unwrap();
        assert_eq!(
            roster["gpt-5.6-luna"]["options"]["fast"],
            json!({"type": "boolean", "default": false, "description": "2x speed, increased usage"})
        );
        assert!(roster["gpt-5.3-codex-spark"].get("options").is_none());
    }

    #[test]
    fn withdrawn_fast_tier_is_dropped_on_the_next_rebuild() {
        let mut with_fast = live_entry("gpt-5.6-luna", &["low"], "low");
        with_fast["additionalSpeedTiers"] = json!(["fast"]);
        let first = rebuild_codex_roster(&json!({"data": [with_fast]}), &BTreeMap::new(), |_| true)
            .unwrap();
        assert!(first["gpt-5.6-luna"]["options"].get("fast").is_some());

        let without_fast = live_entry("gpt-5.6-luna", &["low"], "low");
        let second =
            rebuild_codex_roster(&json!({"data": [without_fast]}), &first, |_| true).unwrap();
        assert!(
            second["gpt-5.6-luna"].get("options").is_none(),
            "the previous roster's fast option must not survive: {:?}",
            second["gpt-5.6-luna"]
        );
    }
}
