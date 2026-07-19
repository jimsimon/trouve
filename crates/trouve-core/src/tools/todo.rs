//! The agent's task list: a progress artifact the chat UI renders as a
//! checklist. State is seeded from and persisted back to its owning thread;
//! each update's tool result carries the full list, so the transcript (and
//! any model resuming from it) always sees the current plan.

use serde_json::{Value, json};
use trouve_protocol::{TodoItem, TodoStatus};

use super::{Tool, ToolCtx, ToolResult};

const STATUSES: [&str; 4] = ["pending", "in_progress", "completed", "cancelled"];

pub struct TodoWrite;

#[async_trait::async_trait]
impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }
    fn description(&self) -> &'static str {
        "Create or update your task list for this thread. Use it to plan multi-step work and \
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
            incoming.push(todo);
        }
        let merge = args.get("merge").and_then(Value::as_bool).unwrap_or(false);

        let mut list = ctx.todos.lock().unwrap();
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
            .filter(|t| matches!(t.status, TodoStatus::Completed | TodoStatus::Cancelled))
            .count();
        ToolResult::ok(json!({
            "todos": list.clone(),
            "done": done,
            "total": list.len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(tmp: &tempfile::TempDir, thread_id: &str) -> ToolCtx {
        ToolCtx {
            worktree: tmp.path().to_path_buf(),
            thread_id: thread_id.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn replace_then_merge_updates_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = TodoWrite;
        let ctx = ctx(&tmp, "th_1");

        let res = tool
            .run(
                &ctx,
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
                &ctx,
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
                &ctx,
                &json!({"todos": [{"id": "x", "content": "only", "status": "pending"}]}),
            )
            .await;
        assert_eq!(res.result["total"], 1);
    }

    #[tokio::test]
    async fn rejects_bad_input() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = TodoWrite;

        let res = tool.run(&ctx(&tmp, "th_1"), &json!({})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = tool.run(&ctx(&tmp, "th_1"), &json!({"todos": []})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = tool
            .run(
                &ctx(&tmp, "th_1"),
                &json!({"todos": [{"id": "a", "content": "x", "status": "doing"}]}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(res.result["error"].as_str().unwrap().contains("doing"));
    }

    #[tokio::test]
    async fn lists_are_scoped_per_thread_context() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = TodoWrite;
        let first = ctx(&tmp, "th_1");
        let second = ctx(&tmp, "th_2");

        tool.run(
            &first,
            &json!({"todos": [{"id": "a", "content": "one", "status": "pending"}]}),
        )
        .await;
        let res = tool
            .run(
                &second,
                &json!({"merge": true, "todos": [{"id": "b", "content": "two", "status": "pending"}]}),
            )
            .await;
        // The second thread starts from an empty list despite sharing a
        // session worktree with the first.
        assert_eq!(res.result["total"], 1);
        assert_eq!(res.result["todos"][0]["id"], "b");
    }
}
