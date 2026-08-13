//! Compact, snapshot-validated line edits inspired by Oh My Pi's hashline format.
//!
//! `read_file(format = "hashline")` returns a whole-file snapshot tag plus
//! numbered lines. `hashline_edit` consumes that tag and one or more line
//! operations. Every file and operation is validated before staged writes are
//! promoted, and the engine's normal mutation lane surrounds the entire call.
//!
//! Supported operations mirror Oh My Pi's compact edit language while
//! retaining trouve's stricter all-file preflight and mutation-lane safety:
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
//! CUT 40* @handler
//! PUT <80 @handler
//! REM
//! MV "src/new name.rs"
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use trouve_protocol::ToolStatus;
use trouve_search::chunk::syntactic_block_ranges;
use trouve_search::languages::detect_language;

use super::{Tool, ToolCtx, ToolResult};

const SNAPSHOT_HEX_LEN: usize = 12;
const MAX_HASHLINE_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HASHLINE_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HASHLINE_BLOCK_FILE_BYTES: usize = 1024 * 1024;
const MAX_HASHLINE_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_HASHLINE_SECTIONS: usize = 64;
const STALE_CONTEXT_LINES: usize = 7;
const MAX_REGISTER_BYTES: usize = 1024 * 1024;
const MAX_REGISTER_SCOPE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REGISTER_STORE_BYTES: usize = 32 * 1024 * 1024;
const MAX_NAMED_REGISTERS_PER_SCOPE: usize = 16;
const MAX_REGISTER_SCOPES: usize = 64;

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
         sections. Syntax: PUT N.=M: followed by '+' payload rows replaces \
         lines; PUT <N:, >N:, or >$: inserts; N* selects a syntactic block; \
         CUT N.=M [@name] captures and deletes; bodyless PUT <N [@name] \
         pastes a register; REM removes the requested path; MV \"dest\" moves it. \
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
                    "description": "One or more [path#TAG] sections. Use PUT N.=M: plus '+' rows to replace, PUT <N:/ >N:/ >$: to insert, N* for blocks, CUT ... [@name] to capture/delete, bodyless PUT ... [@name] to paste, REM to remove, or MV \"dest\" to move."
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
    operations: Vec<ParsedOperation>,
    action: FileAction,
}

#[derive(Debug)]
enum ParsedOperation {
    Replace {
        target: SpanTarget,
        body: Vec<String>,
    },
    Insert {
        gap: ParsedGap,
        body: Vec<String>,
    },
    Cut {
        target: SpanTarget,
        register: Option<String>,
    },
    Paste {
        target: PasteTarget,
        register: Option<String>,
    },
}

#[derive(Debug, Clone, Copy)]
enum SpanTarget {
    Range { start: usize, end: usize },
    Block { start: usize },
}

#[derive(Debug, Clone, Copy)]
enum PasteTarget {
    Span(SpanTarget),
    Gap(ParsedGap),
}

#[derive(Debug, Clone, Copy)]
enum ParsedGap {
    Before(usize),
    After(usize),
    AfterBlock(usize),
    End,
}

#[derive(Debug, Default)]
enum FileAction {
    #[default]
    Update,
    Remove,
    Move {
        destination: String,
    },
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
    source_path: PathBuf,
    full_path: PathBuf,
    original: String,
    updated: String,
    adds: usize,
    deletes: usize,
    old_snapshot: String,
    new_snapshot: Option<String>,
    action: PreparedAction,
    permissions: std::fs::Permissions,
    symlink: Option<SymlinkInfo>,
}

#[derive(Debug)]
struct LoadedFile {
    source_path: PathBuf,
    full_path: PathBuf,
    original: String,
    permissions: std::fs::Permissions,
    symlink: Option<SymlinkInfo>,
}

#[derive(Debug, Clone)]
struct SymlinkInfo {
    target: PathBuf,
    target_is_dir: bool,
}

#[derive(Debug)]
enum PreparedAction {
    Update,
    Remove,
    Move {
        destination: String,
        destination_full_path: PathBuf,
        case_only: bool,
    },
}

