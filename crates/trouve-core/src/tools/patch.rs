//! apply_patch: OpenAI's V4A patch envelope, the edit format Codex-family
//! models are trained on. One call can add, update (with moves), and delete
//! several files; all operations validate before anything is written.
//!
//! ```text
//! *** Begin Patch
//! *** Update File: src/lib.rs
//! @@ fn main() {
//!  context
//! -old line
//! +new line
//! *** Add File: notes.md
//! +hello
//! *** Delete File: obsolete.rs
//! *** End Patch
//! ```

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

pub struct ApplyPatch;

#[async_trait::async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &'static str {
        "apply_patch"
    }
    fn description(&self) -> &'static str {
        "Apply a patch in the *** Begin Patch / *** End Patch envelope format: \
         \"*** Add File: p\" (+ lines), \"*** Update File: p\" (@@ anchors with \
         context/-/+ lines, optional \"*** Move to: q\"), \"*** Delete File: p\". \
         All changes validate before any file is written."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {"type": "string", "description": "The full patch, from *** Begin Patch to *** End Patch"}
            },
            "required": ["input"]
        })
    }
    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(input) = args
            .get("input")
            .or_else(|| args.get("patch"))
            .and_then(Value::as_str)
        else {
            return ToolResult::error("missing required argument: input");
        };
        let ops = match parse(input) {
            Ok(ops) => ops,
            Err(e) => return ToolResult::error(e),
        };
        if ops.is_empty() {
            return ToolResult::error("patch contains no file operations");
        }

        // Stage everything first; only write when the whole patch applies.
        let mut writes: Vec<(std::path::PathBuf, Option<String>)> = Vec::new();
        let mut report = Vec::new();
        for op in &ops {
            match op {
                Op::Add { path, content } => {
                    let full = match ctx.resolve(path) {
                        Ok(p) => p,
                        Err(e) => return ToolResult::error(e),
                    };
                    if full.exists() {
                        return ToolResult::error(format!(
                            "cannot add {path}: file already exists (use Update File)"
                        ));
                    }
                    report.push(json!({
                        "path": path, "action": "add",
                        "adds": content.lines().count(), "dels": 0,
                    }));
                    writes.push((full, Some(content.clone())));
                }
                Op::Delete { path } => {
                    let full = match ctx.resolve(path) {
                        Ok(p) => p,
                        Err(e) => return ToolResult::error(e),
                    };
                    if !full.is_file() {
                        return ToolResult::error(format!("cannot delete {path}: not a file"));
                    }
                    let dels = std::fs::read_to_string(&full)
                        .map(|t| t.lines().count())
                        .unwrap_or(0);
                    report.push(json!({
                        "path": path, "action": "delete", "adds": 0, "dels": dels,
                    }));
                    writes.push((full, None));
                }
                Op::Update {
                    path,
                    move_to,
                    hunks,
                } => {
                    let full = match ctx.resolve(path) {
                        Ok(p) => p,
                        Err(e) => return ToolResult::error(e),
                    };
                    let content = match std::fs::read_to_string(&full) {
                        Ok(t) => t,
                        Err(e) => return ToolResult::error(format!("cannot read {path}: {e}")),
                    };
                    let updated = match apply_hunks(&content, hunks) {
                        Ok(u) => u,
                        Err(e) => return ToolResult::error(format!("{path}: {e}")),
                    };
                    let (adds, dels) = hunks.iter().fold((0, 0), |(a, d), h| {
                        (a + h.new.len() - h.keep, d + h.old.len() - h.keep)
                    });
                    match move_to {
                        Some(dest) => {
                            let dest_full = match ctx.resolve(dest) {
                                Ok(p) => p,
                                Err(e) => return ToolResult::error(e),
                            };
                            report.push(json!({
                                "path": dest, "action": "move",
                                "from": path, "adds": adds, "dels": dels,
                            }));
                            writes.push((full, None));
                            writes.push((dest_full, Some(updated)));
                        }
                        None => {
                            report.push(json!({
                                "path": path, "action": "update", "adds": adds, "dels": dels,
                            }));
                            writes.push((full, Some(updated)));
                        }
                    }
                }
            }
        }

        for (full, content) in writes {
            match content {
                Some(text) => {
                    if let Some(parent) = full.parent()
                        && let Err(e) = std::fs::create_dir_all(parent)
                    {
                        return ToolResult::error(format!("cannot create parent dirs: {e}"));
                    }
                    if let Err(e) = std::fs::write(&full, text) {
                        return ToolResult::error(format!("cannot write {}: {e}", full.display()));
                    }
                }
                None => {
                    if let Err(e) = std::fs::remove_file(&full) {
                        return ToolResult::error(format!("cannot delete {}: {e}", full.display()));
                    }
                }
            }
        }
        ToolResult::ok(json!({"files": report}))
    }
}

