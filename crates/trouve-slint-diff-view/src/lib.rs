//! A virtualized unified-diff viewer for Slint.
//!
//! [`parse_unified_diff`] turns `git diff` output into a file/hunk/line
//! model; [`rows_model`] flattens it into the widget's virtualized row
//! model, honouring per-file collapse state. Embed `DiffView` from
//! `ui/diff-view.slint` ([`UI_DIR`]) in your own scene or instantiate
//! [`DiffViewWindow`] directly.

slint::include_modules!();

use slint::{ModelRc, SharedString, VecModel};

/// Path to the crate's `.slint` sources.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

/// Parse `git diff` / unified diff text into files, hunks, and numbered
/// lines. Unknown metadata lines (index, mode, ---/+++) are skipped.
pub fn parse_unified_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // "a/path b/path" — take the b/ path.
            let path = rest
                .split_whitespace()
                .last()
                .map(|p| p.trim_start_matches("b/").to_string())
                .unwrap_or_else(|| rest.to_string());
            files.push(FileDiff {
                path,
                hunks: Vec::new(),
            });
        } else if line.starts_with("@@") {
            // @@ -old,count +new,count @@ context
            let (o, n) = parse_hunk_header(line);
            old_no = o;
            new_no = n;
            if files.is_empty() {
                files.push(FileDiff {
                    path: String::new(),
                    hunks: Vec::new(),
                });
            }
            files.last_mut().unwrap().hunks.push(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = files.last_mut().and_then(|f| f.hunks.last_mut()) {
            match line.as_bytes().first() {
                Some(b'+') => {
                    hunk.lines.push(DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text: line[1..].to_string(),
                    });
                    new_no += 1;
                }
                Some(b'-') => {
                    hunk.lines.push(DiffLine {
                        kind: LineKind::Delete,
                        old_no: Some(old_no),
                        new_no: None,
                        text: line[1..].to_string(),
                    });
                    old_no += 1;
                }
                Some(b' ') => {
                    hunk.lines.push(DiffLine {
                        kind: LineKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: line[1..].to_string(),
                    });
                    old_no += 1;
                    new_no += 1;
                }
                _ => {} // "\ No newline at end of file" and metadata
            }
        }
    }
    files
}

/// Split raw diff text into one segment per file, aligned with
/// `parse_unified_diff`'s output (segment i is the raw text of file i).
/// Feeds per-file copy buttons in the diff view.
pub fn split_file_diffs(diff: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in diff.lines() {
        // Same file boundaries as the parser: a git header starts a file,
        // and a bare hunk header starts one for headerless unified diffs.
        if line.starts_with("diff --git ") || (out.is_empty() && line.starts_with("@@")) {
            out.push(String::new());
        }
        if let Some(cur) = out.last_mut() {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    out
}

fn parse_hunk_header(header: &str) -> (u32, u32) {
    let mut old = 1;
    let mut new = 1;
    for part in header.split_whitespace() {
        if let Some(rest) = part.strip_prefix('-') {
            old = rest
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
        } else if let Some(rest) = part.strip_prefix('+') {
            new = rest
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
        }
    }
    (old, new)
}

/// Per-file stats (adds, deletes) for headers/badges.
pub fn file_stats(file: &FileDiff) -> (usize, usize) {
    let mut adds = 0;
    let mut dels = 0;
    for hunk in &file.hunks {
        for line in &hunk.lines {
            match line.kind {
                LineKind::Add => adds += 1,
                LineKind::Delete => dels += 1,
                LineKind::Context => {}
            }
        }
    }
    (adds, dels)
}

/// A flattened diff row as plain data. Embedders that compile `DiffView`
/// into their own Slint unit map this onto their generated `DiffRow` type
/// (generated types don't cross crate boundaries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowData {
    /// 0 file header, 1 hunk header, 2 context, 3 add, 4 delete.
    pub kind: i32,
    pub old_no: String,
    pub new_no: String,
    pub text: String,
    pub file_index: i32,
    pub collapsed: bool,
}

/// Flatten parsed files into virtualizable rows. `collapsed[i]` hides file
/// i's hunks (its header row remains).
pub fn build_rows(files: &[FileDiff], collapsed: &[bool]) -> Vec<RowData> {
    let mut rows = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let is_collapsed = collapsed.get(i).copied().unwrap_or(false);
        let (adds, dels) = file_stats(file);
        rows.push(RowData {
            kind: 0,
            old_no: String::new(),
            new_no: String::new(),
            text: format!("{}  (+{adds} −{dels})", file.path),
            file_index: i as i32,
            collapsed: is_collapsed,
        });
        if is_collapsed {
            continue;
        }
        for hunk in &file.hunks {
            rows.push(RowData {
                kind: 1,
                old_no: String::new(),
                new_no: String::new(),
                text: hunk.header.clone(),
                file_index: i as i32,
                collapsed: false,
            });
            for line in &hunk.lines {
                rows.push(RowData {
                    kind: match line.kind {
                        LineKind::Context => 2,
                        LineKind::Add => 3,
                        LineKind::Delete => 4,
                    },
                    old_no: line.old_no.map(|n| n.to_string()).unwrap_or_default(),
                    new_no: line.new_no.map(|n| n.to_string()).unwrap_or_default(),
                    text: line.text.clone(),
                    file_index: i as i32,
                    collapsed: false,
                });
            }
        }
    }
    rows
}

