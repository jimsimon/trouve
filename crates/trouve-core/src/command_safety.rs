//! Conservative classification of shell commands that only read.
//!
//! The permission layer treats the `shell` tool as mutating: arbitrary shell
//! text can do anything. That is the right default, but it makes every
//! `git log` or `rg` prompt in Ask mode and denies them outright in read-only
//! personas, even though the same reads are free through `read_file` and
//! `grep`. This module recognises a small, fixed vocabulary of read-only
//! commands so the gate can treat those calls as reads. Everything it does
//! not recognise keeps the mutating classification, so an unknown command,
//! an unparsable one, or one that reaches outside the worktree still asks.
//!
//! Design rules, in order of importance:
//! - Fail closed. Any construct the classifier does not model (substitution,
//!   redirection, background jobs, escapes, expansion) rejects the command.
//! - Stay inside the worktree. Absolute paths, `~`, and `..` components are
//!   rejected because a read-only persona must not become a way to read
//!   `~/.ssh` without a prompt. The normal gate still applies to those.
//! - Never execute model-chosen code. Commands and flags that run other
//!   programs (`find -exec`, `rg --pre`, `git -c`, `xargs`) are rejected even
//!   though the wrapper itself only reads.

/// Whether `command`, as passed to `sh -c`, is recognised as read-only.
pub fn shell_command_is_read_only(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || command.chars().any(is_forbidden_char) {
        return false;
    }
    // Split on the list operators the classifier understands. A lone `&`
    // (background job) is rejected below because it survives the split.
    let mut segments = Vec::new();
    let mut rest = command;
    loop {
        let split = ["&&", "||", "|", ";"]
            .iter()
            .filter_map(|op| rest.find(op).map(|at| (at, op.len())))
            .min_by_key(|(at, len)| (*at, std::cmp::Reverse(*len)));
        match split {
            Some((at, len)) => {
                segments.push(&rest[..at]);
                rest = &rest[at + len..];
            }
            None => {
                segments.push(rest);
                break;
            }
        }
    }
    !segments.is_empty()
        && segments.iter().all(|segment| {
            !segment.contains('&')
                && tokenize(segment).is_some_and(|tokens| segment_is_read_only(&tokens))
        })
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

/// A token that could name a location outside the worktree.
fn token_escapes_worktree(token: &str) -> bool {
    token
        .split('=')
        .any(|part| part.starts_with('/') || part.starts_with('~'))
        || token.split('/').any(|component| component == "..")
}

fn segment_is_read_only(tokens: &[String]) -> bool {
    if tokens.iter().any(|token| token_escapes_worktree(token)) {
        return false;
    }
    let (program, args) = tokens.split_first().expect("tokenize never yields empty");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match program.as_str() {
        // Pure readers and text filters with no file-writing options.
        "ls" | "cat" | "head" | "tail" | "wc" | "pwd" | "echo" | "printf" | "true" | "false"
        | "which" | "whoami" | "id" | "uname" | "stat" | "file" | "du" | "df" | "nl" | "cut"
        | "tr" | "realpath" | "basename" | "dirname" | "readlink" | "diff" | "cmp" | "comm"
        | "jq" | "cd" | "test" | "[" | "type" | "grep" | "egrep" | "fgrep" | "column" | "fold"
        | "rev" | "tac" | "strings" | "md5sum" | "sha1sum" | "sha256sum" | "hexdump" | "od"
        | "seq" | "expr" => true,
        "date" => !args.iter().any(|a| *a == "-s" || a.starts_with("--set")),
        "sort" => !args
            .iter()
            .any(|a| a.starts_with("-o") || a.starts_with("--output")),
        "tree" => !args.contains(&"-o"),
        // `uniq in out` writes its second positional.
        "uniq" => args.iter().filter(|a| !a.starts_with('-')).count() <= 1,
        // `--pre` runs a preprocessor for every file.
        "rg" => !args
            .iter()
            .any(|a| a.starts_with("--pre") || a.starts_with("--hostname-bin")),
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
            )
        }),
        "git" => git_is_read_only(&args),
        // Toolchains: only a version probe, which runs nothing else.
        "cargo" | "rustc" | "rustup" | "node" | "npm" | "pnpm" | "yarn" | "python" | "python3"
        | "go" | "java" | "gcc" | "clang" | "make" => {
            matches!(args.as_slice(), ["--version"] | ["-V"] | ["version"])
        }
        _ => false,
    }
}

