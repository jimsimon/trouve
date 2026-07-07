//! Pure render helpers: fold client-core view-model state into plain row
//! data. Plain data crosses the controller-thread → UI-thread boundary; the
//! UI thread maps it onto generated Slint structs.

use std::collections::HashSet;

use slint_markdown::{parse_blocks, BlockKind};
use trouve_client_core::viewmodel::{ChatItem, ThreadViewModel, ToolCallStatus, TurnState};

/// Mirrors the `ChatRow` struct in `app.slint`.
/// Kinds: 0 user, 1 markdown block, 2 tool card, 3 turn status,
/// 4 thinking, 5 activity (spinner + label), 6 raw response text.
#[derive(Debug, Clone, Default)]
pub struct ChatRowData {
    pub kind: i32,
    pub md_kind: i32,
    /// List nesting level for markdown list rows (0 = top level).
    pub md_indent: i32,
    /// Language tag for code-fence rows ("rust", "" when untagged).
    pub md_lang: String,
    pub text: String,
    /// Markdown source for inline styling (bold/italic/code/links) of
    /// non-code markdown blocks; the UI thread parses it into a Slint
    /// `StyledText`. Empty for rows rendered as plain text.
    pub styled_md: String,
    pub tool_name: String,
    pub tool_status: i32,
    pub detail: String,
    pub expanded: bool,
    pub turn_state: i32,
    /// Turn number (status rows), so the UI can address per-turn actions.
    pub turn: i32,
    /// Status rows: this turn's response is showing as raw text.
    pub raw: bool,
}

/// Wrap inline code spans (backtick runs, CommonMark-style: closed by a run
/// of the same length) in a font-color tag. StyledText renders code spans
/// monospace but offers no color knob, and monospace alone doesn't stand
/// out from prose.
fn tint_code_spans(md: &str) -> String {
    const OPEN: &str = "<font color=\"#e5c07b\">";
    const CLOSE: &str = "</font>";
    let bytes = md.as_bytes();
    let mut out = String::with_capacity(md.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            // Copy prose up to the next backtick (ASCII, so slicing at it
            // is always a char boundary).
            let next = md[i..].find('`').map(|o| i + o).unwrap_or(md.len());
            out.push_str(&md[i..next]);
            i = next;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i] == b'`' {
            i += 1;
        }
        let ticks = i - start;
        // Find the closing run of exactly the same length.
        let mut j = i;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] != b'`' {
                j += 1;
                continue;
            }
            let run_start = j;
            while j < bytes.len() && bytes[j] == b'`' {
                j += 1;
            }
            if j - run_start == ticks {
                close = Some(j);
                break;
            }
        }
        match close {
            Some(end) => {
                out.push_str(OPEN);
                out.push_str(&md[start..end]);
                out.push_str(CLOSE);
                i = end;
            }
            // Unbalanced backticks stay literal.
            None => out.push_str(&md[start..i]),
        }
    }
    out
}

fn md_kind(kind: BlockKind) -> i32 {
    match kind {
        BlockKind::Paragraph => 0,
        BlockKind::H1 => 1,
        BlockKind::H2 => 2,
        BlockKind::H3 => 3,
        BlockKind::Bullet => 4,
        BlockKind::Code => 5,
        BlockKind::Numbered => 6,
    }
}

fn tool_status(status: ToolCallStatus) -> i32 {
    match status {
        ToolCallStatus::AwaitingApproval => 0,
        ToolCallStatus::Running => 1,
        ToolCallStatus::Ok => 2,
        ToolCallStatus::Error => 3,
        ToolCallStatus::Denied => 4,
        ToolCallStatus::Aborted => 5,
    }
}

/// Card title for a tool call. Shell-style tools (native `shell`, vendor
/// `Bash`) show the command they run — `Bash (wc -l foo.rs)` — since the
/// tool name alone says nothing about what happened.
fn tool_label(tool: &str, args: &serde_json::Value) -> String {
    let command = matches!(tool, "shell" | "Bash" | "bash")
        .then(|| args.get("command").and_then(|v| v.as_str()))
        .flatten();
    match command {
        Some(cmd) => {
            // Newlines and runs of spaces collapse so the title stays one line.
            let mut one_line = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
            if one_line.len() > 60 {
                one_line.truncate(one_line.floor_boundary(59));
                one_line.push('…');
            }
            format!("{tool} ({one_line})")
        }
        None => tool.to_string(),
    }
}

