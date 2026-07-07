//! Pure render helpers: fold client-core view-model state into plain row
//! data. Plain data crosses the controller-thread → UI-thread boundary; the
//! UI thread maps it onto generated Slint structs.

use std::collections::{HashMap, HashSet};

use slint_markdown::{parse_blocks, BlockKind};
use trouve_client_core::viewmodel::{ChatItem, ThreadViewModel, ToolCallStatus, TurnState};

/// Mirrors the `ChatRow` struct in `app.slint`.
/// Kinds: 0 user, 1 markdown block, 2 tool card, 3 turn status (failures),
/// 4 thinking sub-card (nested in the agent card like tool calls),
/// 5 activity (spinner + label), 6 raw response text,
/// 7 card header (collapsible group for user/agent items),
/// 8 horizontal rule between turns, 9 grouped tool-run header
/// ("Called n tools").
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Text tint for markdown rows: 0 agent (default), 1 user prompt,
    /// 2 thinking.
    pub tone: i32,
    pub tool_name: String,
    pub tool_status: i32,
    /// Read-style tool cards: the file path argument, so the header
    /// filename can open it in the Files view (`text` holds the basename).
    pub tool_file: String,
    pub detail: String,
    pub expanded: bool,
    pub turn_state: i32,
    /// Turn number (status rows), so the UI can address per-turn actions.
    pub turn: i32,
    /// Assistant headers: this turn's response is showing as raw text.
    pub raw: bool,
    /// Assistant headers: token/cost summary once the turn completed.
    pub meta: String,
    /// Header rows: stable key for the collapse toggle ("u:3", "a:5", …).
    pub card_key: String,
    /// Position within a collapsible card, for drawing one continuous
    /// outline across its rows: 0 not carded, 1 header (body follows),
    /// 2 body, 3 last body row, 4 standalone header (collapsed/empty).
    pub card_pos: i32,
    /// First body row of its card (slab rows pad down from the header).
    pub card_first: bool,
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
/// (selectable) block instead of styled markdown. User/assistant/thinking
/// items get a collapsible header row; keys in `collapsed` hide the body.
pub fn chat_rows(
    vm: &ThreadViewModel,
    expanded: &HashSet<String>,
    raw_turns: &HashSet<u64>,
    collapsed: &HashSet<String>,
) -> (Vec<ChatRowData>, Vec<Option<String>>) {
    let mut rows: Vec<ChatRowData> = Vec::new();
    let mut call_ids: Vec<Option<String>> = Vec::new();
    // Tool calls and thinking render inside an assistant card: preferably
    // the nearest assistant item before them (the response that requested
    // them), else the next one after (turns that open with tool calls or
    // thinking before any text). User prompts and turn-status items bound
    // the search so nothing attaches across turns.
    let owner: HashMap<usize, usize> = {
        let mut owner = HashMap::new();
        let mut prev = None;
        for (i, item) in vm.items.iter().enumerate() {
            match item {
                ChatItem::Assistant { .. } => prev = Some(i),
                ChatItem::User { .. } | ChatItem::TurnStatus { .. } => prev = None,
                ChatItem::ToolCall { .. } | ChatItem::Thinking { .. } => {
                    if let Some(a) = prev {
                        owner.insert(i, a);
                    }
                }
            }
        }
        let mut next = None;
        for (i, item) in vm.items.iter().enumerate().rev() {
            match item {
                ChatItem::Assistant { .. } => next = Some(i),
                ChatItem::User { .. } | ChatItem::TurnStatus { .. } => next = None,
                ChatItem::ToolCall { .. } | ChatItem::Thinking { .. } => {
                    if let (None, Some(a)) = (owner.get(&i), next) {
                        owner.insert(i, a);
                    }
                }
            }
        }
        owner
    };
    // Assistant items already folded into an earlier item's card.
    let mut merged: HashSet<usize> = HashSet::new();
    // Item indices are stable (the event fold only appends or edits in
    // place), so they key the collapse state.
    for (i, item) in vm.items.iter().enumerate() {
        match item {
            ChatItem::User { content, .. } => {
                // A user prompt starts a new turn: separate it from the
                // previous one with a horizontal rule.
                if !rows.is_empty() {
                    rows.push(ChatRowData {
                        kind: 8,
                        ..Default::default()
                    });
                    call_ids.push(None);
                }
                let key = format!("u:{i}");
                let open = !collapsed.contains(&key);
                let mut body = Vec::new();
                if open {
                    // Prompts render as markdown too, tinted prompt-blue.
                    push_blocks(&mut body, content);
                    for (b, _) in &mut body {
                        b.tone = 1;
                    }
                }
                let header = ChatRowData {
                    tool_name: "You".into(),
                    text: preview(content),
                    detail: content.clone(),
                    expanded: open,
                    card_key: key,
                    ..Default::default()
                };
                push_card(&mut rows, &mut call_ids, header, body);
            }
            ChatItem::Assistant { turn, .. } => {
                // Consecutive assistant items (a response resuming after
                // its tool calls) merge into one card; this run was already
                // rendered under an earlier item's card.
                if merged.contains(&i) {
                    continue;
                }
                // The run: this item plus every following assistant,
                // (owned) tool-call, or thinking item, until something
                // else intervenes.
                let mut end = i;
                let mut k = i + 1;
                while k < vm.items.len() {
                    match &vm.items[k] {
                        ChatItem::Assistant { .. } => {
                            merged.insert(k);
                            end = k;
                        }
                        ChatItem::ToolCall { .. } | ChatItem::Thinking { .. } => end = k,
                        _ => break,
                    }
                    k += 1;
                }
                let run_content = |m: usize| match &vm.items[m] {
                    ChatItem::Assistant { content, .. } => content.as_str(),
                    _ => "",
                };
                let joined = (i..=end)
                    .map(run_content)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let key = format!("a:{i}");
                let open = !collapsed.contains(&key);
                let raw = raw_turns.contains(turn);
                let done = turn_state(vm, *turn).is_some();
                let mut body = Vec::new();
                if open {
                    // Tool calls / thinking issued before any text
                    // streamed.
                    let mut lead: Vec<usize> = owner
                        .iter()
                        .filter(|&(&j, &a)| a == i && j < i)
                        .map(|(&j, _)| j)
                        .collect();
                    lead.sort_unstable();
                    nested_rows(&mut body, vm, &lead, i, done, collapsed, expanded);
                    // Walk the run in order: text stretches become markdown
                    // rows — or, in raw view, one selectable plain-text row
                    // per stretch (StyledText offers no selection, this is
                    // the escape hatch) — while tool-call and thinking
                    // stretches nest where they happened.
                    let mut k = i;
                    while k <= end {
                        match &vm.items[k] {
                            ChatItem::Assistant { .. } => {
                                let start = k;
                                while k <= end
                                    && matches!(vm.items[k], ChatItem::Assistant { .. })
                                {
                                    k += 1;
                                }
                                let stretch = (start..k)
                                    .map(run_content)
                                    .filter(|s| !s.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("\n\n");
                                if stretch.is_empty() {
                                    // Nothing streamed yet for this item.
                                } else if raw {
                                    body.push((
                                        ChatRowData {
                                            kind: 6,
                                            text: stretch,
                                            ..Default::default()
                                        },
                                        None,
                                    ));
                                } else {
                                    push_blocks(&mut body, &stretch);
                                }
                            }
                            _ => {
                                let start = k;
                                while k <= end
                                    && !matches!(vm.items[k], ChatItem::Assistant { .. })
                                {
                                    k += 1;
                                }
                                let run: Vec<usize> = (start..k).collect();
                                nested_rows(&mut body, vm, &run, i, done, collapsed, expanded);
                            }
                        }
                    }
                }
                // The turn's token/cost summary shows in the header of the
                // turn's last card (where the status row used to sit).
                let last_of_turn = !vm.items[end + 1..].iter().any(
                    |it| matches!(it, ChatItem::Assistant { turn: t, .. } if t == turn),
                );
                let meta = match (last_of_turn, turn_state(vm, *turn)) {
                    (true, Some(TurnState::Completed { usage })) => turn_summary(usage),
                    _ => String::new(),
                };
                let header = ChatRowData {
                    tool_name: "Agent".into(),
                    text: preview(&joined),
                    // The header copy button mirrors what's on screen: the
                    // markdown source in raw view, rendered-ish plain text
                    // (inline markers stripped) in styled view.
                    detail: if raw {
                        joined.clone()
                    } else {
                        plain_text(&joined)
                    },
                    expanded: open,
                    card_key: key,
                    turn: *turn as i32,
                    raw,
                    meta,
                    ..Default::default()
                };
                push_card(&mut rows, &mut call_ids, header, body);
            }
            ChatItem::ToolCall { .. } | ChatItem::Thinking { .. } => {
                // Owned tool calls / thinking were rendered inside their
                // assistant card; ones folded into an earlier synthesized
                // card below are done too.
                if owner.contains_key(&i) || merged.contains(&i) {
                    continue;
                }
                // No assistant item exists yet (a turn that opens with
                // tool calls or thinking, still streaming) — synthesize
                // the Agent card around the run now so the wrapper is
                // present for the whole turn instead of popping in with
                // the first text.
                let mut run = vec![i];
                let mut k = i + 1;
                while k < vm.items.len() && !owner.contains_key(&k) {
                    if matches!(
                        vm.items[k],
                        ChatItem::ToolCall { .. } | ChatItem::Thinking { .. }
                    ) {
                        merged.insert(k);
                        run.push(k);
                    } else {
                        break;
                    }
                    k += 1;
                }
                // The enclosing turn, from the prompt that started it.
                let turn = vm.items[..i]
                    .iter()
                    .rev()
                    .find_map(|it| match it {
                        ChatItem::User { turn, .. } => Some(*turn),
                        _ => None,
                    })
                    .unwrap_or(0);
                let key = format!("a:{i}");
                let open = !collapsed.contains(&key);
                let done = turn_state(vm, turn).is_some();
                let mut body = Vec::new();
                if open {
                    nested_rows(&mut body, vm, &run, i, done, collapsed, expanded);
                }
                // Orphan items mean no assistant item in this turn, so this
                // card is where the turn summary lands once it completes.
                let meta = match turn_state(vm, turn) {
                    Some(TurnState::Completed { usage }) => turn_summary(usage),
                    _ => String::new(),
                };
                let header = ChatRowData {
                    tool_name: "Agent".into(),
                    expanded: open,
                    card_key: key,
                    turn: turn as i32,
                    meta,
                    ..Default::default()
                };
                push_card(&mut rows, &mut call_ids, header, body);
            }
            ChatItem::TurnStatus { state, .. } => {
                // Completed turns show their summary in the assistant card
                // header instead; running turns show the activity row.
                let TurnState::Failed { error } = state else {
                    continue;
                };
                rows.push(ChatRowData {
                    kind: 3,
                    text: format!("failed: {error}"),
                    turn_state: 2,
                    ..Default::default()
                });
                call_ids.push(None);
            }
        }
    }
    if vm.turn_running {
        let label = if vm.thinking {
            "Thinking…"
        } else {
            "Processing…"
        };
        rows.push(ChatRowData {
            kind: 5,
            text: label.into(),
            ..Default::default()
        });
        call_ids.push(None);
    }
    (rows, call_ids)
}

/// Append a collapsible card: the (caller-built) header row, then the body
/// rows (each with an optional tool call id) positioned so the UI can draw
/// one continuous outline (`card_pos`). Body rows inherit the header's
/// title so the outline keeps its tint — except tool rows, whose
/// `tool_name` is the visible label.
fn push_card(
    rows: &mut Vec<ChatRowData>,
    call_ids: &mut Vec<Option<String>>,
    mut header: ChatRowData,
    body: Vec<(ChatRowData, Option<String>)>,
) {
    let n = body.len();
    let title = header.tool_name.clone();
    header.kind = 7;
    header.card_pos = if n == 0 { 4 } else { 1 };
    rows.push(header);
    call_ids.push(None);
    for (j, (mut b, id)) in body.into_iter().enumerate() {
        b.card_pos = if j + 1 == n { 3 } else { 2 };
        b.card_first = j == 0;
        if b.kind != 2 {
            b.tool_name = title.clone();
        }
        rows.push(b);
        call_ids.push(id);
    }
}

/// Append a mixed stretch of tool-call and thinking items to a card body,
/// in order: consecutive tool calls group via [`tool_run_rows`]; each
/// thinking item becomes one collapsible kind-4 sub-card keyed by its item
/// index — expanded while its turn is the latest, collapsed by default once
/// the next prompt is submitted (the reader has moved on).
fn nested_rows(
    body: &mut Vec<(ChatRowData, Option<String>)>,
    vm: &ThreadViewModel,
    run: &[usize],
    anchor: usize,
    done: bool,
    collapsed: &HashSet<String>,
    expanded: &HashSet<String>,
) {
    let mut p = 0;
    while p < run.len() {
        let j = run[p];
        if let ChatItem::Thinking { turn, content, .. } = &vm.items[j] {
            let key = format!("t:{j}");
            // The toggle set flips whichever default applies.
            let toggled = collapsed.contains(&key);
            let open = if *turn < latest_turn(vm) {
                toggled
            } else {
                !toggled
            };
            // Purple header pill; the content follows as ordinary markdown
            // rows (tone 2), indented one level under the pill.
            body.push((
                ChatRowData {
                    kind: 4,
                    detail: content.clone(),
                    // Header teaser for the collapsed state.
                    meta: preview(content),
                    expanded: open,
                    card_key: key,
                    ..Default::default()
                },
                None,
            ));
            if open {
                let start = body.len();
                push_blocks(body, content);
                for (b, _) in &mut body[start..] {
                    b.tone = 2;
                }
            }
            p += 1;
        } else {
            let start = p;
            while p < run.len() && matches!(vm.items[run[p]], ChatItem::ToolCall { .. }) {
                p += 1;
            }
            // Group keys stay stable across renders: first tool's item
            // index plus the owning card's anchor.
            let gkey = format!("g{}:{anchor}", run[start]);
            tool_run_rows(body, vm, &run[start..p], gkey, done, collapsed, expanded);
        }
    }
}

/// Append a run of tool-call items to a card body. Runs of 2+ consecutive
/// tool calls fold under one "Called n tools" header (kind 9) — expanded
/// while the turn streams so progress is visible, collapsed by default once
/// it's done (`gkey` in `collapsed` flips whichever default applies).
fn tool_run_rows(
    body: &mut Vec<(ChatRowData, Option<String>)>,
    vm: &ThreadViewModel,
    run: &[usize],
    gkey: String,
    done: bool,
    collapsed: &HashSet<String>,
    expanded: &HashSet<String>,
) {
    let tool_body = |j: usize| {
        let ChatItem::ToolCall {
            call_id,
            tool,
            args,
            status,
            result,
        } = &vm.items[j]
        else {
            unreachable!("tool runs hold only tool-call items");
        };
        (
            tool_row(call_id, tool, args, *status, result, expanded),
            Some(call_id.clone()),
        )
    };
    if run.len() < 2 {
        body.extend(run.iter().map(|&j| tool_body(j)));
        return;
    }
    // A pending approval must be visible to be answered: it holds the
    // group open regardless of the collapse toggle.
    let needs_approval = run.iter().any(|&j| {
        matches!(
            &vm.items[j],
            ChatItem::ToolCall {
                status: ToolCallStatus::AwaitingApproval,
                ..
            }
        )
    });
    let toggled = collapsed.contains(&gkey);
    let g_open = needs_approval || if done { toggled } else { !toggled };
    body.push((
        ChatRowData {
            kind: 9,
            text: format!("Called {} tools", run.len()),
            expanded: g_open,
            card_key: gkey,
            ..Default::default()
        },
        None,
    ));
    if g_open {
        // One indent level under the group header.
        body.extend(run.iter().map(|&j| {
            let (mut row, id) = tool_body(j);
            row.md_indent = 1;
            (row, id)
        }));
    }
}

/// Append one assistant text segment as markdown-block rows. Each block is
/// an individual virtualized row, so a long streaming answer never
/// re-lays-out the whole chat.
fn push_blocks(body: &mut Vec<(ChatRowData, Option<String>)>, content: &str) {
    for block in parse_blocks(content) {
        // Inline markup survives block parsing verbatim; hand it to
        // StyledText with block-level structure (heading weight, bullet
        // glyph) re-applied as markup. Code fences stay plain text.
        let styled_md = match block.kind {
            BlockKind::Code => String::new(),
            BlockKind::H1 | BlockKind::H2 | BlockKind::H3 => {
                format!("**{}**", block.text)
            }
            BlockKind::Bullet => format!("•  {}", block.text),
            // The marker rides in the text ("1. item"); escape its
            // delimiter so StyledText's markdown pass can't reinterpret
            // the row as list syntax.
            BlockKind::Numbered => {
                let digits = block.text.bytes().take_while(u8::is_ascii_digit).count();
                let mut s = block.text.clone();
                s.insert(digits, '\\');
                s
            }
            BlockKind::Paragraph => block.text.clone(),
        };
        // Inline code spans get a distinct color; code fences are
        // excluded (empty styled_md).
        let styled_md = tint_code_spans(&styled_md);
        body.push((
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
        ));
    }
}

/// Build the row for one tool call card (used standalone and inside
/// assistant card bodies).
fn tool_row(
    call_id: &str,
    tool: &str,
    args: &serde_json::Value,
    status: ToolCallStatus,
    result: &Option<serde_json::Value>,
    expanded: &HashSet<String>,
) -> ChatRowData {
    let mut detail = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    if let Some(result) = result {
        detail.push_str("\n→ ");
        detail.push_str(
            &serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()),
        );
    }
    if detail.len() > 4000 {
        detail.truncate(detail.floor_boundary(4000));
        detail.push('…');
    }
    // Read-style tools (native read_file, Claude Read, cursor read) title
    // as "Read <filename>", with the filename clickable in the UI.
    let file = matches!(tool, "Read" | "read" | "read_file")
        .then(|| {
            args.get("file_path")
                .or_else(|| args.get("path"))
                .and_then(serde_json::Value::as_str)
        })
        .flatten()
        .unwrap_or_default();
    ChatRowData {
        kind: 2,
        tool_name: if file.is_empty() {
            tool_label(tool, args)
        } else {
            "Read".into()
        },
        text: file.rsplit('/').next().unwrap_or_default().to_string(),
        tool_file: file.to_string(),
        tool_status: tool_status(status),
        detail,
        expanded: expanded.contains(call_id),
        ..Default::default()
    }
}

