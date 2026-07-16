//! Pure render helpers: fold client-core view-model state into plain row
//! data. Plain data crosses the controller-thread → UI-thread boundary; the
//! UI thread maps it onto generated Slint structs.

use std::collections::{HashMap, HashSet};

use slint_markdown::{BlockKind, parse_blocks};
use trouve_client_core::viewmodel::{ChatItem, ThreadViewModel, ToolCallStatus, TurnState};
use trouve_protocol::QuestionAnswer;

/// Mirrors the `ChatRow` struct in `app.slint`.
/// Kinds: 0 user, 1 markdown block, 2 tool card, 3 turn status (failures),
/// 4 thinking sub-card (nested in the agent card like tool calls),
/// 5 activity (spinner + label), 6 raw response text,
/// 7 card header (collapsible group for user/agent items),
/// 8 horizontal rule between turns, 9 grouped tool-run header
/// ("Called n tools"), 10 question wizard (pending questions or the
/// answered summary).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatRowData {
    pub kind: i32,
    pub md_kind: i32,
    /// List nesting level for markdown list rows (0 = top level).
    pub md_indent: i32,
    /// Language tag for code-fence rows ("rust", "" when untagged).
    pub md_lang: String,
    /// Syntax-highlighted code-fence lines, computed on the controller
    /// thread so the Slint event loop only maps plain segment data.
    pub code_lines: Vec<Vec<(String, u32)>>,
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
    /// Read-style tool cards: the 1-based inclusive line range read
    /// (0 = whole file / unknown). The header shows it (via `meta`) and
    /// opening the file preselects it.
    pub tool_line_from: i32,
    pub tool_line_to: i32,
    /// Edit-style tool cards: added/removed line counts for the header
    /// badge, and the computed line diff shown as the expanded body.
    pub tool_adds: i32,
    pub tool_dels: i32,
    pub diff: Vec<DiffLine>,
    pub detail: String,
    pub expanded: bool,
    pub turn_state: i32,
    /// Turn number (status rows), so the UI can address per-turn actions.
    pub turn: i32,
    /// Assistant headers: this turn's response is showing as raw text.
    pub raw: bool,
    /// Assistant headers: token/cost summary once the turn completed.
    pub meta: String,
    /// Agent headers: the model that ran the turn, shown dimmed after the
    /// title ("(cursor/claude-fable-5)").
    pub subtitle: String,
    /// Header rows: stable key for the collapse toggle ("u:3", "a:5", …).
    pub card_key: String,
    /// Position within a collapsible card, for drawing one continuous
    /// outline across its rows: 0 not carded, 1 header (body follows),
    /// 2 body, 3 last body row, 4 standalone header (collapsed/empty).
    pub card_pos: i32,
    /// First body row of its card (slab rows pad down from the header).
    pub card_first: bool,
    /// Question wizard (kind 10): the current page's prompt, its options
    /// (label, selected), and page/nav state. `q_summary` carries the
    /// review page (and answered-summary) prompt/answer pairs.
    pub q_prompt: String,
    pub q_options: Vec<(String, bool)>,
    pub q_multi: bool,
    pub q_other: bool,
    pub q_other_text: String,
    pub q_review: bool,
    pub q_done: bool,
    pub q_summary: Vec<(String, String)>,
    pub q_can_back: bool,
    pub q_can_next: bool,
    pub q_last: bool,
}

/// UI-side state of one question wizard, keyed by request id in the
/// controller. `step == questions.len()` is the review page.
#[derive(Debug, Clone, Default)]
pub struct WizardState {
    pub step: usize,
    /// Selected option ids per question; [`OTHER_ID`] marks "Other".
    pub selections: Vec<Vec<String>>,
    /// Free-form "Other" text per question.
    pub other_texts: Vec<String>,
}

/// Synthetic option id for the wizard's trailing free-form choice.
pub const OTHER_ID: &str = "__other__";

impl WizardState {
    pub fn new(question_count: usize) -> Self {
        Self {
            step: 0,
            selections: vec![Vec::new(); question_count],
            other_texts: vec![String::new(); question_count],
        }
    }

    /// The submission payload, once every question has a selection.
    pub fn answers(&self, questions: &[trouve_protocol::Question]) -> Vec<QuestionAnswer> {
        questions
            .iter()
            .enumerate()
            .map(|(qi, q)| {
                let selected = &self.selections[qi];
                QuestionAnswer {
                    question_id: q.id.clone(),
                    selected_option_ids: selected
                        .iter()
                        .filter(|id| *id != OTHER_ID)
                        .cloned()
                        .collect(),
                    other_text: selected
                        .iter()
                        .any(|id| id == OTHER_ID)
                        .then(|| self.other_texts[qi].clone()),
                }
            })
            .collect()
    }
}

