use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const MAX_DIFF_BYTES: usize = 512 * 1024;

/// Read-only base-to-head diff for review/plan agents. Keeping this behind
/// ToolExecutor preserves the same audit and permission path as every other
/// model-visible filesystem operation.
pub struct GitDiff;

#[async_trait::async_trait]
impl Tool for GitDiff {
    fn name(&self) -> &'static str {
        "git_diff"
    }

    fn description(&self) -> &'static str {
        "Return a pageable unified diff between a git base ref and the current workspace HEAD, optionally limited to one changed path."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "base": {
                    "type": "string",
                    "description": "Git base ref or commit SHA supplied by the review task"
                },
                "path": {
                    "type": "string",
                    "description": "Optional repository-relative changed path"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Byte offset returned as next_offset by a previous call"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 524288,
                    "default": 524288,
                    "description": "Maximum UTF-8 bytes to return"
                }
            },
            "required": ["base"]
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(base) = args.get("base").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: base");
        };
        let worktree = ctx.worktree.clone();
        let base = base.to_string();
        let path = args.get("path").and_then(Value::as_str).map(str::to_string);
        if let Some(path) = path.as_deref()
            && let Err(error) = ctx.resolve(path)
        {
            return ToolResult::error(error);
        }
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(MAX_DIFF_BYTES)
            .clamp(1, MAX_DIFF_BYTES);
        match tokio::task::spawn_blocking(move || match path {
            Some(path) => crate::git::session_diff_path(&worktree, &base, &path),
            None => crate::git::session_diff(&worktree, &base),
        })
        .await
        {
            Ok(Ok(diff)) => {
                if offset > diff.len() || !diff.is_char_boundary(offset) {
                    return ToolResult::error("offset is not a valid UTF-8 boundary in this diff");
                }
                let mut end = offset.saturating_add(limit).min(diff.len());
                while end > offset && !diff.is_char_boundary(end) {
                    end -= 1;
                }
                let next_offset = (end < diff.len()).then_some(end);
                ToolResult::ok(json!({
                    "diff": &diff[offset..end],
                    "offset": offset,
                    "next_offset": next_offset,
                    "total_bytes": diff.len(),
                    "truncated": offset > 0 || next_offset.is_some(),
                }))
            }
            Ok(Err(error)) => ToolResult::error(error),
            Err(error) => ToolResult::error(format!("diff task failed: {error}")),
        }
    }
}
