//! Conservative classification of shell commands that only read.
//!
//! The permission layer treats the `shell` tool as mutating: arbitrary shell
//! text can do anything. That is the right default, but it makes every
//! `rg` or `cat` prompt in Ask mode and denies them outright in read-only
//! personas, even though the same reads are free through `read_file` and
//! `grep`. This module recognises a small, fixed vocabulary of read-only
//! commands so the gate can treat those calls as reads. Everything it does
//! not recognise keeps the mutating classification, so an unknown command,
//! an unparsable one, or one that reaches outside the worktree still asks.
//!
//! Design rules, in order of importance:
//! - Fail closed. Any construct the classifier does not model (substitution,
//!   redirection, background jobs, escapes, expansion) rejects the command.
//! - Stay inside the worktree, on the real filesystem. Absolute paths, `~`,
//!   and `..` components are rejected lexically; every operand that names an
//!   existing path is then canonicalized (following symlinks) and must remain
//!   beneath the canonical worktree; glob operands are checked against every
//!   symlink they could expand through; `cd` is tracked so later operands
//!   resolve against the directory the shell will actually be in. A
//!   read-only persona must not become a way to read `~/.ssh` without a
//!   prompt, whether by path, by symlink, or by changing directory.
//! - Never execute model-chosen code. Commands and flags that run other
//!   programs (`find -exec`, `rg --pre`, `xargs`) are rejected even though
//!   the wrapper itself only reads.
//! - Repository-aware Git commands require permission because repository
//!   configuration can execute helpers. Only version probes are pure reads.

use std::path::{Component, Path, PathBuf};

/// Entries a single glob check may visit before giving up and rejecting.
const MAX_GLOB_WALK_ENTRIES: usize = 20_000;

/// Whether `command`, as passed to `sh -c` with `worktree` as its working
/// directory, is recognised as read-only and confined to that worktree.
pub fn shell_command_is_read_only(command: &str, worktree: &Path) -> bool {
    let command = command.trim();
    if command.is_empty() || command.chars().any(is_forbidden_char) {
        return false;
    }
    let Ok(worktree) = worktree.canonicalize() else {
        return false;
    };
    // Split on the list operators the classifier understands, keeping the
    // operator that follows each segment. A lone `&` (background job) is
    // rejected below because it survives the split.
    let mut segments: Vec<(&str, Option<Operator>)> = Vec::new();
    let mut rest = command;
    loop {
        let split = Operator::ALL
            .iter()
            .filter_map(|op| rest.find(op.text()).map(|at| (at, *op)))
            .min_by_key(|(at, op)| (*at, std::cmp::Reverse(op.text().len())));
        match split {
            Some((at, op)) => {
                segments.push((&rest[..at], Some(op)));
                rest = &rest[at + op.text().len()..];
            }
            None => {
                segments.push((rest, None));
                break;
            }
        }
    }
    if segments.is_empty() {
        return false;
    }
    // `cd` is tracked only where the shell is guaranteed to apply it to what
    // follows. In a pipeline `cd` runs in a subshell; after `;` a failed `cd`
    // still lets the next command run in the old directory; with `||` an
    // earlier success skips it. So a command containing `cd` may use only
    // `&&` and `|`, and `cd` itself may not sit in a pipeline and must be
    // followed by `&&` (or end the command): if it fails, nothing after it
    // runs, and if anything before it fails, it and everything after are
    // skipped together.
    let has_cd = segments
        .iter()
        .any(|(segment, _)| tokenize(segment).is_some_and(|t| t[0] == "cd"));
    if has_cd
        && segments
            .iter()
            .any(|(_, op)| matches!(op, Some(Operator::Or) | Some(Operator::Seq)))
    {
        return false;
    }
    let mut cwd = worktree.clone();
    let mut previous_op: Option<Operator> = None;
    for (segment, next_op) in segments {
        if segment.contains('&') {
            return false;
        }
        let Some(tokens) = tokenize(segment) else {
            return false;
        };
        let scope = Scope {
            worktree: &worktree,
            cwd: &cwd,
        };
        match segment_is_read_only(&tokens, &scope) {
            Verdict::Reject => return false,
            Verdict::Read => {}
            Verdict::ChangeDir(dir) => {
                let in_pipeline = matches!(previous_op, Some(Operator::Pipe))
                    || matches!(next_op, Some(Operator::Pipe));
                if in_pipeline || !matches!(next_op, Some(Operator::And) | None) {
                    return false;
                }
                cwd = dir;
            }
        }
        previous_op = next_op;
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    And,
    Or,
    Pipe,
    Seq,
}

impl Operator {
    /// Longest operators first so `&&` and `||` win over `|` at the same
    /// offset (the split also prefers the longer text on ties).
    const ALL: [Operator; 4] = [Operator::And, Operator::Or, Operator::Pipe, Operator::Seq];

    fn text(self) -> &'static str {
        match self {
            Operator::And => "&&",
            Operator::Or => "||",
            Operator::Pipe => "|",
            Operator::Seq => ";",
        }
    }
}

