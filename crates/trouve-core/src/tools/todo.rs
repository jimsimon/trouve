//! The agent's task list: a progress artifact the chat UI renders as a
//! checklist. State lives per worktree for the engine's lifetime; each
//! update's tool result carries the full list, so the transcript (and any
//! model resuming from it) always sees the current plan.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TodoItem {
    id: String,
    content: String,
    status: String,
}

#[derive(Default)]
pub struct TodoWrite {
    lists: Mutex<HashMap<PathBuf, Vec<TodoItem>>>,
}

#[async_trait::async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }
    fn description(&self) -> &'static str {
        "Create or update your task list for this session. Use it to plan multi-step work and \
         mark progress (statuses: pending, in_progress, completed, cancelled). With merge=true, \
         listed items update or extend the existing list by id; otherwise the list is replaced. \
         Keep at most one item in_progress."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Task items to write",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "Stable identifier for the task"},
                            "content": {"type": "string", "description": "What the task is"},
                            "status": {"type": "string", "enum": STATUSES}
                        },
                        "required": ["id", "content", "status"]
                    }
                },
                "merge": {"type": "boolean", "description": "Merge with the existing list by id instead of replacing it (default: false)"}
            },
            "required": ["todos"]
        })
    }
    fn mutates(&self) -> bool {
        // Bookkeeping only — never gated, so planning works in every mode.
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(items) = args.get("todos").and_then(Value::as_array) else {
            return ToolResult::error("missing required argument: todos");
        };
        let mut incoming: Vec<TodoItem> = Vec::new();
        for item in items {
            let todo: TodoItem = match serde_json::from_value(item.clone()) {
                Ok(t) => t,
                Err(e) => return ToolResult::error(format!("bad todo item: {e}")),
            };
            if !STATUSES.contains(&todo.status.as_str()) {
                return ToolResult::error(format!(
                    "bad status \"{}\" (expected one of: {})",
                    todo.status,
                    STATUSES.join(", ")
                ));
            }
            incoming.push(todo);
        }
        let merge = args.get("merge").and_then(Value::as_bool).unwrap_or(false);

        let mut lists = self.lists.lock().unwrap();
        let list = lists.entry(ctx.worktree.clone()).or_default();
        if merge {
            for todo in incoming {
                match list.iter_mut().find(|t| t.id == todo.id) {
                    Some(existing) => *existing = todo,
                    None => list.push(todo),
                }
            }
        } else {
            *list = incoming;
        }
        if list.is_empty() {
            return ToolResult::error("todos must not be empty");
        }

        let done = list
            .iter()
            .filter(|t| t.status == "completed" || t.status == "cancelled")
            .count();
        ToolResult::ok(json!({
            "todos": list.iter().map(|t| json!({
                "id": t.id,
                "content": t.content,
                "status": t.status,
            })).collect::<Vec<_>>(),
            "done": done,
            "total": list.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tmp: &tempfile::TempDir) -> ToolCtx {
        ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn replace_then_merge_updates_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = TodoWrite::default();

        let res = tool
            .run(
                &ctx(&tmp),
                &json!({"todos": [
                    {"id": "a", "content": "first", "status": "in_progress"},
                    {"id": "b", "content": "second", "status": "pending"},
                ]}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["total"], 2);
        assert_eq!(res.result["done"], 0);

        // Merge: complete "a", add "c"; "b" survives untouched.
        let res = tool
            .run(
                &ctx(&tmp),
                &json!({"merge": true, "todos": [
                    {"id": "a", "content": "first", "status": "completed"},
                    {"id": "c", "content": "third", "status": "pending"},
                ]}),
            )
            .await;
        assert_eq!(res.result["total"], 3);
        assert_eq!(res.result["done"], 1);
        assert_eq!(res.result["todos"][1]["content"], "second");

        // Replace resets the list.
        let res = tool
            .run(
                &ctx(&tmp),
                &json!({"todos": [{"id": "x", "content": "only", "status": "pending"}]}),
            )
            .await;
        assert_eq!(res.result["total"], 1);
    }

    #[tokio::test]
    async fn rejects_bad_input() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = TodoWrite::default();

        let res = tool.run(&ctx(&tmp), &json!({})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = tool.run(&ctx(&tmp), &json!({"todos": []})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = tool
            .run(
                &ctx(&tmp),
                &json!({"todos": [{"id": "a", "content": "x", "status": "doing"}]}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(res.result["error"].as_str().unwrap().contains("doing"));
    }

    #[tokio::test]
    async fn lists_are_scoped_per_worktree() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        let tool = TodoWrite::default();

        tool.run(
            &ctx(&tmp1),
            &json!({"todos": [{"id": "a", "content": "one", "status": "pending"}]}),
        )
        .await;
        let res = tool
            .run(
                &ctx(&tmp2),
                &json!({"merge": true, "todos": [{"id": "b", "content": "two", "status": "pending"}]}),
            )
            .await;
        // tmp2's merge starts from an empty list, not tmp1's.
        assert_eq!(res.result["total"], 1);
        assert_eq!(res.result["todos"][0]["id"], "b");
    }
}