/// [`build_rows`] mapped onto this crate's generated `DiffRow` model.
pub fn rows_model(files: &[FileDiff], collapsed: &[bool]) -> ModelRc<DiffRow> {
    let rows: Vec<DiffRow> = build_rows(files, collapsed)
        .into_iter()
        .map(|r| DiffRow {
            kind: r.kind,
            old_no: SharedString::from(r.old_no.as_str()),
            new_no: SharedString::from(r.new_no.as_str()),
            text: SharedString::from(r.text.as_str()),
            file_index: r.file_index,
            collapsed: r.collapsed,
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"extra\");
 }
diff --git a/README.md b/README.md
@@ -10,2 +11,2 @@ ## Title
 unchanged
-removed
+added
";

    #[test]
    fn parses_files_hunks_and_numbers() {
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(files[0].hunks.len(), 1);
        let lines = &files[0].hunks[0].lines;
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!(lines[0].old_no, Some(1));
        assert_eq!(lines[0].new_no, Some(1));
        assert_eq!(lines[1].kind, LineKind::Delete);
        assert_eq!(lines[1].old_no, Some(2));
        assert_eq!(lines[2].kind, LineKind::Add);
        assert_eq!(lines[2].new_no, Some(2));
        assert_eq!(lines[3].kind, LineKind::Add);
        assert_eq!(lines[3].new_no, Some(3));
        assert_eq!(files[1].hunks[0].lines[0].old_no, Some(10));
        assert_eq!(file_stats(&files[0]), (2, 1));
    }

    #[test]
    fn splits_per_file_segments_aligned_with_parse() {
        let segments = split_file_diffs(SAMPLE);
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(segments.len(), files.len());
        assert!(segments[0].starts_with("diff --git a/src/a.rs"));
        assert!(segments[0].contains("println!(\"extra\");"));
        assert!(!segments[0].contains("README"));
        assert!(segments[1].starts_with("diff --git a/README.md"));
        assert!(segments[1].ends_with("+added\n"));
    }

    #[test]
    fn collapse_hides_hunks_but_keeps_header() {
        use slint::Model;
        let files = parse_unified_diff(SAMPLE);
        let all = rows_model(&files, &[false, false]);
        let collapsed = rows_model(&files, &[true, false]);
        assert!(all.row_count() > collapsed.row_count());
        // First row is still the file header, marked collapsed.
        let first = collapsed.row_data(0).unwrap();
        assert_eq!(first.kind, 0);
        assert!(first.collapsed);
        // Second row jumps straight to the second file's header.
        assert_eq!(collapsed.row_data(1).unwrap().kind, 0);
    }
}
