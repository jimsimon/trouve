//! Rebuild the trouve-owned `cursor` roster from the Cursor SDK Bridge's
//! `SdkCursorService/ListModels` response.
//!
//! Cursor reports, per model, the exact parameter ids its agent accepts
//! (`effort`, `reasoning`, `reasoning_effort`, `thinking`, `context`, `fast`,
//! ...), their allowed values, and a default variant. Trouve forwards a turn's
//! model options to Cursor verbatim as `{id, value}` params, so those
//! parameter ids become the option-schema property names as-is. Public models
//! served through Cursor inherit limits and display metadata from their
//! models.dev record via `base_model`; Cursor-only models (Auto, Composer)
//! get a minimal record. Seed patches (bundled or previously refreshed) are
//! kept for models that remain available; models missing from the live list
//! are dropped.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// Build overlay patches for every model in a `ListModels` result. `seed`
/// supplies the current patches for the provider and `has_source(provider,
/// model)` reports whether the public catalog has that record. Fails when the
/// response yields no models so a bad reply never replaces a working roster.
pub fn rebuild_cursor_roster(
    live: &Value,
    seed: &BTreeMap<String, Value>,
    has_source: impl Fn(&str, &str) -> bool,
) -> Result<BTreeMap<String, Value>, String> {
    let Some(items) = live.get("items").and_then(Value::as_array) else {
        return Err("ListModels response has no `items` array".into());
    };
    let mut roster = BTreeMap::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let mut patch = match seed.get(id) {
            Some(patch) => patch.clone(),
            None => match public_base(id, &has_source) {
                Some(base) => json!({ "base_model": base }),
                None => Value::Object(Map::new()),
            },
        };
        let Some(object) = patch.as_object_mut() else {
            continue;
        };
        object.insert("id".into(), Value::String(id.into()));
        if let Some(name) = item
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            object.insert("name".into(), Value::String(name.into()));
        }
        object.entry("tool_call").or_insert(Value::Bool(true));
        // Cursor's parameter ids are the only controls its agent accepts.
        // Inherited models.dev reasoning options would surface under a
        // dialect-chosen key that Cursor may not recognize, so drop them and
        // describe every live parameter as a plain option instead.
        object.insert("reasoning_options".into(), Value::Array(Vec::new()));
        object.insert("temperature".into(), Value::Bool(false));

        // Options are rebuilt from the live parameters alone: a seed or
        // previous roster must not keep offering a parameter Cursor withdrew,
        // since every option is forwarded to the agent verbatim.
        let defaults = default_params(item);
        let mut options = Map::new();
        for parameter in item
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some((key, schema)) = parameter_schema(parameter, &defaults) else {
                continue;
            };
            options.insert(key, schema);
        }
        if options.is_empty() {
            object.remove("options");
        } else {
            object.insert("options".into(), Value::Object(options));
        }
        if let Some(context) = defaults.get("context").and_then(|v| parse_token_count(v)) {
            let limit = object
                .entry("limit")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Some(limit) = limit.as_object_mut() {
                limit.insert("context".into(), Value::from(context));
            }
        }
        roster.insert(id.to_string(), patch);
    }
    if roster.is_empty() {
        return Err("ListModels response lists no models".into());
    }
    Ok(roster)
}

/// Guess the models.dev record a Cursor-served public model corresponds to.
/// Only accepted when the public catalog actually has that record, so an
/// unknown or Cursor-only slug never inherits the wrong metadata.
fn public_base(id: &str, has_source: &impl Fn(&str, &str) -> bool) -> Option<String> {
    let candidates: &[&str] = if id.starts_with("claude-") {
        &["anthropic"]
    } else if id.starts_with("gpt-") || id.starts_with("o1") || id.starts_with("o3") {
        &["openai"]
    } else if id.starts_with("gemini-") {
        &["google"]
    } else if id.starts_with("grok-") {
        &["xai"]
    } else if id.starts_with("kimi-") {
        &["moonshotai"]
    } else if id.starts_with("glm-") {
        &["zai", "zhipuai"]
    } else {
        &[]
    };
    candidates
        .iter()
        .find(|provider| has_source(provider, id))
        .map(|provider| format!("{provider}/{id}"))
}

