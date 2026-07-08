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
    /// Flip the "show archived sessions" filter.
    ToggleArchivedFilter,
    /// Quit once all running agent turns complete.
    QuitWhenIdle,
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
    /// Chat viewport-y sampled by the shell's poll, bookmarked per thread.
    ChatScrolled(f32),
    SendMessage(String),
    Approval {
        row: usize,
        approved: bool,
    },
    ToggleTool(usize),
    /// Toggle a turn between styled markdown and raw selectable text.
    ToggleRawTurn(u64),
    /// Collapse/expand a chat card (user/assistant/thinking header).
    ToggleCard(String),
    ComposerModeChanged(usize),
    ComposerModelChanged(usize),
    ComposerThinkingChanged(usize),
    ComposerContextChanged(usize),
    ComposerFastToggled(bool),

    // Right column.
    RefreshDiff,
    ToggleDiffFile(usize),
    Undo,
    Redo,
    CreatePr,
    RefreshPr,
    FileActivated(usize),
    FileUp,
    /// A filename clicked in a chat tool card; path as the tool saw it
    /// (possibly absolute).
    OpenChatFile(String),

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
    /// Download/update a managed vendor CLI ("cursor-agent", "claude", "codex").
    CliInstall(String),

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
    /// Session-list filter: include archived sessions (default hidden).
    show_archived: bool,
    /// Quit once every agent turn finishes (armed from the quit dialog).
    quit_when_idle: bool,

    threads: Vec<Thread>,
    current_thread: Option<usize>,

    vms: HashMap<String, ThreadViewModel>,
    followed: HashSet<String>,
    expanded_tools: HashSet<String>,
    /// (thread id, turn) pairs showing raw text instead of styled markdown.
    raw_turns: HashSet<(String, u64)>,
    /// (thread id, card key) pairs whose card body is collapsed.
    collapsed_cards: HashSet<(String, String)>,
    row_call_ids: Vec<Option<String>>,

    /// Where-you-left-off bookmark (last session, per-session last thread,
    /// per-thread scroll), persisted to resume.json as it changes.
    resume: crate::winstate::Resume,

    modes: Vec<AgentMode>,
    models: Vec<ModelInfo>,
    /// Thinking dropdown state for the current thread's model: the schema
    /// property the values belong to and the raw value tokens (parallel to
    /// the displayed labels).
    thinking_key: Option<String>,
    thinking_values: Vec<String>,
    /// Context-size dropdown values (schema property "context"), when the
    /// current model offers a choice (e.g. cursor's 300k/1M).
    context_values: Vec<String>,

    new_chat: Option<NewChat>,
    branches: Vec<String>,

    diff_files: Vec<slint_diff_view::FileDiff>,
    diff_collapsed: Vec<bool>,
    diff_raw: String,
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
        show_archived: false,
        quit_when_idle: false,
        threads: Vec::new(),
        current_thread: None,
        vms: HashMap::new(),
        followed: HashSet::new(),
        expanded_tools: HashSet::new(),
        raw_turns: HashSet::new(),
        collapsed_cards: HashSet::new(),
        row_call_ids: Vec::new(),
        resume: crate::winstate::load_resume(),
        modes: Vec::new(),
        models: Vec::new(),
        thinking_key: None,
        thinking_values: Vec::new(),
        context_values: Vec::new(),
        new_chat: None,
        branches: Vec::new(),
        diff_files: Vec::new(),
        diff_collapsed: Vec::new(),
        diff_raw: String::new(),
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

        // Reopen the last open session (select_session then restores its
        // last thread and scroll); fall back to the most recent active
        // session of the home workspace.
        let initial = self
            .sessions
            .iter()
            .position(|s| s.id == self.resume.session_id)
            .or_else(|| {
                self.sessions
                    .iter()
                    .rposition(|s| s.workspace_id == self.home_workspace_id && !s.archived)
            });
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
        let mode_names = self
            .modes
            .iter()
            .map(|m| mode_display_name(&m.display_name, &m.id))
            .collect();
        ui::set_pickers(
            &self.ui,
            mode_names,
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
            if archived_count > 0 && self.show_archived {
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

    /// Push the number of threads with an active turn to the UI (feeds the
    /// quit-confirmation dialog) and, when a deferred quit is armed, leave
    /// as soon as that count reaches zero.
    fn push_agents_running(&mut self) {
        let running = self.vms.values().filter(|vm| vm.turn_running).count() as i32;
        ui::set_agents_running(&self.ui, running);
        if self.quit_when_idle && running == 0 {
            ui::quit(&self.ui);
        }
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
        // Reopen the thread the user last had open in this session; first
        // thread when there's no bookmark (or it was deleted).
        self.current_thread = self
            .resume
            .session_threads
            .get(&session_id)
            .and_then(|tid| self.threads.iter().position(|t| t.id == *tid))
            .or(if self.threads.is_empty() { None } else { Some(0) });
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat(true);
        self.push_context();
        self.remember_position();
        self.restore_scroll();
        self.refresh_usage_text().await;
        self.file_path = ".".into();
        let _ = self.load_files().await;
        let _ = self.refresh_diff().await;
        Ok(())
    }

    /// Record the open session/thread in the resume bookmark and persist it.
    fn remember_position(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        self.resume.session_id = session_id.clone();
        if let Some(thread_id) = self.current_thread_id() {
            self.resume.session_threads.insert(session_id, thread_id);
        }
        crate::winstate::save_resume(&self.resume);
    }

    /// Land the just-opened thread at its saved scroll offset. The render is
    /// queued ahead of this, so it applies after; best effort, as
    /// virtualized row heights settle while they come on screen.
    fn restore_scroll(&self) {
        let Some(thread_id) = self.current_thread_id() else {
            return;
        };
        if let Some(&y) = self.resume.thread_scroll.get(&thread_id) {
            if y < 0.0 {
                ui::set_chat_scroll(&self.ui, y);
            }
        }
    }

    fn push_threads(&self) {
        let mut tabs: Vec<(String, String)> = self
            .threads
            .iter()
            .map(|t| {
                let mode = self
                    .modes
                    .iter()
                    .find(|m| m.id == t.mode)
                    .map(|m| mode_display_name(&m.display_name, &m.id))
                    .unwrap_or_else(|| mode_display_name("", &t.mode));
                (
                    t.id.clone(),
                    format!("{} · {}", mode, short_model(&t.model)),
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
    fn push_picker_indices(&mut self) {
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
        self.push_model_knobs();
    }

    /// Model knobs (thinking dropdown, fast toggle) come from the current
    /// model's options schema; selections from the thread's stored options.
    fn push_model_knobs(&mut self) {
        let thread = self.current_thread.and_then(|i| self.threads.get(i));
        let info = thread.and_then(|t| self.models.iter().find(|m| m.id == t.model));

        let thinking = info.and_then(|m| thinking_property(&m.options_schema));
        let (key, values, default) = match thinking {
            Some(t) => (Some(t.0), t.1, t.2),
            None => (None, Vec::new(), None),
        };
        let current = key
            .as_deref()
            .and_then(|k| thread?.model_options.get(k)?.as_str().map(String::from))
            .or(default);
        let index = current
            .and_then(|c| values.iter().position(|v| *v == c))
            .map(|i| i as i32)
            .unwrap_or(-1);

        let context = info.and_then(|m| context_property(&m.options_schema));
        let (context_values, context_default) = context.unwrap_or_default();
        let context_current = thread
            .and_then(|t| t.model_options.get("context"))
            .and_then(|v| v.as_str().map(String::from))
            .or(context_default);
        let context_index = context_current
            .and_then(|c| context_values.iter().position(|v| *v == c))
            .map(|i| i as i32)
            .unwrap_or(-1);

        let fast_visible = info
            .map(|m| m.options_schema.pointer("/properties/fast").is_some())
            .unwrap_or(false);
        let fast_default = info
            .and_then(|m| m.options_schema.pointer("/properties/fast/default"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let fast_checked = thread
            .and_then(|t| t.model_options.get("fast"))
            .and_then(|v| v.as_bool())
            .unwrap_or(fast_default);

        self.thinking_key = key;
        self.thinking_values = values.clone();
        self.context_values = context_values.clone();
        ui::set_model_knobs(
            &self.ui,
            values.iter().map(|v| level_label(v)).collect(),
            index,
            context_values.iter().map(|v| context_label(v)).collect(),
            context_index,
            fast_visible,
            fast_checked,
        );
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

    /// Re-fold the current thread into chat rows. `scroll` jumps the list to
    /// the end — wanted when content arrives or threads switch, jarring for
    /// in-place toggles (tool details, raw view).
    fn render_chat(&mut self, scroll: bool) {
        let Some(thread_id) = self.current_thread_id() else {
            self.row_call_ids.clear();
            ui::set_chat(&self.ui, Vec::new(), false);
            ui::set_composer_enabled(&self.ui, false);
            return;
        };
        let raw_turns: HashSet<u64> = self
            .raw_turns
            .iter()
            .filter(|(t, _)| *t == thread_id)
            .map(|(_, turn)| *turn)
            .collect();
        let collapsed: HashSet<String> = self
            .collapsed_cards
            .iter()
            .filter(|(t, _)| *t == thread_id)
            .map(|(_, key)| key.clone())
            .collect();
        let vm = self.vms.entry(thread_id).or_default();
        let (rows, call_ids) =
            render::chat_rows(vm, &self.expanded_tools, &raw_turns, &collapsed);
        self.row_call_ids = call_ids;
        ui::set_chat(&self.ui, rows, scroll);
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
        self.diff_raw = diff.diff;
        self.push_diff();
        Ok(())
    }

    fn push_diff(&self) {
        ui::set_diff(
            &self.ui,
            slint_diff_view::build_rows(&self.diff_files, &self.diff_collapsed),
            self.diff_raw.clone(),
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
        self.refresh_clis().await;
    }

    /// Fetch managed vendor-CLI state and render the settings rows. Install
    /// progress is stateless: the server's install status drives the busy
    /// flag and status text on every refresh.
    async fn refresh_clis(&mut self) {
        let Ok(list) = self.client.list_clis().await else {
            return;
        };
        let mut rows = Vec::new();
        for cli in list.clis {
            let install = self
                .client
                .cli_install_status(&cli.id)
                .await
                .ok()
                .filter(|s| s.status != "none");
            let version_label = match (&cli.installed_version, cli.source.as_str()) {
                (Some(v), source) => {
                    let origin = if source == "managed" { "managed" } else { "system" };
                    match (&cli.latest_version, cli.update_available) {
                        (Some(latest), true) => format!("{v} ({origin}) — {latest} available"),
                        _ => format!("{v} ({origin})"),
                    }
                }
                (None, _) => "not installed".to_string(),
            };
            let action_label = if cli.installed_version.is_none() {
                "Install".to_string()
            } else if cli.update_available {
                "Update".to_string()
            } else {
                String::new()
            };
            let (status, busy) = match install.as_ref().map(|s| s.status.as_str()) {
                Some("pending") => (
                    match install.as_ref().and_then(|s| s.version.clone()) {
                        Some(v) => format!("downloading {v}…"),
                        None => "downloading…".to_string(),
                    },
                    true,
                ),
                Some("failed") => (
                    format!(
                        "install failed: {}",
                        install
                            .as_ref()
                            .and_then(|s| s.error.clone())
                            .unwrap_or_default()
                    ),
                    false,
                ),
                _ => (String::new(), false),
            };
            rows.push((cli.id, cli.display_name, version_label, action_label, status, busy));
        }
        ui::set_clis(&self.settings_ui, rows);
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
            UiCommand::ToggleArchivedFilter => {
                self.show_archived = !self.show_archived;
                ui::set_show_archived(&self.ui, self.show_archived);
                self.push_nav();
            }
            UiCommand::SessionDelete { row } => {
                if let Some(i) = self.nav_session(row) {
                    let id = self.sessions[i].id.clone();
                    let was_current = self.current_session == Some(i);
                    self.client.delete_session(&id).await?;
                    // Drop the session's resume bookmarks along with it.
                    if let Some(thread_id) = self.resume.session_threads.remove(&id) {
                        self.resume.thread_scroll.remove(&thread_id);
                    }
                    if self.resume.session_id == id {
                        self.resume.session_id.clear();
                    }
                    crate::winstate::save_resume(&self.resume);
                    if was_current {
                        self.current_session = None;
                        self.threads.clear();
                        self.current_thread = None;
                        self.push_threads();
                        self.render_chat(true);
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
                self.render_chat(true);
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
                    self.render_chat(true);
                    self.push_context();
                    self.remember_position();
                    self.restore_scroll();
                }
                // i == threads.len() is the provisional tab itself: no-op.
            }
            UiCommand::ChatScrolled(y) => {
                if let Some(thread_id) = self.current_thread_id() {
                    if self.resume.thread_scroll.get(&thread_id) != Some(&y) {
                        self.resume.thread_scroll.insert(thread_id, y);
                        crate::winstate::save_resume(&self.resume);
                    }
                }
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
                    self.render_chat(false);
                }
            }
            UiCommand::ToggleRawTurn(turn) => {
                if let Some(thread_id) = self.current_thread_id() {
                    let key = (thread_id, turn);
                    if !self.raw_turns.remove(&key) {
                        self.raw_turns.insert(key);
                    }
                    self.render_chat(false);
                }
            }
            UiCommand::ToggleCard(card_key) => {
                if let Some(thread_id) = self.current_thread_id() {
                    let key = (thread_id, card_key);
                    if !self.collapsed_cards.remove(&key) {
                        self.collapsed_cards.insert(key);
                    }
                    self.render_chat(false);
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
                // Options are per-model; switching models resets them.
                self.update_current_thread(UpdateThreadRequest {
                    model,
                    model_options: Some(serde_json::Map::new()),
                    ..Default::default()
                })
                .await;
            }
            UiCommand::ComposerThinkingChanged(i) => {
                let key = self.thinking_key.clone();
                let token = self.thinking_values.get(i).cloned();
                if let (Some(key), Some(token)) = (key, token) {
                    let mut options = self.current_model_options();
                    options.insert(key, serde_json::Value::String(token));
                    self.update_current_thread(UpdateThreadRequest {
                        model_options: Some(options),
                        ..Default::default()
                    })
                    .await;
                }
            }
            UiCommand::ComposerContextChanged(i) => {
                if let Some(token) = self.context_values.get(i).cloned() {
                    let mut options = self.current_model_options();
                    options.insert("context".into(), serde_json::Value::String(token));
                    self.update_current_thread(UpdateThreadRequest {
                        model_options: Some(options),
                        ..Default::default()
                    })
                    .await;
                }
            }
            UiCommand::ComposerFastToggled(on) => {
                let mut options = self.current_model_options();
                options.insert("fast".into(), serde_json::Value::Bool(on));
                self.update_current_thread(UpdateThreadRequest {
                    model_options: Some(options),
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
                    ui::set_file_view(&self.ui, joined, file.content, lines);
                }
            }
            UiCommand::OpenChatFile(path) => {
                let Some(index) = self.current_session else {
                    return Ok(());
                };
                let session = &self.sessions[index];
                let session_id = session.id.clone();
                // Vendor tools report absolute paths; the server wants
                // worktree-relative ones.
                let rel = path
                    .strip_prefix(session.worktree_path.as_str())
                    .map(|p| p.trim_start_matches('/').to_string())
                    .unwrap_or(path);
                match self.client.session_file(&session_id, &rel).await {
                    Ok(file) => {
                        let lines = render::highlight_file(&file.path, &file.content);
                        ui::set_file_view(&self.ui, rel, file.content, lines);
                        ui::set_right_tab(&self.ui, 1);
                    }
                    Err(e) => self.status(&format!("could not open {rel}: {e}")),
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
            UiCommand::CliInstall(id) => match self.client.start_cli_install(&id).await {
                Ok(()) => {
                    ui::set_settings_status(&self.settings_ui, format!("installing {id}…"));
                    self.refresh_clis().await;
                    // Poll until the install settles; every refresh re-renders
                    // the row from the server's install status.
                    let client = self.client.clone();
                    let settings_ui = self.settings_ui.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        for _ in 0..600 {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            let Ok(status) = client.cli_install_status(&id).await else {
                                return;
                            };
                            match status.status.as_str() {
                                "pending" => continue,
                                "success" => {
                                    ui::set_settings_status(
                                        &settings_ui,
                                        format!(
                                            "installed {id} {}",
                                            status.version.unwrap_or_default()
                                        ),
                                    );
                                }
                                _ => {
                                    ui::set_settings_status(
                                        &settings_ui,
                                        format!(
                                            "install of {id} failed: {}",
                                            status.error.unwrap_or_default()
                                        ),
                                    );
                                }
                            }
                            let _ = tx.send(UiCommand::RefreshSettings);
                            return;
                        }
                    });
                }
                Err(e) => {
                    ui::set_settings_status(&self.settings_ui, format!("{e:#}"));
                }
            },
            UiCommand::Event(thread_id, envelope) => {
                let vm = self.vms.entry(thread_id.clone()).or_default();
                let changed = vm.apply(&envelope);
                if self.current_thread_id().as_deref() == Some(&thread_id) {
                    // Compaction/usage state can change without a chat row
                    // changing, so the dial refreshes on every event.
                    self.push_context();
                    if changed.is_some() {
                        self.render_chat(true);
                    }
                    if matches!(envelope.event, trouve_protocol::Event::TurnCompleted { .. }) {
                        let _ = self.refresh_diff().await;
                        self.refresh_usage_text().await;
                    }
                }
                self.push_agents_running();
            }
            UiCommand::QuitWhenIdle => {
                self.quit_when_idle = true;
                self.status("quitting once all agents finish…");
                self.push_agents_running();
            }
        }
        Ok(())
    }

    /// PATCH the current thread's settings; on failure (e.g. mid-turn
    /// conflict) surface the error and restore the pickers.
    /// The current thread's stored model options (empty when no thread).
    fn current_model_options(&self) -> serde_json::Map<String, serde_json::Value> {
        self.current_thread
            .and_then(|i| self.threads.get(i))
            .map(|t| t.model_options.clone())
            .unwrap_or_default()
    }

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
        self.render_chat(true);
        self.push_context();
        self.remember_position();
        Ok(())
    }
}

fn short_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_string()
}

/// The thinking-style enum in a model's options schema, if any: property
/// name, value tokens, and the schema default. Providers name the knob
/// differently (anthropic: thinking_level, codex: reasoning_effort,
/// cursor's ACP catalog: effort or reasoning).
fn thinking_property(schema: &serde_json::Value) -> Option<(String, Vec<String>, Option<String>)> {
    for key in ["thinking_level", "reasoning_effort", "effort", "reasoning"] {
        let Some(prop) = schema.pointer(&format!("/properties/{key}")) else {
            continue;
        };
        let Some(values) = enum_values(prop) else {
            continue;
        };
        if values.len() > 1 {
            let default = prop["default"].as_str().map(String::from);
            return Some((key.into(), values, default));
        }
    }
    None
}

/// The context-size enum in a model's options schema, if any (cursor models
/// with a 300k/1M choice): value tokens and the schema default.
fn context_property(schema: &serde_json::Value) -> Option<(Vec<String>, Option<String>)> {
    let prop = schema.pointer("/properties/context")?;
    let values = enum_values(prop)?;
    if values.len() > 1 {
        let default = prop["default"].as_str().map(String::from);
        return Some((values, default));
    }
    None
}

fn enum_values(prop: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        prop["enum"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
    )
}

/// Human label for a context-size token ("300k" → "300K", "1m" → "1M").
fn context_label(token: &str) -> String {
    token.to_uppercase()
}

/// Human label for a thinking-level token.
fn level_label(token: &str) -> String {
    match token {
        "off" => "Off".into(),
        "none" => "None".into(),
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "default" => "Default".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "Extra High".into(),
        "max" => "Max".into(),
        other => other.to_string(),
    }
}

/// Prefer the "code" mode as the default picker selection.
/// Display label for a mode: its declared display name, or the id with the
/// first letter capitalized when a (user-defined) mode omits one.
fn mode_display_name(display_name: &str, id: &str) -> String {
    if !display_name.trim().is_empty() {
        return display_name.to_string();
    }
    let mut cs = id.chars();
    match cs.next() {
        Some(first) => first.to_uppercase().collect::<String>() + cs.as_str(),
        None => String::new(),
    }
}

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
