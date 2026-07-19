//! Git plumbing for session worktrees and per-turn checkpoints (ADR 0003).
//!
//! Everything shells out to `git`; all functions are synchronous and are
//! called via `spawn_blocking` from async code.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

fn git(dir: &Path, args: &[&str]) -> Result<String> {
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
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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

    git(repo, &["fetch", "--quiet", "--", &remote])?;
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
    let commit = git(
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
}