fn is_forbidden_char(c: char) -> bool {
    matches!(
        c,
        '`' | '$' | '>' | '<' | '\n' | '\r' | '\\' | '(' | ')' | '{' | '}'
    )
}

/// Split one pipeline segment into words, honouring single and double
/// quotes. Backslashes and `$` are already rejected, so quoted text is
/// literal. Returns `None` for an unterminated quote or an empty segment.
fn tokenize(segment: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in segment.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    tokens.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

struct Scope<'a> {
    worktree: &'a Path,
    cwd: &'a Path,
}

enum Verdict {
    Read,
    ChangeDir(PathBuf),
    Reject,
}

/// A token that lexically names a location outside the worktree.
fn token_escapes_lexically(token: &str) -> bool {
    token
        .split('=')
        .any(|part| part.starts_with('/') || part.starts_with('~'))
        || token.split('/').any(|component| component == "..")
}

fn has_glob_chars(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '*' | '?' | '['))
}

impl Scope<'_> {
    /// Whether `candidate`, an operand the shell will resolve against `cwd`,
    /// stays inside the worktree on the real filesystem. A literal operand
    /// that exists is canonicalized through any symlinks; one that does not
    /// exist cannot leak anything. A glob operand rejects if any symlink it
    /// could expand through points outside the worktree.
    fn operand_is_confined(&self, candidate: &str) -> bool {
        if candidate.is_empty() {
            return true;
        }
        if has_glob_chars(candidate) {
            return self.glob_is_confined(candidate);
        }
        let path = self.cwd.join(candidate);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => path
                .canonicalize()
                .is_ok_and(|real| real.starts_with(self.worktree)),
            // Nonexistent: not a path, or a read that will simply fail.
            Err(_) => true,
        }
    }

    fn glob_is_confined(&self, pattern: &str) -> bool {
        // The literal directory prefix before the first glob component is
        // resolved like an ordinary operand; the shell descends through it.
        let mut prefix = PathBuf::new();
        for component in Path::new(pattern).components() {
            let Component::Normal(part) = component else {
                return false;
            };
            if has_glob_chars(&part.to_string_lossy()) {
                break;
            }
            prefix.push(part);
        }
        let root = self.cwd.join(&prefix);
        if !root.exists() {
            return true;
        }
        if !root
            .canonicalize()
            .is_ok_and(|real| real.starts_with(self.worktree))
        {
            return false;
        }
        // A glob component cannot match across `/`, so only the remaining
        // pattern depth is reachable. This keeps a shallow glob such as `*`
        // from scanning the whole checkout while still inspecting every
        // symlink the shell could expand through.
        let depth = Path::new(pattern)
            .components()
            .skip(prefix.components().count())
            .count()
            .max(1);
        let mut visited = 0usize;
        for entry in ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .follow_links(false)
            .max_depth(Some(depth))
            .build()
        {
            visited += 1;
            if visited > MAX_GLOB_WALK_ENTRIES {
                return false;
            }
            let Ok(entry) = entry else {
                return false;
            };
            if entry.path_is_symlink()
                && !entry
                    .path()
                    .canonicalize()
                    .is_ok_and(|real| real.starts_with(self.worktree))
            {
                return false;
            }
        }
        true
    }

    /// Every operand-like part of `tokens` (non-flag words and `=`-values
    /// of flags) is lexically and physically confined to the worktree.
    fn operands_are_confined(&self, tokens: &[&str]) -> bool {
        tokens.iter().all(|token| {
            if token_escapes_lexically(token) {
                return false;
            }
            if let Some(stripped) = token.strip_prefix('-') {
                // `--flag=value`: only the value can be a path.
                match stripped.split_once('=') {
                    Some((_, value)) => self.operand_is_confined(value),
                    None => true,
                }
            } else {
                self.operand_is_confined(token)
            }
        })
    }
}