/// Flatten a thread's chat items into rows. Returns the rows plus a parallel
/// map from row index to the tool call id (for approvals/expansion).
/// Turns listed in `raw_turns` render their assistant text as one plain
/// (selectable) block instead of styled markdown.
pub fn chat_rows(
    vm: &ThreadViewModel,
    expanded: &HashSet<String>,
    raw_turns: &HashSet<u64>,
) -> (Vec<ChatRowData>, Vec<Option<String>>) {
    let mut rows = Vec::new();
    let mut call_ids = Vec::new();
    let mut push = |row: ChatRowData, call_id: Option<String>| {
        rows.push(row);
        call_ids.push(call_id);
    };
    for item in &vm.items {
        match item {
            ChatItem::User { content, .. } => push(
                ChatRowData {
                    kind: 0,
                    text: content.clone(),
                    ..Default::default()
                },
                None,
            ),
            ChatItem::Assistant { turn, content, .. } => {
                // Raw view: the whole item as one selectable plain-text row
                // (StyledText offers no selection, this is the escape hatch).
                if raw_turns.contains(turn) {
                    push(
                        ChatRowData {
                            kind: 6,
                            text: content.clone(),
                            ..Default::default()
                        },
                        None,
                    );
                    continue;
                }
                // Markdown blocks become individual virtualized rows, so a
                // long streaming answer never re-lays-out the whole chat.
                for block in parse_blocks(content) {
                    // Inline markup survives block parsing verbatim; hand
                    // it to StyledText with block-level structure (heading
                    // weight, bullet glyph) re-applied as markup. Code
                    // fences stay plain text.
                    let styled_md = match block.kind {
                        BlockKind::Code => String::new(),
                        BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => {
                            format!("**{}**", block.text)
                        }
                        BlockKind::Bullet => format!("•  {}", block.text),
                        // The marker rides in the text ("1. item"); escape
                        // its delimiter so StyledText's markdown pass can't
                        // reinterpret the row as list syntax.
                        BlockKind::Numbered => {
                            let digits =
                                block.text.bytes().take_while(u8::is_ascii_digit).count();
                            let mut s = block.text.clone();
                            s.insert(digits, '\\');
                            s
                        }
                        BlockKind::Paragraph => block.text.clone(),
                    };
                    // Inline code spans get a distinct color; code fences
                    // are excluded (empty styled_md).
                    let styled_md = tint_code_spans(&styled_md);
                    push(
                        ChatRowData {
                            kind: 1,
                            md_kind: md_kind(block.kind),
                            md_indent: block.indent,
                            md_lang: block.language,
                            text: block.text,
                            styled_md,
                            ..Default::default()
                        },
                        None,
                    );
                }
            }
            ChatItem::ToolCall {
                call_id,
                tool,
                args,
                status,
                result,
            } => {
                let mut detail =
                    serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
                if let Some(result) = result {
                    detail.push_str("\n→ ");
                    detail.push_str(
                        &serde_json::to_string_pretty(result)
                            .unwrap_or_else(|_| result.to_string()),
                    );
                }
                if detail.len() > 4000 {
                    detail.truncate(detail.floor_boundary(4000));
                    detail.push('…');
                }
                push(
                    ChatRowData {
                        kind: 2,
                        tool_name: tool_label(tool, args),
                        tool_status: tool_status(*status),
                        detail,
                        expanded: expanded.contains(call_id),
                        ..Default::default()
                    },
                    Some(call_id.clone()),
                );
            }
            ChatItem::Thinking { content, .. } => push(
                ChatRowData {
                    kind: 4,
                    text: content.clone(),
                    ..Default::default()
                },
                None,
            ),
            ChatItem::TurnStatus { turn, state } => {
                let (text, code) = match state {
                    // Progress shows as the trailing activity row instead.
                    TurnState::Running => continue,
                    TurnState::Completed { usage } => (turn_summary(usage), 1),
                    TurnState::Failed { error } => (format!("failed: {error}"), 2),
                };
                push(
                    ChatRowData {
                        kind: 3,
                        text,
                        turn_state: code,
                        // The turn's full assistant markdown, for the
                        // "copy response" button on the status row.
                        detail: turn_response(vm, *turn),
                        turn: *turn as i32,
                        raw: raw_turns.contains(turn),
                        ..Default::default()
                    },
                    None,
                );
            }
        }
    }
    if vm.turn_running {
        let label = if vm.thinking {
            "Thinking…"
        } else {
            "Processing…"
        };
        push(
            ChatRowData {
                kind: 5,
                text: label.into(),
                ..Default::default()
            },
            None,
        );
    }
    (rows, call_ids)
}

