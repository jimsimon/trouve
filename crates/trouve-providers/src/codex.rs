//! Shared decoding helpers for Codex wire items.

use serde_json::Value;

/// Extract raw text from a completed Codex reasoning item.
///
/// Reasoning summaries are excluded here (see
/// [`completed_reasoning_summary_text`]): they are a separate stream and only
/// stand in for raw reasoning on models that do not expose it. Both
/// app-server's flattened strings and Responses' typed `{ type, text }` parts
/// are accepted.
pub fn completed_raw_reasoning_text(item: &Value) -> Option<String> {
    joined_text_parts(&item["content"])
}

/// Extract the summary text from a completed Codex reasoning item. Each
/// summary part is a short heading-plus-paragraph section; parts are joined as
/// separate paragraphs.
pub fn completed_reasoning_summary_text(item: &Value) -> Option<String> {
    joined_text_parts(&item["summary"])
}

/// Display text for a completed reasoning item: raw reasoning when the model
/// exposes it, otherwise the reasoning summary.
pub fn completed_reasoning_text(item: &Value) -> Option<String> {
    completed_raw_reasoning_text(item).or_else(|| completed_reasoning_summary_text(item))
}

fn joined_text_parts(parts: &Value) -> Option<String> {
    let parts = parts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.as_str()
                .or_else(|| part.get("text").and_then(Value::as_str))
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_only_raw_content_from_completed_reasoning() {
        let summarized = json!({
            "summary": [
                "Checking the adapter",
                { "type": "summary_text", "text": "Found the rich item shape" },
                "",
            ],
            "content": [{ "type": "reasoning_text", "text": "raw thought" }],
        });
        assert_eq!(
            completed_raw_reasoning_text(&summarized).as_deref(),
            Some("raw thought")
        );
        assert_eq!(
            completed_reasoning_summary_text(&summarized).as_deref(),
            Some("Checking the adapter\n\nFound the rich item shape")
        );

        assert_eq!(
            completed_raw_reasoning_text(&json!({ "content": ["raw thought"] })).as_deref(),
            Some("raw thought")
        );
        assert_eq!(
            completed_raw_reasoning_text(&json!({ "summary": ["heading"] })),
            None
        );
        assert_eq!(completed_raw_reasoning_text(&json!({})), None);
    }

    #[test]
    fn prefers_raw_reasoning_over_summary_for_display() {
        let both = json!({ "summary": ["heading"], "content": ["raw thought"] });
        assert_eq!(
            completed_reasoning_text(&both).as_deref(),
            Some("raw thought")
        );
        assert_eq!(
            completed_reasoning_text(&json!({ "summary": ["heading"], "content": [] })).as_deref(),
            Some("heading")
        );
        assert_eq!(
            completed_reasoning_text(&json!({ "type": "reasoning" })),
            None
        );
    }
}