/// A case-only rename is the narrow exception to the normal no-clobber rule.
/// Restrict it to two spellings of the same final directory entry: allowing a
/// different parent spelling would also accept symlinked-directory aliases,
/// while comparing only canonical paths would accept destination symlinks.
fn case_only_destination_matches_source(
    source_path: &Path,
    source_full_path: &Path,
    destination_path: &Path,
) -> bool {
    let (Some(source_parent), Some(destination_parent)) =
        (source_path.parent(), destination_path.parent())
    else {
        return false;
    };
    let (Some(source_name), Some(destination_name)) =
        (source_path.file_name(), destination_path.file_name())
    else {
        return false;
    };
    if source_parent != destination_parent || source_name == destination_name {
        return false;
    }
    if source_name.to_string_lossy().to_lowercase()
        != destination_name.to_string_lossy().to_lowercase()
    {
        return false;
    }
    let Ok(metadata) = std::fs::symlink_metadata(destination_path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    std::fs::canonicalize(destination_path).is_ok_and(|path| path == source_full_path)
}

#[derive(Debug)]
struct StaleFile {
    path: String,
    expected: String,
    current: String,
    context: String,
}

#[derive(Debug, Default)]
struct RegisterBank {
    values: HashMap<String, Vec<String>>,
    last_used: u64,
}

#[derive(Debug, Default)]
struct RegisterStore {
    scopes: HashMap<String, RegisterBank>,
    sequence: u64,
}

#[derive(Debug, Default)]
struct CallRegisters {
    anonymous: Option<Vec<String>>,
    named: HashMap<String, Vec<String>>,
}

static REGISTERS: OnceLock<Mutex<RegisterStore>> = OnceLock::new();

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
    if sections.len() > MAX_HASHLINE_SECTIONS {
        return ToolResult::error(format!(
            "hashline input has {} file sections; the limit is {MAX_HASHLINE_SECTIONS}",
            sections.len()
        ));
    }

    let mut canonical_paths = HashSet::new();
    let mut resolved_sources = HashSet::new();
    let mut loaded = Vec::with_capacity(sections.len());
    let mut stale = Vec::new();
    let mut total_source_bytes = 0usize;

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
        let symlink = match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = match std::fs::read_link(&resolved) {
                    Ok(target) => target,
                    Err(error) => {
                        return ToolResult::error(format!(
                            "cannot inspect symlink {}: {error}",
                            section.path
                        ));
                    }
                };
                Some(SymlinkInfo {
                    target,
                    target_is_dir: std::fs::metadata(&resolved)
                        .is_ok_and(|metadata| metadata.is_dir()),
                })
            }
            Ok(_) => None,
            Err(error) => {
                return ToolResult::error(format!("cannot inspect {}: {error}", section.path));
            }
        };
        if !resolved_sources.insert(resolved.clone()) {
            return ToolResult::error(format!(
                "{} is targeted by more than one hashline section",
                section.path
            ));
        }
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
        total_source_bytes = match total_source_bytes.checked_add(original.len()) {
            Some(total) if total <= MAX_HASHLINE_TOTAL_BYTES => total,
            _ => {
                return ToolResult::error(format!(
                    "hashline source files exceed the {MAX_HASHLINE_TOTAL_BYTES}-byte per-call limit"
                ));
            }
        };
        let current = snapshot_tag(&original);
        if current != section.expected_snapshot {
            stale.push(stale_file(section, &original, current));
            continue;
        }
        let permissions = match std::fs::metadata(&full_path) {
            Ok(metadata) => metadata.permissions(),
            Err(error) => {
                return ToolResult::error(format!(
                    "cannot inspect permissions for {}: {error}",
                    section.path
                ));
            }
        };
        loaded.push(LoadedFile {
            source_path: resolved,
            full_path,
            original,
            permissions,
            symlink,
        });
    }

    if !stale.is_empty() {
        return stale_result(stale);
    }
    if ctx.cancel.is_cancelled() {
        return ToolResult::error("hashline edit cancelled");
    }

    let mut destinations = HashSet::new();
    let mut case_only_moves = HashSet::new();
    for (section_index, section) in sections.iter().enumerate() {
        let FileAction::Move { destination } = &section.action else {
            continue;
        };
        let destination_path = match ctx.resolve(destination) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        };
        if resolved_sources.contains(&destination_path) {
            return ToolResult::error(format!(
                "cannot move {} to {destination}: the destination is also targeted as a source",
                section.path
            ));
        }
        if !destinations.insert(destination_path.clone()) {
            return ToolResult::error(format!(
                "more than one hashline section moves to {destination}"
            ));
        }
        if destination_path == loaded[section_index].source_path {
            return ToolResult::error(format!(
                "cannot move {} to {destination}: source and destination are identical",
                section.path
            ));
        }
        if destination_path.symlink_metadata().is_ok() {
            if case_only_destination_matches_source(
                &loaded[section_index].source_path,
                &loaded[section_index].full_path,
                &destination_path,
            ) {
                case_only_moves.insert(section_index);
            } else {
                return ToolResult::error(format!(
                    "cannot move {} to {destination}: destination already exists",
                    section.path
                ));
            }
        }
        if !destination_path.parent().is_some_and(Path::is_dir) {
            return ToolResult::error(format!(
                "cannot move {} to {destination}: destination parent does not exist",
                section.path
            ));
        }
    }

    let mut registers = CallRegisters {
        anonymous: None,
        named: load_named_registers(ctx),
    };
    let mut prepared = Vec::with_capacity(sections.len());
    let mut total_output_bytes = 0usize;
    for (section, loaded) in sections.iter().zip(&loaded) {
        if matches!(section.action, FileAction::Move { .. }) && loaded.symlink.is_some() {
            return ToolResult::error(format!(
                "{}: moving a symlink is not supported; move the link explicitly with a filesystem tool",
                section.path
            ));
        }
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("hashline edit cancelled");
        }
        let operations = match materialize_operations(section, &loaded.original, &mut registers) {
            Ok(operations) => operations,
            Err(error) => return ToolResult::error(format!("{}: {error}", section.path)),
        };
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("hashline edit cancelled");
        }
        let section_index = prepared.len();
        if case_only_moves.contains(&section_index) && !operations.is_empty() {
            return ToolResult::error(format!(
                "{}: a case-only move cannot be combined with content edits",
                section.path
            ));
        }
        let (updated, adds, deletes) = match &section.action {
            FileAction::Remove => (
                String::new(),
                0,
                TextShape::from_content(&loaded.original).lines.len(),
            ),
            FileAction::Update | FileAction::Move { .. } => {
                if operations.is_empty() {
                    (loaded.original.clone(), 0, 0)
                } else {
                    match apply_operations(&operations, &loaded.original) {
                        Ok(result) => result,
                        Err(error) => {
                            return ToolResult::error(format!("{}: {error}", section.path));
                        }
                    }
                }
            }
        };
        if updated.len() > MAX_HASHLINE_FILE_BYTES as usize {
            return ToolResult::error(format!(
                "{} would become {} bytes; the limit is {MAX_HASHLINE_FILE_BYTES}",
                section.path,
                updated.len()
            ));
        }
        total_output_bytes = match total_output_bytes.checked_add(updated.len()) {
            Some(total) if total <= MAX_HASHLINE_TOTAL_BYTES => total,
            _ => {
                return ToolResult::error(format!(
                    "hashline output files exceed the {MAX_HASHLINE_TOTAL_BYTES}-byte per-call limit"
                ));
            }
        };
        if matches!(section.action, FileAction::Update) && updated == loaded.original {
            return ToolResult::error(format!(
                "{}: hashline operations produce no changes",
                section.path
            ));
        }
        let action = match &section.action {
            FileAction::Update => PreparedAction::Update,
            FileAction::Remove => PreparedAction::Remove,
            FileAction::Move { destination } => PreparedAction::Move {
                destination: destination.clone(),
                destination_full_path: ctx.resolve(destination).expect("destination was validated"),
                case_only: case_only_moves.contains(&section_index),
            },
        };
        let new_snapshot =
            (!matches!(section.action, FileAction::Remove)).then(|| snapshot_tag(&updated));
        prepared.push(PreparedFile {
            path: section.path.clone(),
            source_path: loaded.source_path.clone(),
            full_path: loaded.full_path.clone(),
            original: loaded.original.clone(),
            updated,
            adds,
            deletes,
            old_snapshot: snapshot_tag(&loaded.original),
            new_snapshot,
            action,
            permissions: loaded.permissions.clone(),
            symlink: loaded.symlink.clone(),
        });
    }

    // Stage every update or move beside its destination before promoting any
    // write. REM has no staged payload, but remains part of the same preflight.
    // This catches allocation, permissions, and disk-space failures before the
    // first live file changes.
    let mut staged = Vec::with_capacity(prepared.len());
    for file in &prepared {
        let target = match &file.action {
            PreparedAction::Update => Some(&file.full_path),
            PreparedAction::Remove => None,
            PreparedAction::Move {
                destination_full_path,
                case_only,
                ..
            } => (!case_only).then_some(destination_full_path),
        };
        let Some(target) = target else {
            staged.push(None);
            continue;
        };
        let Some(parent) = target.parent() else {
            return ToolResult::error(format!("cannot resolve parent for {}", file.path));
        };
        let mut temp = match tempfile::NamedTempFile::new_in(parent) {
            Ok(temp) => temp,
            Err(error) => {
                return ToolResult::error(format!("cannot stage {}: {error}", file.path));
            }
        };
        if let Err(error) = temp.as_file().set_permissions(file.permissions.clone()) {
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
        if let PreparedAction::Move {
            destination,
            destination_full_path,
            case_only,
        } = &file.action
        {
            let destination_exists = destination_full_path.symlink_metadata().is_ok();
            let still_same_source = *case_only
                && case_only_destination_matches_source(
                    &file.source_path,
                    &file.full_path,
                    destination_full_path,
                );
            if destination_exists != *case_only || (*case_only && !still_same_source) {
                return ToolResult::error(format!(
                    "cannot move {} to {destination}: destination changed during preflight",
                    file.path
                ));
            }
        }
    }
    if !raced.is_empty() {
        return stale_result(raced);
    }
    if ctx.cancel.is_cancelled() {
        return ToolResult::error("hashline edit cancelled");
    }

    // Promotion is atomic per update/move destination. If a later operation
    // fails, restore every previously committed source from its preimage.
    for (index, file) in prepared.iter().enumerate() {
        if let Err(error) = commit_prepared(file, staged[index].take()) {
            let mut rollback_errors = Vec::new();
            for restored in prepared[..index].iter().rev() {
                if let Err(rollback) = rollback_prepared(restored) {
                    rollback_errors.push(format!("{}: {rollback}", restored.path));
                }
            }
            let rollback = if rollback_errors.is_empty() {
                "previous files were restored".to_owned()
            } else {
                format!("rollback also failed for {}", rollback_errors.join(", "))
            };
            return ToolResult::error(format!(
                "cannot commit hashline operation for {}: {error}; {rollback}",
                file.path
            ));
        }
    }

    persist_named_registers(ctx, registers.named);

    ToolResult::ok(json!({
        "files": prepared.iter().map(prepared_result).collect::<Vec<_>>()
    }))
}