/// All assistant markdown of one turn, joined — the payload for the
/// "copy response" button. (StyledText offers no selection, so this is the
/// only way to get a styled response out of the app as text.)
fn turn_response(vm: &ThreadViewModel, turn: u64) -> String {
    vm.items
        .iter()
        .filter_map(|item| match item {
            ChatItem::Assistant { turn: t, content, .. } if *t == turn => {
                Some(content.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Turn header: token counts, plus the dollar cost for per-use APIs.
/// Subscription backends never report a cost, so nothing shows there.
fn turn_summary(usage: &trouve_protocol::Usage) -> String {
    let mut s = format!(
        "{} in / {} out tokens",
        usage.input_tokens, usage.output_tokens
    );
    if let Some(cost) = usage.cost_usd.filter(|c| *c > 0.0) {
        s.push_str(&format!(" · ${cost:.4}"));
    }
    s
}

trait FloorBoundary {
    fn floor_boundary(&self, at: usize) -> usize;
}

impl FloorBoundary for String {
    fn floor_boundary(&self, at: usize) -> usize {
        let mut i = at.min(self.len());
        while i > 0 && !self.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

/// Syntax-highlight file content into per-line `(text, rgb)` segments.
pub fn highlight_file(path: &str, content: &str) -> Vec<Vec<(String, u32)>> {
    use std::sync::OnceLock;
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;
    use syntect::util::LinesWithEndings;

    static ASSETS: OnceLock<(SyntaxSet, ThemeSet)> = OnceLock::new();
    let (syntaxes, themes) = ASSETS.get_or_init(|| {
        (
            SyntaxSet::load_defaults_newlines(),
            ThemeSet::load_defaults(),
        )
    });
    let theme = &themes.themes["base16-ocean.dark"];
    let syntax = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| syntaxes.find_syntax_by_extension(e))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(content) {
        let mut segments = Vec::new();
        match highlighter.highlight_line(line, syntaxes) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    let text = text.trim_end_matches(['\n', '\r']);
                    if text.is_empty() {
                        continue;
                    }
                    let fg = style.foreground;
                    let rgb = ((fg.r as u32) << 16) | ((fg.g as u32) << 8) | (fg.b as u32);
                    segments.push((text.to_string(), rgb));
                }
            }
            Err(_) => {
                segments.push((line.trim_end_matches(['\n', '\r']).to_string(), 0));
            }
        }
        if segments.is_empty() {
            segments.push((String::new(), 0));
        }
        lines.push(segments);
    }
    if lines.is_empty() {
        lines.push(vec![(String::new(), 0)]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_spans_get_tinted_and_prose_stays_untouched() {
        assert_eq!(
            tint_code_spans("run `cargo test` twice"),
            "run <font color=\"#e5c07b\">`cargo test`</font> twice"
        );
        // Double-backtick spans (code containing a backtick) match runs of
        // the same length only.
        assert_eq!(
            tint_code_spans("``a ` b`` end"),
            "<font color=\"#e5c07b\">``a ` b``</font> end"
        );
        // Unbalanced backticks stay literal.
        assert_eq!(tint_code_spans("a ` b"), "a ` b");
        assert_eq!(tint_code_spans("no code"), "no code");
    }

    #[test]
    fn turn_summary_shows_cost_only_when_billed() {
        let mut usage = trouve_protocol::Usage {
            input_tokens: 1200,
            output_tokens: 340,
            ..Default::default()
        };
        // Subscription backends report no cost.
        assert_eq!(turn_summary(&usage), "1200 in / 340 out tokens");
        usage.cost_usd = Some(0.0231);
        assert_eq!(turn_summary(&usage), "1200 in / 340 out tokens · $0.0231");
    }

    #[test]
    fn running_turn_renders_trailing_activity_row() {
        let mut vm = ThreadViewModel {
            turn_running: true,
            ..Default::default()
        };
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new());
        assert_eq!(rows.last().unwrap().kind, 5);
        assert_eq!(rows.last().unwrap().text, "Processing…");
        vm.thinking = true;
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new());
        assert_eq!(rows.last().unwrap().text, "Thinking…");
        vm.turn_running = false;
        vm.thinking = false;
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new());
        assert!(rows.is_empty());
    }

    #[test]
    fn raw_turns_render_as_one_plain_row() {
        let vm = ThreadViewModel {
            items: vec![ChatItem::Assistant {
                turn: 3,
                content: "# heading\n\nbody `code`".into(),
                complete: true,
            }],
            ..Default::default()
        };
        // Styled: one row per markdown block.
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new());
        assert!(rows.len() > 1);
        // Raw: the whole item as a single kind-6 row of markdown source.
        let raw: HashSet<u64> = [3].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, 6);
        assert_eq!(rows[0].text, "# heading\n\nbody `code`");
    }

    #[test]
    fn turn_status_carries_the_turns_response_for_copying() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::Assistant {
                    turn: 1,
                    content: "part one".into(),
                    complete: true,
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "part two".into(),
                    complete: true,
                },
                ChatItem::TurnStatus {
                    turn: 1,
                    state: TurnState::Completed {
                        usage: Default::default(),
                    },
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new());
        let status = rows.iter().find(|r| r.kind == 3).unwrap();
        assert_eq!(status.detail, "part one\n\npart two");
    }

    #[test]
    fn shell_tools_show_their_command() {
        let args = serde_json::json!({"command": "wc -l  bench.rs\n"});
        assert_eq!(tool_label("Bash", &args), "Bash (wc -l bench.rs)");
        assert_eq!(tool_label("shell", &args), "shell (wc -l bench.rs)");
        // Non-shell tools and malformed args keep the plain name.
        assert_eq!(tool_label("search", &args), "search");
        assert_eq!(tool_label("Bash", &serde_json::json!({})), "Bash");
        // Long commands truncate on a char boundary with an ellipsis.
        let long = serde_json::json!({ "command": "x".repeat(100) });
        let label = tool_label("Bash", &long);
        assert!(label.len() < 70 && label.ends_with("…)"), "{label}");
    }
}