fn segment_is_read_only(tokens: &[String], scope: &Scope<'_>) -> Verdict {
    let (program, args) = tokens.split_first().expect("tokenize never yields empty");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if !scope.operands_are_confined(&args) {
        return Verdict::Reject;
    }
    let read_only = match program.as_str() {
        "cd" => {
            // Exactly one relative operand that resolves to a directory
            // inside the worktree. Bare `cd` goes to $HOME and `cd -` to an
            // unknown previous directory; both leave the checkout.
            let [target] = args.as_slice() else {
                return Verdict::Reject;
            };
            if *target == "-" || target.starts_with('-') {
                return Verdict::Reject;
            }
            return match scope.cwd.join(target).canonicalize() {
                Ok(real) if real.is_dir() && real.starts_with(scope.worktree) => {
                    Verdict::ChangeDir(real)
                }
                _ => Verdict::Reject,
            };
        }
        // Pure readers and text filters with no file-writing options.
        "cat" | "head" | "tail" | "wc" | "pwd" | "echo" | "printf" | "true" | "false" | "which"
        | "whoami" | "id" | "uname" | "stat" | "file" | "df" | "nl" | "cut" | "tr" | "realpath"
        | "basename" | "dirname" | "readlink" | "diff" | "cmp" | "comm" | "jq" | "test" | "["
        | "type" | "egrep" | "fgrep" | "column" | "fold" | "rev" | "tac" | "strings" | "md5sum"
        | "sha1sum" | "sha256sum" | "hexdump" | "od" | "seq" | "expr" => true,
        // Recursive `ls` and `du` can otherwise follow a nested symlink out
        // of the worktree even though their explicit operand is confined.
        "ls" | "du" => !args
            .iter()
            .any(|a| *a == "--dereference" || is_short_flag_with(a, 'L')),
        // `-R` follows symlinks during recursion.
        "grep" => !args
            .iter()
            .any(|a| *a == "-R" || *a == "--dereference-recursive" || is_short_flag_with(a, 'R')),
        "date" => !args.iter().any(|a| *a == "-s" || a.starts_with("--set")),
        "sort" => !args
            .iter()
            .any(|a| a.starts_with("-o") || a.starts_with("--output")),
        // `-o` writes a file; `-l` follows symlinks.
        "tree" => !args.iter().any(|a| *a == "-o" || *a == "-l"),
        // `uniq in out` writes its second positional.
        "uniq" => args.iter().filter(|a| !a.starts_with('-')).count() <= 1,
        // `--pre` runs a preprocessor for every file; `-L` follows symlinks.
        "rg" => !args.iter().any(|a| {
            a.starts_with("--pre")
                || a.starts_with("--hostname-bin")
                || *a == "-L"
                || *a == "--follow"
                || is_short_flag_with(a, 'L')
        }),
        "find" => !args.iter().any(|a| {
            matches!(
                *a,
                "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-delete"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
                    | "-L"
                    | "-H"
                    | "-follow"
            )
        }),
        // Repository-aware Git commands load repository configuration, which
        // can execute helpers such as fsmonitor, diff drivers, and pagers.
        // They therefore require the mutation/approval lane; only a version
        // probe is safe to auto-classify as a pure read.
        "git" => matches!(args.as_slice(), ["--version"] | ["version"]),
        // Toolchains: only a version probe, which runs nothing else.
        "cargo" | "rustc" | "rustup" | "node" | "npm" | "pnpm" | "yarn" | "python" | "python3"
        | "go" | "java" | "gcc" | "clang" | "make" => {
            matches!(args.as_slice(), ["--version"] | ["-V"] | ["version"])
        }
        _ => false,
    };
    if read_only {
        Verdict::Read
    } else {
        Verdict::Reject
    }
}

