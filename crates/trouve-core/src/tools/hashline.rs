//! Compact, snapshot-validated line edits inspired by Oh My Pi's hashline format.
//!
//! `read_file(format = "hashline")` returns a whole-file snapshot tag plus
//! numbered lines. `hashline_edit` consumes that tag and one or more line
//! operations. Every file and operation is validated before staged writes are
//! promoted, and the engine's normal mutation lane surrounds the entire call.
//!
//! Supported operations:
//!
//! ```text
//! [src/lib.rs#A1B2C3D4E5F6]
//! PUT 8.=10:
//! +replacement line
//! +another line
//! PUT <20:
//! +inserted before line 20
//! PUT >$:
//! +appended at end of file
//! CUT 30.=32
//! ```

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use trouve_protocol::ToolStatus;

use super::{Tool, ToolCtx, ToolResult};

const SNAPSHOT_HEX_LEN: usize = 12;
const MAX_HASHLINE_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HASHLINE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const STALE_CONTEXT_LINES: usize = 7;

/// Render a text-file page in hashline form. The tag covers the complete file,
/// while only the requested lines count against the response-size limit.
pub(crate) fn render_read(
    path: &str,
    content: &str,
    offset: usize,
    limit: Option<usize>,
    max_bytes: usize,
) -> Result<Value, String> {
    let snapshot = snapshot_tag(content);
    let lines = logical_lines(content);
    let total_lines = lines.len();
    let mut output = format!("[{path}#{snapshot}]\n");
    let mut lines_read = 0usize;
    let start = offset.saturating_sub(1).min(total_lines);
    let requested = limit.unwrap_or(usize::MAX);

    for (index, line) in lines.iter().enumerate().skip(start).take(requested) {
        let rendered = format!("{}:{line}\n", index + 1);
        if output.len() + rendered.len() > max_bytes {
            if lines_read == 0 {
                return Err(format!(
                    "line {} of {path} is too large for the {max_bytes}-byte read limit",
                    index + 1
                ));
            }
            break;
        }
        output.push_str(&rendered);
        lines_read += 1;
    }

    let next = start + lines_read;
    let truncated = next < total_lines;
    Ok(json!({
        "content": output,
        "format": "hashline",
        "snapshot": snapshot,
        "truncated": truncated,
        "lines_read": lines_read,
        "next_offset": truncated.then_some(next + 1),
        "total_lines": total_lines,
    }))
}

pub struct HashlineEdit;

#[async_trait::async_trait]
impl Tool for HashlineEdit {
    fn name(&self) -> &'static str {
        "hashline_edit"
    }

    fn description(&self) -> &'static str {
        "Apply compact, line-numbered edits using snapshot tags returned by \
         read_file with format=\"hashline\". Supports multi-file [path#TAG] \
         sections with PUT N.=M:, PUT <N:, PUT >N:, PUT >$:, and CUT N.=M. \
         Every section is preflighted before any file is changed; stale tags \
         return refreshed compact context. Never invent or reuse a tag after \
         the target file changes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Hashline sections and operations. Payload rows begin with '+'."
                }
            },
            "required": ["input"]
        })
    }

    fn mutates(&self) -> bool {
        true
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        let Some(input) = args.get("input").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: input");
        };
        if input.len() > MAX_HASHLINE_INPUT_BYTES {
            return ToolResult::error(format!(
                "hashline input is {} bytes; the limit is {MAX_HASHLINE_INPUT_BYTES}",
                input.len()
            ));
        }
        let input = input.to_owned();
        let ctx = ctx.clone();
        match tokio::task::spawn_blocking(move || apply_hashline(&ctx, &input)).await {
            Ok(result) => result,
            Err(error) => ToolResult::error(format!("hashline worker failed: {error}")),
        }
    }
}

#[derive(Debug)]
struct Section {
    path: String,
    expected_snapshot: String,
    operations: Vec<Operation>,
}

#[derive(Debug)]
enum Operation {
    Replace {
        start: usize,
        end: usize,
        body: Vec<String>,
    },
    Insert {
        gap: Gap,
        body: Vec<String>,
    },
    Cut {
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, Copy)]
enum Gap {
    Before(usize),
    After(usize),
    End,
}

#[derive(Debug)]
struct PreparedFile {
    path: String,
    full_path: PathBuf,
    original: String,
    updated: String,
    adds: usize,
    deletes: usize,
    old_snapshot: String,
    new_snapshot: String,
}

