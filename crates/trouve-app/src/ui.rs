//! UI-thread bridge: converts plain data from the controller into generated
//! Slint models. Every function here is safe to call from any thread — the
//! conversion happens inside `upgrade_in_event_loop`.

use slint::{ModelRc, SharedString, VecModel};

use crate::render::ChatRowData;
use crate::{
    AppWindow, ChatRow, CliItem, DiffRow, FileItem, KnownProviderItem, NavRow, ProviderItem,
    QOption, QPair, TextSegment, ThreadTabItem,
};

type Ui = slint::Weak<AppWindow>;

/// Plain-data mirror of the `NavRow` Slint struct.
#[derive(Debug, Clone, Default)]
pub struct NavRowData {
    pub kind: i32,
    pub title: String,
    pub subtitle: String,
    pub session_index: i32,
    pub selected: bool,
    pub archived: bool,
    pub expanded: bool,
}

/// Bring the window to the front (notification clicks). Wayland
/// compositors may deny focus stealing, in which case the
/// user-attention request at least flashes the taskbar entry.
pub fn raise_window(ui: &Ui) {
    let _ = ui.upgrade_in_event_loop(|ui| {
        use slint::winit_030::{winit, WinitWindowAccessor};
        use slint::ComponentHandle;
        ui.window().with_winit_window(|w| {
            w.set_minimized(false);
            w.focus_window();
            w.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        });
    });
}

pub fn set_error(ui: &Ui, text: &str) {
    let text = text.to_string();
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_error_text(SharedString::from(text)));
}

pub fn set_pickers(ui: &Ui, modes: Vec<String>, models: Vec<String>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_modes(string_model(modes));
        // A fresh list resets the search picker's filter to "everything".
        ui.set_model_filter_matches(ModelRc::new(VecModel::from(
            (0..models.len() as i32).collect::<Vec<i32>>(),
        )));
        ui.set_models(string_model(models));
    });
}

/// Reflect the current thread's mode/model in the composer pickers.
pub fn set_picker_indices(ui: &Ui, mode: i32, model: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_mode_index(mode);
        ui.set_model_index(model);
    });
}

/// Model knobs for the current thread: thinking-level labels + selection,
/// and the fast toggle. Empty options hide the dropdown.
pub fn set_model_knobs(
    ui: &Ui,
    thinking_options: Vec<String>,
    thinking_index: i32,
    context_options: Vec<String>,
    context_index: i32,
    fast_visible: bool,
    fast_checked: bool,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_thinking_options(string_model(thinking_options));
        ui.set_thinking_index(thinking_index);
        ui.set_context_options(string_model(context_options));
        ui.set_context_index(context_index);
        ui.set_fast_visible(fast_visible);
        ui.set_fast_checked(fast_checked);
    });
}

pub fn set_nav(ui: &Ui, rows: Vec<NavRowData>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<NavRow> = rows
            .into_iter()
            .map(|r| NavRow {
                kind: r.kind,
                title: r.title.into(),
                subtitle: r.subtitle.into(),
                session_index: r.session_index,
                selected: r.selected,
                archived: r.archived,
                expanded: r.expanded,
            })
            .collect();
        ui.set_nav_rows(ModelRc::new(VecModel::from(items)));
    });
}

pub fn set_threads(ui: &Ui, threads: Vec<(String, String)>, current: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<ThreadTabItem> = threads
            .into_iter()
            .map(|(id, label)| ThreadTabItem {
                id: id.into(),
                label: label.into(),
            })
            .collect();
        ui.set_threads(ModelRc::new(VecModel::from(items)));
        ui.set_current_thread(current);
    });
}

