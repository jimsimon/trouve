//! UI-thread bridge: converts plain data from the controller into generated
//! Slint models. Every function here is safe to call from any thread — the
//! conversion happens inside `upgrade_in_event_loop`.

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::render::ChatRowData;
use crate::{
    AppWindow, ChatRow, DiffRow, FileItem, KnownProviderItem, NavRow, ProviderItem, SettingsWindow,
    TextSegment, ThreadTabItem,
};

type Ui = slint::Weak<AppWindow>;
type SettingsUi = slint::Weak<SettingsWindow>;

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

pub fn set_status(ui: &Ui, text: &str) {
    let text = text.to_string();
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_status_text(SharedString::from(text)));
}

pub fn set_error(ui: &Ui, text: &str) {
    let text = text.to_string();
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_error_text(SharedString::from(text)));
}

pub fn set_pickers(ui: &Ui, modes: Vec<String>, models: Vec<String>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_modes(string_model(modes));
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
    fast_visible: bool,
    fast_checked: bool,
    max_mode: bool,
) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_thinking_options(string_model(thinking_options));
        ui.set_thinking_index(thinking_index);
        ui.set_fast_visible(fast_visible);
        ui.set_fast_checked(fast_checked);
        ui.set_max_mode(max_mode);
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
    let code_lines: ModelRc<ModelRc<TextSegment>> =
        if r.kind == 1 && r.md_kind == 5 {
            let lines: Vec<ModelRc<TextSegment>> =
                crate::render::highlight_code(&r.md_lang, &r.text)
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
        detail: SharedString::from(r.detail.as_str()),
        expanded: r.expanded,
        turn_state: r.turn_state,
        turn: r.turn,
        raw: r.raw,
        card_key: SharedString::from(r.card_key.as_str()),
        card_pos: r.card_pos,
        card_first: r.card_first,
        meta: SharedString::from(r.meta.as_str()),
    }
}

pub fn set_chat(ui: &Ui, rows: Vec<ChatRowData>, scroll_to_end: bool) {
    use slint::Model as _;
    use std::cell::RefCell;

    // The previous render's source rows, for diffing (UI thread only).
    thread_local! {
        static LAST_CHAT: RefCell<Vec<ChatRowData>> = const { RefCell::new(Vec::new()) };
    }

    let _ = ui.upgrade_in_event_loop(move |ui| {
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
        if scroll_to_end {
            ui.invoke_scroll_chat_to_end();
        }
    });
}

pub fn set_composer_enabled(ui: &Ui, enabled: bool) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_composer_enabled(enabled));
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

pub fn set_pr_status(ui: &Ui, text: String) {
    let _ = ui.upgrade_in_event_loop(move |ui| ui.set_pr_status(SharedString::from(text.as_str())));
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
        ui.set_diff_rows(ModelRc::new(VecModel::from(items)));
        ui.set_diff_text(SharedString::from(raw.as_str()));
    });
}

pub fn set_file_list(ui: &Ui, path: String, entries: Vec<(String, bool)>) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let items: Vec<FileItem> = entries
            .into_iter()
            .map(|(name, is_dir)| FileItem {
                name: name.into(),
                is_dir,
            })
            .collect();
        ui.set_file_path(SharedString::from(path.as_str()));
        ui.set_file_entries(ModelRc::new(VecModel::from(items)));
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

// --- settings window ---------------------------------------------------------

pub fn show_settings(ui: &SettingsUi) {
    let _ = ui.upgrade_in_event_loop(move |ui| {
        let _ = ui.show();
    });
}

/// (id, kind, base_url, has_credentials, auth, experimental) per provider.
pub fn set_settings_data(
    ui: &SettingsUi,
    providers: Vec<(String, String, String, bool, String, bool)>,
    models: Vec<String>,
    default_model_index: i32,
    modes: Vec<String>,
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
        ui.set_providers(ModelRc::new(VecModel::from(items)));
        ui.set_models(string_model(models));
        ui.set_default_model_index(default_model_index);
        ui.set_modes(string_model(modes));
    });
}

pub fn set_known_providers(ui: &SettingsUi, mut known: Vec<trouve_protocol::KnownProvider>) {
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
        let first_load = ui.get_known_provider_names().row_count() == 0;
        ui.set_known_providers(ModelRc::new(VecModel::from(items)));
        ui.set_known_provider_names(string_model(names));
        // Start on "Custom"; later refreshes keep the user's selection.
        if first_load {
            ui.set_preset_index(custom_index);
        }
    });
}

pub fn set_settings_status(ui: &SettingsUi, text: String) {
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
