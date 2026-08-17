//! Git plumbing for session worktrees and per-turn checkpoints (ADR 0003).
//!
//! Everything shells out to `git`; all functions are synchronous and are
//! called via `spawn_blocking` from async code.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CHECKPOINT_IDENTITY_NAME: &str = "trouve";
const CHECKPOINT_IDENTITY_EMAIL: &str = "trouve@localhost";
const WORKTREE_RESERVATION_SUFFIX: &str = ".trouve-creation-owner";
// The legacy aggregate endpoint still needs a strict whole-session budget.
// New clients load a bounded metadata manifest and one bounded patch at a time.
const MAX_SESSION_DIFF_FILES: usize = 250;
const MAX_SESSION_DIFF_CHANGED_LINES: u64 = 20_000;
const MAX_SESSION_DIFF_BYTES: usize = 2 * 1024 * 1024;
const MAX_SESSION_FILE_DIFF_BYTES: usize = 4 * 1024 * 1024;
const MAX_SESSION_SNAPSHOT_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSION_SNAPSHOT_ENTRIES: usize = 1_000_000;
const MAX_SESSION_SNAPSHOT_CHANGED_PATHS: usize = 10_000;
const MAX_SESSION_SNAPSHOT_HASH_BYTES: u64 = 512 * 1024 * 1024;
const SESSION_DIFF_GIT_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_PUSH_GIT_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_TREE_CLEANUP_RESERVE: Duration = Duration::from_secs(5);
const MAX_GIT_STDERR_BYTES: usize = 64 * 1024;
const MATERIALIZED_ATTACHMENTS_PREFIX: &str = ".trouve/attachments/";

fn is_session_internal_path(path: &str) -> bool {
    path == ".trouve/attachments" || path.starts_with(MATERIALIZED_ATTACHMENTS_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDiffStat {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub binary: bool,
}

#[derive(Debug)]
pub struct SessionDiffTooLarge(String);

impl fmt::Display for SessionDiffTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SessionDiffTooLarge {}

fn session_diff_too_large(message: String) -> anyhow::Error {
    SessionDiffTooLarge(message).into()
}

struct TemporaryCheckpointIndex {
    path: PathBuf,
    object_directory: PathBuf,
    alternate_object_directory: PathBuf,
    _directory: tempfile::TempDir,
}

impl TemporaryCheckpointIndex {
    fn new() -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("trouve-checkpoint-index-")
            .tempdir()
            .context("creating temporary checkpoint index directory")?;
        let path = directory.path().join("index");
        let object_directory = directory.path().join("objects");
        std::fs::create_dir_all(object_directory.join("info"))?;
        std::fs::create_dir_all(object_directory.join("pack"))?;
        Ok(Self {
            path,
            object_directory,
            alternate_object_directory: PathBuf::new(),
            _directory: directory,
        })
    }

    fn for_worktree(worktree: &Path, operation: &GitOperation<'_>) -> Result<Self> {
        let mut index = Self::new()?;
        let output = run_git_bounded(
            worktree,
            None,
            &["rev-parse", "--git-path", "objects"],
            None,
            64 * 1024,
            operation,
        )?;
        if output.truncated {
            bail!("git object directory path is unexpectedly long");
        }
        index.alternate_object_directory = PathBuf::from(
            String::from_utf8(output.bytes)
                .context("git object directory path is not UTF-8")?
                .trim_end_matches(['\r', '\n']),
        );
        if !index.alternate_object_directory.is_absolute() {
            index.alternate_object_directory = worktree.join(&index.alternate_object_directory);
        }
        Ok(index)
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    let out = trouve_process::output(&mut command)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    git_result(dir, args, out.status, out.stdout, out.stderr)
}

enum GitCommandInput {
    Bytes(Vec<u8>),
}

struct GitIoThreads {
    input: Option<thread::JoinHandle<std::io::Result<()>>>,
    stdout: thread::JoinHandle<std::io::Result<BoundedGitOutput>>,
    stderr: thread::JoinHandle<std::io::Result<BoundedGitOutput>>,
}

impl GitIoThreads {
    fn detach(self) {
        drop(self.input);
        drop(self.stdout);
        drop(self.stderr);
    }

    fn join(self) -> Result<(BoundedGitOutput, BoundedGitOutput)> {
        if let Some(writer) = self.input {
            writer
                .join()
                .map_err(|_| anyhow::anyhow!("git stdin writer panicked"))??;
        }
        let stdout = self
            .stdout
            .join()
            .map_err(|_| anyhow::anyhow!("git stdout reader panicked"))??;
        let stderr = self
            .stderr
            .join()
            .map_err(|_| anyhow::anyhow!("git stderr reader panicked"))??;
        Ok((stdout, stderr))
    }

    fn join_until(self, deadline: Instant) -> Result<(BoundedGitOutput, BoundedGitOutput)> {
        loop {
            let input_finished = self
                .input
                .as_ref()
                .map(thread::JoinHandle::is_finished)
                .unwrap_or(true);
            if input_finished && self.stdout.is_finished() && self.stderr.is_finished() {
                return self.join();
            }
            let now = Instant::now();
            if now >= deadline {
                self.detach();
                bail!("timed out draining git process pipes");
            }
            thread::sleep(COMMAND_POLL_INTERVAL.min(deadline - now));
        }
    }
}

struct BoundedGitOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

struct GitOperation<'a> {
    deadline: Instant,
    timeout: Duration,
    label: &'static str,
    cancel: Option<&'a tokio_util::sync::CancellationToken>,
}

impl<'a> GitOperation<'a> {
    fn new(cancel: Option<&'a tokio_util::sync::CancellationToken>) -> Self {
        Self {
            deadline: Instant::now() + SESSION_DIFF_GIT_TIMEOUT,
            timeout: SESSION_DIFF_GIT_TIMEOUT,
            label: "session diff",
            cancel,
        }
    }

    fn with_timeout(
        cancel: Option<&'a tokio_util::sync::CancellationToken>,
        timeout: Duration,
        label: &'static str,
    ) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            timeout,
            label,
            cancel,
        }
    }

    fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            bail!("{} cancelled", self.label);
        }
        Ok(())
    }

    fn check(&self) -> Result<()> {
        self.check_cancelled()?;
        if Instant::now() >= self.deadline {
            bail!(
                "{} timed out after {}s",
                self.label,
                self.timeout.as_secs_f32()
            );
        }
        Ok(())
    }
}

fn read_bounded_and_drain(mut pipe: impl Read, limit: usize) -> std::io::Result<BoundedGitOutput> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(BoundedGitOutput { bytes, truncated })
}

fn clear_inherited_git_process_controls(command: &mut Command) {
    const DANGEROUS_GIT_ENV: &[&str] = &[
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_QUARANTINE_PATH",
        "GIT_SHALLOW_FILE",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_EXEC_PATH",
        "GIT_TEMPLATE_DIR",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_SSH_VARIANT",
        "GIT_PROXY_COMMAND",
        "GIT_ASKPASS",
        "GIT_TERMINAL_PROMPT",
        "GIT_ATTR_NOSYSTEM",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_PROTOCOL",
        "GIT_PROTOCOL_FROM_USER",
        "GIT_ALLOW_PROTOCOL",
    ];
    for name in DANGEROUS_GIT_ENV {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let normalized = name.to_string_lossy().to_ascii_uppercase();
        if normalized.starts_with("GIT_")
            || matches!(
                normalized.as_str(),
                "SSH_ASKPASS" | "SSH_ASKPASS_REQUIRE" | "GCM_INTERACTIVE"
            )
        {
            command.env_remove(name);
        }
    }
}

fn run_git_bounded(
    dir: &Path,
    index: Option<&TemporaryCheckpointIndex>,
    args: &[&str],
    input: Option<GitCommandInput>,
    max_stdout: usize,
    operation: &GitOperation<'_>,
) -> Result<BoundedGitOutput> {
    operation.check()?;
    let execution_deadline = operation.deadline - PROCESS_TREE_CLEANUP_RESERVE;
    if Instant::now() >= execution_deadline {
        bail!(
            "{} timed out after {}s",
            operation.label,
            operation.timeout.as_secs_f32()
        );
    }
    let mut command = Command::new("git");
    // `git -C` is not a confinement boundary while inherited GIT_DIR,
    // GIT_WORK_TREE, object-directory, config, exec-path, or askpass controls
    // remain active. Strip the complete Git namespace before adding the few
    // values owned by this invocation.
    clear_inherited_git_process_controls(&mut command);
    let disabled_hooks = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .arg("-C")
        .arg(dir)
        // A repository-local fsmonitor hook is executable configuration. None
        // of the review plumbing needs it, and review must remain side-effect
        // free even for a hostile checkout.
        .args(["-c", "core.fsmonitor=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={disabled_hooks}"))
        .args(["-c", "protocol.ext.allow=never"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0");
    if args.first() == Some(&"--glob-pathspecs") {
        // Only fixed, internal aggregate commands opt in to magic pathspecs so
        // they can exclude harness-owned attachment copies. User-supplied
        // selected paths continue through the literal default below.
        command.env_remove("GIT_LITERAL_PATHSPECS");
    } else {
        command.env("GIT_LITERAL_PATHSPECS", "1");
    }
    if let Some(index) = index {
        command
            .env("GIT_INDEX_FILE", &index.path)
            .env("GIT_OBJECT_DIRECTORY", &index.object_directory)
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                &index.alternate_object_directory,
            );
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = trouve_agents::process_env::spawn_blocking_process_tree(&mut command)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    let input_writer = input.map(|input| {
        let mut stdin = child.take_stdin().expect("piped git stdin");
        thread::spawn(move || -> std::io::Result<()> {
            match input {
                GitCommandInput::Bytes(bytes) => stdin.write_all(&bytes),
            }
        })
    });
    let stdout = child.take_stdout().context("capturing git stdout")?;
    let stderr = child.take_stderr().context("capturing git stderr")?;
    let stdout_reader = thread::spawn(move || read_bounded_and_drain(stdout, max_stdout));
    let stderr_reader = thread::spawn(move || read_bounded_and_drain(stderr, MAX_GIT_STDERR_BYTES));
    let io_threads = GitIoThreads {
        input: input_writer,
        stdout: stdout_reader,
        stderr: stderr_reader,
    };
    let status = loop {
        if operation.cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            if let Err(error) = child.terminate_and_reap_until(operation.deadline) {
                io_threads.detach();
                bail!(
                    "{} cancelled; process-tree cleanup failed: {error}",
                    operation.label
                );
            }
            let _ = io_threads.join_until(operation.deadline);
            bail!("{} cancelled", operation.label);
        }
        if let Some(status) = child
            .try_wait_until(operation.deadline)
            .with_context(|| format!("waiting for git {args:?} in {}", dir.display()))?
        {
            break status;
        }
        let now = Instant::now();
        if now >= execution_deadline {
            if let Err(error) = child.terminate_and_reap_until(operation.deadline) {
                io_threads.detach();
                bail!(
                    "git {} timed out and process-tree cleanup failed in {}: {error}",
                    args.join(" "),
                    dir.display()
                );
            }
            let _ = io_threads.join_until(operation.deadline);
            bail!(
                "git {} timed out after {}s in {}",
                args.join(" "),
                operation.timeout.as_secs_f32(),
                dir.display()
            );
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(execution_deadline - now));
    };
    let (stdout, stderr) = io_threads.join_until(operation.deadline)?;
    if !status.success() {
        let suffix = if stderr.truncated {
            "\n… stderr truncated"
        } else {
            ""
        };
        bail!(
            "git {} failed in {}: {}{}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&stderr.bytes).trim(),
            suffix
        );
    }
    Ok(stdout)
}

fn validate_session_diff_numstat(
    dir: &Path,
    base_ref: &str,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<()> {
    let args = [
        "--glob-pathspecs",
        "diff",
        "--cached",
        "--submodule=short",
        "--no-ext-diff",
        "--no-textconv",
        "--numstat",
        "--end-of-options",
        base_ref,
        "--",
        ".",
        ":(exclude,top).trouve/attachments/**",
    ];
    let output = run_git_bounded(
        dir,
        Some(index),
        &args,
        None,
        MAX_SESSION_DIFF_BYTES,
        operation,
    )?;
    if output.truncated {
        return Err(session_diff_too_large(format!(
            "session diff metadata is too large to render (more than \
             {MAX_SESSION_DIFF_BYTES} bytes)"
        )));
    }
    let mut files = 0usize;
    let mut changed_lines = 0u64;
    for line in output.bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(line);
        let mut fields = text.splitn(3, '\t');
        let added = fields.next().unwrap_or_default();
        let deleted = fields.next().unwrap_or_default();
        if fields.next().is_some() {
            files = files.saturating_add(1);
            // Binary diffs use "-" counts and normally render as one short
            // marker, so only textual line counts contribute to the budget.
            if let (Ok(added), Ok(deleted)) = (added.parse::<u64>(), deleted.parse::<u64>()) {
                changed_lines = changed_lines.saturating_add(added.saturating_add(deleted));
            }
        }
        if files > MAX_SESSION_DIFF_FILES || changed_lines > MAX_SESSION_DIFF_CHANGED_LINES {
            return Err(session_diff_too_large(format!(
                "session diff is too large to render ({files} files, \
                 {changed_lines} changed lines; limit is {MAX_SESSION_DIFF_FILES} files or \
                 {MAX_SESSION_DIFF_CHANGED_LINES} changed lines)"
            )));
        }
    }
    Ok(())
}

fn bounded_session_diff(
    dir: &Path,
    base_ref: &str,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<String> {
    let args = [
        "--glob-pathspecs",
        "diff",
        "--cached",
        "--submodule=short",
        "--no-ext-diff",
        "--no-textconv",
        "--end-of-options",
        base_ref,
        "--",
        ".",
        ":(exclude,top).trouve/attachments/**",
    ];
    let output = run_git_bounded(
        dir,
        Some(index),
        &args,
        None,
        MAX_SESSION_DIFF_BYTES,
        operation,
    )?;
    if output.truncated {
        return Err(session_diff_too_large(format!(
            "session diff is too large to render (more than {MAX_SESSION_DIFF_BYTES} bytes)"
        )));
    }
    Ok(String::from_utf8_lossy(&output.bytes).into_owned())
}

fn bounded_session_diff_path(
    dir: &Path,
    base_ref: &str,
    path: &str,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<String> {
    let args = [
        "diff",
        "--cached",
        "--submodule=short",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--end-of-options",
        base_ref,
        "--",
        path,
    ];
    let output = run_git_bounded(
        dir,
        Some(index),
        &args,
        None,
        MAX_SESSION_FILE_DIFF_BYTES,
        operation,
    )?;
    if output.truncated {
        return Err(session_diff_too_large(format!(
            "selected file diff is too large to render (more than \
             {MAX_SESSION_FILE_DIFF_BYTES} bytes)"
        )));
    }
    Ok(String::from_utf8_lossy(&output.bytes).into_owned())
}

fn git_with_index(dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_INDEX_FILE", index);
    let out = trouve_process::output(&mut command)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    git_result(dir, args, out.status, out.stdout, out.stderr)
}

#[derive(Clone)]
struct SnapshotIndexEntry {
    mode: String,
    oid: String,
    stat: Option<SnapshotIndexStat>,
}

enum SnapshotOverlay {
    Ready(SnapshotIndexEntry),
    Blob { mode: String, bytes: Vec<u8> },
}

#[derive(Clone)]
struct SnapshotIndexStat {
    ctime_seconds: i64,
    ctime_nanoseconds: i64,
    mtime_seconds: i64,
    mtime_nanoseconds: i64,
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    size: u64,
    flags: u32,
}

const INDEX_FLAG_ASSUME_VALID: u32 = 0x0000_8000;
const INDEX_FLAG_INTENT_TO_ADD: u32 = 0x2000_0000;

impl SnapshotIndexStat {
    fn skip_worktree(&self) -> bool {
        self.flags & 0x4000_0000 != 0
    }

    fn forces_worktree_check(&self) -> bool {
        self.flags & (INDEX_FLAG_ASSUME_VALID | INDEX_FLAG_INTENT_TO_ADD) != 0
            || self.skip_worktree()
    }
}

fn parse_nul_paths(bytes: &[u8], operation: &str) -> Result<Vec<String>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            String::from_utf8(entry.to_vec())
                .with_context(|| format!("{operation} returned a non-UTF-8 repository path"))
        })
        .collect()
}