fn to_chat_row(r: &ChatRowData) -> ChatRow {
    // Highlighting runs here — after the diff in `set_chat` — so only new
    // or changed code blocks pay for syntect, not every block every frame.
    let code_lines: ModelRc<ModelRc<TextSegment>> = if r.kind == 1 && r.md_kind == 5 {
        let lines: Vec<ModelRc<TextSegment>> = crate::render::highlight_code(&r.md_lang, &r.text)
            .into_iter()
            .map(|segments| {
                let segs: Vec<TextSegment> = segments
                    .into_iter()
                    .map(|(text, rgb)| TextSegment {
                        text: SharedString::from(text.as_str()),
                        color: slint::Color::from_argb_encoded(0xff00_0000 | rgb),
                    })
                    .collect();
                ModelRc::new(VecModel::from(segs))
            })
            .collect();
        ModelRc::new(VecModel::from(lines))
    } else {
        ModelRc::default()
    };
    ChatRow {
        kind: r.kind,
        md_kind: r.md_kind,
        md_indent: r.md_indent,
        md_lang: SharedString::from(r.md_lang.as_str()),
        text: SharedString::from(r.text.as_str()),
        code_lines,
        // Malformed markup falls back to the raw text rather than
        // dropping the row.
        styled: slint::StyledText::from_markdown(&r.styled_md)
            .unwrap_or_else(|_| slint::StyledText::from_plain_text(&r.styled_md)),
        tone: r.tone,
        tool_name: SharedString::from(r.tool_name.as_str()),
        tool_status: r.tool_status,
        tool_file: SharedString::from(r.tool_file.as_str()),
        tool_line_from: r.tool_line_from,
        tool_line_to: r.tool_line_to,
        // Header badge strings ("+12" / "−3"); empty hides them.
        tool_adds: if r.tool_adds > 0 {
            format!("+{}", r.tool_adds).into()
        } else {
            SharedString::new()
        },
        tool_dels: if r.tool_dels > 0 {
            format!("−{}", r.tool_dels).into()
        } else {
            SharedString::new()
        },
        tool_diff: ModelRc::new(VecModel::from(
            r.diff
                .iter()
                .map(|l| DiffRow {
                    kind: l.kind,
                    old_no: if l.old_no > 0 {
                        l.old_no.to_string().into()
                    } else {
                        SharedString::new()
                    },
                    new_no: if l.new_no > 0 {
                        l.new_no.to_string().into()
                    } else {
                        SharedString::new()
                    },
                    text: SharedString::from(l.text.as_str()),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )),
        // Show gutter columns only when at least one line resolved to a
        // real file position.
        tool_diff_numbered: r.diff.iter().any(|l| l.old_no > 0 || l.new_no > 0),
        detail: SharedString::from(r.detail.as_str()),
        expanded: r.expanded,
        turn_state: r.turn_state,
        turn: r.turn,
        raw: r.raw,
        card_key: SharedString::from(r.card_key.as_str()),
        card_pos: r.card_pos,
        card_first: r.card_first,
        meta: SharedString::from(r.meta.as_str()),
        subtitle: SharedString::from(r.subtitle.as_str()),
        q_prompt: SharedString::from(r.q_prompt.as_str()),
        q_options: ModelRc::new(VecModel::from(
            r.q_options
                .iter()
                .map(|(label, selected)| QOption {
                    label: SharedString::from(label.as_str()),
                    selected: *selected,
                })
                .collect::<Vec<_>>(),
        )),
        q_multi: r.q_multi,
        q_other: r.q_other,
        q_other_text: SharedString::from(r.q_other_text.as_str()),
        q_review: r.q_review,
        q_done: r.q_done,
        q_summary: ModelRc::new(VecModel::from(
            r.q_summary
                .iter()
                .map(|(prompt, answer)| QPair {
                    prompt: SharedString::from(prompt.as_str()),
                    answer: SharedString::from(answer.as_str()),
                })
                .collect::<Vec<_>>(),
        )),
        q_can_back: r.q_can_back,
        q_can_next: r.q_can_next,
        q_last: r.q_last,
    }
}

// The previous render's source rows, for diffing (UI thread only).
thread_local! {
    static LAST_CHAT: std::cell::RefCell<Vec<ChatRowData>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drop the chat diff cache so the next `set_chat` rebuilds every row.
/// Needed when row conversion itself changed meaning — e.g. a theme switch
/// re-bakes syntax-highlight and inline-code colors — and identical source
/// rows must still be re-converted.
pub fn invalidate_chat_cache(ui: &Ui) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        LAST_CHAT.with(|cache| cache.borrow_mut().clear());
        // Zero-length model forces the wholesale-replace path in set_chat.
        ui.set_chat_rows(ModelRc::new(VecModel::<ChatRow>::default()));
    });
}

