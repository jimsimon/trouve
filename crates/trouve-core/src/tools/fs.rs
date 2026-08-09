//! File tools: read, write, list.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

use super::{Tool, ToolCtx, ToolResult};

const MAX_READ_BYTES: usize = 64 * 1024;
/// Images larger than this are rejected rather than truncated (a partial
/// image is useless as vision input).
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Text files larger than this are refused up front. Ranged reads are
/// streamed and stop at the requested page, but retaining this scan bound
/// also protects against pathological single-line files and enormous offsets.
const MAX_TEXT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WRITE_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EDIT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 1_000;

/// MIME type for paths `read_file` should return as vision content.
fn image_mime(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path).extension()?.to_str()?;
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a file from the workspace. Text returns at most 64KB (use offset to page through \
         larger files); images (png/jpeg/gif/webp) are returned as vision content."
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
        // Images return as vision content ("_images" — the engine moves it
        // onto the provider message as native image input).
        if let Some(mime) = image_mime(path) {
            use base64::Engine as _;
            // Check the size before reading so an 8 GB "image" isn't buffered
            // in full only to be rejected.
            if let Ok(meta) = tokio::fs::metadata(&full).await
                && meta.len() > MAX_IMAGE_BYTES as u64
            {
                return ToolResult::error(format!(
                    "image {path} is {} bytes; the limit is {MAX_IMAGE_BYTES}",
                    meta.len()
                ));
            }
            let bytes = match tokio::fs::read(&full).await {
                Ok(b) => b,
                Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
            };
            if bytes.len() > MAX_IMAGE_BYTES {
                return ToolResult::error(format!(
                    "image {path} is {} bytes; the limit is {MAX_IMAGE_BYTES}",
                    bytes.len()
                ));
            }
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return ToolResult::ok(json!({
                "note": format!("{mime} image, {} bytes, attached as vision content", bytes.len()),
                "_images": [{"mime": mime, "data": data}],
            }));
        }
        if let Ok(meta) = tokio::fs::metadata(&full).await
            && meta.len() > MAX_TEXT_FILE_BYTES
        {
            return ToolResult::error(format!(
                "{path} is {} bytes; too large to read whole (limit {MAX_TEXT_FILE_BYTES}). \
                 Use grep to find the relevant lines.",
                meta.len()
            ));
        }
        let file = match tokio::fs::File::open(&full).await {
            Ok(file) => file,
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

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut line_number = 0usize;
        let mut lines_read = 0usize;
        let mut out = String::new();
        let mut truncated = false;
        let mut total_lines = None;
        loop {
            line.clear();
            let read = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    return ToolResult::error("file read cancelled");
                }
                read = reader.read_line(&mut line) => read,
            };
            let bytes = match read {
                Ok(bytes) => bytes,
                Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
            };
            if bytes == 0 {
                total_lines = Some(line_number);
                break;
            }
            line_number += 1;
            if line_number < offset {
                continue;
            }

            let content = line.strip_suffix('\n').unwrap_or(&line);
            let content = content.strip_suffix('\r').unwrap_or(content);
            if out.len() + content.len() + 1 > MAX_READ_BYTES {
                truncated = true;
                break;
            }
            out.push_str(content);
            out.push('\n');
            lines_read += 1;

            if limit.is_some_and(|limit| lines_read >= limit) {
                let has_more = match reader.fill_buf().await {
                    Ok(buffer) => !buffer.is_empty(),
                    Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
                };
                truncated = has_more;
                if !has_more {
                    total_lines = Some(line_number);
                }
                break;
            }
        }
        ToolResult::ok(json!({
            "content": out,
            "truncated": truncated,
            "lines_read": lines_read,
            "next_offset": truncated.then_some(offset + lines_read),
            "total_lines": total_lines,
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
        if content.len() > MAX_WRITE_FILE_BYTES {
            return ToolResult::error(format!(
                "content is {} bytes; write_file's limit is {MAX_WRITE_FILE_BYTES}",
                content.len()
            ));
        }
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("write cancelled");
        }
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

pub struct EditFile;

#[async_trait::async_trait]
impl Tool for EditFile {
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "Replace an exact string in a workspace file. old_string must match the file content \
         exactly (including whitespace and indentation) and must appear exactly once unless \
         replace_all is set. Prefer this over write_file for changing part of a file."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Workspace-relative file path"},
                "old_string": {"type": "string", "description": "Exact text to replace; include surrounding lines to make it unique"},
                "new_string": {"type": "string", "description": "Replacement text"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence (default: false)"}
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let (Some(path), Some(old), Some(new)) = (
            args.get("path").and_then(Value::as_str),
            args.get("old_string").and_then(Value::as_str),
            args.get("new_string").and_then(Value::as_str),
        ) else {
            return ToolResult::error("missing required arguments: path, old_string, new_string");
        };
        if old.is_empty() {
            return ToolResult::error(
                "old_string must not be empty (use write_file to create a file)",
            );
        }
        if old == new {
            return ToolResult::error("old_string and new_string are identical");
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let full = match ctx.resolve(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        if let Ok(metadata) = tokio::fs::metadata(&full).await
            && metadata.len() > MAX_EDIT_FILE_BYTES
        {
            return ToolResult::error(format!(
                "{path} is {} bytes; edit_file's limit is {MAX_EDIT_FILE_BYTES}. Use apply_patch or a targeted shell transformation.",
                metadata.len()
            ));
        }
        let content = match tokio::fs::read_to_string(&full).await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
        };
        let old = old.to_owned();
        let new = new.to_owned();
        let replaced = tokio::task::spawn_blocking(move || {
            let count = content.matches(&old).count();
            if count == 0 {
                return Err("old_string not found".to_owned());
            }
            if count > 1 && !replace_all {
                return Err(format!("old_string matches {count} places"));
            }
            let updated = if replace_all {
                content.replace(&old, &new)
            } else {
                content.replacen(&old, &new, 1)
            };
            Ok((updated, count))
        })
        .await;
        let (updated, count) = match replaced {
            Ok(Ok(replaced)) => replaced,
            Ok(Err(error)) if error == "old_string not found" => {
                return ToolResult::error(format!(
                    "old_string not found in {path}; re-read the file and match its content exactly"
                ));
            }
            Ok(Err(error)) => {
                return ToolResult::error(format!(
                    "{error} in {path}; add surrounding context to make it unique, or set replace_all"
                ));
            }
            Err(error) => {
                return ToolResult::error(format!("edit worker failed: {error}"));
            }
        };
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("edit cancelled");
        }
        match tokio::fs::write(&full, &updated).await {
            Ok(()) => ToolResult::ok(json!({
                "replacements": if replace_all { count } else { 1 },
            })),
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
        let mut truncated = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    return ToolResult::error("directory listing cancelled");
                }
                next = rd.next_entry() => next,
            };
            let entry = match next {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    return ToolResult::error(format!("cannot list {rel}: {error}"));
                }
            };
            if entries.len() >= MAX_LIST_ENTRIES {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let kind = match entry.file_type().await {
                Ok(ft) if ft.is_dir() => "dir",
                Ok(ft) if ft.is_symlink() => "symlink",
                _ => "file",
            };
            entries.push(json!({"name": name, "kind": kind}));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        ToolResult::ok(json!({"entries": entries, "truncated": truncated}))
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
        assert_eq!(res.result["total_lines"], Value::Null);
        assert_eq!(res.result["next_offset"], 3);

        let res = ListDir.run(&ctx, &json!({"path": "a"})).await;
        assert_eq!(res.result["entries"][0]["name"], "b.txt");
    }

    #[tokio::test]
    async fn ranged_read_does_not_consume_an_invalid_tail() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("mixed.txt"), b"first\n\xff\n").unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };

        let res = ReadFile
            .run(&ctx, &json!({"path": "mixed.txt", "offset": 1, "limit": 1}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["content"], "first\n");
        assert_eq!(res.result["truncated"], true);
        assert_eq!(res.result["next_offset"], 2);
    }

    #[tokio::test]
    async fn read_file_returns_images_as_vision_content() {
        use base64::Engine as _;
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        // A 1x1 transparent PNG.
        let png = base64::engine::general_purpose::STANDARD.decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
        ).unwrap();
        std::fs::write(tmp.path().join("dot.png"), &png).unwrap();

        let res = ReadFile.run(&ctx, &json!({"path": "dot.png"})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["_images"][0]["mime"], "image/png");
        let data = res.result["_images"][0]["data"].as_str().unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap(),
            png
        );
        assert!(res.result["note"].as_str().unwrap().contains("image/png"));
    }

    #[tokio::test]
    async fn edit_file_replaces_unique_match() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        std::fs::write(tmp.path().join("f.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "beta", "new_string": "BETA"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_missing_and_ambiguous_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        std::fs::write(tmp.path().join("f.txt"), "x\nx\n").unwrap();

        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "nope", "new_string": "n"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(res.result["error"].as_str().unwrap().contains("not found"));

        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "x", "new_string": "y"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(res.result["error"].as_str().unwrap().contains("2 places"));

        // replace_all resolves the ambiguity.
        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "x", "new_string": "y", "replace_all": true}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(res.result["replacements"], 2);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "y\ny\n"
        );
    }

    #[tokio::test]
    async fn edit_file_rejects_degenerate_arguments() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            worktree: tmp.path().to_path_buf(),
            ..Default::default()
        };
        std::fs::write(tmp.path().join("f.txt"), "abc").unwrap();

        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "", "new_string": "y"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = EditFile
            .run(
                &ctx,
                &json!({"path": "f.txt", "old_string": "abc", "new_string": "abc"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
    }
}