fn commit_prepared(
    file: &PreparedFile,
    staged: Option<tempfile::NamedTempFile>,
) -> Result<(), String> {
    match &file.action {
        PreparedAction::Update => staged
            .expect("updated file has a staged payload")
            .persist(&file.full_path)
            .map(|_| ())
            .map_err(|error| error.error.to_string()),
        PreparedAction::Remove => {
            std::fs::remove_file(&file.source_path).map_err(|error| error.to_string())
        }
        PreparedAction::Move {
            destination_full_path,
            case_only,
            ..
        } => {
            if *case_only {
                // Revalidate at the commit boundary. On a case-insensitive
                // directory the source entry itself occupies both spellings,
                // so another entry cannot claim the destination while the
                // verified source remains in place.
                if !case_only_destination_matches_source(
                    &file.source_path,
                    &file.full_path,
                    destination_full_path,
                ) {
                    return Err("case-only move destination changed before commit".into());
                }
                return std::fs::rename(&file.source_path, destination_full_path)
                    .map_err(|error| error.to_string());
            }
            staged
                .expect("moved file has a staged payload")
                .persist_noclobber(destination_full_path)
                .map_err(|error| error.error.to_string())?;
            if let Err(error) = std::fs::remove_file(&file.source_path) {
                let cleanup = std::fs::remove_file(destination_full_path);
                return Err(match cleanup {
                    Ok(()) => format!("cannot remove move source: {error}"),
                    Err(cleanup) => format!(
                        "cannot remove move source: {error}; destination cleanup also failed: {cleanup}"
                    ),
                });
            }
            Ok(())
        }
    }
}

fn rollback_prepared(file: &PreparedFile) -> Result<(), String> {
    if let PreparedAction::Move {
        destination_full_path,
        case_only,
        ..
    } = &file.action
    {
        if *case_only {
            return std::fs::rename(destination_full_path, &file.source_path)
                .map_err(|error| format!("cannot restore case-only move: {error}"));
        }
        if let Err(error) = std::fs::remove_file(destination_full_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(format!("cannot remove moved destination: {error}"));
        }
    }
    if matches!(file.action, PreparedAction::Remove)
        && let Some(symlink) = &file.symlink
    {
        return create_symlink(&symlink.target, &file.source_path, symlink.target_is_dir)
            .map_err(|error| format!("cannot restore symlink: {error}"));
    }
    std::fs::write(&file.full_path, file.original.as_bytes()).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&file.full_path, file.permissions.clone())
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path, _target_is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

