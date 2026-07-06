//! App controller: owns the spawned local server, the protocol client, and
//! all client state. Runs on a tokio runtime in a background thread; the UI
//! thread sends [`UiCommand`]s in, and the controller pushes rendered plain
//! data back via `Weak::upgrade_in_event_loop`.
//!
//! Invariant 1 holds strictly: the app spawns `trouve-server` as a child
//! process and speaks HTTP/SSE to it — no `trouve-core` import.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use trouve_client_core::client::ProtocolClient;
use trouve_client_core::viewmodel::ThreadViewModel;
use trouve_protocol::{
    AgentMode, ApprovalDecision, CreateSessionRequest, CreateThreadRequest, DirEntry,
    EventEnvelope, ModelInfo, Session, Thread, UpdateSessionRequest, UpdateThreadRequest,
    UpsertProviderRequest, Workspace,
};

use crate::render;
use crate::ui::{self, NavRowData};

#[derive(Debug)]
pub enum UiCommand {
    // Left nav.
    NavRowClicked(usize),
    SessionRename {
        row: usize,
        title: String,
    },
    SessionArchive {
        row: usize,
        archived: bool,
    },
    SessionDelete {
        row: usize,
    },
    /// Native folder picker → register the chosen directory as a workspace.
    OpenWorkspaceDialog,
    /// The "+" on a workspace header row: new session there.
    WorkspaceNewSession(usize),
    OpenSettings,

    // New-chat screens.
    NewSession,
    NewThread,
    CancelNewChat,
    NewChatWorkspaceChanged(usize),
    RegisterWorkspacePath(String),
    StartNewChat {
        workspace_idx: usize,
        branch_idx: usize,
        mode_idx: usize,
        model_idx: usize,
        prompt: String,
    },

    // Chat screen.
    SelectThread(usize),
    SendMessage(String),
    Approval {
        row: usize,
        approved: bool,
    },
    ToggleTool(usize),
    ComposerModeChanged(usize),
    ComposerModelChanged(usize),

    // Right column.
    RefreshDiff,
    ToggleDiffFile(usize),
    Undo,
    Redo,
    CreatePr,
    RefreshPr,
    FileActivated(usize),
    FileUp,

    // Settings window.
    RefreshSettings,
    SaveProvider {
        id: String,
        kind: String,
        base_url: String,
        api_key: String,
    },
    DeleteProvider(String),
    ProviderLogin(String),
    SetDefaultModel(usize),

    /// Internal: an event arrived on some thread's stream.
    Event(String, Box<EventEnvelope>),
}

/// What a left-nav row maps back to.
#[derive(Debug, Clone)]
enum NavEntry {
    /// Index into `Controller::workspaces`.
    Workspace(usize),
    /// Index into `Controller::sessions`.
    Session(usize),
    /// Toggles the archived group of this workspace id.
    ArchivedToggle(String),
}

/// Which flavor of the new-chat screen is open.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NewChat {
    Session,
    Thread,
}

struct Controller {
    ui: slint::Weak<crate::AppWindow>,
    settings_ui: slint::Weak<crate::SettingsWindow>,
    client: ProtocolClient,
    tx: mpsc::UnboundedSender<UiCommand>,

    workspaces: Vec<Workspace>,
    /// The workspace the app was started in (default for new sessions).
    home_workspace_id: String,
    sessions: Vec<Session>,
    nav: Vec<NavEntry>,
    current_session: Option<usize>,
    archived_expanded: HashSet<String>,
    collapsed_workspaces: HashSet<String>,

    threads: Vec<Thread>,
    current_thread: Option<usize>,

    vms: HashMap<String, ThreadViewModel>,
    followed: HashSet<String>,
    expanded_tools: HashSet<String>,
    row_call_ids: Vec<Option<String>>,

    modes: Vec<AgentMode>,
    models: Vec<ModelInfo>,

    new_chat: Option<NewChat>,
    branches: Vec<String>,

    diff_files: Vec<slint_diff_view::FileDiff>,
    diff_collapsed: Vec<bool>,
    file_path: String,
    file_entries: Vec<DirEntry>,
}