fn parse_pair<T: std::str::FromStr>(line: &str, label: &str) -> Result<(T, T)>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let values = line
        .strip_prefix(label)
        .with_context(|| format!("invalid git index stat line: {line:?}"))?;
    let (first, second) = values
        .split_once(':')
        .with_context(|| format!("invalid git index stat pair: {line:?}"))?;
    Ok((first.trim().parse()?, second.trim().parse()?))
}

fn parse_tab_pair<T: std::str::FromStr>(
    line: &str,
    first_label: &str,
    second_label: &str,
) -> Result<(T, T)>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let (first, second) = line
        .split_once('\t')
        .with_context(|| format!("invalid git index stat line: {line:?}"))?;
    let first = first
        .strip_prefix(first_label)
        .with_context(|| format!("invalid git index stat line: {line:?}"))?
        .trim()
        .parse()?;
    let second = second
        .strip_prefix(second_label)
        .with_context(|| format!("invalid git index stat line: {line:?}"))?
        .trim()
        .parse()?;
    Ok((first, second))
}

fn parse_index_stat(block: &[u8]) -> Result<SnapshotIndexStat> {
    let block = std::str::from_utf8(block).context("git index stat metadata is not UTF-8")?;
    let mut lines = block.lines();
    let (ctime_seconds, ctime_nanoseconds) = parse_pair(
        lines.next().context("git index entry missing ctime")?,
        "  ctime: ",
    )?;
    let (mtime_seconds, mtime_nanoseconds) = parse_pair(
        lines.next().context("git index entry missing mtime")?,
        "  mtime: ",
    )?;
    let (device, inode) = parse_tab_pair(
        lines
            .next()
            .context("git index entry missing device/inode")?,
        "  dev: ",
        "ino: ",
    )?;
    let (uid, gid) = parse_tab_pair(
        lines.next().context("git index entry missing uid/gid")?,
        "  uid: ",
        "gid: ",
    )?;
    let size_flags = lines.next().context("git index entry missing size/flags")?;
    let (size, flags_text) = size_flags
        .split_once('\t')
        .context("invalid git index size/flags line")?;
    let size = size
        .strip_prefix("  size: ")
        .context("invalid git index size line")?
        .trim()
        .parse()?;
    let flags_text = flags_text
        .strip_prefix("flags: ")
        .context("invalid git index flags line")?
        .trim();
    let flags = u32::from_str_radix(flags_text, 16)
        .with_context(|| format!("invalid git index flags: {flags_text:?}"))?;
    Ok(SnapshotIndexStat {
        ctime_seconds,
        ctime_nanoseconds,
        mtime_seconds,
        mtime_nanoseconds,
        device,
        inode,
        uid,
        gid,
        size,
        flags,
    })
}

fn parse_stage_entries(bytes: &[u8]) -> Result<BTreeMap<String, SnapshotIndexEntry>> {
    let mut entries = BTreeMap::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let nul = bytes[cursor..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|position| cursor + position)
            .context("git ls-files --debug entry is missing path terminator")?;
        let record = &bytes[cursor..nul];
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("invalid git ls-files --stage record")?;
        let (header, path) = (&record[..tab], &record[tab + 1..]);
        let header = std::str::from_utf8(header).context("invalid git index metadata")?;
        let path = String::from_utf8(path.to_vec())
            .context("git index contains a non-UTF-8 repository path")?;
        let mut fields = header.split_whitespace();
        let mode = fields.next().context("git index entry missing mode")?;
        let oid = fields.next().context("git index entry missing object id")?;
        let stage = fields.next().context("git index entry missing stage")?;
        let stat_start = nul + 1;
        let stat_end = bytes[stat_start..]
            .windows(b"\n  size: ".len())
            .position(|window| window == b"\n  size: ")
            .map(|position| stat_start + position)
            .and_then(|size_line_start| {
                bytes[size_line_start + 1..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|position| size_line_start + 1 + position + 1)
            })
            .context("git ls-files --debug entry is missing stat terminator")?;
        let stat = parse_index_stat(&bytes[stat_start..stat_end])?;
        if stage != "0" {
            bail!("cannot review a worktree with unresolved index conflicts at {path:?}");
        }
        if entries
            .insert(
                path.clone(),
                SnapshotIndexEntry {
                    mode: mode.to_string(),
                    oid: oid.to_string(),
                    stat: Some(stat),
                },
            )
            .is_some()
        {
            bail!("git index returned duplicate stage-zero entry for {path:?}");
        }
        cursor = stat_end;
    }
    Ok(entries)
}

#[cfg(unix)]
fn raw_symlink_target_beneath(worktree: &Path, relative: &str) -> Result<Vec<u8>> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let components = Path::new(relative).components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("invalid repository-relative path: {relative:?}");
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut parent: OwnedFd = options
        .open(worktree)
        .with_context(|| {
            format!(
                "opening worktree root without links: {}",
                worktree.display()
            )
        })?
        .into();
    for component in &components[..components.len() - 1] {
        let std::path::Component::Normal(name) = component else {
            unreachable!("validated above")
        };
        let name = CString::new(name.as_bytes())
            .context("repository path component contains a NUL byte")?;
        // SAFETY: parent and name are valid for this call; the returned fd is
        // immediately transferred into OwnedFd.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("opening symlink parent beneath worktree: {relative:?}"));
        }
        // SAFETY: openat returned one newly-owned descriptor.
        parent = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    let std::path::Component::Normal(name) = components.last().unwrap() else {
        unreachable!("validated above")
    };
    let name = CString::new(name.as_bytes()).context("symlink name contains a NUL byte")?;
    let mut target = vec![0_u8; 64 * 1024];
    // SAFETY: the descriptor and name are valid and target exposes its full
    // writable allocation for readlinkat.
    let length = unsafe {
        libc::readlinkat(
            parent.as_raw_fd(),
            name.as_ptr(),
            target.as_mut_ptr().cast(),
            target.len(),
        )
    };
    if length < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("reading symlink target beneath worktree: {relative:?}"));
    }
    let length = usize::try_from(length).context("invalid symlink target length")?;
    if length == target.len() {
        bail!("symlink target is too long to snapshot safely: {relative:?}");
    }
    target.truncate(length);
    Ok(target)
}

#[cfg(not(unix))]
fn raw_symlink_target_beneath(worktree: &Path, relative: &str) -> Result<Vec<u8>> {
    #[cfg(windows)]
    if Path::new(relative).components().count() != 1 {
        bail!(
            "nested symlink snapshots are unsupported without handle-relative traversal on Windows: {relative:?}"
        );
    }
    let path = worktree.join(relative);
    let target = std::fs::read_link(&path)
        .with_context(|| format!("reading symlink target {}", path.display()))?;
    Ok(target.to_string_lossy().as_bytes().to_vec())
}

fn ensure_path_ancestors_are_real_directories(worktree: &Path, relative: &str) -> Result<()> {
    let path = Path::new(relative);
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut inspected = worktree.to_path_buf();
    for component in parent.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("invalid repository-relative path: {relative:?}");
        };
        inspected.push(component);
        let metadata = std::fs::symlink_metadata(&inspected)
            .with_context(|| format!("reading worktree path metadata: {relative:?}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("worktree path has a linked or non-directory ancestor: {relative:?}");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                bail!("worktree path has a reparse-point ancestor: {relative:?}");
            }
        }
    }
    Ok(())
}

fn hash_snapshot_blobs(
    worktree: &Path,
    index: &TemporaryCheckpointIndex,
    blobs: Vec<(String, String, Vec<u8>)>,
    operation: &GitOperation<'_>,
) -> Result<Vec<(String, SnapshotIndexEntry)>> {
    if blobs.is_empty() {
        return Ok(Vec::new());
    }
    let hash_inputs = tempfile::Builder::new()
        .prefix("trouve-diff-blobs-")
        .tempdir()
        .context("creating private diff blob input directory")?;
    let mut stdin_paths = Vec::new();
    let mut metadata = Vec::with_capacity(blobs.len());
    for (sequence, (path, mode, bytes)) in blobs.into_iter().enumerate() {
        let input = hash_inputs.path().join(sequence.to_string());
        std::fs::write(&input, bytes).context("writing private diff blob input")?;
        let encoded = input.as_os_str().as_encoded_bytes();
        if encoded.contains(&b'\n') || encoded.contains(&b'\r') {
            bail!("temporary diff blob path cannot be represented safely to git");
        }
        stdin_paths.extend_from_slice(encoded);
        stdin_paths.push(b'\n');
        metadata.push((path, mode));
    }
    let output = run_git_bounded(
        worktree,
        Some(index),
        &["hash-object", "-w", "--no-filters", "--stdin-paths"],
        Some(GitCommandInput::Bytes(stdin_paths)),
        metadata.len().saturating_mul(65),
        operation,
    )?;
    if output.truncated {
        bail!("git hash-object returned unexpectedly much output");
    }
    let oids = std::str::from_utf8(&output.bytes)
        .context("git hash-object returned non-UTF-8 object ids")?
        .lines()
        .collect::<Vec<_>>();
    if oids.len() != metadata.len() {
        bail!(
            "git hash-object returned {} object ids for {} inputs",
            oids.len(),
            metadata.len()
        );
    }
    metadata
        .into_iter()
        .zip(oids)
        .map(|((path, mode), oid)| {
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("git hash-object returned an invalid object id: {oid:?}");
            }
            Ok((
                path,
                SnapshotIndexEntry {
                    mode,
                    oid: oid.to_string(),
                    stat: None,
                },
            ))
        })
        .collect()
}

fn executable_mode(
    metadata: &std::fs::Metadata,
    preserve_mode: Option<&str>,
    filemode: bool,
) -> String {
    if !filemode {
        if let Some(mode @ ("100644" | "100755")) = preserve_mode {
            return mode.to_string();
        }
        return "100644".to_string();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 != 0 {
            return "100755".to_string();
        }
    }
    "100644".to_string()
}

#[cfg(unix)]
fn metadata_matches_index(metadata: &std::fs::Metadata, stat: &SnapshotIndexStat) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    metadata.ctime() == stat.ctime_seconds
        && metadata.ctime_nsec() == stat.ctime_nanoseconds
        && metadata.mtime() == stat.mtime_seconds
        && metadata.mtime_nsec() == stat.mtime_nanoseconds
        && u64::from(metadata.dev() as u32) == stat.device
        && u64::from(metadata.ino() as u32) == stat.inode
        && metadata.uid() == stat.uid
        && metadata.gid() == stat.gid
        && u64::from(metadata.size() as u32) == stat.size
}

#[cfg(windows)]
fn metadata_matches_index(_metadata: &std::fs::Metadata, _stat: &SnapshotIndexStat) -> bool {
    // Git's Windows index stat does not expose enough stable file identity
    // here (notably ctime/inode equivalents) to prove that an unchanged size
    // and mtime still names the indexed bytes. Hash by default instead of
    // allowing a same-size, restored-timestamp edit to disappear from review.
    false
}

#[cfg(not(any(unix, windows)))]
fn metadata_matches_index(_metadata: &std::fs::Metadata, _stat: &SnapshotIndexStat) -> bool {
    false
}

#[cfg(unix)]
fn open_regular_beneath(
    worktree: &Path,
    relative: &str,
) -> Result<(std::fs::File, std::fs::Metadata)> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut directory = OpenOptions::new();
    directory
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let mut parent: OwnedFd = directory
        .open(worktree)
        .with_context(|| {
            format!(
                "opening worktree root without following links: {}",
                worktree.display()
            )
        })?
        .into();
    let components = Path::new(relative).components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("invalid repository-relative path: {relative:?}");
    }
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            unreachable!("validated above")
        };
        use std::os::unix::ffi::OsStrExt as _;
        let name = CString::new(name.as_bytes())
            .context("repository path component contains a NUL byte")?;
        let final_component = index + 1 == components.len();
        let flags = if final_component {
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        // SAFETY: parent and name remain owned for the duration of openat.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!("opening worktree path beneath root without links: {relative:?}")
            });
        }
        // SAFETY: openat returned one newly-owned descriptor.
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if final_component {
            let file = std::fs::File::from(opened);
            let metadata = file
                .metadata()
                .with_context(|| format!("reading opened worktree file metadata: {relative:?}"))?;
            if !metadata.is_file() {
                bail!("worktree path is not a regular file: {relative:?}");
            }
            return Ok((file, metadata));
        }
        parent = opened;
    }
    unreachable!("non-empty component list returns on its final component")
}