pub fn set_chat(ui: &Ui, rows: Vec<ChatRowData>, scroll_to_end: bool) {
    use slint::Model as _;

    let _ = ui.upgrade_in_event_loop(move |ui| {
        // Sampled before the model changes: row add/removes make the
        // ListView re-derive viewport-y from estimated row heights, so a
        // toggle at the tail would land the view at a visibly wrong spot
        // unless we re-pin it below.
        let was_at_bottom = ui.get_chat_at_bottom();
        LAST_CHAT.with(|cache| {
            let mut cache = cache.borrow_mut();
            let model = ui.get_chat_rows();
            // Update the existing model in place: replacing it wholesale
            // makes the ListView re-instantiate every row and recompute
            // its viewport, which visibly jumps the scroll position on
            // every expand/collapse. The cache/model length check guards
            // against a model this function didn't build.
            match model.as_any().downcast_ref::<VecModel<ChatRow>>() {
                Some(vec) if vec.row_count() == cache.len() => {
                    let common = cache.len().min(rows.len());
                    for (i, row) in rows.iter().take(common).enumerate() {
                        if cache[i] != *row {
                            vec.set_row_data(i, to_chat_row(row));
                        }
                    }
                    for row in &rows[common..] {
                        vec.push(to_chat_row(row));
                    }
                    for i in (rows.len()..cache.len()).rev() {
                        vec.remove(i);
                    }
                }
                _ => {
                    let items: Vec<ChatRow> = rows.iter().map(to_chat_row).collect();
                    ui.set_chat_rows(ModelRc::new(VecModel::from(items)));
                }
            }
            *cache = rows;
        });
        if scroll_to_end || was_at_bottom {
            // At the tail, the bottom edge is the user's anchor: keep it
            // glued there through the re-layout (collapsing a card at the
            // bottom must not leave the viewport mid-drift).
            ui.invoke_scroll_chat_to_end();
        } else {
            // In-place re-renders (expand/collapse, raw toggles) higher up
            // must not yank the view back to the tail when heights change.
            ui.set_chat_follow(false);
        }
    });
}

pub fn set_composer_enabled(ui: &Ui, enabled: bool) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_composer_enabled(enabled));
}

/// Slash commands the current thread's harness accepts, as (name,
/// description) pairs — the prompt box's "/" completion popup.
pub fn set_slash_commands(ui: &Ui, commands: Vec<(String, String)>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let (names, details): (Vec<SharedString>, Vec<SharedString>) = commands
            .into_iter()
            .map(|(n, d)| {
                (
                    SharedString::from(n.as_str()),
                    SharedString::from(d.as_str()),
                )
            })
            .unzip();
        ui.set_slash_commands(ModelRc::new(VecModel::from(names)));
        ui.set_slash_details(ModelRc::new(VecModel::from(details)));
    });
}

/// The current thread's queued prompts (run order) and whether the thread
/// is idle (idle + non-empty queue surfaces the "Send now" pill).
pub fn set_queue(ui: &Ui, prompts: Vec<String>, idle: bool) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let rows: Vec<SharedString> = prompts
            .iter()
            .map(|p| SharedString::from(p.as_str()))
            .collect();
        ui.set_queue_prompts(ModelRc::new(VecModel::from(rows)));
        ui.set_queue_idle(idle);
    });
}

/// 0 = chat, 1 = new-session screen, 2 = new-thread screen.
pub fn set_center_screen(ui: &Ui, screen: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_center_screen(screen));
}

/// Right-panel tab: 0 = Diff, 1 = Files.
pub fn set_right_tab(ui: &Ui, tab: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_right_tab(tab));
}

/// Session-list filter: whether archived sessions are shown.
pub fn set_show_archived(ui: &Ui, show: bool) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_show_archived(show));
}