/// Wrap inline code spans (backtick runs, CommonMark-style: closed by a run
/// of the same length) in a font-color tag. StyledText renders code spans
/// monospace but offers no color knob, and monospace alone doesn't stand
/// out from prose.
fn tint_code_spans(md: &str) -> String {
    let open = format!(
        "<font color=\"#{:06x}\">",
        INLINE_CODE_TINT.load(std::sync::atomic::Ordering::Relaxed) & 0xff_ffff
    );
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
                out.push_str(&open);
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

/// Collapse whitespace to one line and cap the length for a card title.
fn title_arg(text: &str) -> String {
    let mut one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() > 60 {
        one_line.truncate(one_line.floor_boundary(59));
        one_line.push('…');
    }
    one_line
}

/// Human display name for a raw tool identifier: known tools map to their
/// product name (`search` → `Code Search`), camelCase and snake_case split
/// into capitalized words (`WebSearch` → `Web Search`, `find_related` →
/// `Find Related`), and foreign MCP tools show their server
/// (`mcp__jira__create_issue` → `jira: Create Issue`).
fn tool_display_name(tool: &str) -> String {
    if let Some((server, name)) = tool
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
    {
        if server == "trouve" {
            return tool_display_name(name);
        }
        return format!("{server}: {}", tool_display_name(name));
    }
    match tool {
        "search" => "Code Search".into(),
        "find_related" => "Find Related".into(),
        // Cursor's ACP kind for shell commands.
        "execute" => "Shell".into(),
        _ => {
            // snake_case → words; camelCase → split before upper runs.
            let mut words: Vec<String> = Vec::new();
            for part in tool.split('_') {
                let mut word = String::new();
                for c in part.chars() {
                    if c.is_uppercase() && !word.is_empty() {
                        words.push(word);
                        word = String::new();
                    }
                    word.push(c);
                }
                if !word.is_empty() {
                    words.push(word);
                }
            }
            words
                .iter()
                .map(|w| {
                    let mut cs = w.chars();
                    match cs.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + cs.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }
}

/// Card title for a tool call. Shell-style tools (native `shell`, vendor
/// `Bash`) show the command they run — `Bash (wc -l foo.rs)` — and querying
/// tools show their query — `Code Search markdown renderer` — since the
/// tool name alone says nothing about what happened.
fn tool_label(tool: &str, args: &serde_json::Value) -> String {
    // "execute" is cursor's ACP kind for shell commands.
    let command = matches!(tool, "shell" | "Bash" | "bash" | "execute")
        .then(|| args.get("command").and_then(|v| v.as_str()))
        .flatten();
    if let Some(cmd) = command {
        return format!("{} ({})", tool_display_name(tool), title_arg(cmd));
    }
    let display = tool_display_name(tool);
    // "title" is the human label ACP tool calls carry (e.g. "`ls`"); when
    // it just repeats the tool name (cursor's createPlan), skip it.
    let query = ["query", "pattern", "url", "path", "title"]
        .iter()
        .find_map(|k| args.get(k).and_then(|v| v.as_str()))
        .filter(|q| !q.trim().is_empty() && *q != display);
    match query {
        Some(q) => format!("{display} {}", title_arg(q)),
        None => display,
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
    wizards: &HashMap<String, WizardState>,
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
                ChatItem::ToolCall { .. }
                | ChatItem::Thinking { .. }
                | ChatItem::Questions { .. } => {
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
                ChatItem::ToolCall { .. }
                | ChatItem::Thinking { .. }
                | ChatItem::Questions { .. } => {
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
            ChatItem::User {
                content,
                attachments,
                ..
            } => {
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
                    if !attachments.is_empty() {
                        body.push((
                            ChatRowData {
                                kind: 1,
                                tone: 1,
                                text: attachment_line(attachments),
                                ..Default::default()
                            },
                            None,
                        ));
                    }
                }
                let header = ChatRowData {
                    tool_name: "You".into(),
                    text: if content.trim().is_empty() {
                        attachment_line(attachments)
                    } else {
                        preview(content)
                    },
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
                        ChatItem::ToolCall { .. }
                        | ChatItem::Thinking { .. }
                        | ChatItem::Questions { .. } => end = k,
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
                    // Everything in stream order: tool calls / thinking
                    // issued before any text streamed (the lead), then the
                    // run itself.
                    let mut ordered: Vec<usize> = owner
                        .iter()
                        .filter(|&(&j, &a)| a == i && j < i)
                        .map(|(&j, _)| j)
                        .collect();
                    ordered.sort_unstable();
                    ordered.extend(i..=end);
                    card_body_rows(
                        &mut body, vm, &ordered, i, raw, done, collapsed, expanded, wizards,
                    );
                }
                // The turn's token/cost summary shows in the header of the
                // turn's last card (where the status row used to sit).
                let last_of_turn = !vm.items[end + 1..]
                    .iter()
                    .any(|it| matches!(it, ChatItem::Assistant { turn: t, .. } if t == turn));
                let meta = match (last_of_turn, turn_state(vm, *turn)) {
                    (true, Some(TurnState::Completed { usage })) => turn_meta(vm, *turn, usage),
                    _ => String::new(),
                };
                let header = ChatRowData {
                    tool_name: "Agent".into(),
                    subtitle: turn_model_label(vm, *turn),
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
            ChatItem::ToolCall { .. } | ChatItem::Thinking { .. } | ChatItem::Questions { .. } => {
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
                        ChatItem::ToolCall { .. }
                            | ChatItem::Thinking { .. }
                            | ChatItem::Questions { .. }
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
                let raw = raw_turns.contains(&turn);
                let mut body = Vec::new();
                if open {
                    card_body_rows(
                        &mut body, vm, &run, i, raw, done, collapsed, expanded, wizards,
                    );
                }
                // Orphan items mean no assistant item in this turn, so this
                // card is where the turn summary lands once it completes.
                let meta = match turn_state(vm, turn) {
                    Some(TurnState::Completed { usage }) => turn_meta(vm, turn, usage),
                    _ => String::new(),
                };
                let header = ChatRowData {
                    tool_name: "Agent".into(),
                    subtitle: turn_model_label(vm, turn),
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
        let mut activity = ChatRowData {
            kind: 5,
            text: label.into(),
            ..Default::default()
        };
        // Nest the activity row at the bottom of the Agent card being
        // streamed into, when one is open at the tail of the chat — it then
        // reads as "this card is still being populated". With no open card
        // for the *running* turn (turn just started and nothing has
        // streamed, or the card was collapsed) it stands alone; the check
        // on the card's turn keeps it out of the previous turn's card
        // while a slow model spins up.
        let running_turn = vm.items.iter().rev().find_map(|it| match it {
            ChatItem::TurnStatus {
                turn,
                state: TurnState::Running,
            } => Some(*turn as i32),
            _ => None,
        });
        let agent_card_open =
            rows.iter().rev().find(|r| r.kind == 7).is_some_and(|h| {
                h.tool_name == "Agent" && h.expanded && running_turn == Some(h.turn)
            });
        if agent_card_open {
            match rows.last_mut() {
                // Take over as the card's last body row.
                Some(last) if last.card_pos == 3 => {
                    last.card_pos = 2;
                    activity.card_pos = 3;
                    activity.tool_name = "Agent".into();
                }
                // Header-only card: become its first (and only) body row.
                Some(last) if last.card_pos == 4 && last.kind == 7 => {
                    last.card_pos = 1;
                    activity.card_pos = 3;
                    activity.card_first = true;
                    activity.tool_name = "Agent".into();
                }
                _ => {}
            }
        }
        rows.push(activity);
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

/// One stream-ordered piece of a card body: a stretch of assistant text or
/// a single tool-call / thinking item.
enum Segment {
    Text(String),
    Item(usize),
}

/// Append a card body in stream order. Text stretches — the agent's
/// narration and answer — always render at the card's top level. Runs of
/// 2+ consecutive working items (tool calls, thinking) between them fold
/// under one summarized group header ("Edited 2 files, read 3 files,
/// called 1 tool"), expanded while the turn streams so progress is
/// visible, collapsed by default once it's done (the group key in
/// `collapsed` flips whichever default applies).
#[allow(clippy::too_many_arguments)]
fn card_body_rows(
    body: &mut Vec<(ChatRowData, Option<String>)>,
    vm: &ThreadViewModel,
    ordered: &[usize],
    anchor: usize,
    raw: bool,
    done: bool,
    collapsed: &HashSet<String>,
    expanded: &HashSet<String>,
    wizards: &HashMap<String, WizardState>,
) {
    let mut segments: Vec<Segment> = Vec::new();
    let mut k = 0;
    while k < ordered.len() {
        if matches!(vm.items[ordered[k]], ChatItem::Assistant { .. }) {
            let start = k;
            while k < ordered.len() && matches!(vm.items[ordered[k]], ChatItem::Assistant { .. }) {
                k += 1;
            }
            let stretch = ordered[start..k]
                .iter()
                .filter_map(|&j| match &vm.items[j] {
                    ChatItem::Assistant { content, .. } if !content.is_empty() => {
                        Some(content.as_str())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            if !stretch.is_empty() {
                segments.push(Segment::Text(stretch));
            }
        } else {
            segments.push(Segment::Item(ordered[k]));
            k += 1;
        }
    }

    // Question items stay out of activity groups: the wizard needs to be
    // answered, so it always renders at the card's top level, like text.
    let groupable = |seg: &Segment| matches!(seg, Segment::Item(j) if !matches!(vm.items[*j], ChatItem::Questions { .. }));
    let mut s = 0;
    while s < segments.len() {
        if let Segment::Text(text) = &segments[s] {
            text_rows(body, text, raw);
            s += 1;
            continue;
        }
        if !groupable(&segments[s]) {
            segment_rows(body, vm, &segments[s], 0, raw, collapsed, expanded, wizards);
            s += 1;
            continue;
        }
        // A run of consecutive working items.
        let start = s;
        while s < segments.len() && groupable(&segments[s]) {
            s += 1;
        }
        let run = &segments[start..s];
        if run.len() < 2 {
            for seg in run {
                segment_rows(body, vm, seg, 0, raw, collapsed, expanded, wizards);
            }
            continue;
        }
        // A pending approval must be visible to be answered: it holds the
        // group open regardless of the collapse toggle.
        let needs_approval = run.iter().any(|seg| {
            matches!(seg, Segment::Item(j) if matches!(
                &vm.items[*j],
                ChatItem::ToolCall { status: ToolCallStatus::AwaitingApproval, .. }
            ))
        });
        // Group keys stay stable across renders: the run's first item
        // index plus the owning card's anchor.
        let first = match run[0] {
            Segment::Item(j) => j,
            Segment::Text(_) => unreachable!("runs hold only items"),
        };
        let gkey = format!("g{first}:{anchor}");
        let toggled = collapsed.contains(&gkey);
        let g_open = needs_approval || if done { toggled } else { !toggled };
        body.push((
            ChatRowData {
                kind: 9,
                text: activity_summary(vm, run),
                expanded: g_open,
                card_key: gkey,
                ..Default::default()
            },
            None,
        ));
        if g_open {
            for seg in run {
                segment_rows(body, vm, seg, 1, raw, collapsed, expanded, wizards);
            }
        }
    }
}

/// Append the rows of one body segment. `indent` nests tool cards and
/// thinking pills one level under a group header.
#[allow(clippy::too_many_arguments)]
fn segment_rows(
    body: &mut Vec<(ChatRowData, Option<String>)>,
    vm: &ThreadViewModel,
    segment: &Segment,
    indent: i32,
    raw: bool,
    collapsed: &HashSet<String>,
    expanded: &HashSet<String>,
    wizards: &HashMap<String, WizardState>,
) {
    match segment {
        Segment::Text(text) => text_rows(body, text, raw),
        Segment::Item(j) => match &vm.items[*j] {
            ChatItem::Thinking {
                turn,
                content,
                complete,
            } => {
                let key = format!("t:{j}");
                // The toggle set flips whichever default applies: expanded
                // while its turn is the latest, collapsed once the next
                // prompt is submitted (the reader has moved on).
                let toggled = collapsed.contains(&key);
                let open = if *turn < latest_turn(vm) {
                    toggled
                } else {
                    !toggled
                };
                // Header pill; the content follows as ordinary markdown
                // rows (tone 2), indented one level under it.
                body.push((
                    ChatRowData {
                        kind: 4,
                        text: if *complete { "Thought" } else { "Thinking" }.into(),
                        detail: content.clone(),
                        // Header teaser for the collapsed state.
                        meta: preview(content),
                        expanded: open,
                        card_key: key,
                        md_indent: indent,
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
            }
            ChatItem::ToolCall {
                call_id,
                tool,
                args,
                status,
                result,
            } => {
                let mut row = tool_row(call_id, tool, args, *status, result, expanded);
                row.md_indent = indent;
                body.push((row, Some(call_id.clone())));
            }
            ChatItem::Questions {
                request_id,
                title,
                questions,
                answers,
            } => {
                let mut row = question_row(title, questions, answers, wizards.get(request_id));
                row.md_indent = indent;
                body.push((row, Some(request_id.clone())));
            }
            _ => {}
        },
    }
}

/// Build the kind-10 question row: the wizard while answers are pending
/// (current question page or the review page), or a compact prompt/answer
/// summary once resolved.
fn question_row(
    title: &Option<String>,
    questions: &[trouve_protocol::Question],
    answers: &Option<Option<Vec<QuestionAnswer>>>,
    wizard: Option<&WizardState>,
) -> ChatRowData {
    let heading = title.clone().unwrap_or_else(|| "Questions".into());
    let mut row = ChatRowData {
        kind: 10,
        text: heading,
        ..Default::default()
    };
    // The label an answer shows for one selected option id.
    let option_label = |q: &trouve_protocol::Question, id: &str| {
        q.options
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.label.clone())
            .unwrap_or_else(|| id.to_string())
    };
    if let Some(resolved) = answers {
        row.q_done = true;
        row.q_review = true;
        match resolved {
            Some(list) => {
                row.meta = "Answered".into();
                row.q_summary = questions
                    .iter()
                    .map(|q| {
                        let mut parts: Vec<String> = list
                            .iter()
                            .find(|a| a.question_id == q.id)
                            .map(|a| {
                                let mut p: Vec<String> = a
                                    .selected_option_ids
                                    .iter()
                                    .map(|id| option_label(q, id))
                                    .collect();
                                if let Some(other) = &a.other_text {
                                    p.push(if other.trim().is_empty() {
                                        "Other".into()
                                    } else {
                                        format!("Other: {other}")
                                    });
                                }
                                p
                            })
                            .unwrap_or_default();
                        if parts.is_empty() {
                            parts.push("—".into());
                        }
                        (q.prompt.clone(), parts.join(", "))
                    })
                    .collect();
            }
            None => row.meta = "Skipped".into(),
        }
        return row;
    }
    // Pending: wizard state drives the page. A missing state (first render)
    // acts like a fresh wizard on question 1.
    let fresh = WizardState::new(questions.len());
    let w = wizard.unwrap_or(&fresh);
    let step = w.step.min(questions.len());
    if step == questions.len() {
        // Review page.
        row.q_review = true;
        row.meta = "Review your answers".into();
        row.q_can_back = true;
        row.q_can_next = true;
        row.q_last = true;
        row.q_summary = questions
            .iter()
            .enumerate()
            .map(|(qi, q)| {
                let parts: Vec<String> = w.selections[qi]
                    .iter()
                    .map(|id| {
                        if id == OTHER_ID {
                            let t = w.other_texts[qi].trim();
                            if t.is_empty() {
                                "Other".into()
                            } else {
                                format!("Other: {t}")
                            }
                        } else {
                            option_label(q, id)
                        }
                    })
                    .collect();
                (q.prompt.clone(), parts.join(", "))
            })
            .collect();
        return row;
    }
    let q = &questions[step];
    let selected = &w.selections[step];
    row.meta = format!("Question {} of {}", step + 1, questions.len());
    row.q_prompt = q.prompt.clone();
    row.q_multi = q.allow_multiple;
    row.q_options = q
        .options
        .iter()
        .map(|o| (o.label.clone(), selected.contains(&o.id)))
        .collect();
    row.q_other = selected.iter().any(|id| id == OTHER_ID);
    row.q_other_text = w.other_texts[step].clone();
    row.q_can_back = step > 0;
    row.q_can_next =
        !selected.is_empty() && (!row.q_other || !w.other_texts[step].trim().is_empty());
    row.q_last = step + 1 == questions.len();
    row
}

/// Append one text stretch: markdown-block rows, or — in raw view — one
/// selectable plain-text row (StyledText offers no selection, this is the
/// escape hatch).
fn text_rows(body: &mut Vec<(ChatRowData, Option<String>)>, text: &str, raw: bool) {
    if raw {
        body.push((
            ChatRowData {
                kind: 6,
                text: text.to_string(),
                ..Default::default()
            },
            None,
        ));
    } else {
        push_blocks(body, text);
    }
}

/// Group-header summary of a card's working activity: file edits and reads
/// count distinct paths, shell-style tools count commands, everything else
/// counts as a generic tool call, plus thinking blocks — e.g.
/// "Edited 2 files, read 3 files, ran 1 command, thought 2 times".
fn activity_summary(vm: &ThreadViewModel, segments: &[Segment]) -> String {
    let mut edited: HashSet<&str> = HashSet::new();
    let mut edits_unpathed = 0usize;
    let mut read: HashSet<&str> = HashSet::new();
    let mut reads_unpathed = 0usize;
    let mut commands = 0usize;
    let mut tools = 0usize;
    let mut thoughts = 0usize;
    for seg in segments {
        let Segment::Item(j) = seg else { continue };
        match &vm.items[*j] {
            ChatItem::Thinking { .. } => thoughts += 1,
            ChatItem::ToolCall { tool, args, .. } => {
                // MCP-mangled names classify by their base name.
                let base = tool.rsplit("__").next().unwrap_or(tool);
                let path = args
                    .get("file_path")
                    .or_else(|| args.get("path"))
                    .and_then(serde_json::Value::as_str);
                match base {
                    "edit" | "Edit" | "MultiEdit" | "NotebookEdit" | "Write" | "write"
                    | "edit_file" | "write_file" | "create_file" | "apply_patch" | "delete"
                    | "delete_file" => match path {
                        Some(p) => {
                            edited.insert(p);
                        }
                        None => edits_unpathed += 1,
                    },
                    "read" | "Read" | "read_file" => match path {
                        Some(p) => {
                            read.insert(p);
                        }
                        None => reads_unpathed += 1,
                    },
                    "shell" | "bash" | "Bash" | "execute" => commands += 1,
                    _ => tools += 1,
                }
            }
            _ => {}
        }
    }
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts: Vec<String> = Vec::new();
    let edits = edited.len() + edits_unpathed;
    if edits > 0 {
        parts.push(format!("edited {}", plural(edits, "file", "files")));
    }
    let reads = read.len() + reads_unpathed;
    if reads > 0 {
        parts.push(format!("read {}", plural(reads, "file", "files")));
    }
    if commands > 0 {
        parts.push(format!("ran {}", plural(commands, "command", "commands")));
    }
    if tools > 0 {
        parts.push(format!("called {}", plural(tools, "tool", "tools")));
    }
    if thoughts > 0 {
        parts.push(format!("thought {}", plural(thoughts, "time", "times")));
    }
    let mut summary = parts.join(", ");
    // Sentence-case the first part.
    if let Some(first) = summary.get(..1) {
        let upper = first.to_uppercase();
        summary.replace_range(..1, &upper);
    }
    summary
}

/// Append one assistant text segment as markdown-block rows. Each block is
/// an individual virtualized row, so a long streaming answer never
/// re-lays-out the whole chat.
fn push_blocks(body: &mut Vec<(ChatRowData, Option<String>)>, content: &str) {
    for block in parse_blocks(content) {
        let code_lines = if block.kind == BlockKind::Code {
            highlight_code(&block.language, &block.text)
        } else {
            Vec::new()
        };
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
                code_lines,
                text: block.text,
                styled_md,
                ..Default::default()
            },
            None,
        ));
    }
}

/// One display line of an edit diff. Kinds follow `DiffRow`: 1 separator,
/// 2 context, 3 add, 4 delete. Line numbers are 1-based file positions
/// (0 = unknown, rendered as a blank gutter cell).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiffLine {
    pub kind: i32,
    pub old_no: i32,
    pub new_no: i32,
    pub text: String,
}

impl DiffLine {
    fn new(kind: i32, old_no: i32, new_no: i32, text: impl Into<String>) -> Self {
        Self {
            kind,
            old_no,
            new_no,
            text: text.into(),
        }
    }
}

/// A file-edit tool call folded for display: header verb + path and the
/// body's line diff.
struct EditView {
    verb: &'static str,
    path: String,
    lines: Vec<DiffLine>,
}

/// Recognize file-edit tools across backends and extract a line diff.
/// Claude sends Edit/MultiEdit/Write with old/new strings; cursor's ACP
/// edit kind carries similar raw input under varying key names; anything
/// shipping a unified-diff/patch string renders that directly.
fn edit_view(tool: &str, args: &serde_json::Value) -> Option<EditView> {
    let str_arg = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).and_then(serde_json::Value::as_str))
    };
    let base = tool.rsplit("__").next().unwrap_or(tool);
    let verb = match base {
        "edit" | "Edit" | "MultiEdit" | "NotebookEdit" | "edit_file" | "apply_patch"
        | "fileChange" => "Edit",
        "write" | "Write" | "write_file" | "create_file" => "Write",
        _ => return None,
    };
    let path = str_arg(&["file_path", "path", "abs_path", "target_file", "filePath"])
        .unwrap_or_default()
        .to_string();

    // A ready-made unified diff / patch wins: render its lines as-is.
    if let Some(patch) = str_arg(&["diff", "patch", "unified_diff", "unifiedDiff", "input"]) {
        let lines = patch_lines(patch);
        if !lines.is_empty() {
            return Some(EditView { verb, path, lines });
        }
    }

    // (old, new, 1-based start line — the engine's "_line" hint, 0 unknown).
    let old_new = |v: &serde_json::Value| {
        let get = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
                .map(str::to_string)
        };
        let old = get(&["old_string", "oldText", "old_text", "old_str"]);
        let new = get(&["new_string", "newText", "new_text", "new_str"]);
        let start = v
            .get("_line")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32;
        match (old, new) {
            (None, None) => None,
            (old, new) => Some((old.unwrap_or_default(), new.unwrap_or_default(), start)),
        }
    };
    // MultiEdit: several old/new pairs against one file, separated below.
    let pairs: Vec<(String, String, i32)> = match args.get("edits").and_then(|v| v.as_array()) {
        Some(edits) => edits.iter().filter_map(old_new).collect(),
        None => old_new(args)
            .or_else(|| {
                // Write-style: the whole new content, no old text; a fresh
                // file always numbers from 1.
                str_arg(&["content", "contents", "file_text", "fileText"])
                    .map(|c| (String::new(), c.to_string(), 1))
            })
            .into_iter()
            .collect(),
    };
    if pairs.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for (i, (old, new, start)) in pairs.iter().enumerate() {
        if i > 0 {
            lines.push(DiffLine::new(1, 0, 0, "···"));
        }
        lines.extend(line_diff(old, new, *start));
    }
    Some(EditView { verb, path, lines })
}

/// Diff two text snippets line-by-line (LCS): unchanged lines are context,
/// removals then insertions inside changed blocks. Oversized inputs skip
/// the LCS and show plain delete-all/add-all. `start` is the 1-based file
/// line both sides begin at (0 = unknown: gutters stay blank).
fn line_diff(old: &str, new: &str, start: i32) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Line counters tick only when the position is known (start > 0).
    struct Nums {
        old_no: i32,
        new_no: i32,
        tick: i32,
    }
    impl Nums {
        fn del(&mut self, out: &mut Vec<DiffLine>, l: &str) {
            out.push(DiffLine::new(4, self.old_no, 0, l));
            self.old_no += self.tick;
        }
        fn add(&mut self, out: &mut Vec<DiffLine>, l: &str) {
            out.push(DiffLine::new(3, 0, self.new_no, l));
            self.new_no += self.tick;
        }
        fn ctx(&mut self, out: &mut Vec<DiffLine>, l: &str) {
            out.push(DiffLine::new(2, self.old_no, self.new_no, l));
            self.old_no += self.tick;
            self.new_no += self.tick;
        }
    }
    let mut n = Nums {
        old_no: start,
        new_no: start,
        tick: (start > 0) as i32,
    };
    let mut out = Vec::new();
    if a.len() * b.len() > 1_000_000 {
        a.iter().for_each(|l| n.del(&mut out, l));
        b.iter().for_each(|l| n.add(&mut out, l));
        return out;
    }
    // dp[i][j] = LCS length of a[i..] and b[j..].
    let mut dp = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            n.ctx(&mut out, a[i]);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            n.del(&mut out, a[i]);
            i += 1;
        } else {
            n.add(&mut out, b[j]);
            j += 1;
        }
    }
    a[i..].iter().for_each(|l| n.del(&mut out, l));
    b[j..].iter().for_each(|l| n.add(&mut out, l));
    out
}

/// Map unified-diff/patch text to display lines: +/- prefixes become
/// add/delete rows, hunk markers become separators, file metadata is
/// dropped. `@@ -a,b +c,d @@` headers seed the gutter line numbers.
/// Returns empty when nothing looks like diff content.
fn patch_lines(patch: &str) -> Vec<DiffLine> {
    let mut out = Vec::new();
    let mut saw_change = false;
    let (mut old_no, mut new_no) = (0i32, 0i32);
    for line in patch.lines() {
        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff --git")
            || line.starts_with("index ")
        {
            continue;
        }
        if line.starts_with("@@") || line.starts_with("*** ") {
            (old_no, new_no) = hunk_starts(line).unwrap_or((0, 0));
            out.push(DiffLine::new(1, 0, 0, line));
        } else if let Some(rest) = line.strip_prefix('+') {
            out.push(DiffLine::new(3, 0, new_no, rest));
            new_no += (new_no > 0) as i32;
            saw_change = true;
        } else if let Some(rest) = line.strip_prefix('-') {
            out.push(DiffLine::new(4, old_no, 0, rest));
            old_no += (old_no > 0) as i32;
            saw_change = true;
        } else {
            let text = line.strip_prefix(' ').unwrap_or(line);
            out.push(DiffLine::new(2, old_no, new_no, text));
            old_no += (old_no > 0) as i32;
            new_no += (new_no > 0) as i32;
        }
    }
    if saw_change { out } else { Vec::new() }
}

/// Parse the old/new start lines out of a `@@ -a,b +c,d @@` hunk header.
fn hunk_starts(line: &str) -> Option<(i32, i32)> {
    let rest = line.strip_prefix("@@")?.trim_start();
    let num = |s: &str, sigil: char| -> Option<i32> {
        s.strip_prefix(sigil)?
            .split([',', ' '])
            .next()?
            .parse()
            .ok()
    };
    let mut parts = rest.split_whitespace();
    let old = num(parts.next()?, '-')?;
    let new = num(parts.next()?, '+')?;
    (old > 0 && new > 0).then_some((old, new))
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
    let mut detail = String::new();
    humanize_json(args, 0, &mut detail);
    if let Some(result) = result {
        if !detail.is_empty() {
            detail.push('\n');
        }
        detail.push_str("── result ──\n");
        match text_blocks(result) {
            Some(text) => detail.push_str(&text),
            None => humanize_json(result, 0, &mut detail),
        }
    }
    let mut detail = detail.trim_end().to_string();
    if detail.len() > 4000 {
        detail.truncate(detail.floor_boundary(4000));
        detail.push('…');
    }
    // The agent's task list titles as "Todos (done/total)" with the current
    // item in the header and the full checklist as the expandable detail.
    // The result holds the full merged list; the args may be a partial
    // merge update, so they're only the fallback.
    let base = tool.rsplit("__").next().unwrap_or(tool);
    if matches!(base, "todo_write" | "TodoWrite") {
        let todos = result
            .as_ref()
            .and_then(|r| r.get("todos"))
            .and_then(serde_json::Value::as_array)
            .or_else(|| args.get("todos").and_then(serde_json::Value::as_array));
        if let Some(todos) = todos.filter(|t| !t.is_empty()) {
            let field = |t: &serde_json::Value, k: &str| {
                t.get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let glyph = |s: &str| match s {
                "completed" => "✓",
                "in_progress" => "▸",
                "cancelled" => "✕",
                _ => "○",
            };
            let checklist = todos
                .iter()
                .map(|t| format!("{} {}", glyph(&field(t, "status")), field(t, "content")))
                .collect::<Vec<_>>()
                .join("\n");
            let done = todos
                .iter()
                .filter(|t| matches!(field(t, "status").as_str(), "completed" | "cancelled"))
                .count();
            let current = todos
                .iter()
                .find(|t| field(t, "status") == "in_progress")
                .map(|t| field(t, "content"))
                .filter(|c| !c.is_empty());
            return ChatRowData {
                kind: 2,
                tool_name: "Todos".into(),
                text: current.unwrap_or_else(|| format!("{done}/{} done", todos.len())),
                meta: format!("{done}/{}", todos.len()),
                tool_status: tool_status(status),
                detail: checklist,
                expanded: expanded.contains(call_id),
                ..Default::default()
            };
        }
    }
    // Edit-style tools title as "Edit <filename>" with a clickable
    // filename, +/− counts in the header, and the line diff as the body.
    if let Some(edit) = edit_view(tool, args) {
        // Cap the rendered diff: one chat row hosts all these lines
        // un-virtualized, so a full-file rewrite would balloon the row.
        let mut lines = edit.lines;
        let adds = lines.iter().filter(|l| l.kind == 3).count() as i32;
        let dels = lines.iter().filter(|l| l.kind == 4).count() as i32;
        if lines.len() > 300 {
            let more = lines.len() - 300;
            lines.truncate(300);
            lines.push(DiffLine::new(1, 0, 0, format!("… {more} more lines")));
        }
        return ChatRowData {
            kind: 2,
            tool_name: edit.verb.into(),
            text: edit.path.rsplit('/').next().unwrap_or_default().to_string(),
            tool_file: edit.path,
            tool_adds: adds,
            tool_dels: dels,
            diff: lines,
            tool_status: tool_status(status),
            detail,
            expanded: expanded.contains(call_id),
            ..Default::default()
        };
    }
    // Read-style tools (native read_file, Claude Read, cursor read) title
    // as "Read <filename> L123-456", with the filename clickable in the UI.
    let file = matches!(tool, "Read" | "read" | "read_file")
        .then(|| {
            args.get("file_path")
                .or_else(|| args.get("path"))
                .and_then(serde_json::Value::as_str)
        })
        .flatten()
        .unwrap_or_default();
    let (from, to) = read_range(args);
    ChatRowData {
        kind: 2,
        tool_name: if file.is_empty() {
            tool_label(tool, args)
        } else {
            "Read".into()
        },
        text: file.rsplit('/').next().unwrap_or_default().to_string(),
        tool_file: file.to_string(),
        tool_line_from: if file.is_empty() { 0 } else { from },
        tool_line_to: if file.is_empty() { 0 } else { to },
        meta: match (file.is_empty(), from, to) {
            (true, ..) | (_, 0, _) => String::new(),
            (_, f, t) if t > f => format!("L{f}-{t}"),
            (_, f, _) => format!("L{f}"),
        },
        tool_status: tool_status(status),
        detail,
        expanded: expanded.contains(call_id),
        ..Default::default()
    }
}

/// The 1-based inclusive line range a read-style call covered, from its
/// offset/limit or start/end arguments; (0, 0) when it read the whole
/// file (or the args carry no range).
fn read_range(args: &serde_json::Value) -> (i32, i32) {
    let int_arg = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).and_then(serde_json::Value::as_i64))
            .filter(|n| *n > 0)
            .map(|n| n as i32)
    };
    let start = int_arg(&["offset", "start_line", "startLine", "start"]);
    let end = int_arg(&["end_line", "endLine", "end"]);
    let limit = int_arg(&["limit"]);
    match (start, end, limit) {
        (Some(s), Some(e), _) => (s, e.max(s)),
        (Some(s), None, Some(l)) => (s, s + l - 1),
        (Some(s), None, None) => (s, s),
        (None, Some(e), _) => (1, e),
        (None, None, Some(l)) => (1, l),
        (None, None, None) => (0, 0),
    }
}

/// Render a JSON value as indented `key: value` text — the detail panels
/// show tool args/results to humans, so no quoting or brace noise. Multiline
/// strings become indented blocks; null and empty values are dropped.
fn humanize_json(v: &serde_json::Value, indent: usize, out: &mut String) {
    use serde_json::Value;
    let pad = "  ".repeat(indent);
    let noise = |v: &Value| match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    match v {
        Value::Object(map) => {
            for (key, val) in map {
                if noise(val) {
                    continue;
                }
                match val {
                    Value::String(s) if s.contains('\n') => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        for line in s.lines() {
                            out.push_str(&format!("{pad}  {line}\n"));
                        }
                    }
                    Value::String(s) => out.push_str(&format!("{pad}{key}: {s}\n")),
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        humanize_json(val, indent + 1, out);
                    }
                    other => out.push_str(&format!("{pad}{key}: {other}\n")),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        out.push_str(&format!("{pad}-\n"));
                        humanize_json(item, indent + 1, out);
                    }
                    Value::String(s) => out.push_str(&format!("{pad}- {s}\n")),
                    other => out.push_str(&format!("{pad}- {other}\n")),
                }
            }
        }
        Value::String(s) => {
            for line in s.lines() {
                out.push_str(&format!("{pad}{line}\n"));
            }
        }
        other => out.push_str(&format!("{pad}{other}\n")),
    }
}