#[derive(Debug)]
struct StaleFile {
    path: String,
    expected: String,
    current: String,
    context: String,
}

fn apply_hashline(ctx: &ToolCtx, input: &str) -> ToolResult {
    if ctx.cancel.is_cancelled() {
        return ToolResult::error("hashline edit cancelled");
    }
    let sections = match parse(input) {
        Ok(sections) if sections.is_empty() => {
            return ToolResult::error("hashline input contains no file sections");
        }
        Ok(sections) => sections,
        Err(error) => return ToolResult::error(error),
    };

    let mut canonical_paths = HashSet::new();
    let mut prepared = Vec::with_capacity(sections.len());
    let mut stale = Vec::new();

    // The engine acquires the per-session mutation lane before invoking this
    // mutating tool. Read and validate every snapshot only after that point.
    for section in &sections {
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("hashline edit cancelled");
        }
        let resolved = match ctx.resolve(&section.path) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        let full_path = match std::fs::canonicalize(&resolved) {
            Ok(path) if path.is_file() => path,
            Ok(_) => {
                return ToolResult::error(format!(
                    "cannot edit {}: path is not a file",
                    section.path
                ));
            }
            Err(error) => {
                return ToolResult::error(format!("cannot read {}: {error}", section.path));
            }
        };
        if !canonical_paths.insert(full_path.clone()) {
            return ToolResult::error(format!(
                "{} is targeted by more than one hashline section",
                section.path
            ));
        }
        if std::fs::metadata(&full_path)
            .is_ok_and(|metadata| metadata.len() > MAX_HASHLINE_FILE_BYTES)
        {
            return ToolResult::error(format!(
                "{} exceeds the {MAX_HASHLINE_FILE_BYTES}-byte hashline limit",
                section.path
            ));
        }
        let original = match std::fs::read_to_string(&full_path) {
            Ok(content) => content,
            Err(error) => {
                return ToolResult::error(format!("cannot read {}: {error}", section.path));
            }
        };
        let current = snapshot_tag(&original);
        if current != section.expected_snapshot {
            stale.push(stale_file(section, &original, current));
            continue;
        }
        let (updated, adds, deletes) = match apply_operations(section, &original) {
            Ok(result) => result,
            Err(error) => return ToolResult::error(format!("{}: {error}", section.path)),
        };
        if updated == original {
            return ToolResult::error(format!(
                "{}: hashline operations produce no changes",
                section.path
            ));
        }
        let new_snapshot = snapshot_tag(&updated);
        prepared.push(PreparedFile {
            path: section.path.clone(),
            full_path,
            original,
            updated,
            adds,
            deletes,
            old_snapshot: current,
            new_snapshot,
        });
    }

    if !stale.is_empty() {
        return stale_result(stale);
    }
    if ctx.cancel.is_cancelled() {
        return ToolResult::error("hashline edit cancelled");
    }

    // Stage every output beside its destination before promoting any write.
    // This catches allocation, permissions, and disk-space failures before the
    // first live file changes.
    let mut staged = Vec::with_capacity(prepared.len());
    for file in &prepared {
        let Some(parent) = file.full_path.parent() else {
            return ToolResult::error(format!("cannot resolve parent for {}", file.path));
        };
        let mut temp = match tempfile::NamedTempFile::new_in(parent) {
            Ok(temp) => temp,
            Err(error) => {
                return ToolResult::error(format!("cannot stage {}: {error}", file.path));
            }
        };
        if let Ok(metadata) = std::fs::metadata(&file.full_path)
            && let Err(error) = temp.as_file().set_permissions(metadata.permissions())
        {
            return ToolResult::error(format!(
                "cannot preserve permissions for {}: {error}",
                file.path
            ));
        }
        if let Err(error) = temp.as_file_mut().write_all(file.updated.as_bytes()) {
            return ToolResult::error(format!("cannot stage {}: {error}", file.path));
        }
        if let Err(error) = temp.as_file_mut().flush() {
            return ToolResult::error(format!("cannot flush staged {}: {error}", file.path));
        }
        staged.push(Some(temp));
    }

    // Recheck after staging. The mutation lane excludes other trouve writes;
    // this second check also catches an external editor racing the preflight.
    let mut raced = Vec::new();
    for (index, file) in prepared.iter().enumerate() {
        let current_content = match std::fs::read_to_string(&file.full_path) {
            Ok(content) => content,
            Err(error) => {
                return ToolResult::error(format!(
                    "cannot revalidate {} before commit: {error}",
                    file.path
                ));
            }
        };
        if current_content != file.original {
            raced.push(stale_file(
                &sections[index],
                &current_content,
                snapshot_tag(&current_content),
            ));
        }
    }
    if !raced.is_empty() {
        return stale_result(raced);
    }
    if ctx.cancel.is_cancelled() {
        return ToolResult::error("hashline edit cancelled");
    }

    // Promotion is atomic per file. If a later promotion fails, restore every
    // previously promoted file from the already-retained preimage.
    for (index, file) in prepared.iter().enumerate() {
        let temp = staged[index].take().expect("staged file is present");
        if let Err(error) = temp.persist(&file.full_path) {
            let mut rollback_errors = Vec::new();
            for restored in prepared[..index].iter().rev() {
                if let Err(rollback) =
                    std::fs::write(&restored.full_path, restored.original.as_bytes())
                {
                    rollback_errors.push(format!("{}: {rollback}", restored.path));
                }
            }
            let rollback = if rollback_errors.is_empty() {
                "previous files were restored".to_owned()
            } else {
                format!("rollback also failed for {}", rollback_errors.join(", "))
            };
            return ToolResult::error(format!(
                "cannot promote staged {}: {}; {rollback}",
                file.path, error.error
            ));
        }
    }

    ToolResult::ok(json!({
        "files": prepared.iter().map(|file| json!({
            "path": file.path,
            "action": "update",
            "adds": file.adds,
            "dels": file.deletes,
            "previous_snapshot": file.old_snapshot,
            "snapshot": file.new_snapshot,
        })).collect::<Vec<_>>()
    }))
}