fn prepared_result(file: &PreparedFile) -> Value {
    let (action, destination) = match &file.action {
        PreparedAction::Update => ("update", None),
        PreparedAction::Remove => ("delete", None),
        PreparedAction::Move { destination, .. } => ("move", Some(destination.as_str())),
    };
    json!({
        "path": file.path,
        "action": action,
        "destination": destination,
        "adds": file.adds,
        "dels": file.deletes,
        "previous_snapshot": file.old_snapshot,
        "snapshot": file.new_snapshot,
    })
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
        let mut action = FileAction::Update;

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
            if !matches!(action, FileAction::Update) {
                return Err(format!(
                    "line {}: REM or MV must be the final operation in its file section",
                    index + 1
                ));
            }
            if let Some(target) = line.strip_prefix("PUT ") {
                let operation_line = index + 1;
                let body_form = target.ends_with(':');
                let target = target.strip_suffix(':').unwrap_or(target).trim();
                let (target, register) = parse_target_and_register(target)
                    .map_err(|error| format!("line {operation_line}: {error}"))?;
                let target = parse_paste_target(target)
                    .map_err(|error| format!("line {operation_line}: {error}"))?;
                if body_form {
                    if register.is_some() {
                        return Err(format!(
                            "line {operation_line}: register PUT is bodyless and must not end with ':'"
                        ));
                    }
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
                            "line {operation_line}: PUT requires at least one '+' payload row; use CUT to delete"
                        ));
                    }
                    operations.push(match target {
                        PasteTarget::Span(target) => ParsedOperation::Replace { target, body },
                        PasteTarget::Gap(gap) => ParsedOperation::Insert { gap, body },
                    });
                } else {
                    if matches!(target, PasteTarget::Span(_)) && register.is_none() {
                        return Err(format!(
                            "line {operation_line}: range and block PUT pastes require a named @register"
                        ));
                    }
                    operations.push(ParsedOperation::Paste { target, register });
                    index += 1;
                }
                continue;
            }
            if let Some(target) = line.strip_prefix("CUT ") {
                let operation_line = index + 1;
                let (target, register) = parse_target_and_register(target.trim())
                    .map_err(|error| format!("line {operation_line}: {error}"))?;
                let target = parse_span_target(target)
                    .map_err(|error| format!("line {operation_line}: {error}"))?;
                operations.push(ParsedOperation::Cut { target, register });
                index += 1;
                continue;
            }
            if line == "REM" {
                action = FileAction::Remove;
                index += 1;
                continue;
            }
            if let Some(destination) = line.strip_prefix("MV ") {
                action = FileAction::Move {
                    destination: parse_path_operand(destination)
                        .map_err(|error| format!("line {}: {error}", index + 1))?,
                };
                index += 1;
                continue;
            }
            return Err(format!(
                "line {}: expected PUT, CUT, REM, MV, or another [path#TAG] section; got {raw:?}",
                index + 1
            ));
        }

        if operations.is_empty() && matches!(action, FileAction::Update) {
            return Err(format!(
                "[{path}#{expected_snapshot}] contains no operations"
            ));
        }
        sections.push(Section {
            path,
            expected_snapshot,
            operations,
            action,
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

fn parse_target_and_register(value: &str) -> Result<(&str, Option<String>), String> {
    let mut pieces = value.split_whitespace();
    let Some(target) = pieces.next() else {
        return Err("operation target must not be empty".into());
    };
    let register = pieces.next().map(parse_register_name).transpose()?;
    if pieces.next().is_some() {
        return Err("operation has too many target/register fields".into());
    }
    Ok((target, register))
}

fn parse_register_name(value: &str) -> Result<String, String> {
    let Some(name) = value.strip_prefix('@') else {
        return Err(format!("expected a named @register; got {value:?}"));
    };
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("invalid register name {value:?}"));
    }
    Ok(name.to_owned())
}

fn parse_paste_target(value: &str) -> Result<PasteTarget, String> {
    if value == ">$" {
        return Ok(PasteTarget::Gap(ParsedGap::End));
    }
    if let Some(line) = value.strip_prefix('<') {
        return Ok(PasteTarget::Gap(ParsedGap::Before(parse_line_number(
            line,
        )?)));
    }
    if let Some(line) = value.strip_prefix('>') {
        if let Some(line) = line.strip_suffix('*') {
            return Ok(PasteTarget::Gap(ParsedGap::AfterBlock(parse_line_number(
                line,
            )?)));
        }
        return Ok(PasteTarget::Gap(ParsedGap::After(parse_line_number(line)?)));
    }
    Ok(PasteTarget::Span(parse_span_target(value)?))
}

fn parse_span_target(value: &str) -> Result<SpanTarget, String> {
    if let Some(line) = value.strip_suffix('*') {
        return Ok(SpanTarget::Block {
            start: parse_line_number(line)?,
        });
    }
    let (start, end) = parse_range(value)?;
    Ok(SpanTarget::Range { start, end })
}

fn parse_path_operand(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("MV destination must not be empty".into());
    }
    let parsed = if value.starts_with('"') {
        serde_json::from_str::<String>(value)
            .map_err(|error| format!("invalid quoted MV destination: {error}"))?
    } else if value.starts_with('\'') {
        value
            .strip_prefix('\'')
            .and_then(|inner| inner.strip_suffix('\''))
            .ok_or_else(|| "single-quoted MV destination is not terminated".to_owned())?
            .replace("\\'", "'")
    } else {
        value.to_owned()
    };
    if parsed.is_empty() {
        return Err("MV destination must not be empty".into());
    }
    Ok(parsed)
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