pub async fn run(
    ui: slint::Weak<crate::AppWindow>,
    settings_ui: slint::Weak<crate::SettingsWindow>,
    tx: mpsc::UnboundedSender<UiCommand>,
    mut rx: mpsc::UnboundedReceiver<UiCommand>,
) {
    let (client, _server) = match start_local_server().await {
        Ok(pair) => pair,
        Err(e) => {
            ui::set_status(&ui, &format!("failed to start server: {e:#}"));
            return;
        }
    };

    let mut ctl = Controller {
        ui,
        settings_ui,
        client,
        tx,
        workspaces: Vec::new(),
        home_workspace_id: String::new(),
        sessions: Vec::new(),
        nav: Vec::new(),
        current_session: None,
        archived_expanded: HashSet::new(),
        collapsed_workspaces: HashSet::new(),
        threads: Vec::new(),
        current_thread: None,
        vms: HashMap::new(),
        followed: HashSet::new(),
        expanded_tools: HashSet::new(),
        row_call_ids: Vec::new(),
        modes: Vec::new(),
        models: Vec::new(),
        new_chat: None,
        branches: Vec::new(),
        diff_files: Vec::new(),
        diff_collapsed: Vec::new(),
        file_path: ".".into(),
        file_entries: Vec::new(),
    };

    if let Err(e) = ctl.bootstrap().await {
        ctl.error(&format!("startup error: {e:#}"));
    }

    while let Some(command) = rx.recv().await {
        let result = ctl.handle(command).await;
        if let Err(e) = result {
            ctl.error(&format!("{e:#}"));
        }
    }
}