fn parse(input: &str) -> Result<Vec<Section>, String> {
    let lines = input.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut sections = Vec::new();

    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() || line.starts_with('#') {
            index += 1;
            continue;
        }
        let (path, expected_snapshot) =
            parse_header(line).map_err(|error| format!("line {}: {error}", index + 1))?;
        index += 1;
        let mut operations = Vec::new();

        while index < lines.len() {
            let raw = lines[index];
            let line = raw.trim();
            if line.starts_with('[') {
                break;
            }
            if line.is_empty() || line.starts_with('#') {
                index += 1;
                continue;
            }
            if let Some(target) = line.strip_prefix("PUT ") {
                let operation_line = index + 1;
                let Some(target) = target.strip_suffix(':') else {
                    return Err(format!(
                        "line {}: PUT requires ':' followed by '+' payload rows",
                        operation_line
                    ));
                };
                index += 1;
                let mut body = Vec::new();
                while index < lines.len() {
                    let payload = lines[index];
                    let Some(payload) = payload.strip_prefix('+') else {
                        break;
                    };
                    body.push(payload.to_owned());
                    index += 1;
                }
                if body.is_empty() {
                    return Err(format!(
                        "line {}: PUT requires at least one '+' payload row; use CUT to delete",
                        operation_line
                    ));
                }
                operations.push(
                    parse_put(target.trim(), body)
                        .map_err(|error| format!("line {operation_line}: {error}"))?,
                );
                continue;
            }
            if let Some(target) = line.strip_prefix("CUT ") {
                let (start, end) = parse_range(target.trim())
                    .map_err(|error| format!("line {}: {error}", index + 1))?;
                operations.push(Operation::Cut { start, end });
                index += 1;
                continue;
            }
            return Err(format!(
                "line {}: expected PUT, CUT, or another [path#TAG] section; got {raw:?}",
                index + 1
            ));
        }

        if operations.is_empty() {
            return Err(format!(
                "[{path}#{expected_snapshot}] contains no operations"
            ));
        }
        sections.push(Section {
            path,
            expected_snapshot,
            operations,
        });
    }

    Ok(sections)
}

fn parse_header(line: &str) -> Result<(String, String), String> {
    let inner = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or_else(|| "file section must be exactly [path#TAG]".to_owned())?;
    let (path, snapshot) = inner
        .rsplit_once('#')
        .ok_or_else(|| "file section is missing #TAG".to_owned())?;
    let path = path.trim();
    let snapshot = snapshot.trim().to_ascii_uppercase();
    if path.is_empty() {
        return Err("file section path must not be empty".into());
    }
    if snapshot.len() != SNAPSHOT_HEX_LEN || !snapshot.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "snapshot tag must be {SNAPSHOT_HEX_LEN} hexadecimal characters"
        ));
    }
    Ok((path.to_owned(), snapshot))
}

