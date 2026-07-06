//! Git plumbing for session worktrees and per-turn checkpoints (ADR 0003).
//!
//! Everything shells out to `git`; all functions are synchronous and are
//! called via `spawn_blocking` from async code.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

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

/// Create the session worktree on a new branch from `base_ref`.
pub fn create_worktree(
    repo: &Path,
    worktree_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<()> {
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
    git(worktree, &["diff", base_ref])
}

/// URL of the named remote (usually "origin"), if configured.
pub fn remote_url(worktree: &Path, remote: &str) -> Option<String> {
    git(worktree, &["remote", "get-url", remote])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "init"]);
    }

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Fix the Login Bug!"), "fix-the-login-bug");
        assert_eq!(slugify("---"), "session");
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