fn materialize_operations(
    section: &Section,
    original: &str,
    registers: &mut CallRegisters,
) -> Result<Vec<Operation>, String> {
    let lines = logical_lines(original);
    let block_ranges = resolve_block_ranges(section, original)?;
    let mut operations = Vec::with_capacity(section.operations.len());
    let mut materialized_body_bytes = 0usize;
    for operation in &section.operations {
        match operation {
            ParsedOperation::Replace { target, body } => {
                reserve_materialized_body(body, &mut materialized_body_bytes)?;
                let (start, end) = resolve_span_target(&section.path, *target, &block_ranges)?;
                operations.push(Operation::Replace {
                    start,
                    end,
                    body: body.clone(),
                });
            }
            ParsedOperation::Insert { gap, body } => {
                reserve_materialized_body(body, &mut materialized_body_bytes)?;
                operations.push(Operation::Insert {
                    gap: resolve_parsed_gap(&section.path, *gap, &block_ranges)?,
                    body: body.clone(),
                });
            }
            ParsedOperation::Cut { target, register } => {
                let (start, end) = resolve_span_target(&section.path, *target, &block_ranges)?;
                validate_range(start, end, lines.len())?;
                store_register(
                    register.as_deref(),
                    lines[start - 1..end].to_vec(),
                    registers,
                )?;
                operations.push(Operation::Cut { start, end });
            }
            ParsedOperation::Paste { target, register } => {
                let body = read_register(register.as_deref(), registers)?;
                reserve_materialized_body(body, &mut materialized_body_bytes)?;
                let body = body.clone();
                match target {
                    PasteTarget::Span(target) => {
                        let (start, end) =
                            resolve_span_target(&section.path, *target, &block_ranges)?;
                        operations.push(Operation::Replace { start, end, body });
                    }
                    PasteTarget::Gap(gap) => operations.push(Operation::Insert {
                        gap: resolve_parsed_gap(&section.path, *gap, &block_ranges)?,
                        body,
                    }),
                }
            }
        }
    }
    Ok(operations)
}

fn reserve_materialized_body(body: &[String], total: &mut usize) -> Result<(), String> {
    let bytes = register_content_bytes(body);
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| "materialized hashline payload size overflowed".to_owned())?;
    if *total > MAX_HASHLINE_FILE_BYTES as usize {
        return Err(format!(
            "materialized edit payload is {} bytes; the per-file limit is {MAX_HASHLINE_FILE_BYTES}",
            *total
        ));
    }
    Ok(())
}

fn requested_block_starts(section: &Section) -> HashSet<usize> {
    let mut starts = HashSet::new();
    for operation in &section.operations {
        match operation {
            ParsedOperation::Replace { target, .. } | ParsedOperation::Cut { target, .. } => {
                insert_span_start(&mut starts, *target);
            }
            ParsedOperation::Insert { gap: target, .. } => {
                insert_gap_start(&mut starts, *target);
            }
            ParsedOperation::Paste { target, .. } => match target {
                PasteTarget::Span(target) => insert_span_start(&mut starts, *target),
                PasteTarget::Gap(target) => insert_gap_start(&mut starts, *target),
            },
        }
    }
    starts
}

fn insert_span_start(starts: &mut HashSet<usize>, target: SpanTarget) {
    if let SpanTarget::Block { start } = target {
        starts.insert(start);
    }
}

fn insert_gap_start(starts: &mut HashSet<usize>, target: ParsedGap) {
    if let ParsedGap::AfterBlock(start) = target {
        starts.insert(start);
    }
}

fn resolve_block_ranges(
    section: &Section,
    original: &str,
) -> Result<HashMap<usize, (usize, usize)>, String> {
    let starts = requested_block_starts(section);
    if starts.is_empty() {
        return Ok(HashMap::new());
    }
    if original.len() > MAX_HASHLINE_BLOCK_FILE_BYTES {
        return Err(format!(
            "syntactic block edits are limited to {MAX_HASHLINE_BLOCK_FILE_BYTES}-byte files; use explicit N.=M ranges"
        ));
    }
    let language = detect_language(Path::new(&section.path)).ok_or_else(|| {
        format!(
            "cannot resolve syntactic blocks: unknown language for {}",
            section.path
        )
    })?;
    Ok(if language == "markdown" {
        markdown_section_ranges(original, &starts)
    } else {
        syntactic_block_ranges(original, language, &starts)
    })
}

fn resolve_span_target(
    path: &str,
    target: SpanTarget,
    block_ranges: &HashMap<usize, (usize, usize)>,
) -> Result<(usize, usize), String> {
    match target {
        SpanTarget::Range { start, end } => Ok((start, end)),
        SpanTarget::Block { start } => {
            let language = detect_language(Path::new(path)).ok_or_else(|| {
                format!("cannot resolve block {start}*: unknown language for {path}")
            })?;
            block_ranges.get(&start).copied().ok_or_else(|| {
                format!(
                    "cannot resolve a multi-line {language} block beginning on line {start}; use an explicit N.=M range"
                )
            })
        }
    }
}

fn resolve_parsed_gap(
    path: &str,
    gap: ParsedGap,
    block_ranges: &HashMap<usize, (usize, usize)>,
) -> Result<Gap, String> {
    match gap {
        ParsedGap::Before(line) => Ok(Gap::Before(line)),
        ParsedGap::After(line) => Ok(Gap::After(line)),
        ParsedGap::End => Ok(Gap::End),
        ParsedGap::AfterBlock(start) => {
            let (_, end) = resolve_span_target(path, SpanTarget::Block { start }, block_ranges)?;
            Ok(Gap::After(end))
        }
    }
}

#[cfg(test)]
fn markdown_section_range(source: &str, start: usize) -> Option<(usize, usize)> {
    markdown_section_ranges(source, &HashSet::from([start])).remove(&start)
}

