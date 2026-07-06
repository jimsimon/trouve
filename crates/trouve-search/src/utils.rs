//! Small helpers (port of `semble/utils.py`).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde_json::{json, Value};

use crate::types::{Chunk, SearchResult};

pub const DEFAULT_MODEL_NAME: &str = "minishlab/potion-code-16M";

/// Warn (once per `deprecated` name, on stderr) that a semble-era name is
/// deprecated and which trouve name replaces it.
pub fn warn_deprecated(deprecated: &str, replacement: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut warned = WARNED.get_or_init(Default::default).lock().unwrap();
    if warned.insert(deprecated.to_string()) {
        eprintln!(
            "warning: {deprecated} is deprecated and will be removed in a future release; \
             use {replacement} instead"
        );
    }
}

/// Read an environment variable, falling back to its deprecated semble-era
/// name. The trouve name wins when both are set; using the semble name warns
/// once. Returns the name that supplied the value alongside the value.
pub fn env_var_compat(
    trouve_name: &'static str,
    semble_name: &'static str,
) -> Option<(&'static str, String)> {
    if let Ok(v) = std::env::var(trouve_name) {
        return Some((trouve_name, v));
    }
    let v = std::env::var(semble_name).ok()?;
    warn_deprecated(
        &format!("the {semble_name} environment variable"),
        trouve_name,
    );
    Some((semble_name, v))
}

static GIT_URL_SCHEMES: &[&str] = &[
    "https://",
    "http://",
    "ssh://",
    "git://",
    "git+ssh://",
    "file://",
];

fn scp_git_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\w.-]+@[\w.-]+:[^/]").unwrap())
}

/// Run `f`, printing its wall time to stderr when `TROUVE_TIMING` is set.
pub fn timed<T>(phase: &str, f: impl FnOnce() -> T) -> T {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| std::env::var_os("TROUVE_TIMING").is_some());
    if !enabled {
        return f();
    }
    let start = std::time::Instant::now();
    let out = f();
    eprintln!(
        "[timing] {phase}: {:.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    out
}

/// Return true if `path` looks like a remote git URL rather than a local
/// path. Remote URLs are not supported (trouve does not clone repositories);
/// this exists to reject them with a clear error instead of a confusing
/// "path does not exist".
pub fn is_git_url(path: &str) -> bool {
    GIT_URL_SCHEMES.iter().any(|s| path.starts_with(s)) || scp_git_url_re().is_match(path)
}

/// Resolve the model name, respecting `TROUVE_MODEL_NAME` (with a deprecated
/// `SEMBLE_MODEL_NAME` fallback).
pub fn resolve_model_name() -> String {
    env_var_compat("TROUVE_MODEL_NAME", "SEMBLE_MODEL_NAME")
        .map(|(_, v)| v)
        .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_string())
}

/// Return the chunk containing `line` in `file_path`, or None.
pub fn resolve_chunk<'a>(chunks: &'a [Chunk], file_path: &str, line: u32) -> Option<&'a Chunk> {
    let mut fallback: Option<&Chunk> = None;
    for chunk in chunks {
        if chunk.file_path == file_path && chunk.start_line <= line && line <= chunk.end_line {
            if line < chunk.end_line {
                return Some(chunk);
            }
            if fallback.is_none() {
                // line == end_line: boundary; keep as fallback for end-of-file chunks.
                fallback = Some(chunk);
            }
        }
    }
    fallback
}

/// Render results as a flat JSONable object.
///
/// `max_snippet_lines=None` -> full content per result.
/// `max_snippet_lines=0`    -> file path and line range only, no content.
/// `max_snippet_lines=N>0`  -> first N lines of content.
pub fn format_results(
    query: &str,
    results: &[SearchResult],
    max_snippet_lines: Option<usize>,
) -> Value {
    let formatted: Vec<Value> = results
        .iter()
        .map(|r| {
            let mut entry = json!({
                "file_path": r.chunk.file_path,
                "start_line": r.chunk.start_line,
                "end_line": r.chunk.end_line,
                "score": r.score,
            });
            match max_snippet_lines {
                None => {
                    entry["content"] = Value::String(r.chunk.content.clone());
                }
                Some(0) => {}
                Some(n) => {
                    let lines: Vec<&str> = r.chunk.content.lines().take(n).collect();
                    entry["content"] = Value::String(lines.join("\n"));
                }
            }
            entry
        })
        .collect();
    json!({ "query": query, "results": formatted })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_compat_prefers_trouve_name() {
        // Unique names so parallel tests never race on the same variables.
        std::env::set_var("TROUVE_COMPAT_TEST_A", "new");
        std::env::set_var("SEMBLE_COMPAT_TEST_A", "old");
        assert_eq!(
            env_var_compat("TROUVE_COMPAT_TEST_A", "SEMBLE_COMPAT_TEST_A"),
            Some(("TROUVE_COMPAT_TEST_A", "new".to_string()))
        );
        std::env::remove_var("TROUVE_COMPAT_TEST_A");
        assert_eq!(
            env_var_compat("TROUVE_COMPAT_TEST_A", "SEMBLE_COMPAT_TEST_A"),
            Some(("SEMBLE_COMPAT_TEST_A", "old".to_string()))
        );
        std::env::remove_var("SEMBLE_COMPAT_TEST_A");
        assert_eq!(
            env_var_compat("TROUVE_COMPAT_TEST_A", "SEMBLE_COMPAT_TEST_A"),
            None
        );
    }

    #[test]
    fn detects_git_urls() {
        assert!(is_git_url("https://github.com/org/repo"));
        assert!(is_git_url("git@github.com:org/repo.git"));
        assert!(is_git_url("ssh://git@host/repo"));
        assert!(!is_git_url("/home/user/project"));
        assert!(!is_git_url("./relative/path"));
        assert!(!is_git_url("C:/windows/path"));
    }

    fn chunk(path: &str, start: u32, end: u32) -> Chunk {
        Chunk {
            content: "x".into(),
            file_path: path.into(),
            start_line: start,
            end_line: end,
            language: None,
        }
    }

    #[test]
    fn resolves_chunk_by_line() {
        let chunks = vec![
            chunk("a.py", 1, 10),
            chunk("a.py", 10, 20),
            chunk("b.py", 1, 5),
        ];
        assert_eq!(resolve_chunk(&chunks, "a.py", 5).unwrap().start_line, 1);
        // Line 10 is the boundary: the chunk where it's interior wins.
        assert_eq!(resolve_chunk(&chunks, "a.py", 10).unwrap().start_line, 10);
        assert_eq!(resolve_chunk(&chunks, "a.py", 20).unwrap().start_line, 10);
        assert!(resolve_chunk(&chunks, "a.py", 99).is_none());
        assert!(resolve_chunk(&chunks, "missing.py", 1).is_none());
    }

    #[test]
    fn formats_results_with_snippet_limits() {
        let results = vec![SearchResult {
            chunk: Chunk {
                content: "l1\nl2\nl3".into(),
                file_path: "a.py".into(),
                start_line: 1,
                end_line: 3,
                language: None,
            },
            score: 0.5,
        }];
        let full = format_results("q", &results, None);
        assert_eq!(full["results"][0]["content"], "l1\nl2\nl3");
        let limited = format_results("q", &results, Some(2));
        assert_eq!(limited["results"][0]["content"], "l1\nl2");
        let none = format_results("q", &results, Some(0));
        assert!(none["results"][0].get("content").is_none());
    }
}
