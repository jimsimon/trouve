//! File tools: read, write, list.

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const MAX_READ_BYTES: usize = 64 * 1024;

pub struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a UTF-8 text file from the workspace. Returns at most 64KB; use offset to page through larger files."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Workspace-relative file path"},
                "offset": {"type": "integer", "description": "Line to start from (1-based)", "minimum": 1},
                "limit": {"type": "integer", "description": "Maximum number of lines", "minimum": 1}
            },
            "required": ["path"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: path");
        };
        let full = match ctx.resolve(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let text = match tokio::fs::read_to_string(&full).await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
        };
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|l| l as usize);

        let mut out = String::new();
        let mut truncated = false;
        let mut remaining = limit;
        for line in text.lines().skip(offset - 1) {
            if remaining == Some(0) || out.len() + line.len() + 1 > MAX_READ_BYTES {
                truncated = true;
                break;
            }
            out.push_str(line);
            out.push('\n');
            if let Some(r) = remaining.as_mut() {
                *r -= 1;
            }
        }
        ToolResult::ok(json!({
            "content": out,
            "truncated": truncated,
            "total_lines": text.lines().count(),
        }))
    }
}

pub struct WriteFile;

#[async_trait::async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a file in the workspace with the given content. Parent directories are created."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Workspace-relative file path"},
                "content": {"type": "string", "description": "Full new file content"}
            },
            "required": ["path", "content"]
        })
    }
    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let (Some(path), Some(content)) = (
            args.get("path").and_then(Value::as_str),
            args.get("content").and_then(Value::as_str),
        ) else {
            return ToolResult::error("missing required arguments: path, content");
        };
        let full = match ctx.resolve(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        if let Some(parent) = full.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return ToolResult::error(format!("cannot create parent dirs for {path}: {e}"));
        }
        match tokio::fs::write(&full, content).await {
            Ok(()) => ToolResult::ok(json!({"bytes_written": content.len()})),
            Err(e) => ToolResult::error(format!("cannot write {path}: {e}")),
        }
    }
}

pub struct ListDir;

#[async_trait::async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn description(&self) -> &'static str {
        "List the entries of a workspace directory (non-recursive)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Workspace-relative directory (default: workspace root)"}
            }
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let rel = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let full = match ctx.resolve(rel) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        let mut rd = match tokio::fs::read_dir(&full).await {
            Ok(rd) => rd,
            Err(e) => return ToolResult::error(format!("cannot list {rel}: {e}")),
        };
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let kind = match entry.file_type().await {
                Ok(ft) if ft.is_dir() => "dir",
                Ok(ft) if ft.is_symlink() => "symlink",
                _ => "file",
            };
            entries.push(json!({"name": name, "kind": kind}));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        ToolResult::ok(json!({"entries": entries}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = WriteFile
            .run(
                &ctx,
                &json!({"path": "a/b.txt", "content": "line1\nline2\nline3\n"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);

        let res = ReadFile
            .run(&ctx, &json!({"path": "a/b.txt", "offset": 2, "limit": 1}))
            .await;
        assert_eq!(res.result["content"], "line2\n");
        assert_eq!(res.result["total_lines"], 3);

        let res = ListDir.run(&ctx, &json!({"path": "a"})).await;
        assert_eq!(res.result["entries"][0]["name"], "b.txt");
    }
}