fn markdown_section_ranges(
    source: &str,
    starts: &HashSet<usize>,
) -> HashMap<usize, (usize, usize)> {
    let lines = logical_lines(source);
    let mut headings = Vec::<(usize, usize)>::new();
    let mut fence: Option<(u8, usize)> = None;
    for (offset, line) in lines.iter().enumerate() {
        if let Some((open, length)) = fence {
            if markdown_fence(line).is_some_and(|(next, count, suffix)| {
                next == open
                    && count >= length
                    && suffix.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
            }) {
                fence = None;
            }
            continue;
        }
        if let Some((marker, length, info)) = markdown_fence(line)
            && !(marker == b'`' && info.contains('`'))
        {
            fence = Some((marker, length));
            continue;
        }
        let Some(trimmed) = markdown_line_after_allowed_indent(line) else {
            continue;
        };
        let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
            headings.push((offset + 1, level));
        }
    }
    let mut ranges = HashMap::new();
    for (index, &(start, level)) in headings.iter().enumerate() {
        if !starts.contains(&start) {
            continue;
        }
        let end = headings[index + 1..]
            .iter()
            .find(|(_, next_level)| *next_level <= level)
            .map_or(lines.len(), |(next_start, _)| next_start - 1);
        if end > start {
            ranges.insert(start, (start, end));
        }
    }
    ranges
}

/// Return a CommonMark fenced-code marker and the remainder of its line.
/// Fences may be indented by at most three spaces and contain at least three
/// matching backticks or tildes. Opener info-string and closer restrictions
/// are applied by the caller because they differ.
fn markdown_fence(line: &str) -> Option<(u8, usize, &str)> {
    let trimmed = markdown_line_after_allowed_indent(line)?;
    let marker = trimmed.as_bytes().first().copied()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then(|| (marker, length, &trimmed[length..]))
}

fn markdown_line_after_allowed_indent(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then(|| &line[indent..])
}

fn store_register(
    name: Option<&str>,
    content: Vec<String>,
    registers: &mut CallRegisters,
) -> Result<(), String> {
    let bytes = register_content_bytes(&content);
    if bytes > MAX_REGISTER_BYTES {
        return Err(format!(
            "captured register is {bytes} bytes; the limit is {MAX_REGISTER_BYTES}"
        ));
    }
    if let Some(name) = name {
        if !registers.named.contains_key(name)
            && registers.named.len() >= MAX_NAMED_REGISTERS_PER_SCOPE
        {
            return Err(format!(
                "named register limit ({MAX_NAMED_REGISTERS_PER_SCOPE}) reached"
            ));
        }
        let existing = registers
            .named
            .get(name)
            .map_or(0, |value| register_content_bytes(value));
        let total = named_register_bytes(&registers.named)
            .saturating_sub(existing)
            .saturating_add(bytes);
        if total > MAX_REGISTER_SCOPE_BYTES {
            return Err(format!(
                "named registers would use {total} bytes; the per-thread limit is {MAX_REGISTER_SCOPE_BYTES}"
            ));
        }
        registers.named.insert(name.to_owned(), content);
    } else {
        registers.anonymous = Some(content);
    }
    Ok(())
}

fn read_register<'a>(
    name: Option<&str>,
    registers: &'a CallRegisters,
) -> Result<&'a Vec<String>, String> {
    match name {
        Some(name) => registers
            .named
            .get(name)
            .ok_or_else(|| format!("named register @{name} is empty")),
        None => registers
            .anonymous
            .as_ref()
            .ok_or_else(|| "anonymous register is empty; CUT content first".to_owned()),
    }
}

fn register_content_bytes(content: &[String]) -> usize {
    content
        .iter()
        .map(|line| line.len().saturating_add(1))
        .sum()
}

fn named_register_bytes(values: &HashMap<String, Vec<String>>) -> usize {
    values
        .values()
        .map(|content| register_content_bytes(content))
        .sum()
}

fn register_scope(ctx: &ToolCtx) -> String {
    if ctx.thread_id.is_empty() {
        format!("worktree:{}", ctx.worktree.display())
    } else {
        format!("thread:{}", ctx.thread_id)
    }
}

fn load_named_registers(ctx: &ToolCtx) -> HashMap<String, Vec<String>> {
    let key = register_scope(ctx);
    let mut store = REGISTERS
        .get_or_init(|| Mutex::new(RegisterStore::default()))
        .lock()
        .unwrap();
    store.sequence = store.sequence.wrapping_add(1);
    let sequence = store.sequence;
    let bank = store.scopes.entry(key).or_default();
    bank.last_used = sequence;
    bank.values.clone()
}