/// Restore the chat scroll offset (viewport-y; 0 or negative).
pub fn set_chat_scroll(ui: &Ui, y: f32) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        // A restored bookmark is an explicit position; stop tail-following.
        ui.set_chat_follow(false);
        ui.set_chat_scroll(y);
    });
}

/// How many threads have an agent turn in flight (quit-confirm dialog).
pub fn set_agents_running(ui: &Ui, count: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_agents_running(count));
}

/// Tear down the UI event loop (deferred quit).
pub fn quit(ui: &Ui) {
    let _ = ui.upgrade_in_event_loop(|_| {
        let _ = slint::quit_event_loop();
    });
}

/// Populate the new-chat screen's pickers.
pub fn set_new_chat(
    ui: &Ui,
    workspaces: Vec<String>,
    workspace_index: i32,
    branches: Vec<String>,
    branch_index: i32,
    mode_index: i32,
    model_index: i32,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_nc_workspaces(string_model(workspaces));
        ui.set_nc_workspace_index(workspace_index);
        ui.set_nc_branches(string_model(branches));
        ui.set_nc_branch_index(branch_index);
        ui.set_nc_mode_index(mode_index);
        ui.set_nc_model_index(model_index);
    });
}

pub fn set_branches(ui: &Ui, branches: Vec<String>, branch_index: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_nc_branches(string_model(branches));
        ui.set_nc_branch_index(branch_index);
    });
}

/// Context dial state: fill in 0..=1, busy flag, tooltip stats.
pub fn set_context(ui: &Ui, fill: f32, compacting: bool, tooltip: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_context_fill(fill);
        ui.set_context_compacting(compacting);
        ui.set_context_tooltip(SharedString::from(tooltip.as_str()));
    });
}

pub fn set_usage_text(ui: &Ui, text: String) {
    let _ =
        ui.upgrade_in_event_loop(move |ui| ui.set_usage_text(SharedString::from(text.as_str())));
}

/// Plain-data mirror of the Slint PrItem struct.
pub struct PrView {
    pub title: String,
    pub state: String,
    pub meta: String,
    pub url: String,
    pub checks: String,
    pub reviews: String,
}

pub fn set_prs(
    ui: &Ui,
    configured: bool,
    error: &str,
    labels: Vec<String>,
    items: Vec<PrView>,
    selected: usize,
) {
    let error = error.to_string();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let labels: Vec<SharedString> = labels
            .into_iter()
            .map(|l| SharedString::from(l.as_str()))
            .collect();
        let items: Vec<crate::PrItem> = items
            .into_iter()
            .map(|p| crate::PrItem {
                title: SharedString::from(p.title.as_str()),
                state: SharedString::from(p.state.as_str()),
                meta: SharedString::from(p.meta.as_str()),
                url: SharedString::from(p.url.as_str()),
                checks: SharedString::from(p.checks.as_str()),
                reviews: SharedString::from(p.reviews.as_str()),
            })
            .collect();
        ui.set_pr_configured(configured);
        ui.set_pr_error(SharedString::from(error.as_str()));
        ui.set_pr_labels(ModelRc::new(VecModel::from(labels)));
        ui.set_pr_items(ModelRc::new(VecModel::from(items)));
        ui.set_pr_selected(selected as i32);
    });
}

/// Plain-data mirror of the Slint SubscriptionItem struct; windows are
/// (label, used-percent, resets) tuples.
pub struct SubscriptionView {
    pub provider: String,
    pub status: String,
    pub plan: String,
    pub credits: String,
    pub note: String,
    pub w1: Option<(String, i64, String)>,
    pub w2: Option<(String, i64, String)>,
}

