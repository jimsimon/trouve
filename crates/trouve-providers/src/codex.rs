//! Shared decoding helpers for Codex wire items.

use serde_json::Value;

/// Extract displayable text from a completed Codex reasoning item.
///
/// Summary text is preferred over raw content. Both app-server's flattened
/// strings and Responses' typed `{ type, text }` parts are accepted.
pub fn completed_reasoning_text(item: &Value) -> Option<String> {
    for field in ["summary", "content"] {
        let parts = item[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| {
                part.as_str()
                    .or_else(|| part.get("text").and_then(Value::as_str))
            })
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            return Some(parts.join("\n\n"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_summary_or_content_from_completed_reasoning() {
        let summarized = json!({
            "summary": [
                "Checking the adapter",
                { "type": "summary_text", "text": "Found the rich item shape" },
                "",
            ],
            "content": [{ "type": "reasoning_text", "text": "raw thought" }],
        });
        assert_eq!(
            completed_reasoning_text(&summarized).as_deref(),
            Some("Checking the adapter\n\nFound the rich item shape")
        );

        assert_eq!(
            completed_reasoning_text(&json!({ "summary": [], "content": ["raw thought"] }))
                .as_deref(),
            Some("raw thought")
        );
        assert_eq!(completed_reasoning_text(&json!({})), None);
    }
}