enum Op {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
}

struct Hunk {
    /// `@@ <anchor>` search hint; the hunk applies after this line.
    anchor: Option<String>,
    /// Pre-image: context + deleted lines, in order.
    old: Vec<String>,
    /// Post-image: context + added lines, in order.
    new: Vec<String>,
    /// Context lines (in both `old` and `new`) — for add/del counting.
    keep: usize,
}

fn parse(input: &str) -> Result<Vec<Op>, String> {
    let mut lines = input.trim().lines().peekable();
    if lines.next().map(str::trim_end) != Some("*** Begin Patch") {
        return Err("patch must start with \"*** Begin Patch\"".into());
    }
    let mut ops = Vec::new();
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line == "*** End Patch" {
            return Ok(ops);
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            while let Some(next) = lines.peek() {
                if next.starts_with("*** ") {
                    break;
                }
                let next = lines.next().unwrap();
                let body = next.strip_prefix('+').ok_or_else(|| {
                    format!("Add File {path}: every content line must start with '+', got {next:?}")
                })?;
                content.push_str(body);
                content.push('\n');
            }
            ops.push(Op::Add {
                path: path.trim().to_string(),
                content,
            });
        } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
            ops.push(Op::Delete {
                path: path.trim().to_string(),
            });
        } else if let Some(path) = line.strip_prefix("*** Update File: ") {
            let move_to = lines
                .peek()
                .and_then(|l| l.trim_end().strip_prefix("*** Move to: "))
                .map(|d| d.trim().to_string());
            if move_to.is_some() {
                lines.next();
            }
            let mut hunks: Vec<Hunk> = Vec::new();
            let mut hunk = Hunk {
                anchor: None,
                old: Vec::new(),
                new: Vec::new(),
                keep: 0,
            };
            let flush = |hunks: &mut Vec<Hunk>, hunk: &mut Hunk| {
                if !hunk.old.is_empty() || !hunk.new.is_empty() {
                    hunks.push(std::mem::replace(
                        hunk,
                        Hunk {
                            anchor: None,
                            old: Vec::new(),
                            new: Vec::new(),
                            keep: 0,
                        },
                    ));
                }
            };
            while let Some(next) = lines.peek() {
                if next.starts_with("*** ") {
                    break;
                }
                let next = lines.next().unwrap();
                if let Some(anchor) = next.strip_prefix("@@") {
                    flush(&mut hunks, &mut hunk);
                    let anchor = anchor.trim();
                    hunk.anchor = (!anchor.is_empty()).then(|| anchor.to_string());
                } else if let Some(add) = next.strip_prefix('+') {
                    hunk.new.push(add.to_string());
                } else if let Some(del) = next.strip_prefix('-') {
                    hunk.old.push(del.to_string());
                } else {
                    // Context: present in both images (a bare empty line is
                    // an empty context line).
                    let ctx_line = next.strip_prefix(' ').unwrap_or(next).to_string();
                    hunk.old.push(ctx_line.clone());
                    hunk.new.push(ctx_line);
                    hunk.keep += 1;
                }
            }
            flush(&mut hunks, &mut hunk);
            if hunks.is_empty() {
                return Err(format!("Update File {path}: no hunks"));
            }
            ops.push(Op::Update {
                path: path.trim().to_string(),
                move_to,
                hunks,
            });
        } else if !line.is_empty() {
            return Err(format!("unexpected line outside a file section: {line:?}"));
        }
    }
    Err("patch must end with \"*** End Patch\"".into())
}