pub fn set_subscriptions(ui: &Ui, items: Vec<SubscriptionView>, status: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<crate::SubscriptionItem> = items
            .into_iter()
            .map(|s| {
                let window = |w: &Option<(String, i64, String)>| {
                    w.clone().unwrap_or((String::new(), 0, String::new()))
                };
                let (w1_label, w1_pct, w1_resets) = window(&s.w1);
                let (w2_label, w2_pct, w2_resets) = window(&s.w2);
                crate::SubscriptionItem {
                    provider: SharedString::from(s.provider.as_str()),
                    status: SharedString::from(s.status.as_str()),
                    plan: SharedString::from(s.plan.as_str()),
                    credits: SharedString::from(s.credits.as_str()),
                    note: SharedString::from(s.note.as_str()),
                    has_w1: s.w1.is_some(),
                    w1_label: SharedString::from(w1_label.as_str()),
                    w1_pct: w1_pct as i32,
                    w1_resets: SharedString::from(w1_resets.as_str()),
                    has_w2: s.w2.is_some(),
                    w2_label: SharedString::from(w2_label.as_str()),
                    w2_pct: w2_pct as i32,
                    w2_resets: SharedString::from(w2_resets.as_str()),
                }
            })
            .collect();
        ui.set_settings_subscriptions(ModelRc::new(VecModel::from(items)));
        ui.set_settings_subscriptions_status(SharedString::from(status.as_str()));
    });
}

/// Plain-data mirror of the Slint McpServerItem struct.
pub struct McpView {
    pub name: String,
    pub scope: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub command_line: String,
    pub env_lines: String,
    pub health: String,
    pub detail: String,
}

pub fn set_mcp_servers(ui: &Ui, items: Vec<McpView>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<crate::McpServerItem> = items
            .into_iter()
            .map(|s| crate::McpServerItem {
                name: SharedString::from(s.name.as_str()),
                scope: SharedString::from(s.scope.as_str()),
                workspace_id: SharedString::from(s.workspace_id.as_str()),
                workspace_name: SharedString::from(s.workspace_name.as_str()),
                command_line: SharedString::from(s.command_line.as_str()),
                env_lines: SharedString::from(s.env_lines.as_str()),
                health: SharedString::from(s.health.as_str()),
                detail: SharedString::from(s.detail.as_str()),
            })
            .collect();
        ui.set_settings_mcp_servers(ModelRc::new(VecModel::from(items)));
    });
}

/// The current session's effective MCP config for the right-panel tab.
pub fn set_session_mcp(ui: &Ui, items: Vec<McpView>, status: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<crate::McpServerItem> = items
            .into_iter()
            .map(|s| crate::McpServerItem {
                name: SharedString::from(s.name.as_str()),
                scope: SharedString::from(s.scope.as_str()),
                workspace_id: SharedString::from(s.workspace_id.as_str()),
                workspace_name: SharedString::from(s.workspace_name.as_str()),
                command_line: SharedString::from(s.command_line.as_str()),
                env_lines: SharedString::from(s.env_lines.as_str()),
                health: SharedString::from(s.health.as_str()),
                detail: SharedString::from(s.detail.as_str()),
            })
            .collect();
        ui.set_session_mcp_servers(ModelRc::new(VecModel::from(items)));
        ui.set_session_mcp_status(SharedString::from(status.as_str()));
    });
}

/// Workspace picker options for the MCP add-to-workspace form.
pub fn set_mcp_workspaces(ui: &Ui, names: Vec<String>, ids: Vec<String>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_settings_mcp_workspace_names(string_model(names));
        ui.set_settings_mcp_workspace_ids(string_model(ids));
    });
}

pub fn set_mcp_status(ui: &Ui, status: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_settings_mcp_status(SharedString::from(status.as_str()));
    });
}

pub fn set_mcp_logs(ui: &Ui, name: String, text: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_settings_mcp_logs_name(SharedString::from(name.as_str()));
        ui.set_settings_mcp_logs_text(SharedString::from(text.as_str()));
    });
}

pub fn set_github_integration(ui: &Ui, configured: bool, source: &str) {
    let source = source.to_string();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_settings_github_configured(configured);
        ui.set_settings_github_source(SharedString::from(source.as_str()));
    });
}

pub fn set_settings_section(ui: &Ui, section: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_settings_section(section));
}

pub fn set_diff(ui: &Ui, rows: Vec<slint_diff_view::RowData>, raw: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<DiffRow> = rows
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
        let file_texts: Vec<SharedString> = slint_diff_view::split_file_diffs(&raw)
            .into_iter()
            .map(|s| SharedString::from(s.as_str()))
            .collect();
        ui.set_diff_rows(ModelRc::new(VecModel::from(items)));
        ui.set_diff_file_texts(ModelRc::new(VecModel::from(file_texts)));
        ui.set_diff_text(SharedString::from(raw.as_str()));
    });
}