fn parse_put(target: &str, body: Vec<String>) -> Result<Operation, String> {
    if target == ">$" {
        return Ok(Operation::Insert {
            gap: Gap::End,
            body,
        });
    }
    if let Some(line) = target.strip_prefix('<') {
        return Ok(Operation::Insert {
            gap: Gap::Before(parse_line_number(line)?),
            body,
        });
    }
    if let Some(line) = target.strip_prefix('>') {
        return Ok(Operation::Insert {
            gap: Gap::After(parse_line_number(line)?),
            body,
        });
    }
    let (start, end) = parse_range(target)?;
    Ok(Operation::Replace { start, end, body })
}

fn parse_range(value: &str) -> Result<(usize, usize), String> {
    let (start, end) = match value.split_once(".=") {
        Some((start, end)) => (parse_line_number(start)?, parse_line_number(end)?),
        None => {
            let line = parse_line_number(value)?;
            (line, line)
        }
    };
    if end < start {
        return Err(format!("range end {end} precedes start {start}"));
    }
    Ok((start, end))
}

fn parse_line_number(value: &str) -> Result<usize, String> {
    let line = value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{value:?} is not a positive line number"))?;
    if line == 0 {
        return Err("line numbers are 1-based".into());
    }
    Ok(line)
}

fn apply_operations(section: &Section, original: &str) -> Result<(String, usize, usize), String> {
    let shape = TextShape::from_content(original);
    let line_count = shape.lines.len();
    let mut ranges = BTreeMap::<usize, (usize, Vec<String>)>::new();
    let mut insertions = BTreeMap::<usize, Vec<String>>::new();
    let mut claimed = vec![false; line_count + 1];
    let mut adds = 0usize;
    let mut deletes = 0usize;

    for operation in &section.operations {
        match operation {
            Operation::Replace { start, end, body } => {
                validate_range(*start, *end, line_count)?;
                claim_range(&mut claimed, *start, *end)?;
                ranges.insert(*start, (*end, body.clone()));
                adds += body.len();
                deletes += end - start + 1;
            }
            Operation::Cut { start, end } => {
                validate_range(*start, *end, line_count)?;
                claim_range(&mut claimed, *start, *end)?;
                ranges.insert(*start, (*end, Vec::new()));
                deletes += end - start + 1;
            }
            Operation::Insert { gap, body } => {
                let gap = resolve_gap(*gap, line_count)?;
                if insertions.insert(gap, body.clone()).is_some() {
                    return Err(format!(
                        "more than one insertion targets the gap after line {gap}"
                    ));
                }
                adds += body.len();
            }
        }
    }

    for gap in insertions.keys().copied() {
        if ranges
            .iter()
            .any(|(start, (end, _))| gap > start.saturating_sub(1) && gap < *end)
        {
            return Err(format!(
                "insertion after line {gap} falls inside a replaced or cut range"
            ));
        }
    }

    let mut output = Vec::with_capacity(line_count + adds);
    if let Some(lines) = insertions.get(&0) {
        output.extend(lines.iter().cloned());
    }
    let mut line = 1usize;
    while line <= line_count {
        if let Some((end, replacement)) = ranges.get(&line) {
            output.extend(replacement.iter().cloned());
            line = end + 1;
            if let Some(lines) = insertions.get(end) {
                output.extend(lines.iter().cloned());
            }
            continue;
        }
        output.push(shape.lines[line - 1].clone());
        if let Some(lines) = insertions.get(&line) {
            output.extend(lines.iter().cloned());
        }
        line += 1;
    }

    Ok((shape.rebuild(&output), adds, deletes))
}

fn validate_range(start: usize, end: usize, line_count: usize) -> Result<(), String> {
    if start == 0 || end < start || end > line_count {
        return Err(format!(
            "range {start}.={end} is outside the file's 1..={line_count} lines"
        ));
    }
    Ok(())
}

fn claim_range(claimed: &mut [bool], start: usize, end: usize) -> Result<(), String> {
    if let Some(offset) = claimed[start..=end].iter().position(|claimed| *claimed) {
        return Err(format!(
            "line {} is targeted by overlapping operations",
            start + offset
        ));
    }
    for slot in &mut claimed[start..=end] {
        *slot = true;
    }
    Ok(())
}

