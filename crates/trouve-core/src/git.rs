//! Git plumbing for session worktrees and per-turn checkpoints (ADR 0003).
//!
//! Everything shells out to `git`; all functions are synchronous and are
//! called via `spawn_blocking` from async code.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CHECKPOINT_IDENTITY_NAME: &str = "trouve";
const CHECKPOINT_IDENTITY_EMAIL: &str = "trouve@localhost";

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    git_result(dir, args, out.status, out.stdout, out.stderr)
}

fn git_untrimmed(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_as_checkpoint_identity(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // Checkpoints are trouve bookkeeping commits, and session creation
        // must not depend on the host having a global Git identity.
        .env("GIT_AUTHOR_NAME", CHECKPOINT_IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", CHECKPOINT_IDENTITY_EMAIL)
        .env("GIT_COMMITTER_NAME", CHECKPOINT_IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", CHECKPOINT_IDENTITY_EMAIL)
        .output()
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    git_result(dir, args, out.status, out.stdout, out.stderr)
}

fn git_with_timeout(dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("running git {args:?} in {}", dir.display()))?;
    let stdout = child.stdout.take().context("capturing git stdout")?;
    let stderr = child.stderr.take().context("capturing git stderr")?;
    let stdout_reader = thread::spawn(move || read_all(stdout));
    let stderr_reader = thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting for git {args:?} in {}", dir.display()))?
        {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            kill_process_tree(&mut child);
            let _ = child.wait();
            bail!(
                "git {} timed out after {}s in {}",
                args.join(" "),
                timeout.as_secs_f32(),
                dir.display()
            );
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(deadline - now));
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("git stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("git stderr reader panicked"))??;
    git_result(dir, args, status, stdout, stderr)
}

fn read_all(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn kill_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: this child was placed in a new process group whose id is
        // its pid, so the negative id targets only that group.
        let _ = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = child.kill();
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

/// Create the session worktree on a new branch from `base_ref`.
pub fn create_worktree(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<()> {
    ensure_safe_ref(base_ref)?;
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            worktree_path.to_str().context("non-utf8 worktree path")?,
            "--end-of-options",
            base_ref,
        ],
    )?;
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
pub fn checkpoint(worktree: &Path, session_id: &str, seq: i64, message: &str) -> Result<String> {
    git(worktree, &["add", "-A"])?;
    let tree = git(worktree, &["write-tree"])?;
    let head = git(worktree, &["rev-parse", "HEAD"])?;
    let commit = git_as_checkpoint_identity(
        worktree,
        &["commit-tree", &tree, "-p", &head, "-m", message],
    )?;
    // Anchor the commit against GC.
    git(
        worktree,
        &[
            "update-ref",
            &format!("refs/trouve/checkpoints/{session_id}/{seq}"),
            &commit,
        ],
    )?;
    Ok(commit)
}

/// Whether the worktree has any changes (staged, unstaged, or untracked)
/// relative to HEAD.
pub fn has_changes(worktree: &Path) -> Result<bool> {
    Ok(!git(worktree, &["status", "--porcelain"])?.is_empty())
}

/// Restore the worktree to a checkpoint commit's tree: index := commit tree,
/// files rewritten, files absent from the commit removed (they become
/// untracked after read-tree, so a scoped clean deletes them).
pub fn restore(worktree: &Path, commit: &str) -> Result<()> {
    git(worktree, &["read-tree", "--reset", commit])?;
    git(worktree, &["checkout-index", "-f", "-a"])?;
    git(worktree, &["clean", "-fd"])?;
    Ok(())
}

/// Unified diff of the session's work: base ref vs the worktree's current
/// state (includes uncommitted changes — checkpoints live on hidden refs).
pub fn session_diff(worktree: &Path, base_ref: &str) -> Result<String> {
    ensure_safe_ref(base_ref)?;
    git(worktree, &["diff", "--end-of-options", base_ref])
}

/// Every changed path in git's deterministic diff order. NUL framing keeps
/// whitespace and newlines in filenames unambiguous.
pub fn session_diff_files(worktree: &Path, base_ref: &str) -> Result<Vec<String>> {
    ensure_safe_ref(base_ref)?;
    let output = git_untrimmed(
        worktree,
        &["diff", "--name-only", "-z", "--end-of-options", base_ref],
    )?;
    Ok(output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// Unified diff for exactly one changed path.
pub fn session_diff_path(worktree: &Path, base_ref: &str, path: &str) -> Result<String> {
    ensure_safe_ref(base_ref)?;
    if path.is_empty()
        || Path::new(path).is_absolute()
        || Path::new(path)
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        bail!("invalid repository-relative diff path: {path:?}");
    }
    git(
        worktree,
        &["diff", "--end-of-options", base_ref, "--", path],
    )
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
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
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

        assert!(error.to_string().contains("timed out after 0.1s"));
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
        run(tmp.path(), &["add", "-A"]);

        let files = session_diff_files(tmp.path(), &base).unwrap();
        assert_eq!(files, ["a.txt", "space name.txt"]);
        let first = session_diff_path(tmp.path(), &base, &files[0]).unwrap();
        let second = session_diff_path(tmp.path(), &base, &files[1]).unwrap();
        assert!(first.contains("+two"));
        assert!(second.contains("+added"));
        assert!(session_diff_path(tmp.path(), &base, "../outside").is_err());
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
        let c0 = checkpoint(&wt, "se_t", 0, "checkpoint 0").unwrap();

        // Mutate: edit a file, add a file.
        std::fs::write(wt.join("a.txt"), "two\n").unwrap();
        std::fs::write(wt.join("new.txt"), "hello\n").unwrap();
        let c1 = checkpoint(&wt, "se_t", 1, "checkpoint 1").unwrap();
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
    fn checkpoint_does_not_require_a_configured_git_identity() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path());
        run(tmp.path(), &["config", "user.name", ""]);
        run(tmp.path(), &["config", "user.email", ""]);

        let commit = checkpoint(tmp.path(), "se_t", 0, "checkpoint").unwrap();
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