fn git_is_read_only(args: &[&str]) -> bool {
    // Global options that run code or retarget the repository are rejected;
    // a relative `-C <dir>` passed the worktree check and is fine.
    let mut rest = args;
    loop {
        match rest {
            [
                "--no-pager" | "-P" | "--no-optional-locks" | "--literal-pathspecs",
                tail @ ..,
            ] => {
                rest = tail;
            }
            ["-C", _, tail @ ..] => rest = tail,
            [flag, ..] if flag.starts_with('-') => return matches!(*flag, "--version"),
            _ => break,
        }
    }
    let Some((subcommand, args)) = rest.split_first() else {
        return false;
    };
    // Options that write files or launch external programs, valid on several
    // of the subcommands below.
    if args.iter().any(|a| {
        a.starts_with("--output")
            || matches!(*a, "--ext-diff" | "-O" | "--open-files-in-pager")
            || a.starts_with("--open-files-in-pager")
    }) {
        return false;
    }
    let positionals = || args.iter().filter(|a| !a.starts_with('-')).count();
    match *subcommand {
        "status" | "log" | "diff" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "blame"
        | "describe" | "shortlog" | "grep" | "cat-file" | "count-objects" | "for-each-ref"
        | "rev-list" | "merge-base" | "name-rev" | "show-ref" | "check-ignore" | "check-attr"
        | "diff-tree" | "diff-index" | "diff-files" | "version" | "help" => true,
        "reflog" => !args
            .iter()
            .any(|a| matches!(*a, "expire" | "delete" | "exists")),
        "stash" => matches!(args.first().copied(), Some("list") | Some("show")),
        "worktree" => args.first().copied() == Some("list"),
        "remote" => match args.first().copied() {
            None => true,
            Some("-v") | Some("--verbose") => args.len() == 1,
            Some("show") | Some("get-url") => true,
            Some(_) => false,
        },
        // `git tag <name>` and `git branch <name>` create; only listings pass.
        "tag" => {
            positionals() == 0
                && !args
                    .iter()
                    .any(|a| matches!(*a, "-d" | "--delete" | "-a" | "-s" | "-f" | "-m" | "-F"))
        }
        "branch" => {
            positionals() == 0
                && !args.iter().any(|a| {
                    matches!(
                        *a,
                        "-d" | "-D"
                            | "--delete"
                            | "-m"
                            | "-M"
                            | "--move"
                            | "-c"
                            | "-C"
                            | "--copy"
                            | "-u"
                            | "--set-upstream-to"
                            | "--unset-upstream"
                            | "--edit-description"
                            | "-f"
                            | "--force"
                    ) || a.starts_with("--set-upstream-to=")
                })
        }
        "config" => {
            args.iter().any(|a| {
                matches!(
                    *a,
                    "--get" | "--get-all" | "--get-regexp" | "--list" | "-l" | "--show-origin"
                )
            }) && !args.iter().any(|a| {
                matches!(
                    *a,
                    "--edit"
                        | "-e"
                        | "--unset"
                        | "--unset-all"
                        | "--add"
                        | "--replace-all"
                        | "--rename-section"
                        | "--remove-section"
                        | "--set"
                )
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::shell_command_is_read_only as read_only;

    #[test]
    fn plain_readers_pass() {
        for cmd in [
            "ls -la",
            "cat src/main.rs",
            "head -n 20 Cargo.toml",
            "wc -l src/*.rs",
            "rg 'fn main' src",
            "grep -rn TODO crates",
            "find . -name '*.rs' -type f",
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
            "git config --get user.name",
            "cargo --version",
            "jq '.name' package.json",
            "cd crates && ls",
            "git log --oneline | head -5",
            "cat a.txt; cat b.txt",
            "test -f Cargo.toml && echo yes || echo no",
            "date",
            "sort names.txt | uniq",
        ] {
            assert!(read_only(cmd), "expected read-only: {cmd}");
        }
    }

    #[test]
    fn writers_and_executors_are_rejected() {
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
            "git config --unset user.name",
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
            assert!(!read_only(cmd), "expected mutating: {cmd}");
        }
    }

    #[test]
    fn shell_constructs_the_classifier_does_not_model_are_rejected() {
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
            assert!(!read_only(cmd), "expected rejected: {cmd}");
        }
    }

    #[test]
    fn reads_outside_the_worktree_are_not_read_only() {
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
            assert!(!read_only(cmd), "expected rejected: {cmd}");
        }
        // Relative paths and option values stay inside the checkout.
        assert!(read_only("cat sub/dir/file.txt"));
        assert!(read_only("rg --glob='*.rs' main"));
        assert!(read_only("git log --since=2.weeks"));
    }

    #[test]
    fn quoted_arguments_are_literal_but_forbidden_characters_still_reject() {
        // A quoted word naming a mutating program is just a pattern.
        assert!(read_only("rg 'rm -rf' src"));
        assert!(read_only("grep \"rm -rf\" notes.md"));
        // Forbidden characters are checked on the raw text before quoting is
        // interpreted, so a quoted redirect-looking string is still refused.
        // That is deliberate: the cost is one prompt, never a missed write.
        assert!(!read_only("grep 'a > b' notes.md"));
        assert!(!read_only("rg \"$HOME\" src"));
    }
}