fn resolve_gap(gap: Gap, line_count: usize) -> Result<usize, String> {
    match gap {
        Gap::End => Ok(line_count),
        Gap::Before(1) => Ok(0),
        Gap::Before(line) if line <= line_count => Ok(line - 1),
        Gap::After(line) if line <= line_count => Ok(line),
        Gap::Before(line) => Err(format!(
            "cannot insert before line {line}; the file has {line_count} lines"
        )),
        Gap::After(line) => Err(format!(
            "cannot insert after line {line}; the file has {line_count} lines"
        )),
    }
}

fn stale_file(section: &Section, content: &str, current: String) -> StaleFile {
    let lines = logical_lines(content);
    let focus = section
        .operations
        .first()
        .map(|operation| match operation {
            Operation::Replace { start, .. } | Operation::Cut { start, .. } => *start,
            Operation::Insert {
                gap: Gap::Before(line),
                ..
            } => *line,
            Operation::Insert {
                gap: Gap::After(line),
                ..
            } => *line,
            Operation::Insert { gap: Gap::End, .. } => lines.len().max(1),
        })
        .unwrap_or(1);
    let start = focus.saturating_sub(STALE_CONTEXT_LINES / 2).max(1);
    let end = (start + STALE_CONTEXT_LINES - 1).min(lines.len());
    let mut context = format!("[{}#{current}]\n", section.path);
    if start <= end {
        for line in start..=end {
            context.push_str(&format!("{line}:{}\n", lines[line - 1]));
        }
    }
    StaleFile {
        path: section.path.clone(),
        expected: section.expected_snapshot.clone(),
        current,
        context,
    }
}

fn stale_result(stale: Vec<StaleFile>) -> ToolResult {
    let names = stale
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    ToolResult {
        status: ToolStatus::Error,
        result: json!({
            "error": format!(
                "stale hashline snapshot for {names}; no files were changed. Re-read the refreshed context and retry with its current tag"
            ),
            "code": "stale_snapshot",
            "stale": stale.into_iter().map(|file| json!({
                "path": file.path,
                "expected_snapshot": file.expected,
                "current_snapshot": file.current,
                "context": file.context,
            })).collect::<Vec<_>>(),
        }),
    }
}

fn snapshot_tag(content: &str) -> String {
    let normalized = normalized_for_hash(content);
    let digest = Sha256::digest(normalized.as_bytes());
    hex::encode_upper(&digest[..SNAPSHOT_HEX_LEN / 2])
}

fn normalized_for_hash(content: &str) -> String {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut normalized = String::with_capacity(content.len());
    for chunk in content.split_inclusive('\n') {
        let has_newline = chunk.ends_with('\n');
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        normalized.push_str(line);
        if has_newline {
            normalized.push('\n');
        }
    }
    normalized
}

fn logical_lines(content: &str) -> Vec<String> {
    content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect()
}

struct TextShape {
    bom: bool,
    newline: &'static str,
    trailing_newline: bool,
    lines: Vec<String>,
}

impl TextShape {
    fn from_content(content: &str) -> Self {
        let body = content.strip_prefix('\u{feff}').unwrap_or(content);
        let first_newline = body.find('\n');
        let newline = if first_newline
            .is_some_and(|index| index > 0 && body.as_bytes().get(index - 1) == Some(&b'\r'))
        {
            "\r\n"
        } else {
            "\n"
        };
        Self {
            bom: body.len() != content.len(),
            newline,
            trailing_newline: body.ends_with('\n'),
            lines: logical_lines(content),
        }
    }