/// Spawn `trouve-server` on an ephemeral local port and wait for it to
/// answer. `TROUVE_SERVER_URL` skips spawning and connects to an existing
/// (possibly remote) server instead.
async fn start_local_server() -> Result<(ProtocolClient, Option<tokio::process::Child>)> {
    if let Ok(url) = std::env::var("TROUVE_SERVER_URL") {
        let client = ProtocolClient::new(&url);
        client
            .info()
            .await
            .with_context(|| format!("connecting to {url}"))?;
        return Ok((client, None));
    }

    let binary = server_binary()?;
    // Reserve a port, then hand it to the server (tiny race, fine locally).
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let addr = format!("127.0.0.1:{port}");

    let mut command = tokio::process::Command::new(&binary);
    command.args(["--addr", &addr]).kill_on_drop(true);
    #[cfg(target_os = "linux")]
    unsafe {
        // Tie the server's lifetime to ours even if we exit uncleanly.
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let child = command
        .spawn()
        .with_context(|| format!("spawning {}", binary.display()))?;

    let client = ProtocolClient::new(&format!("http://{addr}"));
    for _ in 0..100 {
        if client.info().await.is_ok() {
            return Ok((client, Some(child)));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("trouve-server did not become ready on {addr}");
}

/// Locate the server binary: `TROUVE_SERVER_BIN`, next to our own
/// executable (installed layout and cargo target dir), then `$PATH`.
fn server_binary() -> Result<std::path::PathBuf> {
    if let Ok(path) = std::env::var("TROUVE_SERVER_BIN") {
        return Ok(path.into());
    }
    let name = format!("trouve-server{}", std::env::consts::EXE_SUFFIX);
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let sibling = dir.join(&name);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    Ok(name.into()) // resolved via PATH by Command::new
}

impl Controller {
    fn status(&self, text: &str) {
        ui::set_status(&self.ui, text);
    }

    fn error(&self, text: &str) {
        ui::set_error(&self.ui, text);
    }

    async fn bootstrap(&mut self) -> Result<()> {
        let cwd = std::env::current_dir()?;
        let workspace = self
            .client
            .register_workspace(&cwd.to_string_lossy())
            .await
            .context("registering current directory as workspace")?;
        self.home_workspace_id = workspace.id.clone();

        self.reload_catalogs().await;
        self.reload_sessions().await?;

        // Open on the most recent active session of the home workspace.
        let initial = self
            .sessions
            .iter()
            .rposition(|s| s.workspace_id == self.home_workspace_id && !s.archived);
        if let Some(index) = initial {
            self.select_session(index).await?;
        }
        self.status(&format!("workspace: {}", workspace.name));
        Ok(())
    }

    /// Refresh modes/models (after provider changes) and push all pickers.
    async fn reload_catalogs(&mut self) {
        self.modes = self
            .client
            .list_modes(Some(&self.home_workspace_id))
            .await
            .unwrap_or_default();
        self.models = self.client.list_models().await.unwrap_or_default();
        ui::set_pickers(
            &self.ui,
            self.modes.iter().map(|m| m.id.clone()).collect(),
            self.models.iter().map(|m| m.id.clone()).collect(),
        );
        self.push_picker_indices();
    }

    async fn reload_sessions(&mut self) -> Result<()> {
        let current_id = self.current_session_id();
        self.workspaces = self.client.list_workspaces().await?;
        self.sessions = self.client.list_sessions().await?;
        self.current_session =
            current_id.and_then(|id| self.sessions.iter().position(|s| s.id == id));
        self.push_nav();
        Ok(())
    }

    /// Rebuild the grouped left-nav rows and the row → entry map.
    fn push_nav(&mut self) {
        let mut rows = Vec::new();
        let mut nav = Vec::new();
        for (wi, ws) in self.workspaces.iter().enumerate() {
            let expanded = !self.collapsed_workspaces.contains(&ws.id);
            rows.push(NavRowData {
                kind: 0,
                title: ws.name.clone(),
                expanded,
                ..Default::default()
            });
            nav.push(NavEntry::Workspace(wi));
            if !expanded {
                continue;
            }

            let mut archived_count = 0;
            for (i, session) in self.sessions.iter().enumerate() {
                if session.workspace_id != ws.id {
                    continue;
                }
                if session.archived {
                    archived_count += 1;
                    continue;
                }
                rows.push(NavRowData {
                    kind: 1,
                    title: session.title.clone(),
                    subtitle: session.branch.clone(),
                    session_index: i as i32,
                    selected: self.current_session == Some(i),
                    archived: false,
                    expanded: false,
                });
                nav.push(NavEntry::Session(i));
            }
            if archived_count > 0 {
                let expanded = self.archived_expanded.contains(&ws.id);
                rows.push(NavRowData {
                    kind: 2,
                    title: format!("Archived ({archived_count})"),
                    expanded,
                    ..Default::default()
                });
                nav.push(NavEntry::ArchivedToggle(ws.id.clone()));
                if expanded {
                    for (i, session) in self.sessions.iter().enumerate() {
                        if session.workspace_id != ws.id || !session.archived {
                            continue;
                        }
                        rows.push(NavRowData {
                            kind: 1,
                            title: session.title.clone(),
                            subtitle: session.branch.clone(),
                            session_index: i as i32,
                            selected: self.current_session == Some(i),
                            archived: true,
                            expanded: false,
                        });
                        nav.push(NavEntry::Session(i));
                    }
                }
            }
        }
        self.nav = nav;
        ui::set_nav(&self.ui, rows);
    }

    fn nav_session(&self, row: usize) -> Option<usize> {
        match self.nav.get(row) {
            Some(NavEntry::Session(i)) => Some(*i),
            _ => None,
        }
    }

    fn current_session_id(&self) -> Option<String> {
        self.current_session
            .and_then(|i| self.sessions.get(i))
            .map(|s| s.id.clone())
    }

    fn current_thread_id(&self) -> Option<String> {
        self.current_thread.map(|i| self.threads[i].id.clone())
    }

    async fn select_session(&mut self, index: usize) -> Result<()> {
        if index >= self.sessions.len() {
            return Ok(());
        }
        self.current_session = Some(index);
        self.close_new_chat();
        self.push_nav();
        let session_id = self.sessions[index].id.clone();
        self.threads = self.client.list_threads(&session_id).await?;
        self.current_thread = if self.threads.is_empty() {
            None
        } else {
            Some(0)
        };
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat();
        self.push_context();
        self.refresh_usage_text().await;
        self.file_path = ".".into();
        let _ = self.load_files().await;
        let _ = self.refresh_diff().await;
        Ok(())
    }

    fn push_threads(&self) {
        let mut tabs: Vec<(String, String)> = self
            .threads
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    format!("{} · {}", t.mode, short_model(&t.model)),
                )
            })
            .collect();
        // The new-thread form lives in a provisional tab so the previous
        // tab stays one click away; `current_thread` is untouched
        // underneath, making cancel a pure UI dismissal.
        let selected = if matches!(self.new_chat, Some(NewChat::Thread)) {
            tabs.push((String::new(), "New Thread".into()));
            (tabs.len() - 1) as i32
        } else {
            self.current_thread.map(|i| i as i32).unwrap_or(-1)
        };
        ui::set_threads(&self.ui, tabs, selected);
    }

    /// Composer pickers mirror the current thread's mode/model.
    fn push_picker_indices(&self) {
        let (mode, model) = match self.current_thread.and_then(|i| self.threads.get(i)) {
            Some(thread) => (
                self.modes
                    .iter()
                    .position(|m| m.id == thread.mode)
                    .map(|i| i as i32)
                    .unwrap_or(-1),
                self.models
                    .iter()
                    .position(|m| m.id == thread.model)
                    .map(|i| i as i32)
                    .unwrap_or(-1),
            ),
            None => (-1, -1),
        };
        ui::set_picker_indices(&self.ui, mode, model);
    }

    /// Start following the current thread's event stream (idempotent).
    fn follow_current(&mut self) {
        let Some(thread_id) = self.current_thread_id() else {
            return;
        };
        if !self.followed.insert(thread_id.clone()) {
            return;
        }
        self.vms.insert(thread_id.clone(), ThreadViewModel::new());
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let id = thread_id.clone();
            let result = client
                .follow_thread_events(&thread_id, 0, |envelope| {
                    let _ = tx.send(UiCommand::Event(id.clone(), Box::new(envelope)));
                    std::ops::ControlFlow::Continue(())
                })
                .await;
            if let Err(e) = result {
                tracing::warn!("event stream for {thread_id} ended: {e:#}");
            }
        });
    }

    fn render_chat(&mut self) {
        let Some(thread_id) = self.current_thread_id() else {
            self.row_call_ids.clear();
            ui::set_chat(&self.ui, Vec::new());
            ui::set_composer_enabled(&self.ui, false);
            return;
        };
        let vm = self.vms.entry(thread_id).or_default();
        let (rows, call_ids) = render::chat_rows(vm, &self.expanded_tools);
        self.row_call_ids = call_ids;
        ui::set_chat(&self.ui, rows);
        ui::set_composer_enabled(&self.ui, true);
    }

    /// Push the context dial: last turn's input tokens vs the model window.
    fn push_context(&mut self) {
        let Some(thread) = self.current_thread.and_then(|i| self.threads.get(i)) else {
            ui::set_context(&self.ui, 0.0, false, "no thread selected".into());
            return;
        };
        let window = self
            .models
            .iter()
            .find(|m| m.id == thread.model)
            .map(|m| m.context_window);
        let vm = self.vms.entry(thread.id.clone()).or_default();
        let used = vm
            .last_usage
            .as_ref()
            .map(|u| u.input_tokens + u.cached_input_tokens)
            .unwrap_or(0);
        let (fill, tooltip) = match (used, window) {
            (0, _) => (0.0, "context: no usage yet".to_string()),
            (used, Some(window)) if window > 0 => {
                let fill = (used as f32 / window as f32).clamp(0.0, 1.0);
                (
                    fill,
                    format!("context: {used} / {window} tokens ({:.0}%)", fill * 100.0),
                )
            }
            (used, _) => (0.0, format!("context: {used} tokens (window unknown)")),
        };
        ui::set_context(&self.ui, fill, vm.compacting, tooltip);
    }

    async fn refresh_usage_text(&self) {
        let Some(session_id) = self.current_session_id() else {
            ui::set_usage_text(&self.ui, String::new());
            return;
        };
        if let Ok(usage) = self.client.session_usage(&session_id).await {
            let mut text = format!(
                "{} in / {} out",
                format_tokens(usage.input_tokens),
                format_tokens(usage.output_tokens),
            );
            // Subscription backends report no per-turn cost; only per-use
            // APIs accumulate a nonzero total worth showing.
            if usage.cost_usd > 0.0 {
                text.push_str(&format!(" · ${:.4}", usage.cost_usd));
            }
            ui::set_usage_text(&self.ui, text);
        }
    }

    async fn refresh_diff(&mut self) -> Result<()> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(());
        };
        let diff = self.client.session_diff(&session_id).await?;
        self.diff_files = slint_diff_view::parse_unified_diff(&diff.diff);
        self.diff_collapsed = vec![false; self.diff_files.len()];
        self.push_diff();
        Ok(())
    }

    fn push_diff(&self) {
        ui::set_diff(
            &self.ui,
            slint_diff_view::build_rows(&self.diff_files, &self.diff_collapsed),
        );
    }

    async fn load_files(&mut self) -> Result<()> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(());
        };
        self.file_entries = self
            .client
            .session_files(&session_id, &self.file_path)
            .await?;
        ui::set_file_list(
            &self.ui,
            self.file_path.clone(),
            self.file_entries
                .iter()
                .map(|e| (e.name.clone(), e.is_dir))
                .collect(),
        );
        Ok(())
    }

    // --- new-chat screens ----------------------------------------------------

    /// `workspace`: pre-selected workspace index (the per-workspace "+"),
    /// or None to default to the current session's / home workspace.
    async fn open_new_session_screen(&mut self, workspace: Option<usize>) -> Result<()> {
        self.new_chat = Some(NewChat::Session);
        let ws_index = workspace
            .filter(|i| *i < self.workspaces.len())
            .unwrap_or_else(|| {
                self.workspaces
                    .iter()
                    .position(|w| {
                        Some(w.id.as_str())
                            == self
                                .current_session
                                .and_then(|i| self.sessions.get(i))
                                .map(|s| s.workspace_id.as_str())
                                .or(Some(self.home_workspace_id.as_str()))
                    })
                    .unwrap_or(0)
            });
        ui::set_new_chat(
            &self.ui,
            self.workspaces.iter().map(|w| w.name.clone()).collect(),
            ws_index as i32,
            Vec::new(),
            -1,
            default_mode_index(&self.modes),
            0,
        );
        ui::set_center_screen(&self.ui, 1);
        self.load_branches(ws_index).await;
        Ok(())
    }

    fn open_new_thread_screen(&mut self) {
        if self.current_session.is_none() {
            self.error("select a session first (threads share its worktree)");
            return;
        }
        if matches!(self.new_chat, Some(NewChat::Thread)) {
            return; // Already on the provisional tab.
        }
        self.new_chat = Some(NewChat::Thread);
        ui::set_new_chat(
            &self.ui,
            Vec::new(),
            -1,
            Vec::new(),
            -1,
            default_mode_index(&self.modes),
            0,
        );
        self.push_threads();
        ui::set_center_screen(&self.ui, 2);
    }

    fn close_new_chat(&mut self) {
        let had_thread_form = matches!(self.new_chat, Some(NewChat::Thread));
        self.new_chat = None;
        if had_thread_form {
            // Drop the provisional tab and land back on the previous one.
            self.push_threads();
        }
        ui::set_center_screen(&self.ui, 0);
    }

    async fn load_branches(&mut self, workspace_idx: usize) {
        let Some(ws) = self.workspaces.get(workspace_idx) else {
            return;
        };
        match self.client.workspace_branches(&ws.id).await {
            Ok(list) => {
                let head = list.branches.iter().position(|b| *b == list.head);
                self.branches = list.branches;
                ui::set_branches(
                    &self.ui,
                    self.branches.clone(),
                    head.map(|i| i as i32).unwrap_or(-1),
                );
            }
            Err(e) => {
                self.branches.clear();
                ui::set_branches(&self.ui, Vec::new(), -1);
                self.error(&format!("failed to list branches: {e:#}"));
            }
        }
    }

    async fn start_new_chat(
        &mut self,
        workspace_idx: usize,
        branch_idx: usize,
        mode_idx: usize,
        model_idx: usize,
        prompt: String,
    ) -> Result<()> {
        match self.new_chat {
            Some(NewChat::Thread) => {
                self.close_new_chat();
                self.create_thread(mode_idx, model_idx).await?;
                if let Some(thread_id) = self.current_thread_id() {
                    self.client.send_message(&thread_id, &prompt).await?;
                }
            }
            _ => {
                let workspace = self
                    .workspaces
                    .get(workspace_idx)
                    .context("no workspace selected")?
                    .clone();
                let session = self
                    .client
                    .create_session(&CreateSessionRequest {
                        workspace_id: workspace.id,
                        title: Some(session_title(&prompt)),
                        base_ref: self.branches.get(branch_idx).cloned(),
                    })
                    .await?;
                self.close_new_chat();
                self.reload_sessions().await?;
                let index = self
                    .sessions
                    .iter()
                    .position(|s| s.id == session.id)
                    .unwrap_or(0);
                self.select_session(index).await?;
                self.create_thread(mode_idx, model_idx).await?;
                if let Some(thread_id) = self.current_thread_id() {
                    self.client.send_message(&thread_id, &prompt).await?;
                }
            }
        }
        Ok(())
    }

    // --- settings --------------------------------------------------------------

    async fn refresh_settings(&mut self) {
        let providers = match self.client.list_providers().await {
            Ok(p) => p,
            Err(e) => {
                ui::set_settings_status(&self.settings_ui, format!("failed to load: {e:#}"));
                return;
            }
        };
        let model_ids: Vec<String> = self.models.iter().map(|m| m.id.clone()).collect();
        let default_index = model_ids
            .iter()
            .position(|m| *m == providers.default_model)
            .map(|i| i as i32)
            .unwrap_or(-1);
        ui::set_settings_data(
            &self.settings_ui,
            providers
                .providers
                .into_iter()
                .map(|p| {
                    (
                        p.id,
                        p.kind,
                        p.base_url.unwrap_or_default(),
                        p.has_credentials,
                        p.auth,
                        p.experimental,
                    )
                })
                .collect(),
            model_ids,
            default_index,
            self.modes
                .iter()
                .map(|m| {
                    format!(
                        "{}  ·  {}{}",
                        m.id,
                        m.display_name,
                        if m.read_only { "  ·  read-only" } else { "" }
                    )
                })
                .collect(),
        );
        // Preset catalog is static server data; fetch alongside the rest.
        if let Ok(known) = self.client.known_providers().await {
            ui::set_known_providers(&self.settings_ui, known);
        }
    }

    // --- command dispatch --------------------------------------------------------

    async fn handle(&mut self, command: UiCommand) -> Result<()> {
        match command {
            UiCommand::NavRowClicked(row) => match self.nav.get(row).cloned() {
                Some(NavEntry::Session(i)) => self.select_session(i).await?,
                Some(NavEntry::Workspace(wi)) => {
                    if let Some(ws) = self.workspaces.get(wi) {
                        if !self.collapsed_workspaces.remove(&ws.id) {
                            self.collapsed_workspaces.insert(ws.id.clone());
                        }
                        self.push_nav();
                    }
                }
                Some(NavEntry::ArchivedToggle(ws_id)) => {
                    if !self.archived_expanded.remove(&ws_id) {
                        self.archived_expanded.insert(ws_id);
                    }
                    self.push_nav();
                }
                _ => {}
            },
            UiCommand::SessionRename { row, title } => {
                if let Some(i) = self.nav_session(row) {
                    let id = self.sessions[i].id.clone();
                    self.client
                        .update_session(
                            &id,
                            &UpdateSessionRequest {
                                title: Some(title),
                                archived: None,
                            },
                        )
                        .await?;
                    self.reload_sessions().await?;
                }
            }
            UiCommand::SessionArchive { row, archived } => {
                if let Some(i) = self.nav_session(row) {
                    let id = self.sessions[i].id.clone();
                    self.client
                        .update_session(
                            &id,
                            &UpdateSessionRequest {
                                title: None,
                                archived: Some(archived),
                            },
                        )
                        .await?;
                    self.reload_sessions().await?;
                }
            }
            UiCommand::SessionDelete { row } => {
                if let Some(i) = self.nav_session(row) {
                    let id = self.sessions[i].id.clone();
                    let was_current = self.current_session == Some(i);
                    self.client.delete_session(&id).await?;
                    if was_current {
                        self.current_session = None;
                        self.threads.clear();
                        self.current_thread = None;
                        self.push_threads();
                        self.render_chat();
                    }
                    self.reload_sessions().await?;
                    self.status("session deleted");
                }
            }
            UiCommand::OpenWorkspaceDialog => {
                // The portal dialog can stay open indefinitely; run it off
                // the command loop so events keep flowing meanwhile.
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Open workspace (git repository)")
                        .pick_folder()
                        .await;
                    if let Some(folder) = picked {
                        let _ = tx.send(UiCommand::RegisterWorkspacePath(
                            folder.path().display().to_string(),
                        ));
                    }
                });
            }
            UiCommand::WorkspaceNewSession(row) => {
                if let Some(NavEntry::Workspace(wi)) = self.nav.get(row).cloned() {
                    self.open_new_session_screen(Some(wi)).await?;
                }
            }
            UiCommand::OpenSettings => {
                self.refresh_settings().await;
                ui::show_settings(&self.settings_ui);
            }
            UiCommand::NewSession => self.open_new_session_screen(None).await?,
            UiCommand::NewThread => self.open_new_thread_screen(),
            UiCommand::CancelNewChat => {
                self.close_new_chat();
                self.render_chat();
            }
            UiCommand::NewChatWorkspaceChanged(i) => self.load_branches(i).await,
            UiCommand::RegisterWorkspacePath(path) => {
                let ws = self.client.register_workspace(&path).await?;
                self.reload_sessions().await?;
                let index = self
                    .workspaces
                    .iter()
                    .position(|w| w.id == ws.id)
                    .unwrap_or(0);
                // Refresh the new-session pickers only when that screen is
                // up ("+ Open" also lands here with the chat view showing).
                if matches!(self.new_chat, Some(NewChat::Session)) {
                    ui::set_new_chat(
                        &self.ui,
                        self.workspaces.iter().map(|w| w.name.clone()).collect(),
                        index as i32,
                        Vec::new(),
                        -1,
                        default_mode_index(&self.modes),
                        0,
                    );
                    self.load_branches(index).await;
                }
                self.status(&format!("registered workspace: {}", ws.name));
            }
            UiCommand::StartNewChat {
                workspace_idx,
                branch_idx,
                mode_idx,
                model_idx,
                prompt,
            } => {
                self.start_new_chat(workspace_idx, branch_idx, mode_idx, model_idx, prompt)
                    .await?
            }
            UiCommand::SelectThread(i) => {
                if i < self.threads.len() {
                    // Clicking a real tab while the provisional "New Thread"
                    // tab is up dismisses the form (its tab disappears).
                    if self.new_chat.is_some() {
                        self.close_new_chat();
                    }
                    self.current_thread = Some(i);
                    self.push_threads();
                    self.push_picker_indices();
                    self.follow_current();
                    self.render_chat();
                    self.push_context();
                }
                // i == threads.len() is the provisional tab itself: no-op.
            }
            UiCommand::SendMessage(text) => {
                if let Some(thread_id) = self.current_thread_id() {
                    self.client.send_message(&thread_id, &text).await?;
                }
            }
            UiCommand::Approval { row, approved } => {
                if let Some(Some(call_id)) = self.row_call_ids.get(row) {
                    let decision = if approved {
                        ApprovalDecision::Approve
                    } else {
                        ApprovalDecision::Deny
                    };
                    self.client.resolve_approval(call_id, decision).await?;
                }
            }
            UiCommand::ToggleTool(row) => {
                if let Some(Some(call_id)) = self.row_call_ids.get(row) {
                    if !self.expanded_tools.remove(call_id) {
                        self.expanded_tools.insert(call_id.clone());
                    }
                    self.render_chat();
                }
            }
            UiCommand::ComposerModeChanged(i) => {
                let mode = self.modes.get(i).map(|m| m.id.clone());
                self.update_current_thread(UpdateThreadRequest {
                    mode,
                    ..Default::default()
                })
                .await;
            }
            UiCommand::ComposerModelChanged(i) => {
                let model = self.models.get(i).map(|m| m.id.clone());
                self.update_current_thread(UpdateThreadRequest {
                    model,
                    ..Default::default()
                })
                .await;
            }
            UiCommand::RefreshDiff => self.refresh_diff().await?,
            UiCommand::ToggleDiffFile(i) => {
                if let Some(flag) = self.diff_collapsed.get_mut(i) {
                    *flag = !*flag;
                    self.push_diff();
                }
            }
            UiCommand::FileActivated(i) => {
                let Some(entry) = self.file_entries.get(i).cloned() else {
                    return Ok(());
                };
                let joined = if self.file_path == "." {
                    entry.name.clone()
                } else {
                    format!("{}/{}", self.file_path, entry.name)
                };
                if entry.is_dir {
                    self.file_path = joined;
                    self.load_files().await?;
                } else if let Some(session_id) = self.current_session_id() {
                    let file = self.client.session_file(&session_id, &joined).await?;
                    let lines = render::highlight_file(&file.path, &file.content);
                    ui::set_file_view(&self.ui, joined, lines);
                }
            }
            UiCommand::FileUp => {
                self.file_path = match self.file_path.rsplit_once('/') {
                    Some((parent, _)) => parent.to_string(),
                    None => ".".into(),
                };
                self.load_files().await?;
            }
            UiCommand::Undo => {
                if let Some(session_id) = self.current_session_id() {
                    self.client.undo(&session_id).await?;
                    self.refresh_diff().await?;
                    self.status("undid last checkpoint");
                }
            }
            UiCommand::Redo => {
                if let Some(session_id) = self.current_session_id() {
                    self.client.redo(&session_id).await?;
                    self.refresh_diff().await?;
                    self.status("redid checkpoint");
                }
            }
            UiCommand::CreatePr => {
                if let (Some(session_id), Some(index)) =
                    (self.current_session_id(), self.current_session)
                {
                    let title = self.sessions[index].title.clone();
                    let pr = self
                        .client
                        .create_session_pr(
                            &session_id,
                            &trouve_protocol::CreatePrRequest {
                                title,
                                body: "Opened from trouve.".into(),
                                base: None,
                                draft: false,
                            },
                        )
                        .await?;
                    ui::set_pr_status(&self.ui, format_pr(&pr));
                }
            }
            UiCommand::RefreshPr => {
                if let Some(session_id) = self.current_session_id() {
                    let status = match self.client.session_pr(&session_id).await? {
                        Some(pr) => format_pr(&pr),
                        None => "no open PR for this session".to_string(),
                    };
                    ui::set_pr_status(&self.ui, status);
                }
            }
            UiCommand::RefreshSettings => self.refresh_settings().await,
            UiCommand::SaveProvider {
                id,
                kind,
                base_url,
                api_key,
            } => {
                let result = self
                    .client
                    .upsert_provider(
                        &id,
                        &UpsertProviderRequest {
                            kind,
                            base_url: (!base_url.is_empty()).then_some(base_url),
                            api_key: (!api_key.is_empty()).then_some(api_key),
                        },
                    )
                    .await;
                match result {
                    Ok(info) => {
                        ui::set_settings_status(
                            &self.settings_ui,
                            format!(
                                "saved provider {}{}",
                                info.id,
                                if info.has_credentials {
                                    ""
                                } else {
                                    " (no credentials yet — add an API key)"
                                }
                            ),
                        );
                        self.reload_catalogs().await;
                        self.refresh_settings().await;
                    }
                    Err(e) => {
                        ui::set_settings_status(&self.settings_ui, format!("{e:#}"));
                    }
                }
            }
            UiCommand::DeleteProvider(id) => match self.client.delete_provider(&id).await {
                Ok(()) => {
                    ui::set_settings_status(&self.settings_ui, format!("removed {id}"));
                    self.reload_catalogs().await;
                    self.refresh_settings().await;
                }
                Err(e) => {
                    ui::set_settings_status(&self.settings_ui, format!("{e:#}"));
                }
            },
            UiCommand::ProviderLogin(id) => match self.client.start_login(&id).await {
                Ok(started) => {
                    // CLI-driven logins may open the browser themselves and
                    // print no URL for us to show.
                    let msg = match (&started.user_code, started.verification_url.is_empty()) {
                        (Some(code), _) => format!(
                            "opening browser — enter code {code} at {}",
                            started.verification_url
                        ),
                        (None, false) => format!("opening browser to log in to {id}…"),
                        (None, true) => {
                            format!("login started for {id} — follow the vendor's prompts…")
                        }
                    };
                    ui::set_settings_status(&self.settings_ui, msg);
                    if !started.verification_url.is_empty() {
                        open_in_browser(&started.verification_url);
                    }
                    // Poll the login in the background so the UI stays live;
                    // report the outcome and refresh the provider list.
                    let client = self.client.clone();
                    let settings_ui = self.settings_ui.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        for _ in 0..300 {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            let Ok(status) = client.login_status(&id).await else {
                                return;
                            };
                            match status.status.as_str() {
                                "pending" => continue,
                                "success" => {
                                    ui::set_settings_status(
                                        &settings_ui,
                                        format!("logged in to {id}"),
                                    );
                                    let _ = tx.send(UiCommand::RefreshSettings);
                                }
                                _ => {
                                    ui::set_settings_status(
                                        &settings_ui,
                                        format!(
                                            "login to {id} failed: {}",
                                            status.error.unwrap_or_default()
                                        ),
                                    );
                                }
                            }
                            return;
                        }
                    });
                }
                Err(e) => {
                    ui::set_settings_status(&self.settings_ui, format!("{e:#}"));
                }
            },
            UiCommand::SetDefaultModel(i) => {
                if let Some(model) = self.models.get(i) {
                    match self.client.set_default_model(&model.id).await {
                        Ok(()) => {
                            ui::set_settings_status(
                                &self.settings_ui,
                                format!("default model: {}", model.id),
                            );
                            self.refresh_settings().await;
                        }
                        Err(e) => {
                            ui::set_settings_status(&self.settings_ui, format!("{e:#}"));
                        }
                    }
                }
            }
            UiCommand::Event(thread_id, envelope) => {
                let vm = self.vms.entry(thread_id.clone()).or_default();
                let changed = vm.apply(&envelope);
                if self.current_thread_id().as_deref() == Some(&thread_id) {
                    // Compaction/usage state can change without a chat row
                    // changing, so the dial refreshes on every event.
                    self.push_context();
                    if changed.is_some() {
                        self.render_chat();
                    }
                    if matches!(envelope.event, trouve_protocol::Event::TurnCompleted { .. }) {
                        let _ = self.refresh_diff().await;
                        self.refresh_usage_text().await;
                    }
                }
            }
        }
        Ok(())
    }

    /// PATCH the current thread's settings; on failure (e.g. mid-turn
    /// conflict) surface the error and restore the pickers.
    async fn update_current_thread(&mut self, req: UpdateThreadRequest) {
        let Some(index) = self.current_thread else {
            return;
        };
        let thread_id = self.threads[index].id.clone();
        match self.client.update_thread(&thread_id, &req).await {
            Ok(thread) => {
                self.threads[index] = thread;
                self.push_threads();
                self.push_picker_indices();
                self.push_context();
            }
            Err(e) => {
                self.error(&format!("{e:#}"));
                self.push_picker_indices();
            }
        }
    }

    async fn create_thread(&mut self, mode_idx: usize, model_idx: usize) -> Result<()> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(());
        };
        let thread = self
            .client
            .create_thread(&CreateThreadRequest {
                session_id,
                mode: self.modes.get(mode_idx).map(|m| m.id.clone()),
                model: self.models.get(model_idx).map(|m| m.id.clone()),
                model_options: Default::default(),
                permission_mode: None,
            })
            .await?;
        self.threads.push(thread);
        self.current_thread = Some(self.threads.len() - 1);
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat();
        self.push_context();
        Ok(())
    }
}