#[cfg(not(unix))]
fn open_regular_beneath(
    worktree: &Path,
    relative: &str,
) -> Result<(std::fs::File, std::fs::Metadata)> {
    #[cfg(windows)]
    if Path::new(relative).components().count() != 1 {
        bail!(
            "nested worktree snapshots are unsupported without handle-relative traversal on Windows: {relative:?}"
        );
    }
    let mut inspected = worktree.to_path_buf();
    for component in Path::new(relative).components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            bail!("invalid repository-relative path: {relative:?}");
        }
        inspected.push(component);
        let metadata = std::fs::symlink_metadata(&inspected)
            .with_context(|| format!("reading worktree path metadata: {relative:?}"))?;
        if metadata.file_type().is_symlink() {
            bail!("worktree path contains a symbolic link: {relative:?}");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                != 0
            {
                bail!("worktree path contains a reparse point: {relative:?}");
            }
        }
    }
    open_regular_without_following(&inspected)
}

#[cfg(not(unix))]
fn open_regular_without_following(path: &Path) -> Result<(std::fs::File, std::fs::Metadata)> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).with_context(|| {
        format!(
            "opening worktree file without following links: {}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading opened file metadata: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("worktree path is not a regular file: {}", path.display());
    }
    Ok((file, metadata))
}

#[cfg(unix)]
fn same_opened_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    before.dev() == opened.dev()
        && before.ino() == opened.ino()
        && before.size() == opened.size()
        && before.ctime() == opened.ctime()
        && before.ctime_nsec() == opened.ctime_nsec()
        && before.mtime() == opened.mtime()
        && before.mtime_nsec() == opened.mtime_nsec()
}

#[cfg(windows)]
fn same_opened_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    before.file_attributes() == opened.file_attributes()
        && before.file_size() == opened.file_size()
        && before.creation_time() == opened.creation_time()
        && before.last_write_time() == opened.last_write_time()
}

#[cfg(windows)]
fn opened_file_identity(file: &std::fs::File) -> Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error())
            .context("querying opened worktree file identity");
    }
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
fn same_opened_file(_before: &std::fs::Metadata, _opened: &std::fs::Metadata) -> bool {
    false
}

fn snapshot_entry_for_path(
    worktree: &Path,
    path: &str,
    tracked: Option<&SnapshotIndexEntry>,
    filemode: bool,
    _operation: &GitOperation<'_>,
) -> Result<Option<SnapshotOverlay>> {
    ensure_path_ancestors_are_real_directories(worktree, path)?;
    let absolute = worktree.join(path);
    let metadata = match std::fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if tracked.is_some_and(|entry| {
                entry.mode == "160000"
                    || entry
                        .stat
                        .as_ref()
                        .is_some_and(SnapshotIndexStat::skip_worktree)
            }) {
                return Ok(tracked.cloned().map(SnapshotOverlay::Ready));
            }
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading metadata for {path:?}"));
        }
    };

    if tracked.is_some_and(|entry| entry.mode == "160000") && metadata.is_dir() {
        // Running Git with a pathname cwd would follow a directory swapped to
        // a symlink after the metadata check. There is no portable way to give
        // Git a pre-opened directory handle, so materialized submodule overlays
        // fail closed rather than allowing a raced current_dir escape.
        bail!("cannot safely render a review diff with materialized submodule {path:?}");
    }

    let (mode, bytes) = if metadata.file_type().is_symlink() {
        (
            "120000".to_string(),
            raw_symlink_target_beneath(worktree, path)?,
        )
    } else if metadata.is_file() {
        let mode = if tracked.is_some_and(|entry| entry.mode == "120000") {
            // With core.symlinks=false, a tracked symlink is checked out as a
            // regular file containing its target text.
            "120000".to_string()
        } else {
            executable_mode(
                &metadata,
                tracked.map(|entry| entry.mode.as_str()),
                filemode,
            )
        };
        let (mut file, opened_metadata) = open_regular_beneath(worktree, path)?;
        #[cfg(windows)]
        let opened_identity = opened_file_identity(&file)?;
        if !same_opened_file(&metadata, &opened_metadata) {
            bail!("worktree file changed while preparing diff: {path:?}");
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(opened_metadata.len())
                .unwrap_or(usize::MAX)
                .min(1024 * 1024),
        );
        file.read_to_end(&mut bytes)
            .with_context(|| format!("reading worktree file {path:?}"))?;
        let after_read = file
            .metadata()
            .with_context(|| format!("re-reading opened file metadata: {path:?}"))?;
        #[cfg(windows)]
        if opened_file_identity(&file)? != opened_identity {
            bail!("worktree file changed while preparing diff: {path:?}");
        }
        if !same_opened_file(&opened_metadata, &after_read)
            || u64::try_from(bytes.len()).ok() != Some(after_read.len())
        {
            bail!("worktree file changed while preparing diff: {path:?}");
        }
        (mode, bytes)
    } else {
        return Ok(None);
    };
    Ok(Some(SnapshotOverlay::Blob { mode, bytes }))
}

fn build_session_snapshot_index(
    worktree: &Path,
    operation: &GitOperation<'_>,
) -> Result<TemporaryCheckpointIndex> {
    let index = TemporaryCheckpointIndex::for_worktree(worktree, operation)?;
    run_git_bounded(
        worktree,
        Some(&index),
        &["read-tree", "--empty"],
        None,
        1,
        operation,
    )?;

    // Reuse object ids from the real index for stat-clean paths. This both
    // preserves staged choices and avoids false changes for CRLF/smudge-filter
    // checkouts whose worktree bytes intentionally differ from blob bytes.
    let staged = run_git_bounded(
        worktree,
        None,
        &["ls-files", "--stage", "--debug", "-z", "--"],
        None,
        MAX_SESSION_SNAPSHOT_MANIFEST_BYTES,
        operation,
    )?;
    if staged.truncated {
        return Err(session_diff_too_large(format!(
            "session file manifest is too large (more than \
             {MAX_SESSION_SNAPSHOT_MANIFEST_BYTES} bytes)"
        )));
    }
    let mut entries = parse_stage_entries(&staged.bytes)?;
    entries.retain(|path, _| !is_session_internal_path(path));
    if entries.len() > MAX_SESSION_SNAPSHOT_ENTRIES {
        return Err(session_diff_too_large(format!(
            "session file manifest has more than {MAX_SESSION_SNAPSHOT_ENTRIES} entries"
        )));
    }

    let untracked = run_git_bounded(
        worktree,
        None,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
        None,
        MAX_SESSION_SNAPSHOT_MANIFEST_BYTES,
        operation,
    )?;
    if untracked.truncated {
        return Err(session_diff_too_large(format!(
            "session file manifest is too large (more than \
             {MAX_SESSION_SNAPSHOT_MANIFEST_BYTES} bytes)"
        )));
    }

    let real_index = run_git_bounded(
        worktree,
        None,
        &["rev-parse", "--git-path", "index"],
        None,
        64 * 1024,
        operation,
    )?;
    if real_index.truncated {
        bail!("git index path is unexpectedly long");
    }
    let real_index = PathBuf::from(
        String::from_utf8(real_index.bytes)
            .context("git index path is not UTF-8")?
            .trim_end_matches(['\r', '\n']),
    );
    let real_index = if real_index.is_absolute() {
        real_index
    } else {
        worktree.join(real_index)
    };
    let index_modified = std::fs::metadata(real_index)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| {
            (
                duration.as_secs() as i64,
                i64::from(duration.subsec_nanos()),
            )
        });

    let mut overlay = Vec::new();
    for (path, entry) in &entries {
        let metadata = std::fs::symlink_metadata(worktree.join(path));
        let clean = metadata.as_ref().ok().is_some_and(|metadata| {
            entry.stat.as_ref().is_some_and(|stat| {
                !stat.forces_worktree_check()
                    && metadata_matches_index(metadata, stat)
                    // Git considers an otherwise stat-clean entry racy only
                    // when its worktree mtime is not strictly older than the
                    // index file. Compare nanoseconds too: second-only
                    // comparison falsely dirties ordinary CRLF checkouts made
                    // just before an index write in the same second.
                    && index_modified.is_some_and(|index_time| {
                        (stat.mtime_seconds, stat.mtime_nanoseconds) < index_time
                    })
            })
        });
        let absent_sparse = metadata.as_ref().is_err_and(|error| {
            error.kind() == std::io::ErrorKind::NotFound
                && entry
                    .stat
                    .as_ref()
                    .is_some_and(SnapshotIndexStat::skip_worktree)
        });
        if entry.mode == "160000" && metadata.is_ok() || !clean && !absent_sparse {
            overlay.push(path.clone());
        }
    }
    overlay.extend(
        parse_nul_paths(&untracked.bytes, "git ls-files")?
            .into_iter()
            .filter(|path| !is_session_internal_path(path)),
    );
    overlay.sort_unstable();
    overlay.dedup();
    if overlay.len() > MAX_SESSION_SNAPSHOT_CHANGED_PATHS {
        return Err(session_diff_too_large(format!(
            "session diff is too large to render (more than \
             {MAX_SESSION_SNAPSHOT_CHANGED_PATHS} changed paths)"
        )));
    }

    let filemode = run_git_bounded(
        worktree,
        None,
        &["config", "--type=bool", "--get", "core.filemode"],
        None,
        16,
        operation,
    )
    .ok()
    .and_then(|output| String::from_utf8(output.bytes).ok())
    .is_some_and(|value| value.trim() == "true");
    let mut hashed_bytes = 0_u64;
    let mut blobs = Vec::new();
    for path in overlay {
        operation.check()?;
        let prior = entries.get(&path).cloned();
        if let Ok(metadata) = std::fs::symlink_metadata(worktree.join(&path))
            && metadata.is_file()
        {
            hashed_bytes = hashed_bytes.saturating_add(metadata.len());
            if hashed_bytes > MAX_SESSION_SNAPSHOT_HASH_BYTES {
                return Err(session_diff_too_large(format!(
                    "session changes contain more than {MAX_SESSION_SNAPSHOT_HASH_BYTES} bytes"
                )));
            }
        }
        match snapshot_entry_for_path(worktree, &path, prior.as_ref(), filemode, operation)? {
            Some(SnapshotOverlay::Ready(entry)) => {
                entries.insert(path, entry);
            }
            Some(SnapshotOverlay::Blob { mode, bytes }) => blobs.push((path, mode, bytes)),
            None => {
                entries.remove(&path);
            }
        }
    }
    for (path, entry) in hash_snapshot_blobs(worktree, &index, blobs, operation)? {
        entries.insert(path, entry);
    }

    let mut index_info = Vec::new();
    for (path, entry) in entries {
        index_info.extend_from_slice(entry.mode.as_bytes());
        index_info.push(b' ');
        index_info.extend_from_slice(entry.oid.as_bytes());
        index_info.push(b'\t');
        index_info.extend_from_slice(path.as_bytes());
        index_info.push(0);
        if index_info.len() > MAX_SESSION_SNAPSHOT_MANIFEST_BYTES {
            return Err(session_diff_too_large(format!(
                "session file manifest is too large (more than \
                 {MAX_SESSION_SNAPSHOT_MANIFEST_BYTES} bytes)"
            )));
        }
    }
    run_git_bounded(
        worktree,
        Some(&index),
        &["update-index", "-z", "--index-info"],
        Some(GitCommandInput::Bytes(index_info)),
        1,
        operation,
    )?;
    Ok(index)
}

fn with_session_snapshot_index<T>(
    worktree: &Path,
    operation: &GitOperation<'_>,
    action: impl FnOnce(&TemporaryCheckpointIndex) -> Result<T>,
) -> Result<T> {
    let index = build_session_snapshot_index(worktree, operation)?;
    action(&index)
}

