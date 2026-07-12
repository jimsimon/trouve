//! Regex search over the worktree, honouring .gitignore.
//!
//! Phase 5 upgrades search to trouve-search's hybrid semantic/BM25 index;
//! this lexical tool keeps phase 1 offline and dependency-light.

use regex::Regex;
use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const MAX_RESULTS: usize = 200;
/// Skip files larger than this: grep slurps each file into a String, so a
/// single huge or newline-free file would spike memory and CPU.
const MAX_GREP_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub struct Grep;

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &'static str {
        "Search workspace files with a regular expression. Respects .gitignore. Returns up to 200 matches as path:line pairs."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Rust-flavoured regular expression"},
                "path": {"type": "string", "description": "Workspace-relative directory to search (default: root)"},
                "case_insensitive": {"type": "boolean"}
            },
            "required": ["pattern"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(pattern) = args.get("pattern").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: pattern");
        };
        let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let root = match ctx.resolve(rel) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let regex = match regex::RegexBuilder::new(pattern)
            .case_insensitive(
                args.get("case_insensitive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("invalid pattern: {e}")),
        };
        let worktree = ctx.worktree.clone();
        // The walker is synchronous; run it off the async threads.
        let matches = tokio::task::spawn_blocking(move || search(&worktree, &root, &regex))
            .await
            .unwrap_or_else(|e| Err(format!("search panicked: {e}")));
        match matches {
            Ok((matches, truncated)) => ToolResult::ok(json!({
                "matches": matches,
                "truncated": truncated,
            })),
            Err(e) => ToolResult::error(e),
        }
    }
}

fn search(
    worktree: &std::path::Path,
    root: &std::path::Path,
    regex: &Regex,
) -> Result<(Vec<Value>, bool), String> {
    let mut matches = Vec::new();
    let mut truncated = false;
    // `require_git(false)` keeps .gitignore effective even outside a repo
    // (deterministic behaviour; worktrees are repos anyway).
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build();
    'outer: for entry in walker {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        // Skip files too large to slurp: one huge or newline-free file
        // (minified/generated) would otherwise blow up memory and CPU.
        if entry
            .metadata()
            .is_ok_and(|m| m.len() > MAX_GREP_FILE_BYTES)
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue; // binary or unreadable
        };
        for (i, line) in text.lines().enumerate() {
            if regex.is_match(line) {
                if matches.len() >= MAX_RESULTS {
                    truncated = true;
                    break 'outer;
                }
                let rel = entry
                    .path()
                    .strip_prefix(worktree)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_string();
                matches.push(json!({
                    "path": rel,
                    "line": i + 1,
                    "text": line.chars().take(500).collect::<String>(),
                }));
            }
        }
    }
    Ok((matches, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_matches_and_respects_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(tmp.path().join("hit.txt"), "needle here\nnothing\n").unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "needle here too\n").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = Grep.run(&ctx, &json!({"pattern": "needle"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        let matches = res.result["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["path"], "hit.txt");
        assert_eq!(matches[0]["line"], 1);
    }
}