fn short_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

/// Prefer the "code" mode as the default picker selection.
fn default_mode_index(modes: &[AgentMode]) -> i32 {
    modes
        .iter()
        .position(|m| m.id == "code")
        .map(|i| i as i32)
        .unwrap_or(0)
}

/// Derive a session title from the first prompt: first line, word-truncated.
fn session_title(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or(prompt).trim();
    if line.len() <= 48 {
        return line.to_string();
    }
    let mut cut = 48;
    while cut > 0 && !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &line[..cut];
    let head = head.rsplit_once(' ').map(|(h, _)| h).unwrap_or(head);
    format!("{head}…")
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", url])
        .spawn();
}

fn format_pr(pr: &trouve_protocol::PrInfo) -> String {
    let checks = if pr.checks.is_empty() {
        "no checks".to_string()
    } else {
        let done = pr
            .checks
            .iter()
            .filter(|c| c.conclusion.as_deref() == Some("success"))
            .count();
        format!("{done}/{} checks green", pr.checks.len())
    };
    let reviews = if pr.reviews.is_empty() {
        "no reviews".to_string()
    } else {
        pr.reviews
            .iter()
            .map(|r| format!("{}: {}", r.reviewer, r.state))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "PR #{} ({}) — {}\n{}\n{checks} · {reviews}",
        pr.number, pr.state, pr.title, pr.url
    )
}

#[cfg(test)]
mod tests {
    use super::session_title;

    #[test]
    fn session_title_truncates_at_word_boundary() {
        assert_eq!(session_title("Fix the login bug"), "Fix the login bug");
        let long = "Refactor the authentication middleware to support refresh tokens";
        let title = session_title(long);
        assert!(title.ends_with('…'));
        assert!(title.len() <= 50);
        assert!(!title.contains('\n'));
        assert_eq!(session_title("first line\nsecond"), "first line");
    }
}