/// One-line teaser for a collapsed card header: the first non-empty line,
/// capped; the header row elides it further to fit.
fn preview(content: &str) -> String {
    let line = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    let mut s = line.trim().to_string();
    if s.len() > 120 {
        s.truncate(s.floor_boundary(119));
        s.push('…');
    }
    s
}

/// The highest turn number present in the thread (0 when empty).
fn latest_turn(vm: &ThreadViewModel) -> u64 {
    vm.items
        .iter()
        .filter_map(|item| match item {
            ChatItem::User { turn, .. }
            | ChatItem::Assistant { turn, .. }
            | ChatItem::Thinking { turn, .. }
            | ChatItem::TurnStatus { turn, .. } => Some(*turn),
            ChatItem::ToolCall { .. } => None,
        })
        .max()
        .unwrap_or(0)
}

/// The final state of a turn, if its status item arrived.
fn turn_state(vm: &ThreadViewModel, turn: u64) -> Option<&TurnState> {
    vm.items.iter().find_map(|item| match item {
        ChatItem::TurnStatus { turn: t, state } if *t == turn => Some(state),
        _ => None,
    })
}

/// Approximate the on-screen text of a styled markdown response for the
/// header copy button: block structure kept, inline markers (emphasis,
/// code-span backticks) stripped, bullets rendered as they display.
fn plain_text(md: &str) -> String {
    let strip = |s: &str| s.replace("**", "").replace('`', "");
    parse_blocks(md)
        .iter()
        .map(|b| match b.kind {
            BlockKind::Code => b.text.clone(),
            BlockKind::Bullet => format!(
                "{}•  {}",
                "  ".repeat(b.indent as usize),
                strip(&b.text)
            ),
            _ => strip(&b.text),
        })
        .collect::<Vec<_>>()
        .join("\n")
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

fn syntect_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::ThemeSet) {
    use std::sync::OnceLock;
    static ASSETS: OnceLock<(syntect::parsing::SyntaxSet, syntect::highlighting::ThemeSet)> =
        OnceLock::new();
    ASSETS.get_or_init(|| {
        (
            syntect::parsing::SyntaxSet::load_defaults_newlines(),
            syntect::highlighting::ThemeSet::load_defaults(),
        )
    })
}

