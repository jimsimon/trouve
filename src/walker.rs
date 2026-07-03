//! File walking with gitignore/trouveignore support.
//!
//! Port of `semble/index/file_walker.py`. A base spec of always-ignored
//! directories is combined with per-directory `.gitignore` and `.trouveignore`
//! specs; the last matching pattern wins (gitignore semantics). Negation
//! patterns with a file extension (e.g. `!special.kjs`) bypass the extension
//! filter, matching upstream behaviour. Upstream's `.sembleignore` is still
//! honoured but deprecated.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

pub static DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".cache/",
    ".eggs/",
    ".git/",
    ".hg/",
    ".mypy_cache/",
    ".next/",
    ".pytest_cache/",
    ".ruff_cache/",
    ".semble/",
    ".trouve/",
    ".svn/",
    ".tox/",
    ".venv/",
    "__pycache__/",
    "build/",
    "dist/",
    "node_modules/",
    "venv/",
];

struct IgnoreSpec {
    base: PathBuf,
    spec: Gitignore,
}

fn build_spec(base: &Path, lines: &[String]) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(base);
    let mut any = false;
    for line in lines {
        if builder.add_line(None, line).is_ok() {
            any = true;
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

fn load_ignore_for_dir(directory: &Path) -> Option<Gitignore> {
    let mut lines: Vec<String> = Vec::new();
    // Later files win on conflicting patterns; the deprecated `.sembleignore`
    // is loaded before `.trouveignore` so the new name takes precedence.
    for name in [".gitignore", ".sembleignore", ".trouveignore"] {
        let p = directory.join(name);
        if p.is_file() {
            if name == ".sembleignore" {
                crate::utils::warn_deprecated(".sembleignore", ".trouveignore");
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                lines.extend(text.lines().map(|l| l.to_string()));
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        build_spec(directory, &lines)
    }
}

/// Returns `(ignored, found)`: whether the path is ignored, and whether the
/// last matching pattern was a negation with a file-extension suffix (which
/// bypasses the extension filter).
fn is_ignored(path: &Path, is_dir: bool, specs: &[IgnoreSpec]) -> (bool, bool) {
    let mut ignored = false;
    let mut found = false;
    for ignore_spec in specs {
        let Ok(relative) = path.strip_prefix(&ignore_spec.base) else {
            continue;
        };
        match ignore_spec.spec.matched(relative, is_dir) {
            ignore::Match::None => {}
            ignore::Match::Ignore(_) => {
                ignored = true;
                found = false;
            }
            ignore::Match::Whitelist(glob) => {
                ignored = false;
                let pattern = glob.original().trim_end_matches('/');
                found = Path::new(pattern).extension().is_some();
            }
        }
    }
    (ignored, found)
}

/// Yield files under `root` matching `extensions`, skipping ignored paths.
///
/// Directories matching [`DEFAULT_IGNORED_DIRS`] plus any patterns in
/// `ignore_patterns` are always skipped. `.gitignore` and `.trouveignore`
/// files are honoured per directory, inherited downward (plus the deprecated
/// `.sembleignore`).
pub fn walk_files(root: &Path, extensions: &[String], ignore_patterns: &[String]) -> Vec<PathBuf> {
    let extension_set: HashSet<String> = extensions.iter().cloned().collect();
    let mut base_lines: Vec<String> = DEFAULT_IGNORED_DIRS.iter().map(|s| s.to_string()).collect();
    base_lines.extend(ignore_patterns.iter().cloned());
    let mut specs: Vec<IgnoreSpec> = Vec::new();
    if let Some(spec) = build_spec(root, &base_lines) {
        specs.push(IgnoreSpec {
            base: root.to_path_buf(),
            spec,
        });
    }
    let mut out = Vec::new();
    walk(root, &specs, &extension_set, &mut out);
    out
}

fn walk(
    directory: &Path,
    inherited: &[IgnoreSpec],
    extensions: &HashSet<String>,
    out: &mut Vec<PathBuf>,
) {
    let mut specs_storage;
    let specs: &[IgnoreSpec] = match load_ignore_for_dir(directory) {
        Some(spec) => {
            specs_storage = Vec::with_capacity(inherited.len() + 1);
            for s in inherited {
                specs_storage.push(IgnoreSpec {
                    base: s.base.clone(),
                    spec: s.spec.clone(),
                });
            }
            specs_storage.push(IgnoreSpec {
                base: directory.to_path_buf(),
                spec,
            });
            &specs_storage
        }
        None => inherited,
    };

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    items.sort();

    for item in items {
        // Don't follow symlinks.
        if item
            .symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        let is_dir = item.is_dir();
        let (ignored, found) = is_ignored(&item, is_dir, specs);
        if ignored {
            continue;
        }
        if is_dir {
            walk(&item, specs, extensions, out);
        } else if item.is_file() {
            let suffix = item
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{}", e.to_lowercase()))
                .unwrap_or_default();
            if found || extensions.contains(&suffix) {
                out.push(item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn exts() -> Vec<String> {
        vec![".py".into(), ".rs".into()]
    }

    #[test]
    fn walks_matching_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.py"), "print()");
        touch(&root.join("b.rs"), "fn main() {}");
        touch(&root.join("c.txt"), "hello");
        touch(&root.join("sub/d.py"), "x = 1");
        let files = walk_files(root, &exts(), &[]);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.py", "b.rs", "sub/d.py"]);
    }

    #[test]
    fn skips_default_ignored_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("node_modules/x.py"), "x");
        touch(&root.join(".git/y.py"), "y");
        touch(&root.join("__pycache__/z.py"), "z");
        touch(&root.join(".trouve/t.py"), "t");
        touch(&root.join(".semble/s.py"), "s");
        touch(&root.join("keep.py"), "k");
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("keep.py"));
    }

    #[test]
    fn honours_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join(".gitignore"), "secret.py\nignored_dir/\n");
        touch(&root.join("secret.py"), "s");
        touch(&root.join("visible.py"), "v");
        touch(&root.join("ignored_dir/inner.py"), "i");
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("visible.py"));
    }

    #[test]
    fn honours_trouveignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join(".trouveignore"), "secret.py\n");
        touch(&root.join("secret.py"), "s");
        touch(&root.join("visible.py"), "v");
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("visible.py"));
    }

    #[test]
    fn honours_deprecated_sembleignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join(".sembleignore"), "secret.py\n");
        touch(&root.join("secret.py"), "s");
        touch(&root.join("visible.py"), "v");
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("visible.py"));
    }

    #[test]
    fn trouveignore_wins_over_sembleignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join(".sembleignore"), "special.py\n");
        touch(&root.join(".trouveignore"), "!special.py\n");
        touch(&root.join("special.py"), "s");
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("special.py"));
    }

    #[test]
    fn honours_nested_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("sub/.gitignore"), "local.py\n");
        touch(&root.join("sub/local.py"), "l");
        touch(&root.join("sub/other.py"), "o");
        touch(&root.join("local.py"), "top");
        let files = walk_files(root, &exts(), &[]);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["local.py", "sub/other.py"]);
    }

    #[test]
    fn negation_with_extension_bypasses_filter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // .kjs is not in the extension list, but a negation pattern with a
        // suffix bypasses the extension filter (upstream behaviour).
        touch(&root.join(".gitignore"), "*.kjs\n!special.kjs\n");
        touch(&root.join("special.kjs"), "s");
        touch(&root.join("other.kjs"), "o");
        touch(&root.join("a.py"), "a");
        let files = walk_files(root, &exts(), &[]);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.py", "special.kjs"]);
    }

    #[test]
    #[cfg(unix)]
    fn skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("real.py"), "r");
        std::os::unix::fs::symlink(root.join("real.py"), root.join("link.py")).unwrap();
        let files = walk_files(root, &exts(), &[]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("real.py"));
    }
}
