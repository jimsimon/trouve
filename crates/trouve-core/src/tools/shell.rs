//! Shell tool: run a command inside the session worktree.

use std::time::Duration;

use serde_json::{json, Value};

use super::{Tool, ToolCtx, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_CAPTURE_BYTES: usize = 32 * 1024;

fn truncate_utf8(mut bytes: Vec<u8>) -> (String, bool) {
    let truncated = bytes.len() > MAX_CAPTURE_BYTES;
    if truncated {
        bytes.truncate(MAX_CAPTURE_BYTES);
    }
    (String::from_utf8_lossy(&bytes).into_owned(), truncated)
}

pub struct Shell;

#[async_trait::async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn description(&self) -> &'static str {
        "Run a shell command in the workspace root. Captures stdout/stderr (truncated at 32KB each); times out after 120s by default."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Command passed to `sh -c`"},
                "timeout_secs": {"type": "integer", "minimum": 1, "maximum": 3600}
            },
            "required": ["command"]
        })
    }
    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: command");
        };
        let timeout = Duration::from_secs(
            args.get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
        );
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&ctx.worktree)
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(timeout, child).await {
            Err(_) => ToolResult::error(format!("command timed out after {}s", timeout.as_secs())),
            Ok(Err(e)) => ToolResult::error(format!("failed to spawn: {e}")),
            Ok(Ok(output)) => {
                let (stdout, stdout_truncated) = truncate_utf8(output.stdout);
                let (stderr, stderr_truncated) = truncate_utf8(output.stderr);
                ToolResult::ok(json!({
                    "exit_code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                    "truncated": stdout_truncated || stderr_truncated,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_in_worktree_and_reports_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("hello.txt"), "hi").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = Shell.run(&ctx, &json!({"command": "ls"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert!(res.result["stdout"].as_str().unwrap().contains("hello.txt"));
        assert_eq!(res.result["exit_code"], 0);

        let res = Shell.run(&ctx, &json!({"command": "exit 3"})).await;
        assert_eq!(res.result["exit_code"], 3);
    }

    #[tokio::test]
    async fn times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let res = Shell
            .run(&ctx, &json!({"command": "sleep 5", "timeout_secs": 1}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }
}