fn git_as_checkpoint_identity(dir: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        // Checkpoints are trouve bookkeeping commits, and session creation
        // must not depend on the host having a global Git identity.
        .env("GIT_AUTHOR_NAME", CHECKPOINT_IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", CHECKPOINT_IDENTITY_EMAIL)
        .env("GIT_COMMITTER_NAME", CHECKPOINT_IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", CHECKPOINT_IDENTITY_EMAIL);
    let out = trouve_process::output(&mut command)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    git_result(dir, args, out.status, out.stdout, out.stderr)
}

fn git_with_timeout(dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut command = Command::new("git");
    clear_inherited_git_process_controls(&mut command);
    let disabled_hooks = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={disabled_hooks}"))
        .args(["-c", "protocol.ext.allow=never"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = trouve_agents::process_env::spawn_blocking_process_tree(&mut command)
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    let stdout = child.take_stdout().context("capturing git stdout")?;
    let stderr = child.take_stderr().context("capturing git stderr")?;
    let io_threads = GitIoThreads {
        input: None,
        stdout: thread::spawn(move || read_bounded_and_drain(stdout, MAX_SESSION_DIFF_BYTES)),
        stderr: thread::spawn(move || read_bounded_and_drain(stderr, MAX_GIT_STDERR_BYTES)),
    };

    let deadline = Instant::now() + timeout;
    let cleanup_reserve = PROCESS_TREE_CLEANUP_RESERVE.min(timeout / 2);
    let execution_deadline = deadline - cleanup_reserve;
    let status = loop {
        if let Some(status) = child
            .try_wait_until(deadline)
            .with_context(|| format!("waiting for git {args:?} in {}", dir.display()))?
        {
            break status;
        }
        let now = Instant::now();
        if now >= execution_deadline {
            if let Err(error) = child.terminate_and_reap_until(deadline) {
                io_threads.detach();
                bail!(
                    "git {} timed out and process-tree cleanup failed in {}: {error}",
                    args.join(" "),
                    dir.display()
                );
            }
            let _ = io_threads.join_until(deadline);
            bail!(
                "git {} timed out after {}s in {}",
                args.join(" "),
                timeout.as_secs_f32(),
                dir.display()
            );
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(execution_deadline - now));
    };

    let (stdout, stderr) = io_threads.join_until(deadline)?;
    if stdout.truncated {
        bail!(
            "git {} returned more than {MAX_SESSION_DIFF_BYTES} bytes in {}",
            args.join(" "),
            dir.display()
        );
    }
    git_result(dir, args, status, stdout.bytes, stderr.bytes)
}

fn git_result(
    dir: &Path,
    args: &[&str],
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<String> {
    if !status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// Reject a ref/commit-ish that could be misread by git as an option (the
/// value reaches git as a positional argument, and one starting with `-`
/// would be parsed as a flag — e.g. `git diff` accepts file-writing options
/// like `--output=`). These come from the HTTP API, so validate before use.
fn ensure_safe_ref(r: &str) -> Result<()> {
    if r.is_empty() {
        bail!("empty git ref");
    }
    if r.starts_with('-') {
        bail!("invalid git ref (must not start with '-'): {r}");
    }
    Ok(())
}

pub fn is_git_repo(path: &Path) -> bool {
    git(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s == "true")
        .unwrap_or(false)
}

pub fn head_ref(repo: &Path) -> Result<String> {
    // Prefer the branch name; fall back to the commit for detached HEAD.
    match git(repo, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(branch) => Ok(branch),
        Err(_) => git(repo, &["rev-parse", "HEAD"]),
    }
}

/// Local branch names, most recently committed first.
pub fn list_branches(repo: &Path) -> Result<Vec<String>> {
    let out = git(
        repo,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)",
            "refs/heads",
        ],
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Turn a session title into a branch-safe slug.
pub fn slugify(title: &str) -> String {
    let mut slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "session".into()
    } else {
        slug
    }
}

/// The freshly fetched upstream commit for a local base branch.
pub struct FetchedBase {
    /// Short remote-tracking ref (for example `origin/main`).
    pub upstream_ref: String,
    /// Immutable commit to use when creating the worktree branch.
    pub commit: String,
}

/// Fetch a local base branch's configured upstream without moving the local
/// branch or any checkout.
///
/// Refs that are not local branches (for example a checkpoint commit) and
/// branches without an upstream return `None` so callers can use the original
/// ref as-is. The remote-tracking ref is resolved to a commit after fetching,
/// rather than exposing the repository-global `FETCH_HEAD` to races.
pub fn fetch_upstream_base(repo: &Path, base_ref: &str) -> Result<Option<FetchedBase>> {
    fetch_upstream_base_with_timeout(repo, base_ref, FETCH_TIMEOUT)
}

fn fetch_upstream_base_with_timeout(
    repo: &Path,
    base_ref: &str,
    timeout: Duration,
) -> Result<Option<FetchedBase>> {
    ensure_safe_ref(base_ref)?;

    let full_ref = git(
        repo,
        &["rev-parse", "--symbolic-full-name", "--verify", base_ref],
    )?;
    if !full_ref.starts_with("refs/heads/") {
        return Ok(None);
    }

    let remote = git(
        repo,
        &["for-each-ref", "--format=%(upstream:remotename)", &full_ref],
    )?;
    let upstream = git(repo, &["for-each-ref", "--format=%(upstream)", &full_ref])?;
    if remote.is_empty() || upstream.is_empty() {
        return Ok(None);
    }

    git_with_timeout(repo, &["fetch", "--quiet", "--", &remote], timeout)?;
    let upstream_ref = git(
        repo,
        &["for-each-ref", "--format=%(refname:short)", &upstream],
    )?;
    let commit = git(
        repo,
        &["rev-parse", "--verify", &format!("{upstream}^{{commit}}")],
    )?;
    Ok(Some(FetchedBase {
        upstream_ref,
        commit,
    }))
}

/// Proof that this process atomically reserved and created one session
/// worktree. Failed-session cleanup requires this receipt; a preflight absence
/// check alone is never treated as ownership.
pub struct WorktreeCreation {
    worktree_path: PathBuf,
    branch_ref: String,
    branch_oid: String,
    reservation_path: PathBuf,
    reservation_token: String,
    directory_identity: same_file::Handle,
}

impl WorktreeCreation {
    pub fn worktree_path(&self) -> &Path {
        &self.worktree_path
    }
}

fn reservation_path(worktree_path: &Path) -> Result<PathBuf> {
    let parent = worktree_path
        .parent()
        .context("worktree path has no parent directory")?;
    let mut name = worktree_path
        .file_name()
        .context("worktree path has no final component")?
        .to_os_string();
    name.push(WORKTREE_RESERVATION_SUFFIX);
    Ok(parent.join(name))
}

fn write_creation_reservation(path: &Path, token: &str) -> Result<()> {
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("reserving session creation marker {}", path.display()))?;
    marker.write_all(token.as_bytes())?;
    marker.sync_all()?;
    Ok(())
}

fn reservation_matches(creation: &WorktreeCreation) -> bool {
    std::fs::read_to_string(&creation.reservation_path)
        .is_ok_and(|token| token == creation.reservation_token)
}

fn directory_identity_matches(creation: &WorktreeCreation) -> bool {
    same_file::Handle::from_path(&creation.worktree_path)
        .is_ok_and(|current| current == creation.directory_identity)
}

fn remove_creation_reservation(creation: &WorktreeCreation) -> Result<()> {
    if !reservation_matches(creation) {
        bail!(
            "session creation marker no longer belongs to this attempt: {}",
            creation.reservation_path.display()
        );
    }
    std::fs::remove_file(&creation.reservation_path).with_context(|| {
        format!(
            "removing session creation marker {}",
            creation.reservation_path.display()
        )
    })
}

fn delete_ref_if_matches(repo: &Path, reference: &str, expected_oid: &str) -> Result<()> {
    ensure_safe_ref(reference)?;
    git(repo, &["update-ref", "-d", reference, expected_oid])?;
    Ok(())
}

/// Create the session worktree on a new branch from `base_ref`.
pub fn create_worktree(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<WorktreeCreation> {
    ensure_safe_ref(base_ref)?;
    let branch_ref = format!("refs/heads/{branch}");
    ensure_safe_ref(&branch_ref)?;
    let branch_oid = git(
        repo,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )?;
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let reservation_path = reservation_path(worktree_path)?;
    let reservation_token = uuid::Uuid::new_v4().to_string();
    write_creation_reservation(&reservation_path, &reservation_token)?;
    if let Err(error) = std::fs::create_dir(worktree_path) {
        let _ = std::fs::remove_file(&reservation_path);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            bail!(
                "refusing to replace pre-existing worktree path {}",
                worktree_path.display()
            );
        }
        return Err(error)
            .with_context(|| format!("reserving worktree path {}", worktree_path.display()));
    }
    let directory_identity = match same_file::Handle::from_path(worktree_path) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = std::fs::remove_dir(worktree_path);
            let _ = std::fs::remove_file(&reservation_path);
            return Err(error)
                .with_context(|| format!("recording identity for {}", worktree_path.display()));
        }
    };

    // Empty expected OID is Git's atomic "must not exist" compare-and-swap.
    if let Err(error) = git(repo, &["update-ref", &branch_ref, &branch_oid, ""]) {
        if same_file::Handle::from_path(worktree_path)
            .is_ok_and(|current| current == directory_identity)
        {
            let _ = std::fs::remove_dir(worktree_path);
        }
        let _ = std::fs::remove_file(&reservation_path);
        return Err(error).context("atomically reserving session branch");
    }

    let creation = WorktreeCreation {
        worktree_path: worktree_path.to_path_buf(),
        branch_ref,
        branch_oid,
        reservation_path,
        reservation_token,
        directory_identity,
    };
    if let Err(error) = git(
        repo,
        &[
            "worktree",
            "add",
            worktree_path.to_str().context("non-utf8 worktree path")?,
            "--end-of-options",
            branch,
        ],
    ) {
        if let Err(rollback) = rollback_worktree_creation(repo, &creation, None) {
            return Err(error).context(format!(
                "worktree creation failed; ownership-safe rollback also failed: {rollback:#}"
            ));
        }
        return Err(error);
    }
    Ok(creation)
}

/// Remove only the artifacts proven to belong to one failed creation attempt.
pub fn rollback_worktree_creation(
    repo: &Path,
    creation: &WorktreeCreation,
    checkpoint: Option<(&str, &str, &str)>,
) -> Result<()> {
    if let Some((session_id, checkpoint_id, expected_oid)) = checkpoint {
        let reference = format!("refs/trouve/checkpoints/{session_id}/{checkpoint_id}");
        delete_ref_if_matches(repo, &reference, expected_oid)
            .context("deleting owned checkpoint ref")?;
    }

    if !reservation_matches(creation) || !directory_identity_matches(creation) {
        bail!(
            "refusing to remove ambiguous worktree path {}",
            creation.worktree_path.display()
        );
    }
    // Claim cleanup atomically before touching the checkout. If the branch
    // advanced or was rebound, CAS fails and the path remains untouched. A
    // branch recreated after this point is a new identity and is never
    // considered by this receipt again.
    delete_ref_if_matches(repo, &creation.branch_ref, &creation.branch_oid)
        .context("claiming owned session branch for cleanup")?;

    if let Err(remove_error) = remove_worktree(repo, &creation.worktree_path) {
        if !reservation_matches(creation) || !directory_identity_matches(creation) {
            return Err(remove_error)
                .context("git worktree removal failed and path ownership changed before fallback");
        }
        std::fs::remove_dir_all(&creation.worktree_path).with_context(|| {
            format!(
                "removing owned failed-session worktree {} after git cleanup failed",
                creation.worktree_path.display()
            )
        })?;
    }
    prune_worktrees(repo)?;
    remove_creation_reservation(creation)?;
    Ok(())
}

/// Release the short-lived ownership marker after session state is durable.
pub fn finalize_worktree_creation(creation: &WorktreeCreation) -> Result<()> {
    remove_creation_reservation(creation)
}

fn ref_exists(repo: &Path, reference: &str) -> Result<bool> {
    ensure_safe_ref(reference)?;
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet", reference]);
    let status = trouve_process::status(&mut command)
        .with_context(|| format!("checking git ref {reference} in {}", repo.display()))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git show-ref --verify failed in {} with {status}",
            repo.display()
        ),
    }
}

pub fn local_branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    ref_exists(repo, &format!("refs/heads/{branch}"))
}

pub fn checkpoint_ref_exists(repo: &Path, session_id: &str, checkpoint_id: &str) -> Result<bool> {
    ref_exists(
        repo,
        &format!("refs/trouve/checkpoints/{session_id}/{checkpoint_id}"),
    )
}

pub fn delete_local_branch(repo: &Path, branch: &str) -> Result<()> {
    let reference = format!("refs/heads/{branch}");
    ensure_safe_ref(&reference)?;
    git(repo, &["update-ref", "-d", &reference])?;
    Ok(())
}

pub fn delete_checkpoint_ref(repo: &Path, session_id: &str, checkpoint_id: &str) -> Result<()> {
    let reference = format!("refs/trouve/checkpoints/{session_id}/{checkpoint_id}");
    ensure_safe_ref(&reference)?;
    git(repo, &["update-ref", "-d", &reference])?;
    Ok(())
}

pub fn reconcile_checkpoint_refs(
    repo: &Path,
    session_id: &str,
    live_checkpoint_ids: &[String],
) -> Result<()> {
    use std::collections::HashSet;

    let prefix = format!("refs/trouve/checkpoints/{session_id}/");
    ensure_safe_ref(&prefix)?;
    let output = git(repo, &["for-each-ref", "--format=%(refname)", &prefix])?;
    let live = live_checkpoint_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    for reference in output.lines() {
        let Some(checkpoint_id) = reference.strip_prefix(&prefix) else {
            continue;
        };
        // Preserve legacy sequence refs from releases before immutable
        // checkpoint identities. They may still be the only GC anchor for a
        // live row and cannot be mapped safely without a migration.
        if checkpoint_id.parse::<i64>().is_ok() || live.contains(checkpoint_id) {
            continue;
        }
        git(repo, &["update-ref", "-d", reference])?;
    }
    Ok(())
}

pub fn delete_session_checkpoint_refs(repo: &Path, session_id: &str) -> Result<()> {
    let prefix = format!("refs/trouve/checkpoints/{session_id}/");
    ensure_safe_ref(&prefix)?;
    let output = git(repo, &["for-each-ref", "--format=%(refname)", &prefix])?;
    for reference in output.lines() {
        git(repo, &["update-ref", "-d", reference])?;
    }
    Ok(())
}

/// Canonical identity shared by a repository and all of its linked worktrees.
/// Git process controls inherited from the server environment are ignored.
pub fn common_directory(repo: &Path) -> Result<PathBuf> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-common-dir"]);
    clear_inherited_git_process_controls(&mut command);
    let output = trouve_process::output(&mut command)
        .with_context(|| format!("resolving git common directory in {}", repo.display()))?;
    let common = git_result(
        repo,
        &["rev-parse", "--git-common-dir"],
        output.status,
        output.stdout,
        output.stderr,
    )?;
    let common = PathBuf::from(common);
    let common = if common.is_absolute() {
        common
    } else {
        repo.join(common)
    };
    common
        .canonicalize()
        .with_context(|| format!("canonicalizing git common directory {}", common.display()))
}

/// Remove an unpersisted immutable checkpoint anchor only while it still names
/// the commit produced by that attempt. The old-OID argument is the ownership
/// proof: a concurrent replacement makes this fail closed.
pub fn rollback_checkpoint_ref(
    repo: &Path,
    session_id: &str,
    checkpoint_id: &str,
    failed_commit: &str,
) -> Result<()> {
    let reference = format!("refs/trouve/checkpoints/{session_id}/{checkpoint_id}");
    ensure_safe_ref(&reference)?;
    git(repo, &["update-ref", "-d", &reference, failed_commit])?;
    Ok(())
}

pub fn prune_worktrees(repo: &Path) -> Result<()> {
    git(repo, &["worktree", "prune"])?;
    Ok(())
}

/// Remove the session worktree. The branch is kept (the user may still want
/// to merge or inspect it).
pub fn remove_worktree(repo: &Path, worktree_path: &Path) -> Result<()> {
    git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().context("non-utf8 worktree path")?,
        ],
    )?;
    Ok(())
}