/// Push the terminal screen: styled rows (resolved RGB spans from the
/// vt100 grid), cursor cell (None = hidden), scrollback offset in lines,
/// a status note ("shell exited"), and whether a terminal is attached.
pub fn set_term(
    ui: &Ui,
    rows: Vec<Vec<slint_terminal::GridSpan>>,
    cursor: Option<(u16, u16)>,
    scrollback: usize,
    status: String,
    attached: bool,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let lines: Vec<ModelRc<crate::TermSpan>> = rows
            .into_iter()
            .map(|spans| {
                let spans: Vec<crate::TermSpan> = spans
                    .into_iter()
                    .map(|s| crate::TermSpan {
                        text: SharedString::from(s.text.as_str()),
                        fg: slint::Color::from_argb_encoded(0xff00_0000 | s.fg),
                        bg: slint::Color::from_argb_encoded(0xff00_0000 | s.bg),
                        has_bg: s.has_bg,
                    })
                    .collect();
                ModelRc::new(VecModel::from(spans))
            })
            .collect();
        ui.set_term_lines(ModelRc::new(VecModel::from(lines)));
        let (row, col) = cursor
            .map(|(r, c)| (r as i32, c as i32))
            .unwrap_or((-1, -1));
        ui.set_term_cursor_row(row);
        ui.set_term_cursor_col(col);
        ui.set_term_scrollback(scrollback as i32);
        ui.set_term_status(SharedString::from(status.as_str()));
        ui.set_term_attached(attached);
    });
}

/// Rows of the Files tree, already flattened in display order:
/// (name, is_dir, depth, expanded).
pub fn set_file_list(ui: &Ui, entries: Vec<(String, bool, i32, bool)>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<FileItem> = entries
            .into_iter()
            .map(|(name, is_dir, depth, expanded)| FileItem {
                name: name.into(),
                is_dir,
                depth,
                expanded,
            })
            .collect();
        ui.set_file_entries(ModelRc::new(VecModel::from(items)));
    });
}

/// Select (and scroll to) a 0-based inclusive line range in the file view;
/// `from < 0` clears any selection. Bumping the seq is what makes the
/// CodeView apply it.
pub fn set_file_selection(ui: &Ui, from: i32, to: i32) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_file_sel_from(from);
        ui.set_file_sel_to(to);
        ui.set_file_sel_seq(ui.get_file_sel_seq().wrapping_add(1));
    });
}

pub fn set_file_view(ui: &Ui, name: String, content: String, lines: Vec<Vec<(String, u32)>>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        // Markdown files get a rendered-preview toggle; the preview reuses
        // the chat's markdown row shape and renderer.
        let is_md = std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
        let preview: Vec<ChatRow> = if is_md {
            crate::render::markdown_rows(&content)
                .iter()
                .map(to_chat_row)
                .collect()
        } else {
            Vec::new()
        };
        ui.set_file_is_markdown(is_md);
        ui.set_file_preview_rows(ModelRc::new(VecModel::from(preview)));
        ui.set_file_preview(false);

        let count = lines.len();
        let rows: Vec<ModelRc<TextSegment>> = lines
            .into_iter()
            .map(|segments| {
                let segs: Vec<TextSegment> = segments
                    .into_iter()
                    .map(|(text, rgb)| TextSegment {
                        text: SharedString::from(text.as_str()),
                        color: slint::Color::from_argb_encoded(0xff00_0000 | rgb),
                    })
                    .collect();
                ModelRc::new(VecModel::from(segs))
            })
            .collect();
        ui.set_file_lines(ModelRc::new(VecModel::from(rows)));
        ui.set_file_numbers(ModelRc::new(VecModel::from(
            (1..=count as i32).collect::<Vec<i32>>(),
        )));
        ui.set_open_file_name(SharedString::from(name.as_str()));
        ui.set_open_file_text(SharedString::from(content.as_str()));
    });
}

