//! trouve desktop client. Spawns the server as a child process (all traffic
//! still goes over the protocol on localhost) and runs the Slint UI.

mod controller;
mod render;
mod ui;

slint::include_modules!();

use controller::UiCommand;
use slint::ComponentHandle;

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

    // Controller (and spawned server) live on a background tokio runtime.
    let weak = window.as_weak();
    let settings_weak = settings.as_weak();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(controller::run(weak, settings_weak, tx, rx));
    });

    window.run()?;
    Ok(())
}
