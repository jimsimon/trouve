//! Shared decoding helpers for Codex wire items.

use serde_json::Value;

/// Extract raw text from a completed Codex reasoning item.
///
/// Reasoning summaries are intentionally excluded: Codex exposes them as a
/// separate stream, and they are short section headings rather than the raw
/// reasoning content emitted by models that support it. Both app-server's
/// flattened strings and Responses' typed `{ type, text }` parts are accepted.
pub fn completed_raw_reasoning_text(item: &Value) -> Option<String> {
    let parts = item["content"]
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
            completed_raw_reasoning_text(&json!({ "content": ["raw thought"] })).as_deref(),
            Some("raw thought")
        );
        assert_eq!(
            completed_raw_reasoning_text(&json!({ "summary": ["heading"] })),
            None
        );
        assert_eq!(completed_raw_reasoning_text(&json!({})), None);
    }
}