// --- settings screen ---------------------------------------------------------

/// (id, kind, base_url, has_credentials, auth, experimental) per provider.
pub fn set_settings_data(
    ui: &Ui,
    providers: Vec<(String, String, String, bool, String, bool)>,
    models: Vec<String>,
    default_model_index: i32,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<ProviderItem> = providers
            .into_iter()
            .map(
                |(id, kind, base_url, has_credentials, auth, experimental)| ProviderItem {
                    id: id.into(),
                    kind: kind.into(),
                    base_url: base_url.into(),
                    has_credentials,
                    auth: auth.into(),
                    experimental,
                },
            )
            .collect();
        ui.set_settings_providers(ModelRc::new(VecModel::from(items)));
        ui.set_settings_models(string_model(models));
        ui.set_settings_default_model_index(default_model_index);
    });
}

/// Plain-data mirror of the Slint ModeItem struct.
pub struct ModeView {
    pub id: String,
    pub display_name: String,
    pub origin: String,
    pub read_only: bool,
    pub system_prompt: String,
    pub allowed_tools: String,
    pub permission_index: i32,
    pub model_index: i32,
}

/// Mode cards for the Modes & Models section, plus the per-mode model
/// picker options ("Global default" + every model id).
pub fn set_settings_modes(ui: &Ui, modes: Vec<ModeView>, mut model_names: Vec<String>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<crate::ModeItem> = modes
            .into_iter()
            .map(|m| crate::ModeItem {
                id: SharedString::from(m.id.as_str()),
                display_name: SharedString::from(m.display_name.as_str()),
                origin: SharedString::from(m.origin.as_str()),
                read_only: m.read_only,
                system_prompt: SharedString::from(m.system_prompt.as_str()),
                allowed_tools: SharedString::from(m.allowed_tools.as_str()),
                permission_index: m.permission_index,
                model_index: m.model_index,
            })
            .collect();
        ui.set_settings_mode_items(ModelRc::new(VecModel::from(items)));
        model_names.insert(0, "Global default".into());
        ui.set_settings_mode_model_names(string_model(model_names));
    });
}

/// Aligned with the composer/new-chat mode picker: each mode's default
/// model as an index into the models list (-1 = none).
pub fn set_mode_model_indices(ui: &Ui, indices: Vec<i32>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_mode_model_indices(ModelRc::new(VecModel::from(indices)));
    });
}

/// One vendor-CLI row for the settings Providers section (plain-data
/// mirror of Slint's `CliItem`).
pub struct CliView {
    pub id: String,
    pub display_name: String,
    pub version_label: String,
    pub action_label: String,
    pub status: String,
    pub busy: bool,
    /// Download percent while busy (-1 when the size is unknown).
    pub progress: i32,
    /// A trouve-managed install exists (can be uninstalled).
    pub managed: bool,
}

pub fn set_clis(ui: &Ui, clis: Vec<CliView>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<CliItem> = clis
            .into_iter()
            .map(|c| CliItem {
                id: c.id.into(),
                display_name: c.display_name.into(),
                version_label: c.version_label.into(),
                action_label: c.action_label.into(),
                status: c.status.into(),
                busy: c.busy,
                progress: c.progress,
                managed: c.managed,
            })
            .collect();
        ui.set_settings_clis(ModelRc::new(VecModel::from(items)));
    });
}

/// One local model row for the settings Local Models section (plain-data
/// mirror of Slint's `LocalModelItem`).
pub struct LocalModelView {
    pub id: String,
    pub name: String,
    /// "7B · 4.7 GB"
    pub meta: String,
    /// "gpu" / "cpu" / "too-large"
    pub fit: String,
    pub fit_label: String,
    pub notes: String,
    pub downloaded: bool,
    pub downloading: bool,
    /// Download progress percent (0-99 while pending).
    pub progress: i32,
    pub error: String,
    pub custom: bool,
}