/// Snapshot the worktree as a commit on a hidden ref, without touching the
/// session branch. Returns the commit hash.
pub fn checkpoint(
    worktree: &Path,
    session_id: &str,
    checkpoint_id: &str,
    message: &str,
) -> Result<String> {
    let head = git(worktree, &["rev-parse", "HEAD"])?;
    // Build the snapshot in a disposable index. Besides preserving any
    // staging choices made by the user, starting from HEAD prevents a file
    // accidentally staged by an earlier checkpoint from remaining tracked
    // after it becomes ignored.
    let index = TemporaryCheckpointIndex::new()?;
    git_with_index(worktree, &index.path, &["read-tree", &head])?;
    // Materialized vendor-attachment copies are harness state, not user work.
    // Remove even a previously tracked copy from the private checkpoint index;
    // this never touches the real index or working tree.
    git_with_index(
        worktree,
        &index.path,
        &[
            "rm",
            "-r",
            "-f",
            "--cached",
            "--ignore-unmatch",
            "--",
            ".trouve/attachments",
        ],
    )?;
    git_with_index(
        worktree,
        &index.path,
        &[
            "add",
            "-A",
            "--",
            ".",
            ":(exclude,top).trouve/attachments/**",
        ],
    )?;
    let tree = git_with_index(worktree, &index.path, &["write-tree"])?;
    let commit = git_as_checkpoint_identity(
        worktree,
        &["commit-tree", &tree, "-p", &head, "-m", message],
    )?;
    // Anchor the commit against GC under its immutable database identity.
    // Retried or divergent checkpoints never replace an already-persisted
    // anchor, so a crash before SQLite commit leaves only a harmless orphan.
    git(
        worktree,
        &[
            "update-ref",
            &format!("refs/trouve/checkpoints/{session_id}/{checkpoint_id}"),
            &commit,
            "",
        ],
    )?;
    Ok(commit)
}

/// Whether the worktree has any changes (staged, unstaged, or untracked)
/// relative to HEAD.
pub fn has_changes(worktree: &Path) -> Result<bool> {
    Ok(!git(
        worktree,
        &[
            "status",
            "--porcelain",
            "--",
            ".",
            ":(exclude,top).trouve/attachments/**",
        ],
    )?
    .is_empty())
}

/// Restore the worktree to a checkpoint commit's tree: index := commit tree,
/// files rewritten, files absent from the commit removed (they become
/// untracked after read-tree, so a scoped clean deletes them).
pub fn restore(worktree: &Path, commit: &str) -> Result<()> {
    git(worktree, &["read-tree", "--reset", commit])?;
    git(worktree, &["checkout-index", "-f", "-a"])?;
    git(worktree, &["clean", "-fd", "-e", ".trouve/attachments/"])?;
    Ok(())
}

/// Unified diff of the session's work: base ref vs the worktree's current
/// state (includes uncommitted changes — checkpoints live on hidden refs).
pub fn session_diff(worktree: &Path, base_ref: &str) -> Result<String> {
    session_diff_with_cancel(worktree, base_ref, None)
}

pub fn session_diff_cancellable(
    worktree: &Path,
    base_ref: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    session_diff_with_cancel(worktree, base_ref, Some(cancel))
}

fn session_diff_with_cancel(
    worktree: &Path,
    base_ref: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String> {
    ensure_safe_ref(base_ref)?;
    let operation = GitOperation::new(cancel);
    with_session_snapshot_index(worktree, &operation, |index| {
        validate_session_diff_numstat(worktree, base_ref, index, &operation)?;
        bounded_session_diff(worktree, base_ref, index, &operation)
    })
}

/// Lightweight metadata for every changed path. Disabling rename detection
/// keeps the NUL-delimited numstat representation unambiguous and matches the
/// path-scoped patch endpoint: a rename is represented as one deletion and one
/// addition rather than requiring the client to understand Git's paired-path
/// encoding.
pub fn session_diff_summary(worktree: &Path, base_ref: &str) -> Result<Vec<SessionDiffStat>> {
    session_diff_summary_with_cancel(worktree, base_ref, None)
}

pub fn session_diff_summary_cancellable(
    worktree: &Path,
    base_ref: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Vec<SessionDiffStat>> {
    session_diff_summary_with_cancel(worktree, base_ref, Some(cancel))
}

fn session_diff_summary_with_cancel(
    worktree: &Path,
    base_ref: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<Vec<SessionDiffStat>> {
    ensure_safe_ref(base_ref)?;
    let operation = GitOperation::new(cancel);
    with_session_snapshot_index(worktree, &operation, |index| {
        session_diff_summary_with_index(worktree, base_ref, index, &operation)
    })
}

fn session_diff_summary_with_index(
    worktree: &Path,
    base_ref: &str,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<Vec<SessionDiffStat>> {
    let args = [
        "--glob-pathspecs",
        "diff",
        "--cached",
        "--submodule=short",
        "--no-ext-diff",
        "--no-textconv",
        "--numstat",
        "--no-renames",
        "-z",
        "--end-of-options",
        base_ref,
        "--",
        ".",
        ":(exclude,top).trouve/attachments/**",
    ];
    let output = run_git_bounded(
        worktree,
        Some(index),
        &args,
        None,
        MAX_SESSION_DIFF_BYTES,
        operation,
    )?;
    if output.truncated {
        return Err(session_diff_too_large(format!(
            "session diff metadata is too large to render (more than \
             {MAX_SESSION_DIFF_BYTES} bytes)"
        )));
    }

    let mut files = Vec::new();
    for entry in output
        .bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let text = std::str::from_utf8(entry)
            .context("git diff --numstat returned a non-UTF-8 repository path")?;
        let mut fields = text.splitn(3, '\t');
        let additions = fields.next().unwrap_or_default();
        let deletions = fields.next().unwrap_or_default();
        let Some(path) = fields.next() else {
            bail!("invalid git diff --numstat record");
        };
        let binary = additions == "-" || deletions == "-";
        let parse_count = |value: &str| -> Result<u64> {
            if value == "-" {
                Ok(0)
            } else {
                value
                    .parse()
                    .with_context(|| format!("invalid git diff --numstat count: {value:?}"))
            }
        };
        let additions = parse_count(additions)?;
        let deletions = parse_count(deletions)?;
        files.push(SessionDiffStat {
            path: path.to_string(),
            additions,
            deletions,
            binary,
        });
    }
    Ok(files)
}

/// Every changed path in git's deterministic diff order. NUL framing keeps
/// whitespace and newlines in filenames unambiguous.
pub fn session_diff_files(worktree: &Path, base_ref: &str) -> Result<Vec<String>> {
    ensure_safe_ref(base_ref)?;
    let operation = GitOperation::new(None);
    with_session_snapshot_index(worktree, &operation, |index| {
        let args = [
            "--glob-pathspecs",
            "diff",
            "--cached",
            "--submodule=short",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "-z",
            "--end-of-options",
            base_ref,
            "--",
            ".",
            ":(exclude,top).trouve/attachments/**",
        ];
        let output = run_git_bounded(
            worktree,
            Some(index),
            &args,
            None,
            MAX_SESSION_DIFF_BYTES,
            &operation,
        )?;
        if output.truncated {
            return Err(session_diff_too_large(format!(
                "session diff path manifest is too large (more than {MAX_SESSION_DIFF_BYTES} bytes)"
            )));
        }
        let paths = parse_nul_paths(&output.bytes, "git diff --name-only")?;
        Ok(paths.into_iter().filter(|path| !path.is_empty()).collect())
    })
}

/// Materialize bounded per-file patches from one immutable private-index
/// snapshot. This avoids rebuilding and rehashing the complete worktree once
/// per file in the headless review loader.
pub struct SessionReviewDiffFile {
    pub path: String,
    pub diff: String,
    /// Newline-separated generated-marker tokens matched in the snapshot-side
    /// file. Deleted files intentionally leave this absent so their diffs stay
    /// visible.
    pub generated_header: Option<String>,
}

fn parse_review_marker_output(
    bytes: &[u8],
    paths: &[&str],
    header_lines: u64,
) -> (HashMap<String, String>, Vec<String>) {
    let mut markers = HashMap::<String, String>::new();
    let mut tail_paths = Vec::new();
    let mut last_complete_path = None;
    let mut rest = bytes;
    loop {
        let Some(name_end) = rest.iter().position(|byte| *byte == 0) else {
            if !rest.is_empty() {
                tail_paths.extend(
                    paths
                        .iter()
                        .filter(|path| path.as_bytes().starts_with(rest))
                        .map(|path| (*path).to_owned()),
                );
            }
            break;
        };
        let name = &rest[..name_end];
        rest = &rest[name_end + 1..];
        let decoded_name = std::str::from_utf8(name)
            .ok()
            .filter(|name| paths.contains(name));
        let Some(line_end) = rest.iter().position(|byte| *byte == 0) else {
            tail_paths.extend(decoded_name.map(str::to_owned));
            break;
        };
        let line = &rest[..line_end];
        rest = &rest[line_end + 1..];
        let Some(content_end) = rest.iter().position(|byte| *byte == b'\n') else {
            // A bounded read may end partway through the final marker token.
            // Only complete grep records are trusted and retained.
            tail_paths.extend(decoded_name.map(str::to_owned));
            break;
        };
        let content = &rest[..content_end];
        rest = &rest[content_end + 1..];
        let Some(name) = decoded_name else {
            continue;
        };
        last_complete_path = Some(name.to_owned());
        let Some(line) = std::str::from_utf8(line)
            .ok()
            .and_then(|line| line.parse::<u64>().ok())
        else {
            continue;
        };
        if line > header_lines {
            continue;
        }
        append_distinct_review_marker(
            markers.entry(name.to_owned()).or_default(),
            &String::from_utf8_lossy(content),
            usize::MAX,
        );
    }
    if tail_paths.is_empty()
        && let Some(path) = last_complete_path
    {
        tail_paths.push(path);
    }
    (markers, tail_paths)
}

fn append_distinct_review_marker(
    header: &mut String,
    marker: &str,
    remaining_bytes: usize,
) -> usize {
    if marker.is_empty()
        || header
            .lines()
            .any(|existing| existing.eq_ignore_ascii_case(marker))
    {
        return 0;
    }
    let added_bytes = marker.len() + usize::from(!header.is_empty());
    if added_bytes > remaining_bytes {
        return 0;
    }
    if !header.is_empty() {
        header.push('\n');
    }
    header.push_str(marker);
    added_bytes
}

fn review_marker_retry_paths<'a>(
    paths: &[&'a str],
    complete_headers: &HashMap<String, String>,
    tail_paths: &[String],
) -> Vec<&'a str> {
    paths
        .iter()
        .copied()
        .filter(|path| {
            !complete_headers.contains_key(*path)
                || tail_paths.iter().any(|tail_path| tail_path == path)
        })
        .collect()
}

fn merge_review_marker_headers(
    headers: &mut HashMap<String, String>,
    additional: HashMap<String, String>,
    retained_bytes: &mut usize,
    max_retained_bytes: usize,
) {
    for (path, markers) in additional {
        let header = headers.entry(path).or_default();
        for marker in markers.lines() {
            let remaining_bytes = max_retained_bytes.saturating_sub(*retained_bytes);
            *retained_bytes += append_distinct_review_marker(header, marker, remaining_bytes);
        }
    }
}

fn run_review_marker_grep(
    worktree: &Path,
    paths: &[&str],
    markers: &[&str],
    max_stdout: usize,
    one_match_per_line: bool,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<Option<BoundedGitOutput>> {
    let mut args = vec![
        "--literal-pathspecs".to_owned(),
        "grep".to_owned(),
        "--cached".to_owned(),
        "-I".to_owned(),
        "-i".to_owned(),
        "-n".to_owned(),
        // Emit only the bounded marker, not a potentially enormous minified
        // source line containing it.
        "-o".to_owned(),
        "-z".to_owned(),
        "-m".to_owned(),
        REVIEW_MARKER_HEADER_LINES.to_string(),
    ];
    if one_match_per_line {
        // Anchoring each single-marker retry and resetting the match start with
        // \K makes `-o` emit that token once per matching line. This prevents
        // repeated tokens on one minified line from producing unbounded output.
        args.push("-P".to_owned());
    }
    for marker in markers {
        args.push("-e".to_owned());
        args.push(if one_match_per_line {
            format!(r"^.*?\K\Q{marker}\E")
        } else {
            (*marker).to_owned()
        });
    }
    args.push("--".to_owned());
    args.extend(paths.iter().map(|path| (*path).to_owned()));
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    match run_git_bounded(worktree, Some(index), &refs, None, max_stdout, operation) {
        Ok(output) => Ok(Some(output)),
        Err(_) => {
            // No matches exits 1. Other lookup failures also safely keep the
            // affected full diffs reviewable. The header pass is optional
            // after patches are loaded, so only explicit caller cancellation
            // aborts the completed diff operation.
            operation.check_cancelled()?;
            Ok(None)
        }
    }
}

const REVIEW_MARKER_HEADER_LINES: u64 = 20;

fn review_marker_output_bound(paths: &[&str], marker: &str) -> usize {
    // `git grep -n -z -o` emits path<NUL>line<NUL>match<LF>. Reserve the
    // maximum u64 line-number width even though only the first 20 matches are
    // retained, so a successful one-match-per-line query cannot truncate.
    const RECORD_OVERHEAD: usize = 1 + 20 + 1 + 1;
    paths.iter().fold(0_usize, |total, path| {
        let record_bytes = path
            .len()
            .saturating_add(marker.len())
            .saturating_add(RECORD_OVERHEAD);
        total.saturating_add(record_bytes.saturating_mul(REVIEW_MARKER_HEADER_LINES as usize))
    })
}

fn run_portable_review_marker_retry(
    worktree: &Path,
    paths: &[&str],
    marker: &str,
    remaining_commands: &mut usize,
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    let mut retained_bytes = 0;
    let mut pending = vec![paths.to_vec()];
    while let Some(group) = pending.pop() {
        if group.is_empty() || *remaining_commands == 0 {
            break;
        }
        *remaining_commands -= 1;
        operation.check_cancelled()?;
        let Some(output) = run_review_marker_grep(
            worktree,
            &group,
            &[marker],
            review_marker_output_bound(&group, marker),
            false,
            index,
            operation,
        )?
        else {
            continue;
        };
        let (group_headers, _) =
            parse_review_marker_output(&output.bytes, &group, REVIEW_MARKER_HEADER_LINES);
        merge_review_marker_headers(&mut headers, group_headers, &mut retained_bytes, usize::MAX);
        if output.truncated && group.len() > 1 {
            // Portable `git grep -o` can repeat one marker many times on a
            // single line. Split only overflowing groups and stop at the
            // shared command budget; omitted headers keep those diffs visible.
            let middle = group.len() / 2;
            pending.push(group[middle..].to_vec());
            pending.push(group[..middle].to_vec());
        }
    }
    Ok(headers)
}

fn review_blob_headers(
    worktree: &Path,
    paths: &[&str],
    index: &TemporaryCheckpointIndex,
    operation: &GitOperation<'_>,
) -> Result<HashMap<String, String>> {
    const PATHS_PER_GREP: usize = 16;
    const OUTPUT_BYTES_PER_PATH: usize = 64 * 1024;
    const MAX_RETAINED_HEADER_BYTES: usize = 64 * 1024;
    const MAX_RETRY_PATHS: usize = 256;
    const MAX_PORTABLE_RETRY_COMMANDS: usize = 64;
    const MARKERS: &[&str] = &[
        "@generated",
        "auto-generated",
        "automatically generated",
        "generated file",
        "do not edit",
    ];
    let mut headers = HashMap::<String, String>::new();
    let mut retained_header_bytes = 0;
    let mut retry_paths = Vec::new();
    for paths in paths.chunks(PATHS_PER_GREP) {
        let Some(output) = run_review_marker_grep(
            worktree,
            paths,
            MARKERS,
            paths.len().saturating_mul(OUTPUT_BYTES_PER_PATH),
            false,
            index,
            operation,
        )?
        else {
            continue;
        };
        let (group_headers, tail_paths) =
            parse_review_marker_output(&output.bytes, paths, REVIEW_MARKER_HEADER_LINES);
        if output.truncated {
            // Retain complete records, then collect incomplete paths for a
            // fixed number of batched, one-match-per-line retries. Paths over
            // the global retry budget safely retain their full review diff.
            for path in review_marker_retry_paths(paths, &group_headers, &tail_paths) {
                if retry_paths.len() == MAX_RETRY_PATHS {
                    break;
                }
                if !retry_paths.contains(&path) {
                    retry_paths.push(path);
                }
            }
        }
        merge_review_marker_headers(
            &mut headers,
            group_headers,
            &mut retained_header_bytes,
            MAX_RETAINED_HEADER_BYTES,
        );
    }
    let mut portable_retry_commands = MAX_PORTABLE_RETRY_COMMANDS;
    for marker in MARKERS {
        if retry_paths.is_empty() {
            break;
        }
        operation.check_cancelled()?;
        let output = run_review_marker_grep(
            worktree,
            &retry_paths,
            &[*marker],
            review_marker_output_bound(&retry_paths, marker),
            true,
            index,
            operation,
        )?;
        let marker_headers = if let Some(output) = output {
            let (marker_headers, _) =
                parse_review_marker_output(&output.bytes, &retry_paths, REVIEW_MARKER_HEADER_LINES);
            if output.truncated {
                let fallback_headers = run_portable_review_marker_retry(
                    worktree,
                    &retry_paths,
                    marker,
                    &mut portable_retry_commands,
                    index,
                    operation,
                )?;
                let mut merged_bytes = marker_headers.values().map(String::len).sum();
                let mut marker_headers = marker_headers;
                merge_review_marker_headers(
                    &mut marker_headers,
                    fallback_headers,
                    &mut merged_bytes,
                    usize::MAX,
                );
                marker_headers
            } else {
                marker_headers
            }
        } else {
            // Exit 1 (no match) and optional-PCRE invocation failures share the
            // bounded runner's error surface. A portable query is cheap for the
            // former and preserves classification on Git builds without PCRE.
            run_portable_review_marker_retry(
                worktree,
                &retry_paths,
                marker,
                &mut portable_retry_commands,
                index,
                operation,
            )?
        };
        merge_review_marker_headers(
            &mut headers,
            marker_headers,
            &mut retained_header_bytes,
            MAX_RETAINED_HEADER_BYTES,
        );
    }
    Ok(headers)
}

pub fn session_diff_patches_cancellable<F>(
    worktree: &Path,
    base_ref: &str,
    max_files: usize,
    max_changed_lines: u64,
    max_total_bytes: usize,
    cancel: &tokio_util::sync::CancellationToken,
    capture_header: F,
) -> Result<Vec<SessionReviewDiffFile>>
where
    F: Fn(&str) -> bool,
{
    ensure_safe_ref(base_ref)?;
    let operation = GitOperation::new(Some(cancel));
    with_session_snapshot_index(worktree, &operation, |index| {
        let summary = session_diff_summary_with_index(worktree, base_ref, index, &operation)?;
        let changed_lines = summary.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.additions)
                .and_then(|total| total.checked_add(file.deletions))
                .context("session review diff line count overflow")
        })?;
        if summary.len() > max_files || changed_lines > max_changed_lines {
            return Err(session_diff_too_large(format!(
                "review diff is too large ({} files, {changed_lines} changed lines; limit is \
                 {max_files} files or {max_changed_lines} changed lines)",
                summary.len()
            )));
        }

        let mut total_bytes = 0_usize;
        let mut patches = Vec::with_capacity(summary.len());
        for file in summary {
            operation.check()?;
            let diff =
                bounded_session_diff_path(worktree, base_ref, &file.path, index, &operation)?;
            total_bytes = total_bytes
                .checked_add(diff.len())
                .context("review diff byte count overflow")?;
            if total_bytes > max_total_bytes {
                return Err(session_diff_too_large(format!(
                    "review diff is too large (more than {max_total_bytes} bytes)"
                )));
            }
            patches.push(SessionReviewDiffFile {
                path: file.path,
                diff,
                generated_header: None,
            });
        }
        let current = patches
            .iter()
            .filter(|file| capture_header(&file.path))
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        let mut headers = review_blob_headers(worktree, &current, index, &operation)?;
        for file in &mut patches {
            file.generated_header = headers.remove(&file.path);
        }
        Ok(patches)
    })
}

