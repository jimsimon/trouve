//! Recursive filename search by glob pattern, honouring .gitignore.

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const MAX_RESULTS: usize = 500;

pub struct Glob;

#[async_trait::async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &'static str {
        "Find workspace files whose path matches a glob pattern (e.g. \"*.rs\", \
         \"src/**/*.slint\"). Bare patterns match at any depth. Respects .gitignore; \
         results are sorted by modification time, newest first."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern; \"*.rs\" matches at any depth, use \"/\" for structure (\"src/**/*.ts\")"},
                "path": {"type": "string", "description": "Workspace-relative directory to search (default: root)"}
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
        // A pattern without a separator ("*.rs") means "at any depth" —
        // what models invariably intend — so it gets a **/ prefix.
        let full_pattern = if pattern.contains('/') {
            pattern.to_string()
        } else {
            format!("**/{pattern}")
        };
        let matcher = match globset::GlobBuilder::new(&full_pattern)
            .literal_separator(true)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolResult::error(format!("invalid pattern: {e}")),
        };
        let worktree = ctx.worktree.clone();
        // The walker is synchronous; run it off the async threads.
        let found = tokio::task::spawn_blocking(move || search(&worktree, &root, &matcher))
            .await
            .unwrap_or_else(|e| Err(format!("glob walk panicked: {e}")));
        match found {
            Ok((files, truncated)) => ToolResult::ok(json!({
                "files": files,
                "truncated": truncated,
            })),
            Err(e) => ToolResult::error(e),
        }
    }
}

fn search(
    worktree: &std::path::Path,
    root: &std::path::Path,
    matcher: &globset::GlobMatcher,
) -> Result<(Vec<Value>, bool), String> {
    let mut files: Vec<(std::time::SystemTime, String)> = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        // Match against the path relative to the search root, so "src/*.rs"
        // works whether the search starts at the worktree root or below.
        let rel_to_root = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if !matcher.is_match(rel_to_root) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let rel = entry
            .path()
            .strip_prefix(worktree)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();
        files.push((mtime, rel));
    }
    // Newest first: recently touched files are almost always the relevant
    // ones. Path as tiebreaker keeps the order deterministic.
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let truncated = files.len() > MAX_RESULTS;
    files.truncate(MAX_RESULTS);
    let files = files.into_iter().map(|(_, p)| json!(p)).collect();
    Ok((files, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matches_at_any_depth_and_respects_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/deep")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "skipped.rs\n").unwrap();
        std::fs::write(tmp.path().join("top.rs"), "").unwrap();
        std::fs::write(tmp.path().join("src/deep/nested.rs"), "").unwrap();
        std::fs::write(tmp.path().join("src/deep/skipped.rs"), "").unwrap();
        std::fs::write(tmp.path().join("readme.md"), "").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = Glob.run(&ctx, &json!({"pattern": "*.rs"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        let mut files: Vec<String> = res.result["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        files.sort();
        assert_eq!(files, ["src/deep/nested.rs", "top.rs"]);
        assert_eq!(res.result["truncated"], false);
    }

    #[tokio::test]
    async fn structured_patterns_anchor_to_the_search_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/a")).unwrap();
        std::fs::create_dir_all(tmp.path().join("other")).unwrap();
        std::fs::write(tmp.path().join("src/a/x.ts"), "").unwrap();
        std::fs::write(tmp.path().join("other/y.ts"), "").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = Glob.run(&ctx, &json!({"pattern": "src/**/*.ts"})).await;
        let files = res.result["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "src/a/x.ts");

        // Scoped to a subdirectory, the relative pattern applies there.
        let res = Glob
            .run(&ctx, &json!({"pattern": "*.ts", "path": "other"}))
            .await;
        let files = res.result["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "other/y.ts");
    }

    #[tokio::test]
    async fn rejects_bad_patterns() {
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            ..Default::default()
        };
        let res = Glob.run(&ctx, &json!({"pattern": "a{"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }
}