/// Everything the Local Models settings section shows.
pub struct LocalView {
    pub enabled: bool,
    pub hw_line: String,
    pub runtime_label: String,
    /// "Install" when not installed, "" otherwise.
    pub runtime_action: String,
    pub runtime_busy: bool,
    /// Download percent while busy (-1 when the size is unknown).
    pub runtime_progress: i32,
    /// Managed install (updatable/uninstallable here).
    pub runtime_managed: bool,
    /// A newer llama.cpp build is available for a managed install.
    pub runtime_update: bool,
    pub runtime_status: String,
    /// "llama-server is running <model>" or "" when stopped.
    pub server_line: String,
    /// Sidecar is loading a model (stop/restart hidden until it settles).
    pub server_busy: bool,
    pub models: Vec<LocalModelView>,
}

pub fn set_local(ui: &Ui, view: LocalView) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<crate::LocalModelItem> = view
            .models
            .into_iter()
            .map(|m| crate::LocalModelItem {
                id: SharedString::from(m.id.as_str()),
                name: SharedString::from(m.name.as_str()),
                meta: SharedString::from(m.meta.as_str()),
                fit: SharedString::from(m.fit.as_str()),
                fit_label: SharedString::from(m.fit_label.as_str()),
                notes: SharedString::from(m.notes.as_str()),
                downloaded: m.downloaded,
                downloading: m.downloading,
                progress: m.progress,
                error: SharedString::from(m.error.as_str()),
                custom: m.custom,
            })
            .collect();
        ui.set_local_enabled(view.enabled);
        ui.set_local_hw_line(SharedString::from(view.hw_line.as_str()));
        ui.set_local_runtime_label(SharedString::from(view.runtime_label.as_str()));
        ui.set_local_runtime_action(SharedString::from(view.runtime_action.as_str()));
        ui.set_local_runtime_busy(view.runtime_busy);
        ui.set_local_runtime_progress(view.runtime_progress);
        ui.set_local_runtime_managed(view.runtime_managed);
        ui.set_local_runtime_update(view.runtime_update);
        ui.set_local_runtime_status(SharedString::from(view.runtime_status.as_str()));
        ui.set_local_server_line(SharedString::from(view.server_line.as_str()));
        ui.set_local_server_busy(view.server_busy);
        ui.set_local_models(ModelRc::new(VecModel::from(items)));
    });
}

/// Error/status line for the Local Models section.
pub fn set_local_status(ui: &Ui, status: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_local_status(SharedString::from(status.as_str()));
    });
}

/// Clear the custom-GGUF form after a successful add.
pub fn clear_local_form(ui: &Ui) {
    let _ = ui.upgrade_in_event_loop(|ui| {
        ui.set_local_form_repo(SharedString::new());
        ui.set_local_form_file(SharedString::new());
    });
}

pub fn set_known_providers(ui: &Ui, mut known: Vec<trouve_protocol::KnownProvider>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        // Presets alphabetically, then "Custom" (hand-entered details) last;
        // preset-index i maps to known-providers[i], Custom is index == len.
        known.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        });
        let mut names: Vec<String> = known.iter().map(|k| k.display_name.clone()).collect();
        names.push("Custom".into());
        let custom_index = known.len() as i32;
        let items: Vec<KnownProviderItem> = known
            .into_iter()
            .map(|k| KnownProviderItem {
                id: k.id.into(),
                display_name: k.display_name.into(),
                kind: k.kind.into(),
                base_url: k.base_url.unwrap_or_default().into(),
                api_key_env: k.api_key_env.unwrap_or_default().into(),
                auth: k.auth.into(),
                experimental: k.experimental,
            })
            .collect();
        use slint::Model as _;
        let first_load = ui.get_settings_known_provider_names().row_count() == 0;
        ui.set_settings_known_providers(ModelRc::new(VecModel::from(items)));
        ui.set_settings_known_provider_names(string_model(names));
        // Start on "Custom"; later refreshes keep the user's selection.
        if first_load {
            ui.set_settings_preset_index(custom_index);
        }
    });
}

pub fn set_settings_status(ui: &Ui, text: String) {
    let _ = ui
        .upgrade_in_event_loop(move |ui| ui.set_settings_status(SharedString::from(text.as_str())));
}

fn string_model(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}