/// Unified diff for exactly one changed path.
pub fn session_diff_path(worktree: &Path, base_ref: &str, path: &str) -> Result<String> {
    session_diff_path_with_cancel(worktree, base_ref, path, None)
}

pub fn session_diff_path_cancellable(
    worktree: &Path,
    base_ref: &str,
    path: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    session_diff_path_with_cancel(worktree, base_ref, path, Some(cancel))
}

fn session_diff_path_with_cancel(
    worktree: &Path,
    base_ref: &str,
    path: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String> {
    ensure_safe_ref(base_ref)?;
    if is_session_internal_path(path) {
        bail!("path is internal session state and is excluded from diffs: {path:?}");
    }
    if path.is_empty()
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        bail!("invalid repository-relative diff path: {path:?}");
    }
    let operation = GitOperation::new(cancel);
    with_session_snapshot_index(worktree, &operation, |index| {
        let changed = session_diff_summary_with_index(worktree, base_ref, index, &operation)?;
        if !changed.iter().any(|entry| entry.path == path) {
            bail!("path is not a changed file in this session snapshot: {path:?}");
        }
        bounded_session_diff_path(worktree, base_ref, path, index, &operation)
    })
}

/// URL of the named remote (usually "origin"), if configured.
pub fn remote_url(worktree: &Path, remote: &str) -> Option<String> {
    git(worktree, &["remote", "get-url", remote])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Strip a configured remote name from a short remote-tracking ref.
pub fn remote_branch_name(worktree: &Path, remote_ref: &str) -> Option<String> {
    let remotes = git(worktree, &["remote"]).ok()?;
    remotes
        .lines()
        .filter_map(|remote| {
            remote_ref
                .strip_prefix(remote)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|branch| (remote.len(), branch.to_string()))
        })
        .max_by_key(|(remote_len, _)| *remote_len)
        .map(|(_, branch)| branch)
}

fn remote_branch_name_with_operation(
    worktree: &Path,
    remote_ref: &str,
    operation: &GitOperation<'_>,
) -> Result<Option<String>> {
    let remotes = run_git_bounded(worktree, None, &["remote"], None, 64 * 1024, operation)?;
    if remotes.truncated {
        bail!("git remote list is unexpectedly large");
    }
    let remotes = String::from_utf8(remotes.bytes).context("git remote list is not UTF-8")?;
    Ok(remotes
        .lines()
        .filter_map(|remote| {
            remote_ref
                .strip_prefix(remote)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(|branch| (remote.len(), branch.to_string()))
        })
        .max_by_key(|(remote_len, _)| *remote_len)
        .map(|(_, branch)| branch))
}

/// Resolve the PR base and push the exact session branch through the bounded,
/// cancellable process-tree runner. The explicit same-name refspec prevents a
/// corrupted branch field from being interpreted as an arbitrary push mapping.
pub fn push_session_branch_cancellable(
    worktree: &Path,
    session_base: &str,
    requested_base: Option<&str>,
    branch: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    ensure_safe_ref(session_base)?;
    ensure_safe_ref(branch)?;
    if let Some(base) = requested_base {
        ensure_safe_ref(base)?;
    }
    let operation = GitOperation::with_timeout(
        Some(cancel),
        SESSION_PUSH_GIT_TIMEOUT,
        "session branch push",
    );
    let checked = run_git_bounded(
        worktree,
        None,
        &["check-ref-format", "--branch", branch],
        None,
        1024,
        &operation,
    )?;
    if checked.truncated {
        bail!("validated branch name is unexpectedly long");
    }
    let base = match requested_base {
        Some(base) => base.to_string(),
        None => remote_branch_name_with_operation(worktree, session_base, &operation)?
            .unwrap_or_else(|| session_base.to_string()),
    };
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    run_git_bounded(
        worktree,
        None,
        &["push", "--set-upstream", "--", "origin", &refspec],
        None,
        MAX_SESSION_DIFF_BYTES,
        &operation,
    )?;
    Ok(base)
}

/// Push the session branch to the remote (sets upstream).
pub fn push_branch(worktree: &Path, remote: &str, branch: &str) -> Result<()> {
    git(worktree, &["push", "--set-upstream", remote, branch])?;
    Ok(())
}

