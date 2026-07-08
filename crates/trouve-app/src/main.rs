//! trouve desktop client. Spawns the server as a child process (all traffic
//! still goes over the protocol on localhost) and runs the Slint UI.

mod controller;
mod render;
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

    let window = AppWindow::new()?;
    let settings = SettingsWindow::new()?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<UiCommand>();

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
        window.on_approval_resolved(move |row, approved| {
            let _ = tx.send(UiCommand::Approval {
                row: row as usize,
                approved,
            });
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
        window.on_refresh_diff(move || {
            let _ = tx.send(UiCommand::RefreshDiff);
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
        window.on_file_up(move || {
            let _ = tx.send(UiCommand::FileUp);
        });
    }
    {
        let tx = tx.clone();
        window.on_chat_file_opened(move |path| {
            let _ = tx.send(UiCommand::OpenChatFile(path.to_string()));
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
        window.on_refresh_pr(move || {
            let _ = tx.send(UiCommand::RefreshPr);
        });
    }

    // --- settings window callbacks -------------------------------------------
    {
        let tx = tx.clone();
        settings.on_provider_saved(move |id, kind, base_url, api_key| {
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
        settings.on_provider_deleted(move |id| {
            let _ = tx.send(UiCommand::DeleteProvider(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        settings.on_provider_login(move |id| {
            let _ = tx.send(UiCommand::ProviderLogin(id.to_string()));
        });
    }
    {
        let tx = tx.clone();
        settings.on_default_model_picked(move |i| {
            let _ = tx.send(UiCommand::SetDefaultModel(i.max(0) as usize));
        });
    }
    {
        let tx = tx.clone();
        settings.on_refresh_settings(move || {
            let _ = tx.send(UiCommand::RefreshSettings);
        });
    }
    {
        let tx = tx.clone();
        settings.on_cli_install(move |id| {
            let _ = tx.send(UiCommand::CliInstall(id.to_string()));
        });
    }

    // Controller (and spawned server) live on a background tokio runtime.
    let scroll_tx = tx.clone();
    let weak = window.as_weak();
    let settings_weak = settings.as_weak();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(controller::run(weak, settings_weak, tx, rx));
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
        geometry_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_secs(1),
            move || {
                let Some(window) = weak.upgrade() else { return };
                let w = window.window();
                let mut next = last.borrow().unwrap_or_default();
                next.maximized = w.is_maximized();
                if !next.maximized {
                    let pos = w.position();
                    let size = w.size();
                    (next.x, next.y) = (pos.x, pos.y);
                    (next.width, next.height) = (size.width, size.height);
                }
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
