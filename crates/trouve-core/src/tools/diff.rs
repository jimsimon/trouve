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
        "Return the unified diff between a git base ref and the current workspace HEAD. Use this first when reviewing a pull request."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "base": {
                    "type": "string",
                    "description": "Git base ref or commit SHA supplied by the review task"
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
        match tokio::task::spawn_blocking(move || crate::git::session_diff(&worktree, &base)).await
        {
            Ok(Ok(mut diff)) => {
                let truncated = diff.len() > MAX_DIFF_BYTES;
                if truncated {
                    diff.truncate(MAX_DIFF_BYTES);
                }
                ToolResult::ok(json!({"diff": diff, "truncated": truncated}))
            }
            Ok(Err(error)) => ToolResult::error(error),
            Err(error) => ToolResult::error(format!("diff task failed: {error}")),
        }
    }
}
