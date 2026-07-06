//! A virtualized, selectable code viewer for Slint.
//!
//! The widget renders plain data — lines of colored text segments — so any
//! highlighter can drive it (tree-sitter, server-emitted tokens, none). Rows
//! are virtualized via `ListView`; only visible lines are instantiated.
//!
//! Two ways to use it:
//! - from Rust: instantiate [`CodeViewWindow`] and fill it with
//!   [`lines_model`]/[`plain_lines_model`];
//! - from your own `.slint` scene: add this crate's `ui/` directory
//!   ([`UI_DIR`]) to your `slint-build` include paths and
//!   `import { CodeView } from "code-view.slint";`.

slint::include_modules!();

use slint::{Color, ModelRc, SharedString, VecModel};

/// Path to the crate's `.slint` sources, for `slint_build::CompilerConfiguration::with_include_paths`.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

/// One highlight span: byte columns within a line and an RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub color: u32,
}

/// Split one line into `(text, rgb)` parts given (sorted, non-overlapping)
/// highlight spans; uncovered stretches get color 0 (default foreground).
/// Pure data — usable by any embedder regardless of its generated Slint
/// types.
pub fn segment_parts(line: &str, spans: &[Span]) -> Vec<(String, u32)> {
    let mut parts: Vec<(String, u32)> = Vec::new();
    let mut cursor = 0usize;
    let push = |parts: &mut Vec<(String, u32)>, text: &str, color: u32| {
        if !text.is_empty() {
            parts.push((text.to_string(), color));
        }
    };
    for span in spans {
        let start = span.start.min(line.len());
        let end = span.end.min(line.len());
        if start < cursor || start >= end {
            continue;
        }
        push(&mut parts, &line[cursor..start], 0);
        push(&mut parts, &line[start..end], span.color);
        cursor = end;
    }
    push(&mut parts, &line[cursor..], 0);
    if parts.is_empty() {
        // Empty lines still need a segment so the row has content height.
        parts.push((String::new(), 0));
    }
    parts
}

/// [`segment_parts`] mapped onto this crate's generated `TextSegment`.
pub fn segments_for_line(line: &str, spans: &[Span]) -> Vec<TextSegment> {
    segment_parts(line, spans)
        .into_iter()
        .map(|(text, color)| TextSegment {
            text: SharedString::from(text.as_str()),
            color: Color::from_argb_encoded(0xff00_0000 | color),
        })
        .collect()
}

/// Build the widget's `lines` model from text plus per-line highlight spans.
/// `spans[i]` colors line `i`; missing entries render unhighlighted.
pub fn lines_model(text: &str, spans: &[Vec<Span>]) -> ModelRc<ModelRc<TextSegment>> {
    let rows: Vec<ModelRc<TextSegment>> = text
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let line_spans = spans.get(i).map(Vec::as_slice).unwrap_or(&[]);
            ModelRc::new(VecModel::from(segments_for_line(line, line_spans)))
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// Unhighlighted variant of [`lines_model`].
pub fn plain_lines_model(text: &str) -> ModelRc<ModelRc<TextSegment>> {
    lines_model(text, &[])
}

/// 1..=n line-number model for `text`.
pub fn line_numbers_model(text: &str) -> ModelRc<i32> {
    ModelRc::new(VecModel::from(
        (1..=text.lines().count() as i32).collect::<Vec<_>>(),
    ))
}

/// Extract the selected text from the widget's grid selection
/// (start line/col inclusive, end col exclusive), using character columns.
pub fn selection_text(
    lines: &[&str],
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
) -> String {
    let clamp = |line: &str, col: usize| -> usize {
        line.char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    };
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end_line + 1).skip(start_line) {
        let from = if i == start_line {
            clamp(line, start_col)
        } else {
            0
        };
        let to = if i == end_line {
            clamp(line, end_col)
        } else {
            line.len()
        };
        if i > start_line {
            out.push('\n');
        }
        if from < to {
            out.push_str(&line[from..to]);
        }
    }
    out
}

/// Read the selection state out of a [`CodeViewWindow`] and produce the
/// text to copy, or `None` when there is no selection.
pub fn copy_text_from(view: &CodeViewWindow, source: &str) -> Option<String> {
    if !view.get_has_selection() {
        return None;
    }
    let lines: Vec<&str> = source.lines().collect();
    let start_line = view.get_sel_start_line().max(0) as usize;
    let end_line = (view.get_sel_end_line().max(0) as usize).min(lines.len().saturating_sub(1));
    Some(selection_text(
        &lines,
        start_line,
        view.get_sel_start_col().max(0) as usize,
        end_line,
        view.get_sel_end_col().max(0) as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_cover_whole_line() {
        let segs = segments_for_line(
            "let x = 42;",
            &[
                Span {
                    start: 0,
                    end: 3,
                    color: 0x569cd6,
                }, // let
                Span {
                    start: 8,
                    end: 10,
                    color: 0xb5cea8,
                }, // 42
            ],
        );
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "let x = 42;");
        assert_eq!(segs.len(), 4);
    }

    #[test]
    fn overlapping_or_invalid_spans_are_skipped() {
        let segs = segments_for_line(
            "abcdef",
            &[
                Span {
                    start: 0,
                    end: 4,
                    color: 1,
                },
                Span {
                    start: 2,
                    end: 5,
                    color: 2,
                }, // overlaps: skipped
                Span {
                    start: 9,
                    end: 12,
                    color: 3,
                }, // out of range: clamped/empty
            ],
        );
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "abcdef");
    }

    #[test]
    fn selection_single_and_multi_line() {
        let lines = vec!["hello world", "second line", "third"];
        assert_eq!(selection_text(&lines, 0, 6, 0, 11), "world");
        assert_eq!(
            selection_text(&lines, 0, 6, 2, 5),
            "world\nsecond line\nthird"
        );
        // Columns past the end clamp.
        assert_eq!(selection_text(&lines, 2, 0, 2, 99), "third");
    }
}