/// Parameter values selected by the model's default variant.
fn default_params(item: &Value) -> BTreeMap<String, String> {
    item.get("variants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|variant| variant.get("isDefault").and_then(Value::as_bool) == Some(true))
        .and_then(|variant| variant.get("params").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|param| {
            Some((
                param.get("id")?.as_str()?.to_string(),
                param.get("value")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// One Cursor parameter definition as a JSON Schema property keyed by the
/// parameter id. Two-valued `true`/`false` parameters become booleans (the
/// SDK adapter stringifies them back); everything else is a string enum.
fn parameter_schema(
    parameter: &Value,
    defaults: &BTreeMap<String, String>,
) -> Option<(String, Value)> {
    let id = parameter.get("id")?.as_str()?;
    if id.is_empty() {
        return None;
    }
    let values: Vec<(&str, Option<&str>)> = parameter
        .get("values")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            Some((
                value.get("value")?.as_str()?,
                value
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty()),
            ))
        })
        .collect();
    if values.is_empty() {
        return None;
    }
    let title = parameter
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let default = defaults.get(id).map(String::as_str);
    let mut schema = Map::new();
    if let Some(title) = title {
        schema.insert("title".into(), Value::String(title.into()));
    }
    let is_boolean = values.len() == 2
        && values.iter().any(|(value, _)| *value == "true")
        && values.iter().any(|(value, _)| *value == "false");
    if is_boolean {
        schema.insert("type".into(), Value::String("boolean".into()));
        if let Some(default) = default {
            schema.insert("default".into(), Value::Bool(default == "true"));
        }
    } else {
        schema.insert("type".into(), Value::String("string".into()));
        schema.insert(
            "enum".into(),
            Value::Array(
                values
                    .iter()
                    .map(|(value, _)| Value::String((*value).into()))
                    .collect(),
            ),
        );
        if values.iter().any(|(_, name)| name.is_some()) {
            schema.insert(
                "x-enumNames".into(),
                Value::Array(
                    values
                        .iter()
                        .map(|(value, name)| Value::String(name.unwrap_or(value).into()))
                        .collect(),
                ),
            );
        }
        if let Some(default) = default.filter(|d| values.iter().any(|(value, _)| value == d)) {
            schema.insert("default".into(), Value::String(default.into()));
        }
    }
    Some((id.to_string(), Value::Object(schema)))
}

/// `272k` → 272000, `1m` → 1000000, `200000` → 200000.
fn parse_token_count(value: &str) -> Option<u64> {
    let value = value.trim().to_ascii_lowercase();
    let (digits, multiplier) = match value.strip_suffix('m') {
        Some(digits) => (digits, 1_000_000u64),
        None => match value.strip_suffix('k') {
            Some(digits) => (digits, 1_000u64),
            None => (value.as_str(), 1u64),
        },
    };
    let number: f64 = digits.parse().ok()?;
    (number > 0.0).then(|| (number * multiplier as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sol() -> Value {
        json!({
            "id": "gpt-5.6-sol",
            "displayName": "GPT-5.6 Sol",
            "parameters": [
                {"id": "context", "displayName": "Context", "values": [{"value": "272k"}, {"value": "1m", "displayName": "1M"}]},
                {"id": "reasoning", "displayName": "Reasoning", "values": [
                    {"value": "low", "displayName": "Low"}, {"value": "medium", "displayName": "Medium"}, {"value": "high", "displayName": "High"}]},
                {"id": "fast", "displayName": "Fast", "values": [{"value": "false"}, {"value": "true", "displayName": "Fast"}]}
            ],
            "variants": [
                {"params": [{"id": "context", "value": "272k"}, {"id": "reasoning", "value": "low"}, {"id": "fast", "value": "false"}]},
                {"params": [{"id": "context", "value": "1m"}, {"id": "reasoning", "value": "medium"}, {"id": "fast", "value": "false"}], "isDefault": true}
            ]
        })
    }

    #[test]
    fn public_models_inherit_and_expose_cursor_parameter_ids() {
        let live = json!({"items": [sol()]});
        let roster = rebuild_cursor_roster(&live, &BTreeMap::new(), |p, m| {
            p == "openai" && m == "gpt-5.6-sol"
        })
        .unwrap();
        let sol = &roster["gpt-5.6-sol"];
        assert_eq!(sol["base_model"], "openai/gpt-5.6-sol");
        assert_eq!(sol["name"], "GPT-5.6 Sol");
        assert_eq!(sol["reasoning_options"], json!([]));
        assert_eq!(sol["temperature"], false);
        assert_eq!(sol["limit"]["context"], 1_000_000);
        let options = sol["options"].as_object().unwrap();
        assert_eq!(
            options["reasoning"],
            json!({"title": "Reasoning", "type": "string", "enum": ["low", "medium", "high"],
                   "x-enumNames": ["Low", "Medium", "High"], "default": "medium"})
        );
        assert_eq!(
            options["fast"],
            json!({"title": "Fast", "type": "boolean", "default": false})
        );
        assert_eq!(options["context"]["enum"], json!(["272k", "1m"]));
        assert_eq!(options["context"]["x-enumNames"], json!(["272k", "1M"]));
        assert_eq!(options["context"]["default"], "1m");
    }

    #[test]
    fn cursor_only_models_get_minimal_records_and_seed_patches_survive() {
        let mut seed = BTreeMap::new();
        seed.insert(
            "composer-2.5".to_string(),
            json!({"id": "composer-2.5", "attachment": true, "limit": {"context": 200000}}),
        );
        seed.insert(
            "gemini-3.1-pro".to_string(),
            json!({"id": "gemini-3.1-pro", "base_model": "google/gemini-3.1-pro-preview"}),
        );
        seed.insert(
            "gone".to_string(),
            json!({"id": "gone", "base_model": "openai/gone"}),
        );
        let live = json!({"items": [
            {"id": "default", "displayName": "Auto", "variants": [{"displayName": "Auto", "isDefault": true}]},
            {"id": "composer-2.5", "displayName": "Composer 2.5",
             "parameters": [{"id": "fast", "values": [{"value": "false"}, {"value": "true"}]}],
             "variants": [{"params": [{"id": "fast", "value": "true"}], "isDefault": true}]},
            {"id": "gemini-3.1-pro", "displayName": "Gemini 3.1 Pro"}
        ]});
        let roster = rebuild_cursor_roster(&live, &seed, |_, _| false).unwrap();
        assert_eq!(roster.len(), 3);
        assert!(!roster.contains_key("gone"));

        let auto = &roster["default"];
        assert_eq!(auto["name"], "Auto");
        assert_eq!(auto["tool_call"], true);
        assert!(auto.get("base_model").is_none());
        assert!(auto.get("options").is_none());

        let composer = &roster["composer-2.5"];
        assert_eq!(composer["attachment"], true, "seed extras survive");
        assert_eq!(composer["limit"]["context"], 200000);
        assert_eq!(
            composer["options"]["fast"],
            json!({"type": "boolean", "default": true})
        );

        assert_eq!(
            roster["gemini-3.1-pro"]["base_model"], "google/gemini-3.1-pro-preview",
            "seed slug remaps are kept"
        );
    }

    #[test]
    fn withdrawn_parameters_are_dropped_on_the_next_rebuild() {
        let first =
            rebuild_cursor_roster(&json!({"items": [sol()]}), &BTreeMap::new(), |_, _| false)
                .unwrap();
        assert!(first["gpt-5.6-sol"]["options"].get("fast").is_some());

        let mut reduced = sol();
        reduced["parameters"] = json!([
            {"id": "reasoning", "values": [{"value": "low"}, {"value": "high"}]}
        ]);
        let second =
            rebuild_cursor_roster(&json!({"items": [reduced]}), &first, |_, _| false).unwrap();
        let options = second["gpt-5.6-sol"]["options"].as_object().unwrap();
        assert_eq!(options.keys().collect::<Vec<_>>(), ["reasoning"]);

        let mut bare = sol();
        bare["parameters"] = json!([]);
        let third =
            rebuild_cursor_roster(&json!({"items": [bare]}), &second, |_, _| false).unwrap();
        assert!(third["gpt-5.6-sol"].get("options").is_none());
    }

    #[test]
    fn base_inference_requires_a_public_record() {
        let live = json!({"items": [
            {"id": "claude-opus-5", "displayName": "Claude Opus 5"},
            {"id": "grok-4.6", "displayName": "Cursor Grok 4.6"},
            {"id": "muse-spark-1.3", "displayName": "Muse Spark 1.3"}
        ]});
        let roster = rebuild_cursor_roster(&live, &BTreeMap::new(), |p, m| {
            (p, m) == ("anthropic", "claude-opus-5")
        })
        .unwrap();
        assert_eq!(
            roster["claude-opus-5"]["base_model"],
            "anthropic/claude-opus-5"
        );
        assert!(roster["grok-4.6"].get("base_model").is_none());
        assert!(roster["muse-spark-1.3"].get("base_model").is_none());
    }

    #[test]
    fn empty_or_malformed_responses_fail() {
        assert!(
            rebuild_cursor_roster(&json!({"items": []}), &BTreeMap::new(), |_, _| true).is_err()
        );
        assert!(rebuild_cursor_roster(&json!({}), &BTreeMap::new(), |_, _| true).is_err());
        assert!(
            rebuild_cursor_roster(&json!({"items": [{"id": ""}]}), &BTreeMap::new(), |_, _| {
                true
            })
            .is_err()
        );
    }

    #[test]
    fn token_counts_parse_cursor_labels() {
        assert_eq!(parse_token_count("272k"), Some(272_000));
        assert_eq!(parse_token_count("1m"), Some(1_000_000));
        assert_eq!(parse_token_count("1.5M"), Some(1_500_000));
        assert_eq!(parse_token_count("200000"), Some(200_000));
        assert_eq!(parse_token_count("auto"), None);
    }
}