fn persist_named_registers(ctx: &ToolCtx, values: HashMap<String, Vec<String>>) {
    let key = register_scope(ctx);
    let mut store = REGISTERS
        .get_or_init(|| Mutex::new(RegisterStore::default()))
        .lock()
        .unwrap();
    store.sequence = store.sequence.wrapping_add(1);
    let sequence = store.sequence;
    store.scopes.insert(
        key,
        RegisterBank {
            values,
            last_used: sequence,
        },
    );
    while store.scopes.len() > MAX_REGISTER_SCOPES
        || store
            .scopes
            .values()
            .map(|bank| named_register_bytes(&bank.values))
            .sum::<usize>()
            > MAX_REGISTER_STORE_BYTES
    {
        let Some(oldest) = store
            .scopes
            .iter()
            .min_by_key(|(_, bank)| bank.last_used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        store.scopes.remove(&oldest);
    }
}

fn apply_operations(
    operations: &[Operation],
    original: &str,
) -> Result<(String, usize, usize), String> {
    let shape = TextShape::from_content(original);
    let line_count = shape.lines.len();
    let mut ranges = BTreeMap::<usize, (usize, Vec<String>)>::new();
    let mut insertions = BTreeMap::<usize, Vec<String>>::new();
    let mut claimed = vec![false; line_count + 1];
    let mut adds = 0usize;
    let mut deletes = 0usize;

    for operation in operations {
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
            ParsedOperation::Replace { target, .. } | ParsedOperation::Cut { target, .. } => {
                span_start(*target)
            }
            ParsedOperation::Insert { gap, .. } => parsed_gap_focus(*gap, lines.len()),
            ParsedOperation::Paste { target, .. } => match target {
                PasteTarget::Span(target) => span_start(*target),
                PasteTarget::Gap(gap) => parsed_gap_focus(*gap, lines.len()),
            },
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

fn span_start(target: SpanTarget) -> usize {
    match target {
        SpanTarget::Range { start, .. } | SpanTarget::Block { start } => start,
    }
}

fn parsed_gap_focus(gap: ParsedGap, line_count: usize) -> usize {
    match gap {
        ParsedGap::Before(line) | ParsedGap::After(line) | ParsedGap::AfterBlock(line) => line,
        ParsedGap::End => line_count.max(1),
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
    async fn resolves_syntactic_blocks_and_after_block_gaps() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "fn old() {\n    work();\n}\n\nfn keep() {}\n";
        std::fs::write(tmp.path().join("lib.rs"), original).unwrap();
        let input = tagged(
            "lib.rs",
            original,
            "PUT 1*:\n+fn new() {\n+    better_work();\n+}\nPUT >1*:\n+// between functions\n",
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("lib.rs")).unwrap(),
            "fn new() {\n    better_work();\n}\n// between functions\n\nfn keep() {}\n"
        );
    }

    #[tokio::test]
    async fn named_and_anonymous_registers_move_content_across_files_and_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let source = "alpha\nbeta\ngamma\n";
        let target = "start\nend\n";
        std::fs::write(tmp.path().join("source.txt"), source).unwrap();
        std::fs::write(tmp.path().join("target.txt"), target).unwrap();
        let input = format!(
            "{}\n{}",
            tagged("source.txt", source, "CUT 1.=1 @saved\nCUT 2.=2\nPUT >3\n"),
            tagged("target.txt", target, "PUT <2 @saved\n")
        );
        let tool = HashlineEdit;

        let result = tool.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("source.txt")).unwrap(),
            "gamma\nbeta\n"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("target.txt")).unwrap(),
            "start\nalpha\nend\n"
        );

        let current = "start\nalpha\nend\n";
        let second = tagged("target.txt", current, "PUT >$ @saved\n");
        let result = tool.run(&ctx(&tmp), &json!({"input": second})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("target.txt")).unwrap(),
            "start\nalpha\nend\nalpha\n"
        );
    }

    #[tokio::test]
    async fn removes_and_moves_files_in_one_preflighted_call() {
        let tmp = tempfile::tempdir().unwrap();
        let remove = "obsolete\n";
        let moving = "old\n";
        std::fs::write(tmp.path().join("remove.txt"), remove).unwrap();
        std::fs::write(tmp.path().join("moving.txt"), moving).unwrap();
        std::fs::create_dir(tmp.path().join("nested")).unwrap();
        let input = format!(
            "{}\n{}",
            tagged("remove.txt", remove, "REM\n"),
            tagged(
                "moving.txt",
                moving,
                "PUT 1.=1:\n+new\nMV \"nested/moved file.txt\"\n"
            )
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert!(!tmp.path().join("remove.txt").exists());
        assert!(!tmp.path().join("moving.txt").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("nested/moved file.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(result.result["files"][0]["action"], "delete");
        assert_eq!(result.result["files"][1]["action"], "move");
        assert_eq!(
            result.result["files"][1]["destination"],
            "nested/moved file.txt"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removing_a_symlink_removes_only_the_requested_link() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real.txt");
        let link = tmp.path().join("link.txt");
        std::fs::write(&target, "keep\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let input = tagged("link.txt", "keep\n", "REM\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert!(!link.exists());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moving_a_symlink_is_rejected_without_touching_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real.txt");
        let link = tmp.path().join("link.txt");
        std::fs::write(&target, "keep\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let input = tagged("link.txt", "keep\n", "MV moved.txt\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep\n");
        assert!(!tmp.path().join("moved.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn move_rejects_a_destination_symlink_to_its_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("source-link.txt");
        std::fs::write(&source, "keep\n").unwrap();
        std::os::unix::fs::symlink(&source, &destination).unwrap();
        let input = tagged("source.txt", "keep\n", "MV source-link.txt\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;

        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "keep\n");
        assert!(
            destination
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn move_rejects_a_hardlink_alias_to_its_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("source-alias.txt");
        std::fs::write(&source, "keep\n").unwrap();
        std::fs::hard_link(&source, &destination).unwrap();
        let input = tagged("source.txt", "keep\n", "MV source-alias.txt\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;

        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "keep\n");
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[test]
    fn case_only_move_commit_rejects_a_destination_replaced_by_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("source-link.txt");
        std::fs::write(&source, "keep\n").unwrap();
        std::os::unix::fs::symlink(&source, &destination).unwrap();
        let prepared = PreparedFile {
            path: "source.txt".into(),
            source_path: source.clone(),
            full_path: source.clone(),
            original: "keep\n".into(),
            updated: "keep\n".into(),
            adds: 0,
            deletes: 0,
            old_snapshot: snapshot_tag("keep\n"),
            new_snapshot: Some(snapshot_tag("keep\n")),
            action: PreparedAction::Move {
                destination: "source-link.txt".into(),
                destination_full_path: destination.clone(),
                case_only: true,
            },
            permissions: std::fs::metadata(&source).unwrap().permissions(),
            symlink: None,
        };

        assert!(commit_prepared(&prepared, None).is_err());
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "keep\n");
        assert!(
            destination
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn legitimate_case_only_move_is_preserved_when_the_filesystem_supports_it() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("lower.txt");
        let destination = tmp.path().join("Lower.txt");
        std::fs::write(&source, "keep\n").unwrap();
        let full_path = std::fs::canonicalize(&source).unwrap();
        if !case_only_destination_matches_source(&source, &full_path, &destination) {
            return;
        }
        let input = tagged("lower.txt", "keep\n", "MV Lower.txt\n");

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;

        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "keep\n");
        assert!(
            tmp.path()
                .read_dir()
                .unwrap()
                .any(|entry| { entry.unwrap().file_name() == std::ffi::OsStr::new("Lower.txt") })
        );
    }

    #[test]
    fn move_commit_never_clobbers_a_destination_created_after_preflight() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("destination.txt");
        std::fs::write(&source, "old\n").unwrap();
        std::fs::write(&destination, "occupied\n").unwrap();
        let mut staged = tempfile::NamedTempFile::new_in(tmp.path()).unwrap();
        staged.write_all(b"new\n").unwrap();
        let prepared = PreparedFile {
            path: "source.txt".into(),
            source_path: source.clone(),
            full_path: source.clone(),
            original: "old\n".into(),
            updated: "new\n".into(),
            adds: 1,
            deletes: 1,
            old_snapshot: snapshot_tag("old\n"),
            new_snapshot: Some(snapshot_tag("new\n")),
            action: PreparedAction::Move {
                destination: "destination.txt".into(),
                destination_full_path: destination.clone(),
                case_only: false,
            },
            permissions: std::fs::metadata(&source).unwrap().permissions(),
            symlink: None,
        };

        assert!(commit_prepared(&prepared, Some(staged)).is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "old\n");
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "occupied\n");
    }

    #[tokio::test]
    async fn cut_register_can_capture_content_before_removing_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = "move me\nremove me\n";
        let target = "keep\n";
        std::fs::write(tmp.path().join("source.txt"), source).unwrap();
        std::fs::write(tmp.path().join("target.txt"), target).unwrap();
        let input = format!(
            "{}\n{}",
            tagged("source.txt", source, "CUT 1.=1 @saved\nREM\n"),
            tagged("target.txt", target, "PUT <1 @saved\n")
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Ok, "{:?}", result.result);
        assert!(!tmp.path().join("source.txt").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("target.txt")).unwrap(),
            "move me\nkeep\n"
        );
    }

    #[tokio::test]
    async fn existing_move_destination_prevents_every_file_change() {
        let tmp = tempfile::tempdir().unwrap();
        let remove = "keep me\n";
        let moving = "source\n";
        std::fs::write(tmp.path().join("remove.txt"), remove).unwrap();
        std::fs::write(tmp.path().join("moving.txt"), moving).unwrap();
        std::fs::write(tmp.path().join("occupied.txt"), "occupied\n").unwrap();
        let input = format!(
            "{}\n{}",
            tagged("remove.txt", remove, "REM\n"),
            tagged("moving.txt", moving, "MV occupied.txt\n")
        );

        let result = HashlineEdit.run(&ctx(&tmp), &json!({"input": input})).await;
        assert_eq!(result.status, ToolStatus::Error);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("remove.txt")).unwrap(),
            remove
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("moving.txt")).unwrap(),
            moving
        );
    }

    #[test]
    fn markdown_blocks_cover_their_section() {
        let source = "# Top\nintro\n## Child\nbody\n# Next\nend\n";
        assert_eq!(markdown_section_range(source, 1), Some((1, 4)));
        assert_eq!(markdown_section_range(source, 3), Some((3, 4)));
    }

    #[test]
    fn markdown_blocks_ignore_headings_inside_fenced_code() {
        let source = "# Top\n\n```sh\n# not a heading\n```\nbody\n# Next\nend\n";
        assert_eq!(markdown_section_range(source, 1), Some((1, 6)));
        assert_eq!(markdown_section_range(source, 4), None);
    }

    #[test]
    fn markdown_fences_reject_indented_openers_and_invalid_backtick_info() {
        let indented = "# Top\n    ```\n# Next\nend\n";
        assert_eq!(markdown_section_range(indented, 1), Some((1, 2)));
        assert_eq!(markdown_section_range(indented, 3), Some((3, 4)));

        let invalid_info = "# Top\n```bad`info\n# Next\n```\n";
        assert_eq!(markdown_section_range(invalid_info, 1), Some((1, 2)));
        assert_eq!(markdown_section_range(invalid_info, 3), Some((3, 4)));
    }

    #[test]
    fn markdown_fences_close_only_with_whitespace_after_a_matching_marker() {
        let marker_text = "# Top\n```sh\n```not-a-close\n# hidden\n```\n# Next\nend\n";
        assert_eq!(markdown_section_range(marker_text, 1), Some((1, 5)));
        assert_eq!(markdown_section_range(marker_text, 4), None);
        assert_eq!(markdown_section_range(marker_text, 6), Some((6, 7)));

        let indented_close = "# Top\n~~~text\n    ~~~\n# hidden\n~~~   \t\n# Next\nend\n";
        assert_eq!(markdown_section_range(indented_close, 1), Some((1, 5)));
        assert_eq!(markdown_section_range(indented_close, 4), None);
        assert_eq!(markdown_section_range(indented_close, 6), Some((6, 7)));
    }

    #[test]
    fn materialized_register_pastes_have_a_per_file_budget() {
        let body = vec!["x".repeat(MAX_REGISTER_BYTES - 1)];
        let mut total = 0;
        for _ in 0..(MAX_HASHLINE_FILE_BYTES as usize / MAX_REGISTER_BYTES) {
            reserve_materialized_body(&body, &mut total).unwrap();
        }
        assert!(reserve_materialized_body(&body, &mut total).is_err());
    }

    #[test]
    fn named_registers_have_an_aggregate_scope_budget() {
        let mut registers = CallRegisters::default();
        let body = vec!["x".repeat(MAX_REGISTER_BYTES - 1)];
        for index in 0..(MAX_REGISTER_SCOPE_BYTES / MAX_REGISTER_BYTES) {
            store_register(Some(&format!("r{index}")), body.clone(), &mut registers).unwrap();
        }
        assert!(store_register(Some("overflow"), body, &mut registers).is_err());
    }

    #[test]
    fn runtime_schema_documents_the_operation_grammar() {
        let description = HashlineEdit.description();
        assert!(description.contains("PUT N.=M:"));
        assert!(description.contains("CUT N.=M"));
        assert!(description.contains("MV \"dest\""));
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
