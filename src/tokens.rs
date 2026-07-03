//! Identifier tokenization for BM25 indexing (port of `semble/tokens.py`).

use std::sync::OnceLock;

use regex::Regex;

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*").unwrap())
}

/// Emulates Python's `[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+` findall.
///
/// The Rust `regex` crate has no lookahead, so acronym-followed-by-word
/// boundaries ("getHTTPResponse" -> "HTTP" + "Response") are handled manually.
fn camel_findall(token: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_uppercase() {
            // Run of uppercase letters.
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_ascii_uppercase() {
                j += 1;
            }
            let upper_run = j - i;
            if j < bytes.len() && (bytes[j] as char).is_ascii_lowercase() {
                if upper_run > 1 {
                    // Acronym followed by a Word: split before the last upper.
                    parts.push(token[i..j - 1].to_string());
                    i = j - 1;
                }
                // Single upper followed by lowers: [A-Z]?[a-z]+
                let mut k = i + 1;
                while k < bytes.len() && (bytes[k] as char).is_ascii_lowercase() {
                    k += 1;
                }
                parts.push(token[i..k].to_string());
                i = k;
            } else {
                parts.push(token[i..j].to_string());
                i = j;
            }
        } else if c.is_ascii_lowercase() {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_ascii_lowercase() {
                j += 1;
            }
            parts.push(token[i..j].to_string());
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
                j += 1;
            }
            parts.push(token[i..j].to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    parts
}

/// Split a single identifier into sub-tokens via camelCase/snake_case.
///
/// Returns the original token (lowered) plus any sub-tokens.
/// E.g. "HandlerStack" -> ["handlerstack", "handler", "stack"]
///      "my_func" -> ["my_func", "my", "func"]
///      "simple" -> ["simple"]
pub fn split_identifier(token: &str) -> Vec<String> {
    let lower = token.to_lowercase();
    let parts: Vec<String> = if token.contains('_') {
        lower
            .split('_')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect()
    } else {
        camel_findall(token)
            .into_iter()
            .map(|p| p.to_lowercase())
            .collect()
    };
    if parts.len() >= 2 {
        let mut out = Vec::with_capacity(parts.len() + 1);
        out.push(lower);
        out.extend(parts);
        out
    } else {
        vec![lower]
    }
}

/// Split text into lowercase identifier-like tokens for BM25 indexing.
///
/// Compound identifiers (camelCase, PascalCase, snake_case) are expanded into
/// sub-tokens so that partial matches work. The original compound token is
/// preserved for exact-match boosting.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    for m in token_re().find_iter(text) {
        result.extend(split_identifier(m.as_str()));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_snake_case() {
        assert_eq!(split_identifier("my_func"), vec!["my_func", "my", "func"]);
        // Single sub-part after splitting: only the lowered original survives.
        assert_eq!(split_identifier("_private"), vec!["_private"]);
    }

    #[test]
    fn splits_camel_case() {
        assert_eq!(
            split_identifier("HandlerStack"),
            vec!["handlerstack", "handler", "stack"]
        );
        assert_eq!(
            split_identifier("getHTTPResponse"),
            vec!["gethttpresponse", "get", "http", "response"]
        );
        assert_eq!(
            split_identifier("XMLParser"),
            vec!["xmlparser", "xml", "parser"]
        );
    }

    #[test]
    fn simple_token_unchanged() {
        assert_eq!(split_identifier("simple"), vec!["simple"]);
        assert_eq!(split_identifier("UPPER"), vec!["upper"]);
    }

    #[test]
    fn tokenize_extracts_identifiers() {
        let toks = tokenize("def saveModel(path): return None");
        assert!(toks.contains(&"savemodel".to_string()));
        assert!(toks.contains(&"save".to_string()));
        assert!(toks.contains(&"model".to_string()));
        assert!(toks.contains(&"path".to_string()));
        assert!(!toks.contains(&"(".to_string()));
    }

    #[test]
    fn tokenize_numbers_in_identifiers() {
        assert_eq!(
            split_identifier("utf8Decode"),
            vec!["utf8decode", "utf", "8", "decode"]
        );
    }
}
