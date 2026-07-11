//! trouve desktop client. Spawns the server as a child process (all traffic
//! still goes over the protocol on localhost) and runs the Slint UI.

mod controller;
mod notify;
mod render;
mod theme;
mod ui;
mod winstate;

slint::include_modules!();

use controller::UiCommand;
use slint::{ComponentHandle, Model};

/// Indices into `items` fuzzy-matching `query`, best score first (stable by
/// position on ties). An empty query keeps the full list in its own order.
fn fuzzy_match_indices(items: &[String], query: &str) -> Vec<i32> {
    use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
    let query = query.trim();
    if query.is_empty() {
        return (0..items.len() as i32).collect();
    }
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| matcher.fuzzy_match(s, query).map(|score| (score, i as i32)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Wayland/X11 app id. Compositors resolve taskbar/titlebar icons through a
    // desktop file matching this id (see packaging/linux/trouve.desktop);
    // must be set after the backend is initialized but before the window is
    // created.
    slint::BackendSelector::new()
        .select()
        .map_err(|e| anyhow::anyhow!("failed to initialize UI backend: {e}"))?;
    slint::set_xdg_app_id("trouve")?;

    let window = AppWindow::new()?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<UiCommand>();

    // Window focus for the controller's notification gate (events on the
    // focused, on-screen thread never pop a desktop notification). Sampled
    // off winit by the 1s geometry poll below; starts false so a launch
    // that never gains focus (or a locked screen) doesn't suppress
    // notifications.
    let window_focused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // --- appearance: restore, populate the pickers, wire the callbacks ------
    // All handled here on the UI thread (palette swaps are direct property
    // writes); the controller is only pinged to re-render baked colors.
    let appearance = std::rc::Rc::new(std::cell::RefCell::new(winstate::load_appearance()));
    let font_families = std::rc::Rc::new(theme::font_families());
    {
        let a = appearance.borrow();
        window.set_appearance_theme_names(slint::ModelRc::new(slint::VecModel::from(
            theme::THEMES
                .iter()
                .map(|t| slint::SharedString::from(t.name))
                .collect::<Vec<_>>(),
        )));
        window.set_appearance_theme_index(
            theme::THEMES
                .iter()
                .position(|t| t.id == a.theme)
                .unwrap_or(0) as i32,
        );
        window.set_appearance_font_size_names(slint::ModelRc::new(slint::VecModel::from(
            theme::FONT_SIZES
                .iter()
                .map(|s| slint::SharedString::from(format!("{s} px")))
                .collect::<Vec<_>>(),
        )));
        window.set_appearance_font_size_index(
            theme::FONT_SIZES
                .iter()
                .position(|s| *s == a.font_size)
                .unwrap_or(2) as i32,
        );
        let mut font_names = vec![slint::SharedString::from("System default")];
        font_names.extend(
            font_families
                .iter()
                .map(|f| slint::SharedString::from(f.as_str())),
        );
        window.set_appearance_font_names(slint::ModelRc::new(slint::VecModel::from(font_names)));
        window.set_appearance_font_index(
            font_families
                .iter()
                .position(|f| *f == a.font_family)
                .map(|i| i as i32 + 1)
                .unwrap_or(0),
        );
        window.set_appearance_reduce_motion(a.reduce_motion);
        theme::apply(&window, &a);
    }
    // Shared handler: mutate one field, re-apply, persist, and have the
    // controller re-render rows with baked (syntax/inline-code) colors.
    let on_appearance = {
        let appearance = appearance.clone();
        let weak = window.as_weak();
        let tx = tx.clone();
        move |change: &dyn Fn(&mut winstate::Appearance)| {
            let window = weak.unwrap();
            let mut a = appearance.borrow_mut();
            change(&mut a);
            theme::apply(&window, &a);
            winstate::save_appearance(&a);
            let _ = tx.send(UiCommand::AppearanceChanged);
        }
    };
    {
        let on_appearance = on_appearance.clone();
        window.on_appearance_theme_picked(move |i| {
            on_appearance(&|a| {
                if let Some(t) = theme::THEMES.get(i.max(0) as usize) {
                    a.theme = t.id.to_string();
                }
            });
        });
    }
    {
        let on_appearance = on_appearance.clone();
        window.on_appearance_font_size_picked(move |i| {
            on_appearance(&|a| {
                if let Some(s) = theme::FONT_SIZES.get(i.max(0) as usize) {
                    a.font_size = *s;
                }
            });
        });
    }
    {
        let on_appearance = on_appearance.clone();
        let font_families = font_families.clone();
        window.on_appearance_font_picked(move |i| {
            on_appearance(&|a| {
                // Index 0 is "System default" (empty family).
                a.font_family = match i.max(0) as usize {
                    0 => String::new(),
                    i => font_families.get(i - 1).cloned().unwrap_or_default(),
                };
            });
        });
    }
    {
        let on_appearance = on_appearance.clone();
        window.on_appearance_reduce_motion_toggled(move |on| {
            on_appearance(&|a| a.reduce_motion = on);
        });
    }

    // --- notifications: restore, wire the toggles ----------------------------
    // Persisted on this thread like appearance; the controller keeps a copy
    // to gate what event notifications fire.
    {
        let prefs = winstate::load_notifications();
        window.set_notify_enabled(prefs.enabled);
        window.set_notify_finish(prefs.on_finish);
        window.set_notify_fail(prefs.on_fail);
        window.set_notify_attention(prefs.on_attention);
        window.set_notify_sound(prefs.sound);
        let prefs = std::rc::Rc::new(std::cell::RefCell::new(prefs));
        let tx_prefs = tx.clone();
        window.on_notify_pref_toggled(move |which, on| {
            let mut p = prefs.borrow_mut();
            match which {
                0 => p.enabled = on,
                1 => p.on_finish = on,
                2 => p.on_fail = on,
                3 => p.on_attention = on,
                _ => p.sound = on,
            }
            winstate::save_notifications(&p);
            let _ = tx_prefs.send(UiCommand::NotifyPrefsChanged(p.clone()));
        });
    }
    {
        let tx = tx.clone();
        window.on_notify_test(move || {
            let _ = tx.send(UiCommand::NotifyTest);
        });
    }

    // --- main window callbacks → controller commands -------------------------
    {
        let tx = tx.clone();
        window.on_nav_row_clicked(move |row| {
            let _ = tx.send(UiCommand::NavRowClicked(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_new_session(move || {
            let _ = tx.send(UiCommand::NewSession);
        });
    }
    {
        let tx = tx.clone();
        window.on_open_workspace(move || {
            let _ = tx.send(UiCommand::OpenWorkspaceDialog);
        });
    }
    window.on_open_link(|url| {
        // http(s) only: chat markdown is model output, so no file:// or
        // arbitrary schemes reach the system opener.
        if url.starts_with("https://") || url.starts_with("http://") {
            let _ = open::that_detached(url.as_str());
        }
    });
    {
        // Model search picker: fuzzy-filter the model list. Pure UI-thread
        // work, so it never round-trips through the controller.
        let weak = window.as_weak();
        window.on_model_filter_changed(move |query| {
            let window = weak.unwrap();
            let models: Vec<String> = window.get_models().iter().map(|s| s.to_string()).collect();
            let matches = fuzzy_match_indices(&models, &query);
            window.set_model_filter_matches(slint::ModelRc::new(slint::VecModel::from(matches)));
        });
    }
    {
        // "/" skill completion: re-rank on every composer edit. The popup is
        // live while the draft is a bare "/query" first token (no whitespace
        // yet — a space means the user is typing arguments).
        let weak = window.as_weak();
        window.on_slash_filter_changed(move |text| {
            let window = weak.unwrap();
            let query = match text.strip_prefix('/') {
                Some(q) if !q.contains(char::is_whitespace) => q,
                _ => {
                    window.set_slash_active(false);
                    return;
                }
            };
            let names: Vec<String> = window
                .get_slash_commands()
                .iter()
                .map(|s| s.to_string())
                .collect();
            let mut matches = fuzzy_match_indices(&names, query);
            matches.truncate(8);
            match matches.first() {
                Some(&top) => {
                    window.set_slash_completion(format!("/{}", names[top as usize]).into());
                    window.set_slash_matches(slint::ModelRc::new(slint::VecModel::from(matches)));
                    window.set_slash_active(true);
                }
                None => window.set_slash_active(false),
            }
        });
    }
    {
        let tx = tx.clone();
        window.on_workspace_new_session(move |row| {
            let _ = tx.send(UiCommand::WorkspaceNewSession(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_open_settings(move || {
            let _ = tx.send(UiCommand::OpenSettings);
        });
    }
    {
        let tx = tx.clone();
        window.on_session_renamed(move |row, title| {
            let _ = tx.send(UiCommand::SessionRename {
                row: row as usize,
                title: title.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_session_archived(move |row, archived| {
            let _ = tx.send(UiCommand::SessionArchive {
                row: row as usize,
                archived,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_session_deleted(move |row| {
            let _ = tx.send(UiCommand::SessionDelete { row: row as usize });
        });
    }
    {
        let tx = tx.clone();
        window.on_thread_selected(move |i| {
            let _ = tx.send(UiCommand::SelectThread(i as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_new_thread(move || {
            let _ = tx.send(UiCommand::NewThread);
        });
    }
    {
        let tx = tx.clone();
        window.on_cancel_new_chat(move || {
            let _ = tx.send(UiCommand::CancelNewChat);
        });
    }
    {
        let tx = tx.clone();
        window.on_nc_workspace_changed(move |i| {
            let _ = tx.send(UiCommand::NewChatWorkspaceChanged(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_register_workspace_path(move |path| {
            let _ = tx.send(UiCommand::RegisterWorkspacePath(path.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_start_new_chat(move |ws, branch, mode, model, prompt| {
            let _ = tx.send(UiCommand::StartNewChat {
                workspace_idx: ws.max(0) as usize,
                branch_idx: branch.max(0) as usize,
                mode_idx: mode.max(0) as usize,
                model_idx: model.max(0) as usize,
                prompt: prompt.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_send_message(move |text| {
            let _ = tx.send(UiCommand::SendMessage(text.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_attach_file(move || {
            let _ = tx.send(UiCommand::AttachFileDialog);
        });
    }
    {
        let tx = tx.clone();
        window.on_attachment_removed(move |index| {
            let _ = tx.send(UiCommand::AttachmentRemoved(index.max(0) as usize));
        });
    }
    {
        // Ctrl/Cmd+V in the composer: if the clipboard holds an image
        // (a screenshot, usually), stage it as an attachment and swallow
        // the paste; otherwise let the TextInput paste text as normal.
        // Checked synchronously on the UI thread — clipboard reads are
        // local IPC and small relative to a keystroke.
        let tx = tx.clone();
        window.on_paste_image_attempted(move || match clipboard_image_png() {
            Some(bytes) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = tx.send(UiCommand::AddAttachment {
                    name: format!("pasted-{stamp}.png"),
                    mime: "image/png".into(),
                    bytes,
                });
                true
            }
            None => false,
        });
    }
    {
        let tx = tx.clone();
        window.on_queue_edited(move |index, content| {
            let _ = tx.send(UiCommand::QueueEdit {
                index: index.max(0) as usize,
                content: content.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_queue_deleted(move |index| {
            let _ = tx.send(UiCommand::QueueDelete(index.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_queue_moved(move |index, delta| {
            let _ = tx.send(UiCommand::QueueMove {
                index: index.max(0) as usize,
                delta,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_queue_reordered(move |from, to| {
            let _ = tx.send(UiCommand::QueueReorder {
                from: from.max(0) as usize,
                to: to.max(0) as usize,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_queue_send_now(move || {
            let _ = tx.send(UiCommand::QueueSendNow);
        });
    }
    {
        let tx = tx.clone();
        window.on_approval_resolved(move |row, approved| {
            let _ = tx.send(UiCommand::Approval {
                row: row as usize,
                approved,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_question_option_toggled(move |row, option| {
            let _ = tx.send(UiCommand::QuestionOption {
                row: row as usize,
                option: option.max(0) as usize,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_question_other_edited(move |row, text| {
            let _ = tx.send(UiCommand::QuestionOtherEdited {
                row: row as usize,
                text: text.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_question_back(move |row| {
            let _ = tx.send(UiCommand::QuestionBack(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_question_next(move |row| {
            let _ = tx.send(UiCommand::QuestionNext(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_question_skip(move |row| {
            let _ = tx.send(UiCommand::QuestionSkip(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_tool_toggled(move |row| {
            let _ = tx.send(UiCommand::ToggleTool(row as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_raw_toggled(move |turn| {
            let _ = tx.send(UiCommand::ToggleRawTurn(turn.max(0) as u64));
        });
    }
    {
        let tx = tx.clone();
        window.on_card_toggled(move |key| {
            let _ = tx.send(UiCommand::ToggleCard(key.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_composer_mode_changed(move |i| {
            let _ = tx.send(UiCommand::ComposerModeChanged(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_composer_model_changed(move |i| {
            let _ = tx.send(UiCommand::ComposerModelChanged(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_composer_thinking_changed(move |i| {
            let _ = tx.send(UiCommand::ComposerThinkingChanged(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_composer_context_changed(move |i| {
            let _ = tx.send(UiCommand::ComposerContextChanged(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_composer_fast_toggled(move |on| {
            let _ = tx.send(UiCommand::ComposerFastToggled(on));
        });
    }
    {
        let tx = tx.clone();
        window.on_diff_file_toggled(move |i| {
            let _ = tx.send(UiCommand::ToggleDiffFile(i as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_file_activated(move |i| {
            let _ = tx.send(UiCommand::FileActivated(i as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_file_opened_externally(move |path| {
            let _ = tx.send(UiCommand::OpenFileExternally(path.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_right_tab_changed(move |tab| {
            let _ = tx.send(UiCommand::RightTabChanged(tab));
        });
    }
    {
        // shift is folded into the key text by Slint already; Ctrl+Shift+V
        // (paste) never reaches this callback.
        let tx = tx.clone();
        window.on_term_key(move |text, ctrl, alt, _shift| {
            let _ = tx.send(UiCommand::TermKey {
                text: text.to_string(),
                ctrl,
                alt,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_term_paste(move |text| {
            let _ = tx.send(UiCommand::TermPaste(text.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_term_wheel(move |lines| {
            let _ = tx.send(UiCommand::TermWheel(lines));
        });
    }
    {
        let tx = tx.clone();
        window.on_term_resized(move |cols, rows| {
            let _ = tx.send(UiCommand::TermResized {
                cols: cols.clamp(2, 500) as u16,
                rows: rows.clamp(2, 500) as u16,
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_term_restart(move || {
            let _ = tx.send(UiCommand::TermRestart);
        });
    }
    {
        let tx = tx.clone();
        window.on_chat_file_opened(move |path, from, to| {
            let _ = tx.send(UiCommand::OpenChatFile(path.to_string(), from, to));
        });
    }
    {
        let tx = tx.clone();
        window.on_archived_filter_toggled(move || {
            let _ = tx.send(UiCommand::ToggleArchivedFilter);
        });
    }

    // Closing with agents mid-turn asks first (quit / quit when idle /
    // cancel) instead of tearing the run down silently.
    {
        let weak = window.as_weak();
        window.window().on_close_requested(move || {
            let window = weak.unwrap();
            if window.get_agents_running() > 0 {
                window.set_quit_dialog(true);
                slint::CloseRequestResponse::KeepWindowShown
            } else {
                slint::CloseRequestResponse::HideWindow
            }
        });
    }
    window.on_quit_now(|| {
        let _ = slint::quit_event_loop();
    });
    {
        let tx = tx.clone();
        window.on_quit_when_idle(move || {
            let _ = tx.send(UiCommand::QuitWhenIdle);
        });
    }
    {
        let tx = tx.clone();
        window.on_undo_turn(move || {
            let _ = tx.send(UiCommand::Undo);
        });
    }
    {
        let tx = tx.clone();
        window.on_redo_turn(move || {
            let _ = tx.send(UiCommand::Redo);
        });
    }
    {
        let tx = tx.clone();
        window.on_create_pr(move || {
            let _ = tx.send(UiCommand::CreatePr);
        });
    }
    {
        let tx = tx.clone();
        window.on_refresh_prs(move || {
            let _ = tx.send(UiCommand::RefreshPrs);
        });
    }
    {
        let tx = tx.clone();
        window.on_refresh_session_mcp(move || {
            let _ = tx.send(UiCommand::RefreshSessionMcp);
        });
    }
    {
        let tx = tx.clone();
        window.on_pr_picked(move |i| {
            let _ = tx.send(UiCommand::SelectPr(i as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_open_pr_url(move |url| {
            let _ = tx.send(UiCommand::OpenPrUrl(url.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_open_integrations_settings(move || {
            let _ = tx.send(UiCommand::OpenIntegrationsSettings);
        });
    }
    {
        let tx = tx.clone();
        window.on_github_token_saved(move |token| {
            let _ = tx.send(UiCommand::SaveGithubToken(token.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_mcp_refresh(move || {
            let _ = tx.send(UiCommand::RefreshMcp);
        });
    }
    {
        let tx = tx.clone();
        window.on_mcp_saved(move |name, scope, command, env, workspace_id| {
            let _ = tx.send(UiCommand::SaveMcpServer {
                name: name.to_string(),
                scope: scope.to_string(),
                command_line: command.to_string(),
                env_lines: env.to_string(),
                workspace_id: workspace_id.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_mcp_deleted(move |name, scope, workspace_id| {
            let _ = tx.send(UiCommand::DeleteMcpServer {
                name: name.to_string(),
                scope: scope.to_string(),
                workspace_id: workspace_id.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_mcp_logs_requested(move |name| {
            let _ = tx.send(UiCommand::McpLogs(name.to_string()));
        });
    }
    {
        let ui = window.as_weak();
        window.on_mcp_logs_closed(move || {
            if let Some(ui) = ui.upgrade() {
                ui.set_settings_mcp_logs_name(Default::default());
                ui.set_settings_mcp_logs_text(Default::default());
            }
        });
    }

    // --- settings screen callbacks -------------------------------------------
    {
        let tx = tx.clone();
        window.on_provider_saved(move |id, kind, base_url, api_key| {
            let _ = tx.send(UiCommand::SaveProvider {
                id: id.to_string(),
                kind: kind.to_string(),
                base_url: base_url.to_string(),
                api_key: api_key.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_provider_deleted(move |id| {
            let _ = tx.send(UiCommand::DeleteProvider(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_provider_login(move |id| {
            let _ = tx.send(UiCommand::ProviderLogin(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_default_model_picked(move |i| {
            let _ = tx.send(UiCommand::SetDefaultModel(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        window.on_mode_saved(move |id, display, prompt, tools, read_only, perm, model| {
            let _ = tx.send(UiCommand::SaveMode(
                id.to_string(),
                display.to_string(),
                prompt.to_string(),
                tools.to_string(),
                read_only,
                perm,
                model,
            ));
        });
    }
    {
        let tx = tx.clone();
        window.on_mode_deleted(move |id| {
            let _ = tx.send(UiCommand::DeleteMode(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_mode_model_picked(move |id, model| {
            let _ = tx.send(UiCommand::SetModeModel(id.to_string(), model));
        });
    }
    {
        let tx = tx.clone();
        window.on_refresh_settings(move || {
            let _ = tx.send(UiCommand::RefreshSettings);
        });
    }
    {
        let tx = tx.clone();
        window.on_cli_install(move |id| {
            let _ = tx.send(UiCommand::CliInstall(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_cli_cancel(move |id| {
            let _ = tx.send(UiCommand::CliCancel(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_cli_uninstall(move |id| {
            let _ = tx.send(UiCommand::CliUninstall(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_refresh(move || {
            let _ = tx.send(UiCommand::RefreshLocal);
        });
    }
    {
        let tx = tx.clone();
        window.on_local_enabled_toggled(move |enabled| {
            let _ = tx.send(UiCommand::LocalEnabledToggled(enabled));
        });
    }
    {
        // The llama.cpp runtime installs/cancels/uninstalls through the
        // same managed-CLI machinery as the vendor CLIs.
        let tx = tx.clone();
        window.on_local_runtime_install(move || {
            let _ = tx.send(UiCommand::CliInstall("llama-server".into()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_runtime_cancel(move || {
            let _ = tx.send(UiCommand::CliCancel("llama-server".into()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_runtime_uninstall(move || {
            let _ = tx.send(UiCommand::CliUninstall("llama-server".into()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_download(move |id| {
            let _ = tx.send(UiCommand::LocalDownload(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_cancel(move |id| {
            let _ = tx.send(UiCommand::LocalCancelDownload(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_delete(move |id| {
            let _ = tx.send(UiCommand::LocalDeleteModel(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_local_stop_server(move || {
            let _ = tx.send(UiCommand::LocalStopServer);
        });
    }
    {
        let tx = tx.clone();
        window.on_local_restart_server(move || {
            let _ = tx.send(UiCommand::LocalRestartServer);
        });
    }
    {
        let tx = tx.clone();
        window.on_local_added(move |repo, file| {
            let _ = tx.send(UiCommand::LocalAddModel {
                repo: repo.to_string(),
                file: file.to_string(),
            });
        });
    }
    {
        let tx = tx.clone();
        window.on_local_search(move |query| {
            let _ = tx.send(UiCommand::LocalSearch(query.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_close_settings(move || {
            let _ = tx.send(UiCommand::CloseSettings);
        });
    }
    {
        let tx = tx.clone();
        window.on_open_automations(move || {
            let _ = tx.send(UiCommand::OpenAutomations);
        });
    }
    {
        let tx = tx.clone();
        window.on_close_automations(move || {
            let _ = tx.send(UiCommand::CloseAutomations);
        });
    }
    {
        let tx = tx.clone();
        window.on_automations_refresh(move || {
            let _ = tx.send(UiCommand::RefreshAutomations);
        });
    }
    {
        let tx = tx.clone();
        window.on_automation_saved(
            move |id, name, prompt, workspace_id, kind, minute, time, days, enabled| {
                let _ = tx.send(UiCommand::SaveAutomation {
                    id: id.to_string(),
                    name: name.to_string(),
                    prompt: prompt.to_string(),
                    workspace_id: workspace_id.to_string(),
                    kind: kind.to_string(),
                    minute: minute.to_string(),
                    time: time.to_string(),
                    days: days.to_string(),
                    enabled,
                });
            },
        );
    }
    {
        let tx = tx.clone();
        window.on_automation_toggled(move |id, enabled| {
            let _ = tx.send(UiCommand::AutomationToggled(id.to_string(), enabled));
        });
    }
    {
        let tx = tx.clone();
        window.on_automation_run(move |id| {
            let _ = tx.send(UiCommand::RunAutomation(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        window.on_automation_deleted(move |id| {
            let _ = tx.send(UiCommand::DeleteAutomation(id.to_string()));
        });
    }

    // Controller (and spawned server) live on a background tokio runtime.
    let scroll_tx = tx.clone();
    let weak = window.as_weak();
    let focused = window_focused.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(controller::run(weak, tx, rx, focused));
    });

    // Restore the last window geometry (position picks the monitor too);
    // an absent or implausible file keeps the defaults from app.slint.
    let restored = winstate::load();
    if let Some(state) = restored {
        let w = window.window();
        w.set_size(slint::PhysicalSize::new(state.width, state.height));
        w.set_position(slint::PhysicalPosition::new(state.x, state.y));
        if state.maximized {
            w.set_maximized(true);
        }
        // Panel splitters (0 = never dragged, keep the slint defaults). The
        // slint-side clamps re-fit them if the window shrank meanwhile.
        if state.left_width > 0 {
            window.set_left_width(state.left_width as f32);
        }
        if state.right_width > 0 {
            window.set_right_width(state.right_width as f32);
        }
    }

    // Slint has no move/resize callbacks, so poll for geometry changes and
    // persist them as they happen. While maximized, keep the last floating
    // rect so unmaximizing on a later launch lands where it used to. The
    // same poll samples the chat scroll offset for the controller's
    // per-thread resume bookmark (scrolling has no callback either).
    let geometry_timer = slint::Timer::default();
    {
        let weak = window.as_weak();
        let last = std::cell::RefCell::new(restored);
        let last_scroll = std::cell::RefCell::new(f32::NAN);
        let focused = window_focused.clone();
        geometry_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(1),
            move || {
                let Some(window) = weak.upgrade() else { return };
                let w = window.window();
                {
                    use slint::winit_030::WinitWindowAccessor;
                    if let Some(f) = w.with_winit_window(|w| w.has_focus()) {
                        focused.store(f, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                let mut next = last.borrow().unwrap_or_default();
                next.maximized = w.is_maximized();
                if !next.maximized {
                    let pos = w.position();
                    let size = w.size();
                    (next.x, next.y) = (pos.x, pos.y);
                    (next.width, next.height) = (size.width, size.height);
                }
                next.left_width = window.get_left_width() as u32;
                next.right_width = window.get_right_width() as u32;
                {
                    let mut last = last.borrow_mut();
                    if *last != Some(next) {
                        winstate::save(&next);
                        *last = Some(next);
                    }
                }
                let scroll = window.get_chat_scroll();
                let mut last_scroll = last_scroll.borrow_mut();
                if *last_scroll != scroll {
                    *last_scroll = scroll;
                    let _ = scroll_tx.send(UiCommand::ChatScrolled(scroll));
                }
            },
        );
    }

    window.run()?;
    Ok(())
}

/// The clipboard's image as PNG bytes, or `None` when it holds no image
/// (or the clipboard isn't reachable). Used by the composer's Ctrl+V hook
/// to turn pasted screenshots into attachments.
fn clipboard_image_png() -> Option<Vec<u8>> {
    let image = arboard::Clipboard::new().ok()?.get_image().ok()?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.bytes).ok()?;
    }
    Some(out)
}