/// Where session worktrees live: `<data_dir>/worktrees/<session_id>`.
pub fn worktree_dir(data_dir: &Path, session_id: &str) -> PathBuf {
    data_dir.join("worktrees").join(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir).args(args);
        let out = trouve_process::output(&mut command).unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(dir: &Path) {
        run(dir, &["init", "-b", "main"]);
        run(dir, &["config", "user.email", "test@example.com"]);
        run(dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-m", "init"]);
    }

    #[test]
    fn materialized_attachments_are_not_user_changes_diffs_or_checkpoints() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let attachments = tmp.path().join(".trouve/attachments");
        std::fs::create_dir_all(&attachments).unwrap();
        let tracked = attachments.join("tracked.bin");
        std::fs::write(&tracked, b"initial").unwrap();
        run(tmp.path(), &["add", ".trouve/attachments/tracked.bin"]);
        run(tmp.path(), &["commit", "-m", "tracked harness artifact"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);

        std::fs::write(&tracked, b"materialized replacement").unwrap();
        std::fs::write(attachments.join("new.bin"), b"materialized new").unwrap();

        assert!(!has_changes(tmp.path()).unwrap());
        assert!(session_diff(tmp.path(), &base).unwrap().is_empty());
        assert!(session_diff_summary(tmp.path(), &base).unwrap().is_empty());
        assert!(session_diff_files(tmp.path(), &base).unwrap().is_empty());
        assert!(session_diff_path(tmp.path(), &base, ".trouve/attachments/tracked.bin").is_err());

        let commit = checkpoint(tmp.path(), "se_internal", "cp_internal", "checkpoint").unwrap();
        let tree_paths = run(tmp.path(), &["ls-tree", "-r", "--name-only", &commit]);
        assert!(!tree_paths.lines().any(is_session_internal_path));
        assert_eq!(
            std::fs::read(&tracked).unwrap(),
            b"materialized replacement"
        );

        restore(tmp.path(), &commit).unwrap();
        assert_eq!(
            std::fs::read(&tracked).unwrap(),
            b"materialized replacement"
        );
        assert_eq!(
            std::fs::read(attachments.join("new.bin")).unwrap(),
            b"materialized new"
        );
    }

    #[test]
    fn bounded_git_runner_clears_repository_redirection_environment() {
        let mut command = Command::new("git");
        command
            .env("GIT_DIR", "/outside/repository")
            .env("GIT_OBJECT_DIRECTORY", "/outside/objects")
            .env("GIT_EXEC_PATH", "/outside/helpers")
            .env("GIT_ASKPASS", "/outside/askpass");
        clear_inherited_git_process_controls(&mut command);
        let controls = command
            .get_envs()
            .filter_map(|(name, value)| {
                name.to_str()
                    .filter(|name| name.starts_with("GIT_"))
                    .map(|name| (name.to_string(), value.is_none()))
            })
            .collect::<BTreeMap<_, _>>();
        for name in [
            "GIT_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_EXEC_PATH",
            "GIT_ASKPASS",
        ] {
            assert_eq!(controls.get(name), Some(&true), "{name} was not removed");
        }
    }

    #[test]
    fn worktree_creation_preserves_an_independently_created_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let worktree = tmp.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join("independent.txt"), "keep").unwrap();

        assert!(create_worktree(&repo, &worktree, "trouve/collision", "main").is_err());
        assert_eq!(
            std::fs::read_to_string(worktree.join("independent.txt")).unwrap(),
            "keep"
        );
        assert!(!local_branch_exists(&repo, "trouve/collision").unwrap());
    }

    #[test]
    fn worktree_creation_preserves_an_independently_created_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        run(&repo, &["branch", "trouve/collision"]);
        let expected = run(&repo, &["rev-parse", "trouve/collision"]);
        let worktree = tmp.path().join("worktree");

        assert!(create_worktree(&repo, &worktree, "trouve/collision", "main").is_err());
        assert_eq!(run(&repo, &["rev-parse", "trouve/collision"]), expected);
        assert!(!worktree.exists());
    }

    #[test]
    fn finalizing_worktree_creation_removes_only_its_reservation_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let worktree = tmp.path().join("worktree");
        let creation = create_worktree(&repo, &worktree, "trouve/finalize", "main").unwrap();
        assert!(creation.reservation_path.exists());

        finalize_worktree_creation(&creation).unwrap();

        assert!(!creation.reservation_path.exists());
        assert!(worktree.join("a.txt").exists());
    }

    #[test]
    fn checkpoints_use_immutable_identity_refs() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let expected = run(tmp.path(), &["rev-parse", "HEAD"]);
        run(
            tmp.path(),
            &[
                "update-ref",
                "refs/trouve/checkpoints/se_collision/cp_old",
                &expected,
            ],
        );

        std::fs::write(tmp.path().join("a.txt"), "replacement\n").unwrap();
        let replacement = checkpoint(tmp.path(), "se_collision", "cp_new", "collision").unwrap();
        assert_ne!(replacement, expected);
        assert_eq!(
            run(
                tmp.path(),
                &["rev-parse", "refs/trouve/checkpoints/se_collision/cp_new"]
            ),
            replacement
        );
        assert_eq!(
            run(
                tmp.path(),
                &["rev-parse", "refs/trouve/checkpoints/se_collision/cp_old"]
            ),
            expected
        );

        std::fs::write(tmp.path().join("a.txt"), "collision\n").unwrap();
        assert!(checkpoint(tmp.path(), "se_collision", "cp_old", "collision").is_err());
        assert_eq!(
            run(
                tmp.path(),
                &["rev-parse", "refs/trouve/checkpoints/se_collision/cp_old"]
            ),
            expected
        );
    }

    #[test]
    fn failed_checkpoint_persistence_removes_only_the_attempt_anchor_with_cas() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let old = checkpoint(repo, "se_rollback", "cp_old", "redo").unwrap();
        std::fs::write(repo.join("file.txt"), "replacement\n").unwrap();
        let failed = checkpoint(repo, "se_rollback", "cp_failed", "unpersisted").unwrap();
        assert_ne!(failed, old);

        rollback_checkpoint_ref(repo, "se_rollback", "cp_failed", &failed).unwrap();
        assert!(!checkpoint_ref_exists(repo, "se_rollback", "cp_failed").unwrap());
        assert_eq!(
            run(
                repo,
                &["rev-parse", "refs/trouve/checkpoints/se_rollback/cp_old"]
            ),
            old
        );

        std::fs::write(repo.join("file.txt"), "another owner\n").unwrap();
        let concurrent = checkpoint(repo, "se_rollback", "cp_race", "concurrent").unwrap();
        run(
            repo,
            &[
                "update-ref",
                "refs/trouve/checkpoints/se_rollback/cp_failed",
                &concurrent,
            ],
        );
        assert!(rollback_checkpoint_ref(repo, "se_rollback", "cp_failed", &failed).is_err());
        assert_eq!(
            run(
                repo,
                &["rev-parse", "refs/trouve/checkpoints/se_rollback/cp_failed"]
            ),
            concurrent
        );
    }

    #[test]
    fn checkpoint_reconciliation_prunes_orphans_and_preserves_live_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        checkpoint(repo, "se_reconcile", "cp_live", "live").unwrap();
        std::fs::write(repo.join("file.txt"), "orphan\n").unwrap();
        checkpoint(repo, "se_reconcile", "cp_orphan", "orphan").unwrap();

        reconcile_checkpoint_refs(repo, "se_reconcile", &["cp_live".into()]).unwrap();

        assert!(checkpoint_ref_exists(repo, "se_reconcile", "cp_live").unwrap());
        assert!(!checkpoint_ref_exists(repo, "se_reconcile", "cp_orphan").unwrap());
    }

    #[test]
    fn deleted_session_checkpoint_cleanup_removes_legacy_and_immutable_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo);
        let commit = checkpoint(repo, "se_delete", "cp_live", "live").unwrap();
        run(
            repo,
            &["update-ref", "refs/trouve/checkpoints/se_delete/0", &commit],
        );

        delete_session_checkpoint_refs(repo, "se_delete").unwrap();

        assert!(!checkpoint_ref_exists(repo, "se_delete", "cp_live").unwrap());
        assert!(!checkpoint_ref_exists(repo, "se_delete", "0").unwrap());
    }

    #[test]
    fn creation_rollback_preserves_a_replaced_worktree_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let worktree = tmp.path().join("worktree");
        let displaced = tmp.path().join("displaced");
        let creation = create_worktree(&repo, &worktree, "trouve/replaced", "main").unwrap();
        run(
            &repo,
            &[
                "worktree",
                "move",
                worktree.to_str().unwrap(),
                displaced.to_str().unwrap(),
            ],
        );
        std::fs::create_dir(&worktree).unwrap();
        std::fs::write(worktree.join("independent.txt"), "keep").unwrap();

        assert!(rollback_worktree_creation(&repo, &creation, None).is_err());
        assert_eq!(
            std::fs::read_to_string(worktree.join("independent.txt")).unwrap(),
            "keep"
        );
        assert!(local_branch_exists(&repo, "trouve/replaced").unwrap());
    }

    #[test]
    fn creation_rollback_preserves_a_branch_that_changed_oid() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let worktree = tmp.path().join("worktree");
        let creation = create_worktree(&repo, &worktree, "trouve/advanced", "main").unwrap();
        std::fs::write(worktree.join("a.txt"), "advanced\n").unwrap();
        run(&worktree, &["add", "a.txt"]);
        run(&worktree, &["commit", "-m", "advance"]);
        let advanced = run(&repo, &["rev-parse", "trouve/advanced"]);

        assert!(rollback_worktree_creation(&repo, &creation, None).is_err());
        assert_eq!(run(&repo, &["rev-parse", "trouve/advanced"]), advanced);
        assert!(worktree.exists());
    }

    #[test]
    fn creation_rollback_preserves_a_checkpoint_ref_that_changed_oid() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        let worktree = tmp.path().join("worktree");
        let creation = create_worktree(&repo, &worktree, "trouve/checkpoint-race", "main").unwrap();
        let checkpoint_oid =
            checkpoint(&worktree, "se_checkpoint_race", "cp_race", "checkpoint").unwrap();
        let independent = run(&repo, &["rev-parse", "main"]);
        run(
            &repo,
            &[
                "update-ref",
                "refs/trouve/checkpoints/se_checkpoint_race/cp_race",
                &independent,
            ],
        );

        assert!(
            rollback_worktree_creation(
                &repo,
                &creation,
                Some(("se_checkpoint_race", "cp_race", &checkpoint_oid)),
            )
            .is_err()
        );
        assert_eq!(
            run(
                &repo,
                &[
                    "rev-parse",
                    "refs/trouve/checkpoints/se_checkpoint_race/cp_race",
                ],
            ),
            independent
        );
        assert!(worktree.exists());
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Fix the Login Bug!"), "fix-the-login-bug");
        assert_eq!(slugify("---"), "session");
    }

    #[test]
    fn fetch_upstream_base_returns_remote_commit_without_moving_local_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("remote.git");
        std::fs::create_dir(&remote).unwrap();
        run(&remote, &["init", "--bare", "-b", "main"]);

        let publisher = tmp.path().join("publisher");
        std::fs::create_dir(&publisher).unwrap();
        init_repo(&publisher);
        run(
            &publisher,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run(&publisher, &["push", "-u", "origin", "main"]);

        let repo = tmp.path().join("repo");
        run(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                remote.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        let old_head = run(&repo, &["rev-parse", "main"]);

        std::fs::write(publisher.join("a.txt"), "two\n").unwrap();
        run(&publisher, &["add", "a.txt"]);
        run(&publisher, &["commit", "-m", "update"]);
        run(&publisher, &["push", "origin", "main"]);

        let fetched = fetch_upstream_base(&repo, "main").unwrap().unwrap();
        assert_eq!(fetched.upstream_ref, "origin/main");
        assert_eq!(run(&repo, &["rev-parse", "main"]), old_head);
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\n"
        );

        let wt = tmp.path().join("wt");
        create_worktree(&repo, &wt, "trouve/test", &fetched.commit).unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "two\n");
    }

    #[test]
    fn fetch_upstream_base_without_upstream_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());

        let head = run(tmp.path(), &["rev-parse", "main"]);
        assert!(fetch_upstream_base(tmp.path(), "main").unwrap().is_none());

        assert_eq!(run(tmp.path(), &["rev-parse", "main"]), head);
    }

    #[cfg(unix)]
    #[test]
    fn fetch_upstream_base_times_out_a_stalled_transport() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);

        let ssh = tmp.path().join("sleeping-ssh");
        std::fs::write(&ssh, "#!/bin/sh\nsleep 10\n").unwrap();
        let mut permissions = std::fs::metadata(&ssh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&ssh, permissions).unwrap();

        run(&repo, &["remote", "add", "origin", "ssh://example/repo"]);
        run(&repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        run(&repo, &["branch", "--set-upstream-to=origin/main", "main"]);
        run(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);

        let started = Instant::now();
        let error = fetch_upstream_base_with_timeout(&repo, "main", Duration::from_millis(100))
            .err()
            .unwrap();

        let message = error.to_string();
        assert!(
            message.contains("git fetch") && message.contains("timed out"),
            "unexpected fetch timeout error: {message}"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn remote_branch_name_uses_the_configured_remote_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        run(tmp.path(), &["remote", "add", "upstream", "."]);

        assert_eq!(
            remote_branch_name(tmp.path(), "upstream/feature/x").as_deref(),
            Some("feature/x")
        );
        assert_eq!(remote_branch_name(tmp.path(), "feature/x"), None);
    }

    #[test]
    fn review_diff_lists_and_reads_every_changed_path() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(tmp.path().join("space name.txt"), "added\n").unwrap();

        let full = session_diff(tmp.path(), &base).unwrap();
        assert!(full.contains("+two"));
        assert!(full.contains("+added"));
        let files = session_diff_files(tmp.path(), &base).unwrap();
        assert_eq!(files, ["a.txt", "space name.txt"]);
        let summary = session_diff_summary(tmp.path(), &base).unwrap();
        assert_eq!(
            summary,
            [
                SessionDiffStat {
                    path: "a.txt".into(),
                    additions: 1,
                    deletions: 0,
                    binary: false,
                },
                SessionDiffStat {
                    path: "space name.txt".into(),
                    additions: 1,
                    deletions: 0,
                    binary: false,
                },
            ]
        );
        let first = session_diff_path(tmp.path(), &base, &files[0]).unwrap();
        let second = session_diff_path(tmp.path(), &base, &files[1]).unwrap();
        assert!(first.contains("+two"));
        assert!(second.contains("+added"));
        assert!(session_diff_path(tmp.path(), &base, "../outside").is_err());
    }

    #[test]
    fn review_diff_loads_generated_headers_from_snapshot_and_preserves_deletions() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let generated = tmp.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        let header = "// This file was auto-generated. Do not edit.\n";
        let modified_path = generated.join("modified.rs");
        let deleted_path = generated.join("deleted.rs");
        std::fs::write(
            &modified_path,
            format!("{header}{}old();\n", "// filler\n".repeat(30)),
        )
        .unwrap();
        std::fs::write(&deleted_path, format!("{header}deleted();\n")).unwrap();
        run(tmp.path(), &["add", "generated"]);
        run(tmp.path(), &["commit", "-m", "add generated files"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);

        std::fs::write(
            &modified_path,
            format!("{header}{}new();\n", "// filler\n".repeat(30)),
        )
        .unwrap();
        std::fs::remove_file(&deleted_path).unwrap();

        let files = session_diff_patches_cancellable(
            tmp.path(),
            &base,
            10,
            1_000,
            1024 * 1024,
            &tokio_util::sync::CancellationToken::new(),
            |_| true,
        )
        .unwrap();
        let modified = files
            .iter()
            .find(|file| file.path == "generated/modified.rs")
            .unwrap();
        let deleted = files
            .iter()
            .find(|file| file.path == "generated/deleted.rs")
            .unwrap();

        assert!(!modified.diff.contains("auto-generated"));
        assert!(
            modified
                .generated_header
                .as_deref()
                .unwrap()
                .contains("auto-generated")
        );
        assert!(deleted.generated_header.is_none());
        assert!(deleted.diff.contains("auto-generated"));
    }

    #[test]
    fn review_marker_parser_discards_an_incomplete_tail_record() {
        let output = b"generated/a.rs\x001\x00@generated\n\
                       generated/b.rs\x002\x00auto-generated";

        let (markers, tail_paths) =
            parse_review_marker_output(output, &["generated/a.rs", "generated/b.rs"], 20);

        assert_eq!(markers.len(), 1);
        assert_eq!(
            markers.get("generated/a.rs").map(String::as_str),
            Some("@generated")
        );
        assert!(!markers.contains_key("generated/b.rs"));
        assert_eq!(
            review_marker_retry_paths(&["generated/a.rs", "generated/b.rs"], &markers, &tail_paths,),
            ["generated/b.rs"]
        );
    }

    #[test]
    fn review_marker_retry_includes_a_complete_tail_path() {
        let output = b"generated/a.rs\x001\x00generated file\n\
                       generated/a.rs\x001\x00do not edit";

        let (markers, tail_paths) = parse_review_marker_output(output, &["generated/a.rs"], 20);

        assert_eq!(
            markers.get("generated/a.rs").map(String::as_str),
            Some("generated file")
        );
        assert_eq!(
            review_marker_retry_paths(&["generated/a.rs"], &markers, &tail_paths),
            ["generated/a.rs"]
        );
    }

    #[test]
    fn review_marker_parser_deduplicates_repeated_matches() {
        let output = b"generated/a.rs\x001\x00generated file\n\
                       generated/a.rs\x001\x00Generated File\n\
                       generated/a.rs\x001\x00do not edit\n";

        let (markers, _) = parse_review_marker_output(output, &["generated/a.rs"], 20);

        assert_eq!(
            markers.get("generated/a.rs").map(String::as_str),
            Some("generated file\ndo not edit")
        );
    }

    #[test]
    fn review_marker_merge_enforces_the_aggregate_header_bound() {
        let mut headers = HashMap::new();
        let mut retained_bytes = 0;
        let additional = HashMap::from([(
            "generated/a.rs".to_owned(),
            "generated file\ndo not edit".to_owned(),
        )]);

        merge_review_marker_headers(&mut headers, additional, &mut retained_bytes, 14);

        assert_eq!(retained_bytes, 14);
        assert_eq!(headers["generated/a.rs"], "generated file");
    }

    #[test]
    fn review_marker_retry_bound_scales_with_paths_and_records() {
        let path = "p".repeat(300);
        let paths = vec![path.as_str(); 250];

        assert!(review_marker_output_bound(&paths, "generated file") > 1024 * 1024);
    }

    #[test]
    fn portable_review_marker_retry_deduplicates_a_pathological_line() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let generated = tmp.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        std::fs::write(
            generated.join("compound.rs"),
            format!("// {}\n", "generated file ".repeat(3_000)),
        )
        .unwrap();
        let operation = GitOperation::new(None);
        let mut remaining_commands = 4;

        let headers = with_session_snapshot_index(tmp.path(), &operation, |index| {
            run_portable_review_marker_retry(
                tmp.path(),
                &["generated/compound.rs"],
                "generated file",
                &mut remaining_commands,
                index,
                &operation,
            )
        })
        .unwrap();

        assert_eq!(headers["generated/compound.rs"], "generated file");
        assert_eq!(remaining_commands, 3);
    }

    #[test]
    fn review_diff_recognizes_generated_markers_on_very_long_lines() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let generated = tmp.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        std::fs::write(
            generated.join("large.min.js"),
            format!("// @generated {}\n", "x".repeat(70 * 1024)),
        )
        .unwrap();

        let files = session_diff_patches_cancellable(
            tmp.path(),
            &base,
            10,
            1_000,
            1024 * 1024,
            &tokio_util::sync::CancellationToken::new(),
            |_| true,
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(
            files[0]
                .generated_header
                .as_deref()
                .is_some_and(|header| header.contains("@generated"))
        );
    }

    #[test]
    fn review_diff_recovers_compound_markers_after_single_path_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let generated = tmp.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        std::fs::write(
            generated.join("compound.rs"),
            format!("// {} do not edit\n", "generated file ".repeat(3_000)),
        )
        .unwrap();

        let files = session_diff_patches_cancellable(
            tmp.path(),
            &base,
            10,
            1_000,
            1024 * 1024,
            &tokio_util::sync::CancellationToken::new(),
            |_| true,
        )
        .unwrap();

        let header = files[0].generated_header.as_deref().unwrap();
        assert_eq!(header, "generated file\ndo not edit");
    }

    #[cfg(unix)]
    #[test]
    fn review_diff_uses_replacement_content_instead_of_deleted_base_header() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let generated = tmp.path().join("generated");
        std::fs::create_dir(&generated).unwrap();
        let path = generated.join("replaced.js");
        std::fs::write(&path, "// @generated\nold();\n").unwrap();
        run(tmp.path(), &["add", "generated/replaced.js"]);
        run(tmp.path(), &["commit", "-m", "add generated file"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("ordinary-target.js", &path).unwrap();

        let files = session_diff_patches_cancellable(
            tmp.path(),
            &base,
            10,
            1_000,
            1024 * 1024,
            &tokio_util::sync::CancellationToken::new(),
            |_| true,
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].diff.contains("new file mode 120000"));
        assert!(files[0].generated_header.is_none());
    }

    #[test]
    fn review_diff_includes_untracked_binary_files() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join("new.bin"), [0, 1, 2, 0xff]).unwrap();

        let files = session_diff_files(tmp.path(), &base).unwrap();
        assert_eq!(files, ["new.bin"]);
        let summary = session_diff_summary(tmp.path(), &base).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].path, "new.bin");
        assert!(summary[0].binary);
        let patch = session_diff_path(tmp.path(), &base, "new.bin").unwrap();
        assert!(patch.contains("Binary files"));
    }

    #[cfg(unix)]
    #[test]
    fn review_diff_never_executes_clean_process_or_textconv_filters() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().unwrap();
        let filter_dir = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let sentinel = filter_dir.path().join("filter-ran");
        let filter = filter_dir.path().join("hostile-filter");
        std::fs::write(
            &filter,
            format!(
                "#!/bin/sh\nprintf invoked >> {}\nexit 1\n",
                sentinel.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            tmp.path().join(".gitattributes"),
            "*.txt filter=evil diff=evil\n",
        )
        .unwrap();
        run(
            tmp.path(),
            &["config", "filter.evil.process", filter.to_str().unwrap()],
        );
        run(
            tmp.path(),
            &["config", "filter.evil.clean", filter.to_str().unwrap()],
        );
        run(tmp.path(), &["config", "filter.evil.required", "true"]);
        run(
            tmp.path(),
            &["config", "diff.evil.textconv", filter.to_str().unwrap()],
        );
        std::fs::write(tmp.path().join("a.txt"), "changed\n").unwrap();
        std::fs::write(tmp.path().join("new.txt"), "new\n").unwrap();
        let objects_before = run(tmp.path(), &["count-objects", "-v"]);

        let full = session_diff(tmp.path(), &base).unwrap();
        let files = session_diff_files(tmp.path(), &base).unwrap();
        let summary = session_diff_summary(tmp.path(), &base).unwrap();
        let selected = session_diff_path(tmp.path(), &base, "a.txt").unwrap();

        assert!(full.contains("+changed"));
        assert_eq!(files, [".gitattributes", "a.txt", "new.txt"]);
        assert_eq!(summary.len(), 3);
        assert!(selected.contains("+changed"));
        assert!(
            !sentinel.exists(),
            "review plumbing executed a repository filter"
        );
        assert_eq!(run(tmp.path(), &["count-objects", "-v"]), objects_before);
    }

    #[test]
    fn review_diff_reuses_stat_clean_crlf_index_entries() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::write(tmp.path().join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
        run(tmp.path(), &["add", ".gitattributes"]);
        run(tmp.path(), &["commit", "-m", "attributes"]);
        run(tmp.path(), &["checkout", "--", "a.txt"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);

        assert!(session_diff(tmp.path(), &base).unwrap().is_empty());
        assert!(session_diff_summary(tmp.path(), &base).unwrap().is_empty());
    }

    #[test]
    fn selected_diff_treats_pathspec_magic_as_a_literal_filename() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join(":(glob)**"), "magic\n").unwrap();
        std::fs::write(tmp.path().join("other.txt"), "other\n").unwrap();

        let patch = session_diff_path(tmp.path(), &base, ":(glob)**").unwrap();
        assert!(patch.contains("+magic"));
        assert!(!patch.contains("+other"));
        assert!(session_diff_path(tmp.path(), &base, ".").is_err());
        assert!(session_diff_path(tmp.path(), &base, "unchanged.txt").is_err());
    }

    #[test]
    fn review_diff_preserves_trailing_whitespace_and_newline() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join("a.txt"), "one\n   \n").unwrap();

        let patch = session_diff_path(tmp.path(), &base, "a.txt").unwrap();
        assert!(patch.contains("+   \n"));
        assert!(patch.ends_with('\n'));
    }

    #[test]
    fn review_diff_detects_changes_even_when_assume_unchanged_is_set() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        run(tmp.path(), &["update-index", "--assume-unchanged", "a.txt"]);
        std::fs::write(tmp.path().join("a.txt"), "changed despite hint\n").unwrap();

        let patch = session_diff_path(tmp.path(), &base, "a.txt").unwrap();
        assert!(patch.contains("+changed despite hint"));
    }

    #[test]
    fn absent_skip_worktree_path_is_not_reported_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        run(tmp.path(), &["update-index", "--skip-worktree", "a.txt"]);
        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();

        assert!(session_diff_summary(tmp.path(), &base).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn review_diff_hashes_symlink_targets_without_following_them() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let secret = tmp.path().parent().unwrap().join("outside-secret");
        std::fs::write(&secret, "do not read\n").unwrap();
        symlink(&secret, tmp.path().join("link")).unwrap();

        let patch = session_diff_path(tmp.path(), &base, "link").unwrap();
        assert!(patch.contains("new file mode 120000"));
        assert!(patch.contains(&format!("+{}", secret.display())));
        assert!(!patch.contains("do not read"));
        std::fs::remove_file(secret).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn review_diff_rejects_tracked_files_beneath_an_ancestor_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::create_dir(tmp.path().join("tracked")).unwrap();
        std::fs::write(tmp.path().join("tracked/file.txt"), "inside\n").unwrap();
        run(tmp.path(), &["add", "tracked/file.txt"]);
        run(tmp.path(), &["commit", "-m", "tracked directory"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("file.txt"), "external secret\n").unwrap();
        std::fs::remove_dir_all(tmp.path().join("tracked")).unwrap();
        symlink(outside.path(), tmp.path().join("tracked")).unwrap();

        let error = session_diff(tmp.path(), &base).unwrap_err().to_string();
        assert!(
            error.contains("linked or non-directory ancestor")
                || error.contains("without links")
                || error.contains("Too many levels")
        );
        assert!(!error.contains("external secret"));
    }

    #[test]
    fn cancelled_review_diff_stops_before_snapshot_work() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();

        let error = session_diff_cancellable(tmp.path(), &base, &cancel).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn session_diff_rejects_changes_too_large_for_the_ui() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let content = "changed\n".repeat(MAX_SESSION_DIFF_CHANGED_LINES as usize + 1);
        std::fs::write(tmp.path().join("a.txt"), content).unwrap();

        let error = session_diff(tmp.path(), &base).unwrap_err();
        assert!(error.downcast_ref::<SessionDiffTooLarge>().is_some());
        assert!(error.to_string().contains("too large to render"));
        let summary = session_diff_summary(tmp.path(), &base).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].additions, MAX_SESSION_DIFF_CHANGED_LINES + 1);
    }

    #[test]
    fn session_diff_rejects_too_many_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        for index in 0..=MAX_SESSION_DIFF_FILES {
            std::fs::write(tmp.path().join(format!("file-{index}.txt")), "before\n").unwrap();
        }
        run(tmp.path(), &["add", "-A"]);
        run(tmp.path(), &["commit", "-m", "add files"]);
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        for index in 0..=MAX_SESSION_DIFF_FILES {
            std::fs::write(tmp.path().join(format!("file-{index}.txt")), "after\n").unwrap();
        }

        let error = session_diff(tmp.path(), &base).unwrap_err();
        assert!(error.downcast_ref::<SessionDiffTooLarge>().is_some());
        assert!(error.to_string().contains("too large to render"));
        let summary = session_diff_summary(tmp.path(), &base).unwrap();
        assert_eq!(summary.len(), MAX_SESSION_DIFF_FILES + 1);
    }

    #[test]
    fn session_diff_rejects_too_many_rendered_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let content = format!("{}\n", "x".repeat(MAX_SESSION_DIFF_BYTES + 1));
        std::fs::write(tmp.path().join("a.txt"), content).unwrap();

        let error = session_diff(tmp.path(), &base).unwrap_err();
        assert!(error.downcast_ref::<SessionDiffTooLarge>().is_some());
        assert!(error.to_string().contains("too large to render"));
    }

    #[test]
    fn selected_file_diff_has_an_independent_bound() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        let base = run(tmp.path(), &["rev-parse", "HEAD"]);
        let content = format!("{}\n", "x".repeat(MAX_SESSION_FILE_DIFF_BYTES + 1));
        std::fs::write(tmp.path().join("a.txt"), content).unwrap();

        let error = session_diff_path(tmp.path(), &base, "a.txt").unwrap_err();
        assert!(error.downcast_ref::<SessionDiffTooLarge>().is_some());
        assert!(error.to_string().contains("selected file diff"));
    }

    #[test]
    fn worktree_checkpoint_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);

        let wt = tmp.path().join("wt");
        create_worktree(&repo, &wt, "trouve/test", "main").unwrap();
        assert!(wt.join("a.txt").exists());

        // Checkpoint 0: pristine state.
        let c0 = checkpoint(&wt, "se_t", "cp_0", "checkpoint 0").unwrap();

        // Mutate: edit a file, add a file.
        std::fs::write(wt.join("a.txt"), "two\n").unwrap();
        std::fs::write(wt.join("new.txt"), "hello\n").unwrap();
        let c1 = checkpoint(&wt, "se_t", "cp_1", "checkpoint 1").unwrap();
        assert_ne!(c0, c1);

        // Undo to checkpoint 0: edit reverted, new file gone.
        restore(&wt, &c0).unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "one\n");
        assert!(!wt.join("new.txt").exists());

        // Redo to checkpoint 1.
        restore(&wt, &c1).unwrap();
        assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "two\n");
        assert_eq!(
            std::fs::read_to_string(wt.join("new.txt")).unwrap(),
            "hello\n"
        );

        // Session branch untouched by checkpoints.
        let head = git(&wt, &["log", "--oneline", "trouve/test"]).unwrap();
        assert_eq!(head.lines().count(), 1);

        remove_worktree(&repo, &wt).unwrap();
        assert!(!wt.exists());
    }

    #[test]
    fn checkpoint_preserves_index_and_excludes_ignored_files() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        std::fs::create_dir_all(tmp.path().join("nested/target")).unwrap();
        std::fs::write(tmp.path().join("nested/target/artifact.o"), "build output").unwrap();
        std::fs::write(tmp.path().join("a.txt"), "staged\n").unwrap();
        run(tmp.path(), &["add", "nested/target/artifact.o", "a.txt"]);
        std::fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        run(tmp.path(), &["add", ".gitignore"]);
        let staged_before = run(tmp.path(), &["diff", "--cached"]);

        std::fs::write(tmp.path().join("a.txt"), "worktree\n").unwrap();
        std::fs::write(tmp.path().join("new.txt"), "included\n").unwrap();

        let commit = checkpoint(tmp.path(), "se_t", "cp_0", "checkpoint").unwrap();

        assert_eq!(run(tmp.path(), &["diff", "--cached"]), staged_before);
        assert!(
            run(tmp.path(), &["ls-files", "--stage"])
                .lines()
                .any(|line| line.ends_with("\tnested/target/artifact.o"))
        );
        assert_eq!(
            run(tmp.path(), &["show", &format!("{commit}:a.txt")]),
            "worktree"
        );
        assert_eq!(
            run(tmp.path(), &["show", &format!("{commit}:new.txt")]),
            "included"
        );
        assert!(
            run(tmp.path(), &["ls-tree", "-r", "--name-only", &commit])
                .lines()
                .all(|path| path != "nested/target/artifact.o")
        );
    }

    #[test]
    fn checkpoint_does_not_require_a_configured_git_identity() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        run(tmp.path(), &["config", "user.name", ""]);
        run(tmp.path(), &["config", "user.email", ""]);

        let commit = checkpoint(tmp.path(), "se_t", "cp_0", "checkpoint").unwrap();
        let identity = run(
            tmp.path(),
            &["show", "-s", "--format=%an <%ae>%n%cn <%ce>", &commit],
        );

        assert_eq!(
            identity,
            "trouve <trouve@localhost>\ntrouve <trouve@localhost>"
        );
    }
}