/// Apply hunks to `content`, in order, each searching forward from where
/// the previous one landed.
fn apply_hunks(content: &str, hunks: &[Hunk]) -> Result<String, String> {
    let had_trailing_newline = content.ends_with('\n') || content.is_empty();
    let mut file: Vec<String> = content.lines().map(str::to_string).collect();
    let mut pos = 0usize;
    for (i, hunk) in hunks.iter().enumerate() {
        if let Some(anchor) = &hunk.anchor {
            // The anchor names an enclosing line ("@@ fn main() {"); the
            // hunk applies somewhere after it.
            match file[pos..]
                .iter()
                .position(|l| l.trim() == anchor.trim())
                .map(|off| pos + off)
            {
                Some(idx) => pos = idx + 1,
                None => return Err(format!("hunk {}: anchor {anchor:?} not found", i + 1)),
            }
        }
        if hunk.old.is_empty() {
            // Pure insertion with no context: append at the end.
            file.extend(hunk.new.iter().cloned());
            pos = file.len();
            continue;
        }
        let matches_at = |idx: usize, exact: bool| {
            file[idx..].len() >= hunk.old.len()
                && hunk.old.iter().enumerate().all(|(k, want)| {
                    let got = &file[idx + k];
                    if exact {
                        got == want
                    } else {
                        got.trim_end() == want.trim_end()
                    }
                })
        };
        let end = file.len().saturating_sub(hunk.old.len() - 1);
        let found = (pos..end)
            .find(|&idx| matches_at(idx, true))
            .or_else(|| (pos..end).find(|&idx| matches_at(idx, false)));
        let Some(idx) = found else {
            return Err(format!(
                "hunk {}: context not found (starting near {:?})",
                i + 1,
                hunk.old.first().map(String::as_str).unwrap_or("")
            ));
        };
        file.splice(idx..idx + hunk.old.len(), hunk.new.iter().cloned());
        pos = idx + hunk.new.len();
    }
    let mut out = file.join("\n");
    if had_trailing_newline && !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
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
    async fn add_update_delete_in_one_patch() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(tmp.path().join("gone.txt"), "bye\n").unwrap();

        let patch = "*** Begin Patch\n\
                     *** Update File: a.txt\n\
                     @@\n \
                     one\n\
                     -two\n\
                     +TWO\n \
                     three\n\
                     *** Add File: new/b.txt\n\
                     +hello\n\
                     +world\n\
                     *** Delete File: gone.txt\n\
                     *** End Patch";
        let res = ApplyPatch.run(&ctx(&tmp), &json!({"input": patch})).await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new/b.txt")).unwrap(),
            "hello\nworld\n"
        );
        assert!(!tmp.path().join("gone.txt").exists());
        let files = res.result["files"].as_array().unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0]["action"], "update");
        assert_eq!(files[0]["adds"], 1);
        assert_eq!(files[0]["dels"], 1);
    }

    #[tokio::test]
    async fn anchors_disambiguate_repeated_context() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("f.rs"),
            "fn a() {\n    x = 1;\n}\nfn b() {\n    x = 1;\n}\n",
        )
        .unwrap();

        // Without the anchor this would hit fn a's line; the anchor targets
        // fn b's.
        let patch = "*** Begin Patch\n\
                     *** Update File: f.rs\n\
                     @@ fn b() {\n\
                     -    x = 1;\n\
                     +    x = 2;\n\
                     *** End Patch";
        let res = ApplyPatch.run(&ctx(&tmp), &json!({"input": patch})).await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.rs")).unwrap(),
            "fn a() {\n    x = 1;\n}\nfn b() {\n    x = 2;\n}\n"
        );
    }

    #[tokio::test]
    async fn move_renames_while_updating() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("old.txt"), "keep\nchange\n").unwrap();

        let patch = "*** Begin Patch\n\
                     *** Update File: old.txt\n\
                     *** Move to: renamed.txt\n\
                     @@\n \
                     keep\n\
                     -change\n\
                     +changed\n\
                     *** End Patch";
        let res = ApplyPatch.run(&ctx(&tmp), &json!({"input": patch})).await;
        assert_eq!(
            res.status,
            trouve_protocol::ToolStatus::Ok,
            "{:?}",
            res.result
        );
        assert!(!tmp.path().join("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("renamed.txt")).unwrap(),
            "keep\nchanged\n"
        );
    }

    #[tokio::test]
    async fn failed_hunk_leaves_every_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();

        // The Add would succeed but the Update's context is wrong; nothing
        // may be written.
        let patch = "*** Begin Patch\n\
                     *** Add File: b.txt\n\
                     +new\n\
                     *** Update File: a.txt\n\
                     -nonexistent\n\
                     +x\n\
                     *** End Patch";
        let res = ApplyPatch.run(&ctx(&tmp), &json!({"input": patch})).await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(!tmp.path().join("b.txt").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "one\n"
        );
    }

    #[tokio::test]
    async fn rejects_malformed_envelopes() {
        let tmp = tempfile::tempdir().unwrap();
        let res = ApplyPatch
            .run(&ctx(&tmp), &json!({"input": "no envelope"}))
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);

        let res = ApplyPatch
            .run(
                &ctx(&tmp),
                &json!({"input": "*** Begin Patch\n*** Update File: x\n-a\n+b"}),
            )
            .await;
        assert_eq!(res.status, trouve_protocol::ToolStatus::Error);
        assert!(res.result["error"].as_str().unwrap().contains("End Patch"));
    }
}