    fn rebuild(&self, lines: &[String]) -> String {
        let body_len = lines.iter().map(String::len).sum::<usize>()
            + self.newline.len() * lines.len().saturating_sub(1)
            + usize::from(self.bom) * '\u{feff}'.len_utf8();
        let mut output = String::with_capacity(body_len + self.newline.len());
        if self.bom {
            output.push('\u{feff}');
        }
        output.push_str(&lines.join(self.newline));
        if self.trailing_newline && !lines.is_empty() {
            output.push_str(self.newline);
        }
        output
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

    fn tagged(path: &str, content: &str, operations: &str) -> String {
        format!("[{path}#{}]\n{operations}", snapshot_tag(content))
    }

    #[test]
    fn hashline_read_numbers_a_page_under_a_whole_file_snapshot() {
        let result = render_read("src/lib.rs", "one\ntwo\nthree\n", 2, Some(1), 64 * 1024).unwrap();
        let tag = snapshot_tag("one\ntwo\nthree\n");
        assert_eq!(result["content"], format!("[src/lib.rs#{tag}]\n2:two\n"));
        assert_eq!(result["snapshot"], tag);
        assert_eq!(result["next_offset"], 3);
        assert_eq!(result["total_lines"], 3);
    }

    #[test]
    fn snapshot_normalizes_bom_and_crlf_but_detects_whitespace_changes() {
        assert_eq!(
            snapshot_tag("\u{feff}one\r\ntwo\r\n"),
            snapshot_tag("one\ntwo\n")
        );
        assert_ne!(snapshot_tag("one  \ntwo\n"), snapshot_tag("one\ntwo\n"));
    }

    #[tokio::test]
    async fn replaces_inserts_and_cuts_in_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "one\ntwo\nthree\nfour\nfive\n";
        std::fs::write(tmp.path().join("f.txt"), original).unwrap();
        let input = tagged(
            "f.txt",
            original,
            "PUT 2.=3:\n+TWO\n+THREE\nPUT <1:\n+zero\nPUT >$:\n+six\nCUT 4.=4\n",
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "zero\none\nTWO\nTHREE\nfive\nsix\n"
        );
        assert_eq!(result.result["files"][0]["adds"], 4);
        assert_eq!(result.result["files"][0]["dels"], 3);
    }

    #[tokio::test]
    async fn stale_snapshot_returns_current_context_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let read_version = "one\ntwo\nthree\n";
        let current = "one\nchanged\nthree\n";
        std::fs::write(tmp.path().join("f.txt"), current).unwrap();
        let input = tagged("f.txt", read_version, "PUT 2:\n+TWO\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(result.result["code"], "stale_snapshot");
        assert_eq!(
            result.result["stale"][0]["current_snapshot"],
            snapshot_tag(current)
        );
        assert!(
            result.result["stale"][0]["context"]
                .as_str()
                .unwrap()
                .contains("2:changed")
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            current
        );
    }

    #[tokio::test]
    async fn multi_file_preflight_failure_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let first = "first\n";
        let second_read = "second\n";
        let second_current = "changed elsewhere\n";
        std::fs::write(tmp.path().join("a.txt"), first).unwrap();
        std::fs::write(tmp.path().join("b.txt"), second_current).unwrap();
        let input = format!(
            "{}\n{}",
            tagged("a.txt", first, "PUT 1:\n+FIRST\n"),
            tagged("b.txt", second_read, "PUT 1:\n+SECOND\n")
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            first
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
            second_current
        );
    }

    #[tokio::test]
    async fn overlapping_operations_are_rejected_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "one\ntwo\nthree\n";
        std::fs::write(tmp.path().join("f.txt"), original).unwrap();
        let input = tagged("f.txt", original, "PUT 1.=2:\n+replacement\nCUT 2.=3\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert!(
            result.result["error"]
                .as_str()
                .unwrap()
                .contains("overlapping")
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn preserves_crlf_bom_and_missing_final_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "\u{feff}one\r\ntwo";
        std::fs::write(tmp.path().join("f.txt"), original).unwrap();
        let input = tagged("f.txt", original, "PUT 2:\n+TWO\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "\u{feff}one\r\nTWO"
        );
    }

    #[tokio::test]
    async fn cancellation_before_commit_leaves_file_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "one\n";
        std::fs::write(tmp.path().join("f.txt"), original).unwrap();
        let context = ctx(&tmp);
        context.cancel.cancel();
        let input = tagged("f.txt", original, "PUT 1:\n+ONE\n");

        let result = HashlineEdit.run(&context, &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            original
        );
    }

    #[tokio::test]
    async fn a_second_edit_from_the_same_snapshot_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "one\ntwo\n";
        std::fs::write(tmp.path().join("f.txt"), original).unwrap();
        let first = tagged("f.txt", original, "PUT 1:\n+ONE\n");
        let second = tagged("f.txt", original, "PUT 2:\n+TWO\n");

        let first_result = HashlineEdit.run(&ctx(&tmp), &json!({"input": first})).await;
        assert_eq!(first_result.status, ToolStatus::Ok);
        let second_result = HashlineEdit
            .run(&ctx(&tmp), &json!({"input": second}))
            .await;
        assert_eq!(second_result.status, ToolStatus::Error);
        assert_eq!(second_result.result["code"], "stale_snapshot");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("f.txt")).unwrap(),
            "ONE\ntwo\n"
        );
    }

    #[test]
    fn tool_is_classified_as_mutating() {
        assert!(HashlineEdit.mutates());
    }
}