/// Syntax-highlight file content into per-line `(text, rgb)` segments.
pub fn highlight_file(path: &str, content: &str) -> Vec<Vec<(String, u32)>> {
    let (syntaxes, _) = syntect_assets();
    let syntax = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| syntaxes.find_syntax_by_extension(e))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    highlight_lines(syntax, content)
}

/// Syntax-highlight a fenced code block by its language tag ("rust",
/// "py", …). Unknown or empty tags fall back to plain text (default
/// foreground).
pub fn highlight_code(lang: &str, content: &str) -> Vec<Vec<(String, u32)>> {
    let (syntaxes, _) = syntect_assets();
    let syntax = syntaxes
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
    highlight_lines(syntax, content)
}

fn highlight_lines(
    syntax: &syntect::parsing::SyntaxReference,
    content: &str,
) -> Vec<Vec<(String, u32)>> {
    use syntect::easy::HighlightLines;
    use syntect::util::LinesWithEndings;

    let (syntaxes, themes) = syntect_assets();
    let theme = &themes.themes["base16-ocean.dark"];
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
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(rows.last().unwrap().kind, 5);
        assert_eq!(rows.last().unwrap().text, "Processing…");
        vm.thinking = true;
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(rows.last().unwrap().text, "Thinking…");
        vm.turn_running = false;
        vm.thinking = false;
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
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
        // Styled: a card header, then one row per markdown block.
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(rows[0].kind, 7);
        assert!(rows.len() > 2);
        // Raw: header plus a single kind-6 row of markdown source.
        let raw: HashSet<u64> = [3].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].kind, 6);
        assert_eq!(rows[1].text, "# heading\n\nbody `code`");
        // Collapsed: the header alone, with a one-line preview.
        let collapsed: HashSet<String> = ["a:0".to_string()].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &collapsed);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, 7);
        assert!(!rows[0].expanded);
        assert_eq!(rows[0].text, "# heading");
    }

    #[test]
    fn turn_summary_moves_to_the_last_assistant_header() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::Assistant {
                    turn: 1,
                    content: "part one".into(),
                    complete: true,
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "part **two**".into(),
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
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        // Completed turns no longer emit a status row; the summary rides in
        // the merged assistant card's header.
        assert!(!rows.iter().any(|r| r.kind == 3));
        let headers: Vec<_> = rows
            .iter()
            .filter(|r| r.kind == 7 && r.tool_name == "Agent")
            .collect();
        // Consecutive assistant items fold into one card.
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].meta, "0 in / 0 out tokens");
        // Styled view: the copy payload is the stripped display text of
        // the whole run.
        assert_eq!(headers[0].detail, "part one\npart two");
        // Raw view: the copy payload is the joined markdown source.
        let raw: HashSet<u64> = [1].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new());
        let header = rows
            .iter()
            .find(|r| r.kind == 7 && r.tool_name == "Agent")
            .unwrap();
        assert_eq!(header.detail, "part one\n\npart **two**");
    }

    #[test]
    fn tool_calls_nest_in_their_turns_assistant_card() {
        let tool = |id: &str| ChatItem::ToolCall {
            call_id: id.into(),
            tool: "search".into(),
            args: serde_json::json!({}),
            status: ToolCallStatus::Ok,
            result: None,
        };
        let mut vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 1,
                    content: "q".into(),
                },
                // The agent searched before writing any text.
                tool("t1"),
                tool("t2"),
                ChatItem::Assistant {
                    turn: 1,
                    content: "answer".into(),
                    complete: true,
                },
                tool("t3"),
            ],
            ..Default::default()
        };
        let (rows, ids) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        // Streaming turn: You header, prompt, Assistant header, the 2-tool
        // run under an expanded group header, text, trailing single tool —
        // every tool row inside the card outline.
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 2, 2, 1, 2]);
        assert_eq!(rows[3].text, "Called 2 tools");
        assert!(rows[3].expanded);
        assert!(rows[3..].iter().all(|r| r.card_pos >= 2));
        assert_eq!(rows.last().unwrap().card_pos, 3);
        assert_eq!(ids[4].as_deref(), Some("t1"));
        assert_eq!(ids[7].as_deref(), Some("t3"));
        // Once the turn completes, the run collapses by default.
        vm.items.push(ChatItem::TurnStatus {
            turn: 1,
            state: TurnState::Completed {
                usage: Default::default(),
            },
        });
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 1, 2]);
        assert!(!rows[3].expanded);
        // Toggling the group key (first tool's item index + the owning
        // card's anchor) reopens it.
        let opened: HashSet<String> = ["g1:3".to_string()].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &opened);
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 2, 2, 1, 2]);
        // A tool call with no assistant item (yet) still gets an Assistant
        // wrapper card, so the panel is present from the turn's first tool.
        let vm = ThreadViewModel {
            items: vec![tool("t9")],
            ..Default::default()
        };
        let (rows, ids) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 2]);
        assert_eq!(rows[0].tool_name, "Agent");
        assert_eq!(rows[1].card_pos, 3);
        assert_eq!(ids[1].as_deref(), Some("t9"));
    }

    #[test]
    fn thinking_nests_in_the_agent_card() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 1,
                    content: "q".into(),
                },
                // Thinking before any text (owned by the following
                // assistant item)…
                ChatItem::Thinking {
                    turn: 1,
                    content: "hmm, let me see".into(),
                    complete: true,
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "part one".into(),
                    complete: true,
                },
                // …and mid-response thinking between text segments.
                ChatItem::Thinking {
                    turn: 1,
                    content: "more thought".into(),
                    complete: true,
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "part two".into(),
                    complete: true,
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        // One You card, one Agent card; each thinking item renders as a
        // kind-4 header pill followed by its content as markdown rows
        // (tone 2), in stream order.
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 4, 1, 1, 4, 1, 1]);
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert_eq!(think[0].card_key, "t:1");
        assert!(think[0].expanded, "expanded by default");
        assert_eq!(think[0].meta, "hmm, let me see");
        assert_eq!(rows[4].tone, 2, "thinking content is tinted");
        assert_eq!(rows[5].tone, 0, "agent text is not");
        assert!(rows[3..].iter().all(|r| r.card_pos >= 2), "all nested");
        // Collapsing one thinking block keeps its header pill only.
        let collapsed: HashSet<String> = ["t:1".to_string()].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &collapsed);
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 4, 1, 4, 1, 1]);
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert!(!think[0].expanded);
        assert!(think[1].expanded);
        // Raw view keeps stream order too: each text stretch becomes a
        // kind-6 row in place, not one blob hoisted to the top.
        let raw: HashSet<u64> = [1].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new());
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 4, 1, 6, 4, 1, 6]);
        assert_eq!(rows[5].text, "part one");
        assert_eq!(rows[8].text, "part two");

        // Submitting the next prompt flips the default: earlier turns'
        // thinking collapses to its header pill (the reader moved on)…
        let mut vm = vm;
        vm.items.push(ChatItem::User {
            turn: 2,
            content: "next question".into(),
        });
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert!(think.iter().all(|r| !r.expanded), "collapsed once superseded");
        // …and the toggle set now re-expands instead of collapsing.
        let toggled: HashSet<String> = ["t:1".to_string()].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &toggled);
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert!(think[0].expanded);
        assert!(!think[1].expanded);
    }

    #[test]
    fn streaming_tools_before_any_text_get_the_assistant_wrapper() {
        let tool = |id: &str| ChatItem::ToolCall {
            call_id: id.into(),
            tool: "search".into(),
            args: serde_json::json!({}),
            status: ToolCallStatus::Running,
            result: None,
        };
        // Mid-turn: the agent has made three tool calls but streamed no
        // text yet, so no Assistant item exists.
        let mut vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 2,
                    content: "weather?".into(),
                },
                tool("t1"),
                tool("t2"),
                tool("t3"),
            ],
            turn_running: true,
            ..Default::default()
        };
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        // You card, then a synthesized Assistant card wrapping the grouped
        // run, then the activity row.
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 2, 2, 2, 5]);
        assert_eq!(rows[2].tool_name, "Agent");
        assert_eq!(rows[2].turn, 2);
        assert!(rows[3].expanded, "group stays open while streaming");
        assert!(rows[3..7].iter().all(|r| r.card_pos >= 2));
        // Once text arrives the real assistant item takes the tools over —
        // still exactly one Assistant card.
        vm.items.push(ChatItem::Assistant {
            turn: 2,
            content: "Sunny.".into(),
            complete: false,
        });
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        let headers = rows
            .iter()
            .filter(|r| r.kind == 7 && r.tool_name == "Agent")
            .count();
        assert_eq!(headers, 1);
    }

    #[test]
    fn user_prompts_after_the_first_get_a_rule_above() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 1,
                    content: "one".into(),
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "reply".into(),
                    complete: true,
                },
                ChatItem::User {
                    turn: 2,
                    content: "two".into(),
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_ne!(rows[0].kind, 8, "no rule before the first turn");
        let rules: Vec<_> = rows.iter().enumerate().filter(|(_, r)| r.kind == 8).collect();
        assert_eq!(rules.len(), 1);
        // The rule sits directly above the second prompt's header.
        assert_eq!(rows[rules[0].0 + 1].kind, 7);
        assert_eq!(rows[rules[0].0 + 1].tool_name, "You");
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

    #[test]
    fn read_tools_title_with_a_clickable_filename() {
        let args = serde_json::json!({"file_path": "/w/src/app/main.rs"});
        let row = tool_row("c1", "Read", &args, ToolCallStatus::Ok, &None, &HashSet::new());
        assert_eq!(row.tool_name, "Read");
        assert_eq!(row.text, "main.rs");
        assert_eq!(row.tool_file, "/w/src/app/main.rs");

        // Cursor / native variants use a "path" argument.
        let args = serde_json::json!({"path": "notes.md"});
        let row = tool_row("c2", "read_file", &args, ToolCallStatus::Ok, &None, &HashSet::new());
        assert_eq!(row.tool_name, "Read");
        assert_eq!((row.text.as_str(), row.tool_file.as_str()), ("notes.md", "notes.md"));

        // Non-read tools keep their plain label and no file link.
        let row = tool_row("c3", "search", &args, ToolCallStatus::Ok, &None, &HashSet::new());
        assert_eq!(row.tool_name, "search");
        assert!(row.tool_file.is_empty());
    }
}