/// Vendor results often wrap plain text in content blocks — a bare string,
/// an array of `{type: "text", text}` blocks, or an object with a `content`
/// array (Claude / MCP shapes). Unwrap those to the text itself.
fn text_blocks(v: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    if let Value::String(s) = v {
        return Some(s.clone());
    }
    let arr = match v {
        Value::Array(a) => a,
        Value::Object(o) if o.len() == 1 => o.get("content")?.as_array()?,
        _ => return None,
    };
    let texts: Vec<&str> = arr
        .iter()
        .map(|b| {
            (b.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| b.get("text").and_then(Value::as_str))
                .flatten()
                .ok_or(())
        })
        .collect::<Result<_, _>>()
        .ok()?;
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// "📎 screenshot.png (34 KB) · log.txt (2 KB)" — shown under the prompt
/// text (and as the header teaser for attachment-only prompts).
fn attachment_line(attachments: &[trouve_protocol::Attachment]) -> String {
    let list = attachments
        .iter()
        .map(|a| {
            let size = if a.size_bytes < 1024 {
                format!("{} B", a.size_bytes)
            } else if a.size_bytes < 1024 * 1024 {
                format!("{} KB", a.size_bytes / 1024)
            } else {
                format!("{:.1} MB", a.size_bytes as f64 / (1024.0 * 1024.0))
            };
            format!("{} ({size})", a.name)
        })
        .collect::<Vec<_>>()
        .join(" · ");
    format!("📎 {list}")
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
/// The agent header's dimmed model tag: "(cursor/claude-fable-5)", empty
/// when the turn's model isn't known (old threads predating the event
/// field).
fn turn_model_label(vm: &ThreadViewModel, turn: u64) -> String {
    vm.turn_models
        .get(&turn)
        .filter(|m| !m.is_empty())
        .map(|m| format!("({m})"))
        .unwrap_or_default()
}

fn latest_turn(vm: &ThreadViewModel) -> u64 {
    vm.items
        .iter()
        .filter_map(|item| match item {
            ChatItem::User { turn, .. }
            | ChatItem::Assistant { turn, .. }
            | ChatItem::Thinking { turn, .. }
            | ChatItem::TurnStatus { turn, .. } => Some(*turn),
            ChatItem::ToolCall { .. } | ChatItem::Questions { .. } => None,
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
            BlockKind::Bullet => format!("{}•  {}", "  ".repeat(b.indent as usize), strip(&b.text)),
            _ => strip(&b.text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Full turn-header meta: the token/cost summary plus how long the turn
/// took, when the viewmodel has both its start and end timestamps.
fn turn_meta(vm: &ThreadViewModel, turn: u64, usage: &trouve_protocol::Usage) -> String {
    let mut s = turn_summary(usage);
    if let Some(ms) = vm.turn_duration_ms.get(&turn) {
        s.push_str(&format!(" · {}", human_duration(*ms)));
    }
    s
}

/// "850ms", "12s", "1m 05s", "1h 02m" — coarser units as turns get longer.
fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 1 {
        return format!("{ms}ms");
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {:02}s", secs % 60);
    }
    format!("{}h {:02}m", mins / 60, mins % 60)
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

/// Whether code highlights use the dark syntect theme; flipped with the UI
/// theme so code blocks stay readable on light surfaces. Existing rows keep
/// their baked colors until the caller re-renders them.
static SYNTAX_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Inline-code span tint, baked into styled markup (the theme's `warn`
/// color; defaults to the dark theme's amber).
static INLINE_CODE_TINT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0xffe5_c07b);

pub fn set_syntax_dark(dark: bool) {
    SYNTAX_DARK.store(dark, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_inline_code_tint(argb: u32) {
    INLINE_CODE_TINT.store(argb, std::sync::atomic::Ordering::Relaxed);
}

/// Terminal default foreground/background (the theme's code colors, RGB),
/// baked into grid spans when the controller renders the terminal screen.
static TERM_FG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x00d8_d8c8);
static TERM_BG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x001f_2226);

pub fn set_term_colors(fg_argb: u32, bg_argb: u32) {
    TERM_FG.store(fg_argb & 0xff_ffff, std::sync::atomic::Ordering::Relaxed);
    TERM_BG.store(bg_argb & 0xff_ffff, std::sync::atomic::Ordering::Relaxed);
}

/// (default fg, default bg) as 0xRRGGBB.
pub fn term_colors() -> (u32, u32) {
    (
        TERM_FG.load(std::sync::atomic::Ordering::Relaxed),
        TERM_BG.load(std::sync::atomic::Ordering::Relaxed),
    )
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
/// Markdown-block rows (kind 1) for the file previewer; same shape the
/// chat renderer uses, so the UI mapping is shared.
pub fn markdown_rows(content: &str) -> Vec<ChatRowData> {
    let mut body = Vec::new();
    push_blocks(&mut body, content);
    body.into_iter().map(|(row, _)| row).collect()
}

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
    let dark = SYNTAX_DARK.load(std::sync::atomic::Ordering::Relaxed);
    let theme = &themes.themes[if dark {
        "base16-ocean.dark"
    } else {
        "base16-ocean.light"
    }];
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
    fn fenced_code_is_highlighted_before_ui_mapping() {
        let rows = markdown_rows("```rust\nfn main() {}\n```");
        let code = rows.iter().find(|row| row.md_kind == 5).unwrap();
        assert_eq!(code.md_lang, "rust");
        assert!(!code.code_lines.is_empty());
        assert!(
            code.code_lines
                .iter()
                .flatten()
                .any(|(text, _)| text.contains("fn"))
        );
    }

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
    fn todo_write_renders_a_checklist_card() {
        let args = serde_json::json!({"todos": [
            {"id": "a", "content": "first", "status": "completed"},
        ]});
        let result = serde_json::json!({"todos": [
            {"id": "a", "content": "first", "status": "completed"},
            {"id": "b", "content": "second", "status": "in_progress"},
            {"id": "c", "content": "third", "status": "pending"},
        ]});
        let row = tool_row(
            "c1",
            "todo_write",
            &args,
            ToolCallStatus::Ok,
            &Some(result),
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Todos");
        // Header shows the in-progress item; meta the progress count.
        assert_eq!(row.text, "second");
        assert_eq!(row.meta, "1/3");
        assert_eq!(row.detail, "✓ first\n▸ second\n○ third");

        // Without a result yet (streaming), the args render.
        let row = tool_row(
            "c1",
            "todo_write",
            &args,
            ToolCallStatus::Running,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.meta, "1/1");
        assert_eq!(row.text, "1/1 done");
    }

    #[test]
    fn human_duration_scales_units() {
        assert_eq!(human_duration(850), "850ms");
        assert_eq!(human_duration(12_400), "12s");
        assert_eq!(human_duration(65_000), "1m 05s");
        assert_eq!(human_duration(3_720_000), "1h 02m");
    }

    #[test]
    fn turn_header_includes_response_duration() {
        let mut vm = ThreadViewModel {
            items: vec![
                ChatItem::Assistant {
                    turn: 1,
                    content: "answer".into(),
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
        vm.turn_duration_ms.insert(1, 12_400);
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let header = rows
            .iter()
            .find(|r| r.kind == 7 && r.tool_name == "Agent")
            .unwrap();
        assert_eq!(header.meta, "0 in / 0 out tokens · 12s");
    }

    #[test]
    fn running_turn_renders_trailing_activity_row() {
        let mut vm = ThreadViewModel {
            turn_running: true,
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(rows.last().unwrap().kind, 5);
        assert_eq!(rows.last().unwrap().text, "Processing…");
        vm.thinking = true;
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(rows.last().unwrap().text, "Thinking…");

        // With the running turn's Agent card open at the tail, the activity
        // row nests as the card's last body row instead of standing alone.
        vm.items = vec![
            ChatItem::TurnStatus {
                turn: 0,
                state: TurnState::Running,
            },
            ChatItem::Assistant {
                turn: 0,
                content: "streaming…".into(),
                complete: false,
            },
        ];
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let last = rows.last().unwrap();
        assert_eq!(last.kind, 5);
        assert_eq!(last.card_pos, 3);
        assert_eq!(last.tool_name, "Agent");
        assert_eq!(rows[rows.len() - 2].card_pos, 2);

        // Collapsed card: the activity row stands alone again.
        let collapsed: HashSet<String> = ["a:1".to_string()].into();
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &collapsed,
            &HashMap::new(),
        );
        let last = rows.last().unwrap();
        assert_eq!(last.kind, 5);
        assert_eq!(last.card_pos, 0);

        vm.items.clear();
        vm.turn_running = false;
        vm.thinking = false;
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn activity_row_stays_out_of_previous_turns_card() {
        // A new turn started (prompt shown) but nothing has streamed yet —
        // e.g. a local model loading. The pulse must trail the new prompt,
        // not nest inside the previous turn's completed Agent card.
        let vm = ThreadViewModel {
            turn_running: true,
            items: vec![
                ChatItem::TurnStatus {
                    turn: 1,
                    state: TurnState::Completed {
                        usage: Default::default(),
                    },
                },
                ChatItem::User {
                    turn: 1,
                    content: "first".into(),
                    attachments: vec![],
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "done".into(),
                    complete: true,
                },
                ChatItem::TurnStatus {
                    turn: 2,
                    state: TurnState::Running,
                },
                ChatItem::User {
                    turn: 2,
                    content: "second".into(),
                    attachments: vec![],
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let last = rows.last().unwrap();
        assert_eq!(last.kind, 5);
        // Standalone (card_pos 0), below the second prompt.
        assert_eq!(last.card_pos, 0);
        let prompt_idx = rows.iter().position(|r| r.text == "second").unwrap();
        assert!(prompt_idx < rows.len() - 1);

        // Once the new turn's card opens, the pulse nests inside *that* card.
        let mut vm = vm;
        vm.items.push(ChatItem::Assistant {
            turn: 2,
            content: "streaming…".into(),
            complete: false,
        });
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let last = rows.last().unwrap();
        assert_eq!(last.kind, 5);
        assert_eq!(last.card_pos, 3);
        assert_eq!(last.tool_name, "Agent");
    }

    fn two_questions() -> Vec<trouve_protocol::Question> {
        let opt = |id: &str, label: &str| trouve_protocol::QuestionOption {
            id: id.into(),
            label: label.into(),
        };
        vec![
            trouve_protocol::Question {
                id: "q1".into(),
                prompt: "Favorite color?".into(),
                options: vec![opt("red", "Red"), opt("blue", "Blue")],
                allow_multiple: false,
            },
            trouve_protocol::Question {
                id: "q2".into(),
                prompt: "Fruits you like?".into(),
                options: vec![opt("apple", "Apple"), opt("banana", "Banana")],
                allow_multiple: true,
            },
        ]
    }

    #[test]
    fn question_wizard_pages_review_and_summary() {
        let questions = two_questions();
        let vm = ThreadViewModel {
            items: vec![ChatItem::Questions {
                request_id: "qr_1".into(),
                title: Some("Preferences".into()),
                questions: questions.clone(),
                answers: None,
            }],
            ..Default::default()
        };

        // Fresh wizard: page 1 with radio options and no Back.
        let mut wizards = HashMap::new();
        wizards.insert("qr_1".to_string(), WizardState::new(2));
        let (rows, ids) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &wizards,
        );
        let w = rows.iter().find(|r| r.kind == 10).unwrap();
        assert_eq!(w.text, "Preferences");
        assert_eq!(w.meta, "Question 1 of 2");
        assert_eq!(w.q_prompt, "Favorite color?");
        assert_eq!(
            w.q_options,
            vec![("Red".to_string(), false), ("Blue".to_string(), false)]
        );
        assert!(!w.q_multi && !w.q_can_back && !w.q_can_next && !w.q_review);
        // The wizard row maps back to its request id.
        let widx = rows.iter().position(|r| r.kind == 10).unwrap();
        assert_eq!(ids[widx].as_deref(), Some("qr_1"));

        // A selection enables Next; the second page is multi-choice; the
        // review page lists both answers including the "Other" text.
        let mut state = WizardState::new(2);
        state.selections[0] = vec!["red".to_string()];
        state.selections[1] = vec!["apple".to_string(), OTHER_ID.to_string()];
        state.other_texts[1] = "mango".into();
        state.step = 1;
        wizards.insert("qr_1".to_string(), state.clone());
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &wizards,
        );
        let w = rows.iter().find(|r| r.kind == 10).unwrap();
        assert_eq!(w.meta, "Question 2 of 2");
        assert!(w.q_multi && w.q_can_back && w.q_can_next && w.q_last && w.q_other);
        assert_eq!(w.q_other_text, "mango");

        state.step = 2;
        wizards.insert("qr_1".to_string(), state.clone());
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &wizards,
        );
        let w = rows.iter().find(|r| r.kind == 10).unwrap();
        assert!(w.q_review && !w.q_done);
        assert_eq!(w.q_summary[0], ("Favorite color?".into(), "Red".into()));
        assert_eq!(
            w.q_summary[1],
            ("Fruits you like?".into(), "Apple, Other: mango".into())
        );

        // The submission payload strips the marker and carries the text.
        let answers = state.answers(&questions);
        assert_eq!(answers[0].selected_option_ids, vec!["red".to_string()]);
        assert_eq!(answers[0].other_text, None);
        assert_eq!(answers[1].selected_option_ids, vec!["apple".to_string()]);
        assert_eq!(answers[1].other_text.as_deref(), Some("mango"));
    }

    #[test]
    fn resolved_questions_render_an_answer_summary() {
        let questions = two_questions();
        let answered = ChatItem::Questions {
            request_id: "qr_1".into(),
            title: None,
            questions: questions.clone(),
            answers: Some(Some(vec![trouve_protocol::QuestionAnswer {
                question_id: "q1".into(),
                selected_option_ids: vec!["blue".into()],
                other_text: None,
            }])),
        };
        let vm = ThreadViewModel {
            items: vec![answered],
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let w = rows.iter().find(|r| r.kind == 10).unwrap();
        assert!(w.q_done && w.q_review);
        assert_eq!(w.meta, "Answered");
        assert_eq!(w.q_summary[0], ("Favorite color?".into(), "Blue".into()));
        // Unanswered questions show a placeholder.
        assert_eq!(w.q_summary[1], ("Fruits you like?".into(), "—".into()));

        // Skipped requests summarize as such (empty summary + meta).
        let vm = ThreadViewModel {
            items: vec![ChatItem::Questions {
                request_id: "qr_2".into(),
                title: None,
                questions,
                answers: Some(None),
            }],
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let w = rows.iter().find(|r| r.kind == 10).unwrap();
        assert!(w.q_done && w.q_summary.is_empty());
        assert_eq!(w.meta, "Skipped");
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
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_eq!(rows[0].kind, 7);
        assert!(rows.len() > 2);
        // Raw: header plus a single kind-6 row of markdown source.
        let raw: HashSet<u64> = [3].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new(), &HashMap::new());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].kind, 6);
        assert_eq!(rows[1].text, "# heading\n\nbody `code`");
        // Collapsed: the header alone, with a one-line preview.
        let collapsed: HashSet<String> = ["a:0".to_string()].into();
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &collapsed,
            &HashMap::new(),
        );
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
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
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
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new(), &HashMap::new());
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
                    attachments: vec![],
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
        let (rows, ids) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        // Streaming turn: You header, prompt, Assistant header, the 2-tool
        // run under an expanded group header, the narration text (never
        // grouped), then the trailing single tool inline.
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 2, 2, 1, 2]);
        assert_eq!(rows[3].text, "Called 2 tools");
        assert!(rows[3].expanded);
        assert!(rows[3..].iter().all(|r| r.card_pos >= 2));
        assert_eq!(rows.last().unwrap().card_pos, 3);
        assert_eq!(ids[4].as_deref(), Some("t1"));
        assert_eq!(ids[7].as_deref(), Some("t3"));
        // Once the turn completes, the run collapses by default; the text
        // and the ungrouped single tool stay visible.
        vm.items.push(ChatItem::TurnStatus {
            turn: 1,
            state: TurnState::Completed {
                usage: Default::default(),
            },
        });
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 1, 2]);
        assert!(!rows[3].expanded);
        // Toggling the group key (the run's first item index + the owning
        // card's anchor) reopens it.
        let opened: HashSet<String> = ["g1:3".to_string()].into();
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &opened,
            &HashMap::new(),
        );
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 9, 2, 2, 1, 2]);
        // A tool call with no assistant item (yet) still gets an Assistant
        // wrapper card, so the panel is present from the turn's first tool.
        let vm = ThreadViewModel {
            items: vec![tool("t9")],
            ..Default::default()
        };
        let (rows, ids) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
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
                    attachments: vec![],
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
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        // One You card, one Agent card; a lone thinking item between text
        // stretches stays inline (no group header for a single item): a
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
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &collapsed,
            &HashMap::new(),
        );
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![7, 1, 7, 4, 1, 4, 1, 1]);
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert!(!think[0].expanded);
        assert!(think[1].expanded);
        // Raw view keeps stream order too: each text stretch becomes a
        // kind-6 row in place, not one blob hoisted to the top.
        let raw: HashSet<u64> = [1].into();
        let (rows, _) = chat_rows(&vm, &HashSet::new(), &raw, &HashSet::new(), &HashMap::new());
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
            attachments: vec![],
        });
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let think: Vec<_> = rows.iter().filter(|r| r.kind == 4).collect();
        assert!(
            think.iter().all(|r| !r.expanded),
            "collapsed once superseded"
        );
        // …and the toggle set now re-expands instead of collapsing.
        let toggled: HashSet<String> = ["t:1".to_string()].into();
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &toggled,
            &HashMap::new(),
        );
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
                    attachments: vec![],
                },
                tool("t1"),
                tool("t2"),
                tool("t3"),
            ],
            turn_running: true,
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
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
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        let headers = rows
            .iter()
            .filter(|r| r.kind == 7 && r.tool_name == "Agent")
            .count();
        assert_eq!(headers, 1);
    }

    #[test]
    fn mixed_thinking_and_tool_runs_group_with_a_summary() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 1,
                    content: "q".into(),
                    attachments: vec![],
                },
                ChatItem::Thinking {
                    turn: 1,
                    content: "hmm".into(),
                    complete: true,
                },
                ChatItem::ToolCall {
                    call_id: "t1".into(),
                    tool: "Read".into(),
                    args: serde_json::json!({"file_path": "a.rs"}),
                    status: ToolCallStatus::Ok,
                    result: None,
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "answer".into(),
                    complete: true,
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        // The thinking + read run groups under one summarized header; the
        // narration/answer text stays outside the group.
        let group = rows.iter().find(|r| r.kind == 9).unwrap();
        assert_eq!(group.text, "Read 1 file, thought 1 time");
        assert!(group.expanded, "open while streaming");
        // Both members nest one level under the header.
        let pill = rows.iter().find(|r| r.kind == 4).unwrap();
        assert_eq!(pill.md_indent, 1);
        let tool = rows.iter().find(|r| r.kind == 2).unwrap();
        assert_eq!(tool.md_indent, 1);
        // The answer text is a top-level markdown row, not grouped.
        assert!(rows.iter().any(|r| r.kind == 1 && r.text == "answer"));
    }

    #[test]
    fn activity_summary_counts_by_category() {
        let tool = |name: &str, args: serde_json::Value| ChatItem::ToolCall {
            call_id: "c".into(),
            tool: name.into(),
            args,
            status: ToolCallStatus::Ok,
            result: None,
        };
        let vm = ThreadViewModel {
            items: vec![
                // Two edits of the same file count once; a third distinct
                // file makes two.
                tool("Edit", serde_json::json!({"file_path": "a.rs"})),
                tool("write_file", serde_json::json!({"path": "a.rs"})),
                tool("Write", serde_json::json!({"file_path": "b.rs"})),
                tool("Read", serde_json::json!({"file_path": "c.rs"})),
                tool("read_file", serde_json::json!({"path": "d.rs"})),
                tool("Bash", serde_json::json!({"command": "ls"})),
                tool("mcp__trouve__search", serde_json::json!({"query": "x"})),
                ChatItem::Thinking {
                    turn: 1,
                    content: "hmm".into(),
                    complete: true,
                },
            ],
            ..Default::default()
        };
        let segments: Vec<Segment> = (0..vm.items.len()).map(Segment::Item).collect();
        assert_eq!(
            activity_summary(&vm, &segments),
            "Edited 2 files, read 2 files, ran 1 command, called 1 tool, thought 1 time"
        );
    }

    #[test]
    fn user_prompts_after_the_first_get_a_rule_above() {
        let vm = ThreadViewModel {
            items: vec![
                ChatItem::User {
                    turn: 1,
                    content: "one".into(),
                    attachments: vec![],
                },
                ChatItem::Assistant {
                    turn: 1,
                    content: "reply".into(),
                    complete: true,
                },
                ChatItem::User {
                    turn: 2,
                    content: "two".into(),
                    attachments: vec![],
                },
            ],
            ..Default::default()
        };
        let (rows, _) = chat_rows(
            &vm,
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &HashMap::new(),
        );
        assert_ne!(rows[0].kind, 8, "no rule before the first turn");
        let rules: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.kind == 8)
            .collect();
        assert_eq!(rules.len(), 1);
        // The rule sits directly above the second prompt's header.
        assert_eq!(rows[rules[0].0 + 1].kind, 7);
        assert_eq!(rows[rules[0].0 + 1].tool_name, "You");
    }

    #[test]
    fn shell_tools_show_their_command() {
        let args = serde_json::json!({"command": "wc -l  bench.rs\n"});
        assert_eq!(tool_label("Bash", &args), "Bash (wc -l bench.rs)");
        assert_eq!(tool_label("shell", &args), "Shell (wc -l bench.rs)");
        // Tools without a recognized arg keep the plain (display) name.
        assert_eq!(tool_label("search", &args), "Code Search");
        assert_eq!(tool_label("Bash", &serde_json::json!({})), "Bash");
        // Long commands truncate on a char boundary with an ellipsis.
        let long = serde_json::json!({ "command": "x".repeat(100) });
        let label = tool_label("Bash", &long);
        assert!(label.len() < 70 && label.ends_with("…)"), "{label}");
    }

    #[test]
    fn tool_titles_are_human_readable() {
        let q = serde_json::json!({"query": "markdown renderer"});
        // trouve's search rides the MCP bridge under a mangled name.
        assert_eq!(
            tool_label("mcp__trouve__search", &q),
            "Code Search markdown renderer"
        );
        assert_eq!(tool_label("search", &q), "Code Search markdown renderer");
        // Vendor camelCase names split into words, with the query appended.
        assert_eq!(
            tool_label("ToolSearch", &q),
            "Tool Search markdown renderer"
        );
        assert_eq!(tool_label("WebSearch", &q), "Web Search markdown renderer");
        assert_eq!(
            tool_label("WebFetch", &serde_json::json!({"url": "https://a.io"})),
            "Web Fetch https://a.io"
        );
        // snake_case splits too; foreign MCP tools keep their server.
        assert_eq!(tool_label("list_dir", &serde_json::json!({})), "List Dir");
        assert_eq!(
            tool_label("mcp__jira__create_issue", &serde_json::json!({})),
            "jira: Create Issue"
        );
    }

    #[test]
    fn tool_details_render_human_readable() {
        let args = serde_json::json!({"command": "ls -la", "cwd": null});
        let result = serde_json::json!({
            "exit_code": 0, "stdout": "a\nb", "stderr": "", "truncated": false,
        });
        let row = tool_row(
            "c1",
            "Bash",
            &args,
            ToolCallStatus::Ok,
            &Some(result),
            &HashSet::new(),
        );
        assert!(
            !row.detail.contains('{') && !row.detail.contains('"'),
            "no JSON noise: {}",
            row.detail
        );
        assert!(row.detail.contains("command: ls -la"));
        assert!(row.detail.contains("── result ──"));
        assert!(row.detail.contains("stdout:\n  a\n  b"), "{}", row.detail);
        assert!(!row.detail.contains("stderr"), "empty values dropped");
        assert!(!row.detail.contains("cwd"), "nulls dropped");

        // Claude-style text-block results unwrap to the plain text.
        let blocks = serde_json::json!([{"type": "text", "text": "42 files"}]);
        let row = tool_row(
            "c2",
            "Bash",
            &args,
            ToolCallStatus::Ok,
            &Some(blocks),
            &HashSet::new(),
        );
        assert!(
            row.detail.ends_with("── result ──\n42 files"),
            "{}",
            row.detail
        );
    }

    #[test]
    fn read_tools_title_with_a_clickable_filename() {
        let args = serde_json::json!({"file_path": "/w/src/app/main.rs"});
        let row = tool_row(
            "c1",
            "Read",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Read");
        assert_eq!(row.text, "main.rs");
        assert_eq!(row.tool_file, "/w/src/app/main.rs");
        // Whole-file read: no range badge.
        assert_eq!((row.tool_line_from, row.tool_line_to), (0, 0));
        assert!(row.meta.is_empty());

        // Ranged read (Claude offset/limit): "L<from>-<to>" in the header.
        let args = serde_json::json!({
            "file_path": "/w/src/app/main.rs", "offset": 100, "limit": 50,
        });
        let row = tool_row(
            "c1",
            "Read",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!((row.tool_line_from, row.tool_line_to), (100, 149));
        assert_eq!(row.meta, "L100-149");

        // start/end variants map directly.
        let args = serde_json::json!({"path": "a.rs", "start_line": 3, "end_line": 9});
        let row = tool_row(
            "c1",
            "read_file",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!((row.tool_line_from, row.tool_line_to), (3, 9));
        assert_eq!(row.meta, "L3-9");

        // Cursor / native variants use a "path" argument.
        let args = serde_json::json!({"path": "notes.md"});
        let row = tool_row(
            "c2",
            "read_file",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Read");
        assert_eq!(
            (row.text.as_str(), row.tool_file.as_str()),
            ("notes.md", "notes.md")
        );

        // Non-read tools get their display label and no file link.
        let row = tool_row(
            "c3",
            "search",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Code Search notes.md");
        assert!(row.tool_file.is_empty());
    }

    #[test]
    fn edit_tools_show_a_line_diff_with_counts() {
        let dl = DiffLine::new;
        // Claude Edit: old/new snippets diff line-by-line. No "_line" hint:
        // the gutter stays blank (all zeros).
        let args = serde_json::json!({
            "file_path": "/w/src/lib.rs",
            "old_string": "fn a() {}\nfn b() {}",
            "new_string": "fn a() {}\nfn b2() {}\nfn c() {}",
        });
        let row = tool_row(
            "c1",
            "Edit",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Edit");
        assert_eq!(row.text, "lib.rs");
        assert_eq!(row.tool_file, "/w/src/lib.rs");
        assert_eq!((row.tool_adds, row.tool_dels), (2, 1));
        assert_eq!(
            row.diff,
            vec![
                dl(2, 0, 0, "fn a() {}"),
                dl(4, 0, 0, "fn b() {}"),
                dl(3, 0, 0, "fn b2() {}"),
                dl(3, 0, 0, "fn c() {}"),
            ]
        );

        // With the engine's "_line" hint both gutters number from the
        // edit's position in the file.
        let args = serde_json::json!({
            "file_path": "/w/src/lib.rs",
            "old_string": "fn a() {}\nfn b() {}",
            "new_string": "fn a() {}\nfn b2() {}\nfn c() {}",
            "_line": 40,
        });
        let row = tool_row(
            "c1",
            "Edit",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(
            row.diff,
            vec![
                dl(2, 40, 40, "fn a() {}"),
                dl(4, 41, 0, "fn b() {}"),
                dl(3, 0, 41, "fn b2() {}"),
                dl(3, 0, 42, "fn c() {}"),
            ]
        );

        // Write: no old text, everything is an addition numbered from 1.
        let args = serde_json::json!({"path": "new.txt", "content": "one\ntwo"});
        let row = tool_row(
            "c2",
            "write_file",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!(row.tool_name, "Write");
        assert_eq!((row.tool_adds, row.tool_dels), (2, 0));
        assert_eq!(row.diff, vec![dl(3, 0, 1, "one"), dl(3, 0, 2, "two")]);

        // MultiEdit: pairs separated by a divider row, each with its own
        // per-edit line hint.
        let args = serde_json::json!({
            "file_path": "/w/a.rs",
            "edits": [
                {"old_string": "x", "new_string": "y", "_line": 3},
                {"old_string": "p", "new_string": "q"},
            ],
        });
        let row = tool_row(
            "c3",
            "MultiEdit",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!((row.tool_adds, row.tool_dels), (2, 2));
        assert_eq!(
            row.diff,
            vec![
                dl(4, 3, 0, "x"),
                dl(3, 0, 3, "y"),
                dl(1, 0, 0, "···"),
                dl(4, 0, 0, "p"),
                dl(3, 0, 0, "q"),
            ]
        );

        // A unified-diff payload renders its lines directly, with numbers
        // seeded from the hunk header.
        let args = serde_json::json!({
            "path": "b.rs",
            "diff": "@@ -10,2 +10,2 @@\n context\n-old\n+new",
        });
        let row = tool_row(
            "c4",
            "edit",
            &args,
            ToolCallStatus::Ok,
            &None,
            &HashSet::new(),
        );
        assert_eq!((row.tool_adds, row.tool_dels), (1, 1));
        assert_eq!(
            row.diff,
            vec![
                dl(1, 0, 0, "@@ -10,2 +10,2 @@"),
                dl(2, 10, 10, "context"),
                dl(4, 11, 0, "old"),
                dl(3, 0, 11, "new"),
            ]
        );
    }
}