/// `-abcR` style bundles: a single-dash token carrying `flag`.
fn is_short_flag_with(token: &str, flag: char) -> bool {
    token.len() > 1
        && token.starts_with('-')
        && !token.starts_with("--")
        && token[1..].contains(flag)
}

#[cfg(test)]
mod tests {
    use super::shell_command_is_read_only;
    use std::path::Path;

    struct Worktree {
        dir: tempfile::TempDir,
    }

    impl Worktree {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("src")).unwrap();
            std::fs::create_dir_all(dir.path().join("crates")).unwrap();
            std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
            std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
            std::fs::write(dir.path().join("names.txt"), "b\na\n").unwrap();
            Self { dir }
        }
        fn path(&self) -> &Path {
            self.dir.path()
        }
        fn read_only(&self, cmd: &str) -> bool {
            shell_command_is_read_only(cmd, self.path())
        }
    }

    #[test]
    fn plain_readers_pass() {
        let wt = Worktree::new();
        for cmd in [
            "ls -la",
            "cat src/main.rs",
            "head -n 20 Cargo.toml",
            "wc -l src/*.rs",
            "rg 'fn main' src",
            "grep -rn TODO crates",
            "find . -name '*.rs' -type f",
            "git --version",
            "git version",
            "cargo --version",
            "jq '.name' package.json",
            "cd crates && ls",
            "cd src && cat main.rs",
            "cat Cargo.toml; cat names.txt",
            "test -f Cargo.toml && echo yes || echo no",
            "date",
            "sort names.txt | uniq",
        ] {
            assert!(wt.read_only(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn writers_and_executors_are_rejected() {
        let wt = Worktree::new();
        for cmd in [
            "rm -rf target",
            "touch x",
            "cargo test",
            "cargo check",
            "npm install",
            "python3 script.py",
            "./configure",
            "make",
            "echo hi > out.txt",
            "cat < in.txt",
            "sort -o sorted.txt names.txt",
            "sort --output=sorted.txt names.txt",
            "uniq in.txt out.txt",
            "tree -o tree.txt",
            "date -s '2020-01-01'",
            "find . -name '*.log' -delete",
            "find . -exec rm {} \\;",
            "rg --pre ./decode secret",
            "git branch feature",
            "git branch -d feature",
            "git branch -D feature",
            "git tag v1.0",
            "git tag -d v1.0",
            "git stash",
            "git stash pop",
            "git worktree add ../x",
            "git remote add origin url",
            "git config user.name bob",
            "git config --local user.name bob",
            "git config --local --unset user.name",
            "git -c core.pager=less log",
            "git log --output=log.txt",
            "git diff --ext-diff",
            "git grep -O pattern",
            "git reflog expire --all",
            "git push",
            "git checkout main",
            "git reset --hard",
            "xargs rm",
            "sudo ls",
            "env",
            "printenv",
        ] {
            assert!(!wt.read_only(cmd), "expected mutating: {cmd}");
        }
    }

    #[test]
    fn helper_execution_and_symlink_following_flags_are_rejected() {
        let wt = Worktree::new();
        for cmd in [
            "git diff --textconv",
            "git show --textconv HEAD:img.png",
            "git cat-file --textconv HEAD:img.png",
            "git cat-file --filters HEAD:file",
            "git log -p --textconv",
            "git --config-env=core.pager=X log",
            "rg -L pattern src",
            "rg --follow pattern",
            "rg -nL pattern",
            "grep -R pattern .",
            "grep -rR pattern .",
            "grep --dereference-recursive pattern .",
            "find -L . -name x",
            "find . -follow -name x",
            "tree -l",
            "ls -LR .",
            "ls --recursive --dereference .",
            "du -L .",
            "du -aL .",
        ] {
            assert!(!wt.read_only(cmd), "expected rejected: {cmd}");
        }
    }

    #[test]
    fn repository_aware_git_commands_require_permission() {
        let wt = Worktree::new();
        for cmd in [
            "git status",
            "git log --oneline -20",
            "git diff HEAD~1 -- src",
            "git show HEAD:Cargo.toml",
            "git --no-pager log -5",
            "git -C crates status",
            "git branch -a",
            "git branch --show-current",
            "git tag -l",
            "git remote -v",
            "git stash list",
            "git worktree list",
            "git log --oneline | head -5",
            "git config --get user.name",
            "git config --list",
            "git config --global --list",
            "git config --system --get credential.helper",
            "git config --local --global --list",
            "git config --local --includes --list",
            "git config --local --file=other --list",
            "git config --local --show-origin user.name attacker",
            "git config --local --show-origin --list --get user.name",
            "git config --local --list user.name",
            "git config --local --get",
            "git config --local --get a b c",
            "git config --local --get-urlmatch http https://x",
            "git config --local --show-origin --list",
            "git config --local --get user.name",
            "git config --local --get remote.origin.url .*github.*",
        ] {
            assert!(!wt.read_only(cmd), "expected permission requirement: {cmd}");
        }
    }

    #[test]
    fn shell_constructs_the_classifier_does_not_model_are_rejected() {
        let wt = Worktree::new();
        for cmd in [
            "cat $(which sh)",
            "cat `which sh`",
            "echo $HOME",
            "ls & rm -rf .",
            "ls | (cd / && ls)",
            "cat a.txt \\; rm b",
            "ls\nrm x",
            "ls {a,b}",
            "cat 'unterminated",
            "",
            "   ",
            "| head",
            "ls &&",
        ] {
            assert!(!wt.read_only(cmd), "expected rejected: {cmd}");
        }
    }

    #[test]
    fn reads_outside_the_worktree_are_not_read_only() {
        let wt = Worktree::new();
        for cmd in [
            "cat /etc/passwd",
            "ls ~",
            "ls ~/.ssh",
            "cat ../other-checkout/secret",
            "git -C /tmp/repo status",
            "git -C ../elsewhere log",
            "rg pattern /home",
            "head --lines=3 /proc/self/environ",
            "cat 'sub/../../x'",
        ] {
            assert!(!wt.read_only(cmd), "expected rejected: {cmd}");
        }
        // Relative paths and option values stay inside the checkout.
        assert!(wt.read_only("cat src/main.rs"));
        assert!(wt.read_only("rg --glob='*.rs' main"));
        assert!(!wt.read_only("git log --since=2.weeks"));
    }

    #[test]
    fn cd_is_tracked_and_confined() {
        let wt = Worktree::new();
        // Bare `cd` goes to $HOME; `cd -` to an unknown directory.
        assert!(!wt.read_only("cd && cat .ssh/id_rsa"));
        assert!(!wt.read_only("cd - && cat secret"));
        assert!(!wt.read_only("cd - && ls"));
        // Nonexistent or non-directory targets fail closed.
        assert!(!wt.read_only("cd nope && ls"));
        assert!(!wt.read_only("cd Cargo.toml && ls"));
        // Later operands resolve against the directory the shell is in.
        assert!(wt.read_only("cd src && cat main.rs"));
        assert!(wt.read_only("cd src && cat main.rs | head -1"));
        assert!(wt.read_only("cd src"));
        assert!(!wt.read_only("cd src && cat ../names.txt"));
    }

    #[test]
    fn cd_is_rejected_where_the_shell_might_not_apply_it() {
        let wt = Worktree::new();
        // After `;` a failed cd still lets the next command run in the old
        // directory; `||` can skip the cd entirely; in a pipeline cd runs in
        // a subshell. The classifier cannot know which directory the next
        // operand resolves against, so it refuses to guess.
        assert!(!wt.read_only("cd src; cat main.rs"));
        assert!(!wt.read_only("cd src; ls"));
        assert!(!wt.read_only("ls || cd src && cat main.rs"));
        assert!(!wt.read_only("cd src || cat main.rs"));
        assert!(!wt.read_only("ls | cd src && cat main.rs"));
        assert!(!wt.read_only("cd src | cat main.rs"));
        assert!(!wt.read_only("cd src && cat main.rs; ls"));
        // Without cd, every operator remains usable.
        assert!(wt.read_only("ls; cat Cargo.toml"));
        assert!(wt.read_only("test -f x || echo missing"));
        assert!(wt.read_only("cat Cargo.toml | head -1"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_worktree_are_not_read_only() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s3cr3t").unwrap();
        let wt = Worktree::new();
        std::os::unix::fs::symlink(outside.path(), wt.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            wt.path().join("src/leak.txt"),
        )
        .unwrap();
        std::os::unix::fs::symlink(Path::new("main.rs"), wt.path().join("src/alias.rs")).unwrap();

        // Literal operands are canonicalized through the link.
        assert!(!wt.read_only("cat escape/secret"));
        assert!(!wt.read_only("cat src/leak.txt"));
        assert!(!wt.read_only("ls escape"));
        assert!(!wt.read_only("git -C escape status"));
        assert!(!wt.read_only("cd escape && cat secret"));
        assert!(!wt.read_only("head --lines=1 src/leak.txt"));
        // Globs that could expand through the link reject too.
        assert!(!wt.read_only("cat src/*.txt"));
        assert!(!wt.read_only("wc -l src/*"));
        assert!(!wt.read_only("cat escape/*"));
        assert!(!wt.read_only("ls *"));
        // A symlink that stays inside the worktree is fine.
        assert!(wt.read_only("cat src/alias.rs"));
        assert!(wt.read_only("cat crates/*"));
        // Recursive readers that do not follow links stay allowed; the
        // operand itself is confined.
        assert!(wt.read_only("rg pattern src"));
        assert!(wt.read_only("grep -rn pattern crates"));
    }

    #[cfg(unix)]
    #[test]
    fn glob_symlink_checks_are_bounded_by_match_depth() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let wt = Worktree::new();
        std::fs::create_dir(wt.path().join("src/nested")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            wt.path().join("src/nested/leak.txt"),
        )
        .unwrap();

        // A single component cannot reach the deeper symlink.
        assert!(wt.read_only("cat src/*.rs"));
        // A nested glob can reach it and therefore fails closed.
        assert!(!wt.read_only("cat src/*/*.txt"));
    }

    #[test]
    fn quoted_arguments_are_literal_but_forbidden_characters_still_reject() {
        let wt = Worktree::new();
        // A quoted word naming a mutating program is just a pattern.
        assert!(wt.read_only("rg 'rm -rf' src"));
        assert!(wt.read_only("grep \"rm -rf\" names.txt"));
        // Forbidden characters are checked on the raw text before quoting is
        // interpreted, so a quoted redirect-looking string is still refused.
        // That is deliberate: the cost is one prompt, never a missed write.
        assert!(!wt.read_only("grep 'a > b' names.txt"));
        assert!(!wt.read_only("rg \"$HOME\" src"));
    }

    #[test]
    fn a_missing_worktree_is_never_read_only() {
        assert!(!shell_command_is_read_only(
            "ls",
            Path::new("/nonexistent/trouve/worktree")
        ));
    }
}
