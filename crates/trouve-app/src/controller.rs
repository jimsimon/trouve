//! App controller: owns the spawned local server, the protocol client, and
//! all client state. Runs on a tokio runtime in a background thread; the UI
//! thread sends [`UiCommand`]s in, and the controller pushes rendered plain
//! data back via `Weak::upgrade_in_event_loop`.
//!
//! Invariant 1 holds in embedded form (ADR 0008): the app runs
//! `trouve-server` in-process through its one bootstrap entry point and
//! speaks HTTP/SSE to it over loopback — no `trouve-core` import, no
//! engine access.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use trouve_client_core::client::ProtocolClient;
use trouve_client_core::viewmodel::ThreadViewModel;
use trouve_protocol::{
    AddLocalModelRequest, AgentMode, ApprovalDecision, CreateSessionRequest, CreateThreadRequest,
    DirEntry, EventEnvelope, ModelInfo, PermissionMode, Session, Thread, TodoStatus,
    UpdateSessionRequest, UpdateThreadRequest, UpsertModeRequest, UpsertProviderRequest, Workspace,
};

use crate::render;
use crate::ui::{self, NavRowData};

/// Right-panel tab index of the integrated terminal (see app.slint's
/// TabWidget order: Diff, Files, Pull Requests, MCP, Terminal).
const TERMINAL_TAB: i32 = 4;
/// Conditional Todos panel. It sits above the stable inspection TabWidget,
/// so showing or hiding it never changes the indices above.
const TODOS_TAB: i32 = 5;
/// GitHub data is fresh enough for a dashboard without creating sustained
/// API pressure from the per-PR enrichment requests.
const PR_DASH_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

fn same_pull_request(left: &trouve_protocol::PrInfo, right: &trouve_protocol::PrInfo) -> bool {
    left.number == right.number
        && left.host.eq_ignore_ascii_case(&right.host)
        && left.repository.eq_ignore_ascii_case(&right.repository)
}

/// Project the account feed into one session without broadening association.
/// Exact session-branch matches are intrinsically related; cross-branch PRs
/// enter only after the session endpoint has returned them in `known` based
/// on durable activity from this session and all of its threads.
fn project_session_prs<'a>(
    session: &Session,
    dashboard: impl IntoIterator<Item = &'a trouve_protocol::PrInfo>,
    known: &[trouve_protocol::PrInfo],
) -> Vec<trouve_protocol::PrInfo> {
    let mut prs = Vec::new();
    for pr in dashboard {
        let exact_branch = pr.workspace_id == session.workspace_id && pr.head == session.branch;
        if exact_branch || known.iter().any(|existing| same_pull_request(existing, pr)) {
            prs.push(pr.clone());
        }
    }
    for pr in known {
        if !prs.iter().any(|existing| same_pull_request(existing, pr)) {
            prs.push(pr.clone());
        }
    }
    prs.sort_by_key(|pr| (pr.state != "open", std::cmp::Reverse(pr.number)));
    prs
}

/// Terminal scrollback the client-side screen model keeps, in lines.
const TERM_SCROLLBACK: usize = 5000;
const TERM_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);
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
    /// Flip the "show archived sessions" filter of one workspace (by the
    /// nav row of its header, where the funnel menu lives).
    ToggleArchivedFilter {
        row: usize,
    },
    /// Remove a workspace from the sidebar without deleting its sessions.
    CloseWorkspace {
        row: usize,
    },
    /// Quit once all running agent turns complete.
    QuitWhenIdle,
    /// Disarm a previously requested deferred quit.
    CancelQuitWhenIdle,
    /// Native folder picker → register the chosen directory as a workspace.
    OpenWorkspaceDialog,
    /// The "+" on a workspace header row: new session there.
    WorkspaceNewSession(usize),
    /// Move a workspace relative to another workspace (pointer drop).
    WorkspaceDropped {
        workspace_id: String,
        target_id: String,
        after: bool,
    },
    /// Move a workspace by a signed number of positions (keyboard / AT).
    WorkspaceMoved {
        workspace_id: String,
        offset: i32,
    },
    OpenSettings,
    CloseSettings,
    /// Theme / font changed: re-render everything with baked colors
    /// (syntax-highlight segments, inline-code tints). The palette itself
    /// was already swapped on the UI thread.
    AppearanceChanged,
    /// Notification preferences toggled (already persisted on the UI
    /// thread); the controller keeps a copy to gate event notifications.
    NotifyPrefsChanged(crate::winstate::Notifications),
    /// Window focus sampled by the UI-thread poll. Returning to the visible
    /// session acknowledges its unread-work badge.
    WindowFocusChanged(bool),
    /// "Send test notification" in settings.
    NotifyTest,
    /// A desktop notification was clicked: raise the window and reveal the
    /// thread it was about.
    NotificationActivated {
        session_id: String,
        thread_id: String,
    },

    // New-chat screens.
    NewSession,
    NewThread,
    CancelNewChat,
    NewChatWorkspaceChanged(usize),
    NewChatModelChanged {
        mode_idx: usize,
        model_idx: usize,
    },
    RegisterWorkspacePath(String),
    StartNewChat {
        workspace_idx: usize,
        branch_idx: usize,
        fetch_latest: bool,
        mode_idx: usize,
        model_idx: usize,
        thinking_idx: usize,
        permission_idx: usize,
        prompt: String,
    },

    // Chat screen.
    SelectThread(usize),
    /// A user-driven chat scroll, captured as the first visible rendered
    /// row plus the offset within it. Carries the thread the viewport was
    /// showing at capture time so a concurrent switch cannot bleed state.
    ChatPositionChanged {
        thread_id: String,
        row: usize,
        offset: f32,
        at_bottom: bool,
    },
    SendMessage(String),
    CancelTurn,
    /// The "@" mention popup opened (or is filtering): refresh the worktree
    /// path list feeding it. Throttled per session by the controller.
    RefreshAtFiles,
    /// Composer 📎 button: pick files to ride with the next prompt.
    AttachFileDialog,
    /// A file's bytes staged as a prompt attachment (from the picker or a
    /// clipboard image paste).
    AddAttachment {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    /// Composer attachment chip ✕ at `index`.
    AttachmentRemoved(usize),
    /// Queued-prompt panel: replace the text of the row at `index`.
    QueueEdit {
        index: usize,
        content: String,
    },
    /// Queued-prompt panel: remove the row at `index`.
    QueueDelete(usize),
    /// Queued-prompt panel: swap the row at `index` with its neighbor
    /// (`delta` is -1 to run earlier, +1 later).
    QueueMove {
        index: usize,
        delta: i32,
    },
    /// Queued-prompt panel: drag-and-drop — move the row at `from` to land
    /// at position `to` (remove-and-insert, not a swap).
    QueueReorder {
        from: usize,
        to: usize,
    },
    /// Queued-prompt panel: start draining an idle thread's queue.
    QueueSendNow,
    QueueSendNowAt(usize),
    Approval {
        row: usize,
        approved: bool,
    },
    /// Question wizard: toggle option `option` (options.len() = "Other") of
    /// the current page of the wizard at `row`.
    QuestionOption {
        row: usize,
        option: usize,
    },
    /// Question wizard: the "Other" free-form text changed.
    QuestionOtherEdited {
        row: usize,
        text: String,
    },
    /// Question wizard: back to the previous question.
    QuestionBack(usize),
    /// Question wizard: advance (next question / review page / submit).
    QuestionNext(usize),
    /// Question wizard: skip the whole request unanswered.
    QuestionSkip(usize),
    ToggleTool(usize),
    /// Toggle a turn between styled markdown and raw selectable text.
    ToggleRawTurn(u64),
    /// Collapse/expand a chat card (user/assistant/thinking header).
    ToggleCard(String),
    ComposerModeChanged(usize),
    ComposerModelChanged(usize),
    ComposerThinkingChanged(usize),
    ComposerPermissionChanged(usize),
    ComposerContextChanged(usize),
    ComposerFastToggled(bool),

    // Right column.
    /// The right panel switched tabs (terminal attaches lazily on visit).
    RightTabChanged(i32),
    RefreshDiff,
    ToggleDiffFile(usize),
    Undo,
    Redo,
    CreatePr,
    RefreshPrs,
    SelectPr(usize),
    OpenPrUrl(String),
    /// Internal: a background PR fetch finished (session it was for, PRs or
    /// an error message).
    PrsLoaded(String, Result<Vec<trouve_protocol::PrInfo>, String>),
    FileActivated(usize),
    /// Open a worktree-relative file in the user's preferred editor.
    OpenFileExternally(String),
    /// A filename clicked in a chat tool card; path as the tool saw it
    /// (possibly absolute), plus the 1-based line range the tool covered
    /// (0 = none) to preselect in the file view.
    OpenChatFile(String, i32, i32),

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
    SetDefaultModel(usize, Option<String>),
    /// Set the global default permission mode (0 ask/1 allow-list/2 yolo).
    SetDefaultPermission(i32),
    /// Create/update a user-level mode (a built-in id customizes it).
    /// Fields: id, display name, system prompt, comma-separated allowed
    /// tools, read-only, permission index (-1 global default/0 ask/
    /// 1 allow-list/2 yolo), model index into the models catalog
    /// (-1 = global default).
    SaveMode(
        String,
        String,
        String,
        String,
        bool,
        i32,
        i32,
        Option<String>,
    ),
    /// Remove a custom mode / reset a customized built-in.
    DeleteMode(String),
    /// Quick per-row change of a mode's default model (-1 = global).
    SetModeModel(String, i32),
    /// Quick per-row change of a mode's thinking level (None = global).
    SetModeThinking(String, Option<String>),
    /// Download/update a managed vendor CLI ("cursor-agent", "claude", "codex").
    CliInstall(String),
    /// Cancel an in-flight CLI install (also used for "llama-server").
    CliCancel(String),
    /// Remove the managed install of a CLI (also used for "llama-server").
    CliUninstall(String),
    /// Re-render just the vendor-CLI rows (install progress polling).
    RefreshClis,
    /// Re-fetch local (offline) model state for the settings screen.
    RefreshLocal,
    /// Internal: the local status + runtime-install fetch finished.
    LocalLoaded(
        Result<
            (
                trouve_protocol::LocalStatus,
                trouve_protocol::CliInstallStatus,
            ),
            String,
        >,
    ),
    /// Start downloading one local model's GGUF.
    LocalDownload(String),
    /// Cancel an in-flight GGUF download (partial file is deleted).
    LocalCancelDownload(String),
    /// Delete a local model's GGUF (custom entries disappear entirely).
    LocalDeleteModel(String),
    /// Stop the llama-server sidecar to free memory.
    LocalStopServer,
    /// Restart the llama-server sidecar with its current model.
    LocalRestartServer,
    /// Turn local models on/off (off stops the sidecar and hides the
    /// "local" provider's models).
    LocalEnabledToggled(bool),
    /// Register a custom GGUF (HuggingFace repo + file).
    LocalAddModel {
        repo: String,
        file: String,
    },
    /// Search HuggingFace for GGUF repos to add as local models.
    LocalSearch(String),
    /// HuggingFace search result filters: show repos with files that fit
    /// the GPU / fit RAM (CPU) / don't fit at all.
    LocalSearchFilters {
        gpu: bool,
        cpu: bool,
        large: bool,
    },
    /// Internal: a local-model search finished.
    LocalSearchLoaded(Result<Vec<trouve_protocol::LocalSearchResult>, String>),
    /// Open the full-window pull-requests dashboard.
    OpenPullRequests,
    /// Leave the pull-requests dashboard (back to chat / new-chat).
    ClosePullRequests,
    /// Re-fetch account PRs from every authenticated GitHub instance.
    RefreshPullRequests,
    /// Internal periodic refresh of the shared GitHub account snapshots.
    GithubRefreshTick,
    /// Internal: the multi-instance refresh command finished. Successful
    /// dashboard data arrives separately as a persisted server event.
    PrDashRefreshFinished(String, Result<(), String>),
    /// Dashboard project filter changed (0 = all projects).
    PrDashFilterPicked(i32),
    /// Collapse/expand a dashboard group.
    PrGroupToggled(String),
    /// Move a dashboard group relative to another group (pointer drop).
    PrGroupDropped {
        key: String,
        target_key: String,
        after: bool,
    },
    /// Move a dashboard group by a signed number of positions (keyboard/AT).
    PrGroupMoved {
        key: String,
        offset: i32,
    },
    /// Jump to the chat whose session owns this PR's branch, or start a
    /// new chat for it when none exists.
    PrChatClicked {
        workspace_id: String,
        branch: String,
    },
    /// Open the full-window automations screen.
    OpenAutomations,
    /// Leave the automations screen (back to chat / new-chat).
    CloseAutomations,
    /// Re-fetch the automations list.
    RefreshAutomations,
    /// Internal: the automations fetch finished.
    AutomationsLoaded(Result<Vec<trouve_protocol::Automation>, String>),
    AutomationTemplatesLoaded(Vec<trouve_protocol::AutomationTemplate>),
    /// Create (id "") or update an automation from the form fields.
    SaveAutomation {
        id: String,
        name: String,
        prompt: String,
        workspace_id: String,
        /// "hourly" / "daily" / "weekly".
        kind: String,
        /// Minute of the hour, as typed (hourly).
        minute: String,
        /// "HH:MM" (daily/weekly).
        time: String,
        /// Comma-separated Monday-first day indices (weekly).
        days: String,
        /// 0 Ask, 1 Allow-list, 2 Yolo.
        permission_index: i32,
        enabled: bool,
    },
    /// Pause/resume an automation.
    AutomationToggled(String, bool),
    /// Fire an automation immediately.
    RunAutomation(String),
    DeleteAutomation(String),
    /// Internal: a server-scope event (session lifecycle, automation runs)
    /// arrived on the global stream.
    ServerEvent(Box<trouve_protocol::EventEnvelope>),
    /// Internal: the transient "back online" notice timed out. Carries the
    /// sequence number of the notice it should clear, so a newer notice
    /// survives an older notice's timer.
    ConnectivityNoticeExpired(u64),
    /// Internal: the server stopped answering (several consecutive probes
    /// failed after the event stream dropped).
    ServerConnectionLost,
    /// Internal: a probe succeeded again after the connection had been
    /// reported lost.
    ServerConnectionRestored,
    /// Internal: the locally spawned server process exited (status text).
    ServerExited(String),
    /// Open settings straight to the Integrations section.
    OpenIntegrationsSettings,
    AddGithubHost(/* host */ String, /* client id */ String),
    RemoveGithubHost(String),
    /// Re-list MCP servers (quick list, then health probes).
    RefreshMcp,
    SaveMcpServer {
        name: String,
        scope: String,
        /// Command plus args as one shell-quoted line.
        command_line: String,
        /// One KEY=VALUE per line.
        env_lines: String,
        /// Which workspace's file to edit (workspace scope only).
        workspace_id: String,
    },
    DeleteMcpServer {
        name: String,
        scope: String,
        workspace_id: String,
    },
    /// Fetch recent log lines for one MCP server.
    McpLogs(String),
    /// Internal: an MCP list fetch finished (true = with health probes).
    McpLoaded(Vec<trouve_protocol::McpServerInfo>, bool),
    /// Re-fetch the current session's effective MCP config (right panel).
    RefreshSessionMcp,
    /// Internal: the session MCP fetch finished.
    SessionMcpLoaded(String, Result<Vec<trouve_protocol::McpServerInfo>, String>),
    /// Internal: a versioned subscription health fetch finished.
    SubscriptionsLoaded {
        generation: u64,
        result: Result<Vec<trouve_protocol::SubscriptionHealth>, String>,
    },

    // Terminal tab.
    /// A key press in the terminal grid (text + modifiers, Slint encoding).
    TermKey {
        text: String,
        ctrl: bool,
        alt: bool,
    },
    /// Clipboard text pasted into the terminal.
    TermPaste(String),
    /// Mouse wheel over the terminal (+ = towards history), in lines.
    TermWheel(i32),
    /// The grid re-measured to a new cell size.
    TermResized {
        cols: u16,
        rows: u16,
    },
    /// Kill the shell and start a fresh one.
    TermRestart,
    /// Internal: output bytes arrived for a terminal (end offset included).
    TermOutput {
        session_id: String,
        terminal_id: String,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Internal: flush a coalesced terminal frame after a burst of output.
    FlushTerm {
        session_id: String,
        terminal_id: String,
    },
    /// Internal: a terminal's output stream ended (shell exit / kill).
    TermEnded {
        session_id: String,
        terminal_id: String,
    },

    /// Internal: an event arrived on some thread's stream.
    Event(String, Box<EventEnvelope>),
    /// Persisted history, coalesced so opening/reconnecting a long thread
    /// applies and renders many envelopes in one controller command. The
    /// flag distinguishes reconnect backlog (may be unread) from startup
    /// history (already viewed in an earlier app run).
    Events(String, Vec<EventEnvelope>, bool),
    /// Internal: threads were discovered for an active background session,
    /// so their streams can feed attention and unread-work badges.
    SessionThreadsLoaded(String, Result<Vec<Thread>, String>),
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

/// Picker values submitted by either new-session or new-thread setup.
struct NewChatSelection {
    workspace_idx: usize,
    branch_idx: usize,
    fetch_latest: bool,
    mode_idx: usize,
    model_idx: usize,
    thinking_idx: usize,
    permission_idx: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AttentionCounts {
    approvals: usize,
    questions: usize,
}

fn thread_attention(vm: &ThreadViewModel) -> AttentionCounts {
    AttentionCounts {
        approvals: vm.pending_approvals.len(),
        questions: vm.pending_questions.len(),
    }
}

const SUBSCRIPTION_REFRESH_TTL: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscriptionRefresh {
    IfStale,
    Force,
}

#[derive(Debug, Default)]
struct SubscriptionRefreshState {
    last_started_at: Option<std::time::Instant>,
    generation: u64,
}

impl SubscriptionRefreshState {
    /// Start a refresh when forced or stale, returning the generation that
    /// must accompany its response. Starting one invalidates every older
    /// in-flight response without requiring the HTTP task to be cancelled.
    fn begin(&mut self, now: std::time::Instant, freshness: SubscriptionRefresh) -> Option<u64> {
        let fresh = self
            .last_started_at
            .is_some_and(|at| now.saturating_duration_since(at) < SUBSCRIPTION_REFRESH_TTL);
        if freshness == SubscriptionRefresh::IfStale && fresh {
            return None;
        }
        self.last_started_at = Some(now);
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }
}

struct Controller {
    ui: slint::Weak<crate::AppWindow>,
    client: ProtocolClient,
    tx: mpsc::UnboundedSender<UiCommand>,

    workspaces: Vec<Workspace>,
    /// Client-local workspace order, persisted independently of protocol
    /// registration order.
    workspace_order: Vec<String>,
    /// The workspace the app was started in (default for new sessions).
    home_workspace_id: String,
    sessions: Vec<Session>,
    nav: Vec<NavEntry>,
    current_session: Option<usize>,
    archived_expanded: HashSet<String>,
    collapsed_workspaces: HashSet<String>,
    /// Session-list filter: workspaces showing their archived sessions
    /// (each workspace header's funnel menu toggles its own entry).
    show_archived: HashSet<String>,
    /// Quit once every agent turn finishes (armed from the quit dialog).
    /// Shared with the UI callback so cancellation takes effect before its
    /// command can race a final session-activity event in this queue.
    quit_when_idle: std::sync::Arc<std::sync::atomic::AtomicBool>,

    threads: Vec<Thread>,
    current_thread: Option<usize>,

    /// GitHub integration state (None until the first fetch answers).
    /// Any GitHub host (github.com or enterprise) has working auth —
    /// gates the PR tab's fetches.
    github_configured: bool,
    /// Per-host auth state for Settings → Integrations.
    github_hosts: Vec<trouve_protocol::GithubHostIntegration>,
    /// Bytes/sec estimates for in-flight downloads, keyed by download id
    /// ("cli:claude", "model:…"). Fed by consecutive progress polls.
    download_rates: HashMap<String, RateSample>,
    /// Sessions with a turn running somewhere (any thread, any window).
    /// Seeded from `Session.active`, kept live by `session.activity`
    /// server events; drives the sidebar activity indicator.
    busy_sessions: HashSet<String>,
    /// The server can't reach the internet (seeded from `ServerInfo.online`,
    /// kept live by `server.connectivity_changed` events). While set, the
    /// model list holds only offline-capable (local) models; when it is
    /// empty too, all prompt entry is blocked with an explanatory banner.
    offline: bool,
    /// Monotonic id of the latest transient connectivity notice, so an old
    /// notice's expiry timer can't clear a newer notice.
    connectivity_notice_seq: u64,
    /// The client can't reach the server at all (distinct from `offline`,
    /// which is the server's own internet state, and strictly worse: every
    /// request fails and event streams are frozen, so all prompt entry is
    /// blocked with a red banner until a probe answers again).
    server_unreachable: bool,
    /// An automatic server respawn failed or the process is crash-looping;
    /// the banner asks for an app restart instead of promising recovery.
    server_failed: bool,
    /// When the last automatic respawn happened (crash-loop guard: a second
    /// death right after a respawn means restarting won't help).
    last_respawn: Option<std::time::Instant>,
    /// Base URL of the server, for connection-error messages.
    server_url: String,
    /// How to respawn the locally spawned server when its process dies
    /// (`None` when connected to an external server via TROUVE_SERVER_URL).
    embedded_server: Option<EmbeddedServer>,
    /// Local models: true while a poller is scheduled for an in-flight
    /// download/install, plus the last seen downloaded-model count (a
    /// change means the model catalog changed → reload pickers).
    local_polling: bool,
    local_downloaded: Option<usize>,
    /// Last HuggingFace model-search results (kept so "✓ added" flags can
    /// be updated in place after an add).
    local_search: Vec<trouve_protocol::LocalSearchResult>,
    /// Search result filters: which fit categories ("gpu", "cpu",
    /// "too-large") stay visible. Mirrors the checkboxes in the UI.
    local_search_fits: (bool, bool, bool),
    /// Automations, as last fetched (kept so pause/resume can resend the
    /// full definition).
    automations: Vec<trouve_protocol::Automation>,
    /// Pre-canned automation templates (static server catalog, fetched on
    /// first open of the screen).
    automation_templates: Vec<trouve_protocol::AutomationTemplate>,
    /// PRs associated with the current session, and the one shown.
    prs: Vec<trouve_protocol::PrInfo>,
    pr_selected: usize,
    pr_error: String,
    /// PR dashboard: per-GitHub-instance account results.
    pr_dash: HashMap<String, trouve_protocol::GithubPrList>,
    /// Multi-instance account refresh failures.
    pr_dash_errors: HashMap<String, String>,
    /// Shared GitHub refreshes in flight (the `github` key is the guard).
    pr_dash_loading: HashSet<String>,
    /// Client-local display order of the dashboard groups, persisted like
    /// the workspace sidebar order.
    pr_group_order: Vec<String>,
    /// Dashboard groups the user collapsed (session-local, like the
    /// workspace tree's collapse state).
    pr_collapsed: HashSet<String>,
    /// Project filter: `host/owner/repo` (None = all projects).
    pr_dash_filter: Option<String>,
    /// PRs per session for the compact sidebar badge. Values come only from
    /// an exact session-branch match or the authoritative session endpoint.
    nav_prs: HashMap<String, Vec<trouve_protocol::PrInfo>>,

    vms: HashMap<String, ThreadViewModel>,
    followed: HashSet<String>,
    /// Long-lived SSE followers, aborted when their owning session vanishes.
    follower_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    /// thread id → session id, for notifications about backgrounded
    /// threads (`threads` only holds the open session's).
    thread_sessions: HashMap<String, String>,
    /// Sessions whose thread list is being/has been watched for background
    /// attention. New threads still arrive through `thread.created`.
    watched_sessions: HashSet<String>,
    /// Aggregate pending requests by session; kept incrementally from each
    /// thread VM so rendering a nav row is O(1).
    attention_by_session: HashMap<String, AttentionCounts>,
    /// Sessions with completed/failed work the user has not brought on
    /// screen since it landed.
    unread_sessions: HashSet<String>,
    /// Sessions with a failed turn the user has not brought on screen since
    /// it landed. This is separate from unread so it can take precedence.
    error_sessions: HashSet<String>,
    /// Desktop notification preferences; persisted on the UI thread, this
    /// copy gates what event notifications fire.
    notify: crate::winstate::Notifications,
    /// Whether the app window has focus (winit Focused events, written on
    /// the UI thread). Focused + on-screen threads never notify.
    window_focused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Files staged for the next prompt (already base64-encoded uploads);
    /// consumed by the next send from either composer.
    pending_attachments: Vec<trouve_protocol::AttachmentUpload>,
    /// Which session's worktree paths currently back the composer's "@"
    /// mention popup, and when they were fetched (throttles refreshes).
    at_files_fetched: Option<(String, std::time::Instant)>,
    expanded_tools: HashSet<String>,
    /// Last time a streaming-delta render ran, to coalesce bursts of deltas
    /// into at most one full chat re-fold per interval (a full turn's worth
    /// of deltas would otherwise re-fold and re-clone the whole transcript
    /// on every token). Non-delta events always render immediately.
    last_delta_render: Option<std::time::Instant>,
    /// (thread id, turn) pairs showing raw text instead of styled markdown.
    raw_turns: HashSet<(String, u64)>,
    /// (thread id, card key) pairs whose card body is collapsed.
    collapsed_cards: HashSet<(String, String)>,
    row_call_ids: Vec<Option<String>>,
    /// Question-wizard state per pending request id (page, selections,
    /// "Other" texts); dropped once the request resolves.
    wizards: HashMap<String, render::WizardState>,

    /// Where-you-left-off bookmark (last session, per-session last thread,
    /// per-thread scroll), persisted to resume.json as it changes.
    resume: crate::winstate::Resume,
    /// The thread whose mid-history row anchor is currently converging.
    /// Kept controller-side so replay can override it when it reveals a
    /// running turn or queued prompts after the thread was opened.
    restoring_thread: Option<String>,

    modes: Vec<AgentMode>,
    /// Provenance per mode, aligned with `modes` (builtin / customized /
    /// custom / workspace) — drives the settings Modes & Models section.
    mode_origins: Vec<String>,
    models: Vec<ModelInfo>,
    /// Global model default, kept with the catalogs so new-session and
    /// new-thread forms start on the model the server would inherit.
    default_model: String,
    /// Global thinking default, kept with the catalogs so switching a live
    /// thread's mode can apply the same inheritance as thread creation.
    default_thinking_level: Option<String>,
    /// Last provider subscription snapshot. Picker rows are derived from
    /// this cache and the current model catalog, so catalog refreshes cannot
    /// leave the two lists misaligned.
    subscription_health: Vec<trouve_protocol::SubscriptionHealth>,
    /// Shared freshness and response-generation gate for every subscription
    /// refresh trigger.
    subscription_refresh: SubscriptionRefreshState,
    /// Thinking dropdown state for the current thread's model: the schema
    /// property the values belong to and the raw value tokens (parallel to
    /// the displayed labels).
    thinking_key: Option<String>,
    thinking_values: Vec<String>,
    /// Context-size dropdown values (schema property "context"), when the
    /// current model offers a choice (e.g. cursor's 300k/1M).
    context_values: Vec<String>,
    new_chat_thinking_key: Option<String>,
    new_chat_thinking_values: Vec<String>,

    new_chat: Option<NewChat>,
    branches: Vec<String>,

    diff_files: Vec<slint_diff_view::FileDiff>,
    diff_collapsed: Vec<bool>,
    diff_raw: String,
    /// Files tab tree: directory listings cached by worktree-relative path
    /// ("." for the root), fetched lazily as folders are expanded.
    file_children: HashMap<String, Vec<DirEntry>>,
    file_expanded: HashSet<String>,
    /// The tree flattened in display order; indices match the UI rows.
    file_rows: Vec<FileRow>,
    /// Worktree-relative path of the file open in the Files tab, for
    /// re-highlighting after a theme change.
    open_file: Option<String>,

    /// Which right-panel tab is showing (terminal attaches lazily on 4).
    right_tab: i32,
    /// Attached terminals by session id. Screen state lives client-side;
    /// followers keep feeding backgrounded sessions so switching back is
    /// instant.
    terms: HashMap<String, TermState>,
    /// Terminal output can arrive in tiny PTY chunks; cap full-grid model
    /// rebuilds to roughly one per display frame.
    last_term_render: Option<std::time::Instant>,
    term_render_pending: Option<(String, String)>,
    /// Last grid size reported by the UI (used for opens before the first
    /// resize event lands).
    term_view: (u16, u16),
}

/// One point in a download's progress, plus the smoothed transfer rate
/// derived from the previous point.
struct RateSample {
    bytes: u64,
    at: std::time::Instant,
    /// Smoothed bytes/sec (0 until two samples exist).
    rate: f64,
}

/// Client-side state of one session's terminal.
struct TermState {
    terminal_id: String,
    grid: slint_terminal::GridState,
    /// Bytes consumed from the output stream (resume offset).
    offset: u64,
    exited: bool,
}

/// One visible row of the Files tree.
#[derive(Debug, Clone)]
struct FileRow {
    /// Worktree-relative path (doubles as the open/expand key).
    path: String,
    name: String,
    is_dir: bool,
    depth: i32,
    expanded: bool,
}

pub async fn run(
    ui: slint::Weak<crate::AppWindow>,
    tx: mpsc::UnboundedSender<UiCommand>,
    mut rx: mpsc::UnboundedReceiver<UiCommand>,
    window_focused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    quit_when_idle: std::sync::Arc<std::sync::atomic::AtomicBool>,
    register_workspace: Option<std::path::PathBuf>,
) {
    let (client, server_url, spawned) = match start_local_server().await {
        Ok(parts) => parts,
        Err(e) => {
            ui::set_error(&ui, &format!("failed to start server: {e:#}"));
            return;
        }
    };
    let (embedded_server, server_handle) = match spawned {
        Some((info, handle)) => (Some(info), Some(handle)),
        None => (None, None),
    };

    let mut ctl = Controller {
        ui,
        client,
        tx,
        workspaces: Vec::new(),
        workspace_order: crate::winstate::load_workspace_order(),
        home_workspace_id: String::new(),
        sessions: Vec::new(),
        nav: Vec::new(),
        current_session: None,
        archived_expanded: HashSet::new(),
        collapsed_workspaces: HashSet::new(),
        show_archived: HashSet::new(),
        quit_when_idle,
        threads: Vec::new(),
        current_thread: None,
        github_configured: false,
        github_hosts: Vec::new(),
        download_rates: HashMap::new(),
        busy_sessions: HashSet::new(),
        offline: false,
        connectivity_notice_seq: 0,
        server_unreachable: false,
        server_failed: false,
        last_respawn: None,
        server_url,
        embedded_server,
        local_polling: false,
        local_downloaded: None,
        local_search: Vec::new(),
        // Matches the UI defaults: models that fit somewhere show, ones
        // this machine can't run are hidden.
        local_search_fits: (true, true, false),
        automations: Vec::new(),
        automation_templates: Vec::new(),
        prs: Vec::new(),
        pr_selected: 0,
        pr_error: String::new(),
        pr_dash: HashMap::new(),
        pr_dash_errors: HashMap::new(),
        pr_dash_loading: HashSet::new(),
        pr_group_order: crate::winstate::load_pr_group_order(),
        pr_collapsed: HashSet::new(),
        pr_dash_filter: None,
        nav_prs: HashMap::new(),
        vms: HashMap::new(),
        followed: HashSet::new(),
        follower_tasks: HashMap::new(),
        thread_sessions: HashMap::new(),
        watched_sessions: HashSet::new(),
        attention_by_session: HashMap::new(),
        unread_sessions: HashSet::new(),
        error_sessions: HashSet::new(),
        notify: crate::winstate::load_notifications(),
        window_focused,
        pending_attachments: Vec::new(),
        at_files_fetched: None,
        expanded_tools: HashSet::new(),
        last_delta_render: None,
        raw_turns: HashSet::new(),
        collapsed_cards: HashSet::new(),
        row_call_ids: Vec::new(),
        wizards: HashMap::new(),
        resume: crate::winstate::load_resume(),
        restoring_thread: None,
        modes: Vec::new(),
        mode_origins: Vec::new(),
        models: Vec::new(),
        default_model: String::new(),
        default_thinking_level: None,
        subscription_health: Vec::new(),
        subscription_refresh: SubscriptionRefreshState::default(),
        thinking_key: None,
        thinking_values: Vec::new(),
        context_values: Vec::new(),
        new_chat_thinking_key: None,
        new_chat_thinking_values: Vec::new(),
        new_chat: None,
        branches: Vec::new(),
        diff_files: Vec::new(),
        diff_collapsed: Vec::new(),
        diff_raw: String::new(),
        file_children: HashMap::new(),
        file_expanded: HashSet::new(),
        file_rows: Vec::new(),
        open_file: None,
        right_tab: 0,
        terms: HashMap::new(),
        last_term_render: None,
        term_render_pending: None,
        term_view: (80, 24),
    };

    // Report the embedded server's death to the command loop, which will
    // attempt one automatic restart before surfacing the failure.
    if let Some(handle) = server_handle {
        watch_embedded_server(ctl.tx.clone(), handle);
    }

    if let Err(e) = ctl.bootstrap(register_workspace).await {
        ctl.error(&format!("startup error: {e:#}"));
    }

    // Auto-refresh the diff panel: picks up agent edits mid-turn and
    // external edits alike. refresh_diff repaints only on real change.
    tokio::spawn({
        let tx = ctl.tx.clone();
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if tx.send(UiCommand::RefreshDiff).is_err() {
                    break;
                }
            }
        }
    });

    // One shared account refresh keeps the dashboard, sidebar PR indicators,
    // and right-panel PR data current. The existing in-flight key prevents
    // this timer from overlapping a slow manual refresh.
    tokio::spawn({
        let tx = ctl.tx.clone();
        async move {
            let mut tick = tokio::time::interval(PR_DASH_REFRESH_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // `interval`'s first tick is immediate; opening the dashboard
            // performs the initial fetch, so wait for the first real period.
            tick.tick().await;
            loop {
                tick.tick().await;
                if tx.send(UiCommand::GithubRefreshTick).is_err() {
                    break;
                }
            }
        }
    });

    while let Some(command) = rx.recv().await {
        let result = ctl.handle(command).await;
        if let Err(e) = result {
            ctl.error(&format!("{e:#}"));
        }
    }
}

/// Start the embedded `trouve-server` on an ephemeral loopback port and
/// wait for it to answer. `TROUVE_SERVER_URL` skips embedding and connects
/// to an existing (possibly remote) server instead.
async fn start_local_server() -> Result<(
    ProtocolClient,
    String,
    Option<(EmbeddedServer, tokio::task::JoinHandle<Result<()>>)>,
)> {
    if let Ok(url) = std::env::var("TROUVE_SERVER_URL") {
        // Connecting to an externally-managed server: the user supplies its
        // token (if any) in the environment.
        let token = std::env::var("TROUVE_AUTH_TOKEN").ok();
        let client = ProtocolClient::with_token(&url, token);
        client
            .info()
            .await
            .with_context(|| format!("connecting to {url}"))?;
        return Ok((client, url, None));
    }

    // A per-launch bearer token so no other local process can drive the
    // server we embed (it can run shell and edit files).
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );

    let (addr, handle) = spawn_embedded_server("127.0.0.1:0".parse()?, &token).await?;
    let info = EmbeddedServer {
        addr,
        token: token.clone(),
    };
    let url = format!("http://{addr}");
    let client = ProtocolClient::with_token(&url, Some(token));
    if let Err(e) = wait_server_ready(&client).await {
        // Abort and join so the listener/engine tear down before we return.
        handle.abort();
        let _ = handle.await;
        return Err(e)
            .with_context(|| format!("embedded trouve-server did not become ready on {addr}"));
    }
    Ok((client, url, Some((info, handle))))
}

/// Everything needed to relaunch the embedded server on the same address
/// with the same per-launch token (the client keeps both).
#[derive(Clone)]
struct EmbeddedServer {
    addr: std::net::SocketAddr,
    token: String,
}

/// Bind and launch the embedded server task (full local engine behind the
/// protocol; the app only ever sees the address). Returns the bound
/// address and the join handle used to observe its exit.
async fn spawn_embedded_server(
    addr: std::net::SocketAddr,
    token: &str,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    let security = trouve_server::ServerSecurity::with_token(token.to_string());
    let (addr, server) = trouve_server::bind_local(addr, security).await?;
    Ok((addr, tokio::spawn(server)))
}

async fn wait_server_ready(client: &ProtocolClient) -> Result<()> {
    for _ in 0..100 {
        if client.info().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("server did not answer /v1/info in time")
}

/// Observe the embedded server task and report its exit to the command
/// loop. The serve future normally runs for the app's whole lifetime, so
/// any exit — error or panic (surfaced as a `JoinError`) — is exceptional.
fn watch_embedded_server(
    tx: mpsc::UnboundedSender<UiCommand>,
    handle: tokio::task::JoinHandle<Result<()>>,
) {
    tokio::spawn(async move {
        let status = match handle.await {
            Ok(Ok(())) => "exited cleanly".to_string(),
            Ok(Err(e)) => format!("{e:#}"),
            Err(e) => format!("panicked: {e}"),
        };
        let _ = tx.send(UiCommand::ServerExited(status));
    });
}

impl Controller {
    fn error(&self, text: &str) {
        ui::set_error(&self.ui, text);
    }

    async fn bootstrap(&mut self, register_workspace: Option<std::path::PathBuf>) -> Result<()> {
        if let Some(path) = register_workspace {
            let path_str = path.to_str().context("workspace path is not valid UTF-8")?;
            let workspace = self
                .client
                .register_workspace(path_str)
                .await
                .with_context(|| format!("registering {} as workspace", path.display()))?;
            self.home_workspace_id = workspace.id.clone();
        }

        self.reload_sessions().await?;
        self.sync_home_workspace();

        // Seed connectivity before the first catalog load: when the server
        // started offline, the model list is already filtered and the
        // banner must say why (transitions arrive as server events later).
        if let Ok(info) = self.client.info().await {
            self.offline = !info.online;
        }
        self.reload_catalogs().await;
        self.refresh_subscriptions(SubscriptionRefresh::IfStale);

        if let Ok(gh) = self.client.github_integration().await {
            self.apply_github_integration(gh);
        }
        self.push_github_integration();
        self.push_prs();
        self.refresh_pr_dashboard();
        self.refresh_nav_prs(false);

        // Follow the server-scope event stream for the lifetime of the app:
        // sessions created in the background (scheduled automations) and
        // automation run outcomes arrive here. The handler ignores stale
        // envelopes, so the history replay on (re)connect is harmless.
        //
        // The task doubles as the connection watchdog: whenever the stream
        // drops it probes /v1/info until the server answers again. A couple
        // of failed probes are a blip; a sustained outage (and its recovery)
        // becomes UI state via ServerConnectionLost/Restored.
        {
            let client = self.client.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let mut after = 0u64;
                let mut lost_reported = false;
                loop {
                    if let Ok(last) = client
                        .follow_server_events(after, |envelope| {
                            let _ = tx.send(UiCommand::ServerEvent(Box::new(envelope)));
                            std::ops::ControlFlow::Continue(())
                        })
                        .await
                    {
                        after = after.max(last);
                    }
                    let mut failures = 0u32;
                    loop {
                        if client.info().await.is_ok() {
                            if lost_reported {
                                lost_reported = false;
                                if tx.send(UiCommand::ServerConnectionRestored).is_err() {
                                    return;
                                }
                            }
                            break;
                        }
                        failures += 1;
                        if failures == 3 && !lost_reported {
                            lost_reported = true;
                            if tx.send(UiCommand::ServerConnectionLost).is_err() {
                                return;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    // The probe answered: reconnect the stream (the cursor
                    // makes replay lossless). The pause keeps a stream that
                    // ends immediately from spinning hot.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            });
        }

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
        Ok(())
    }

    /// Refresh modes/models (after provider changes) and push all pickers.
    async fn reload_catalogs(&mut self) {
        let home_workspace =
            (!self.home_workspace_id.is_empty()).then_some(self.home_workspace_id.as_str());
        let infos = self
            .client
            .list_mode_infos(home_workspace)
            .await
            .unwrap_or_default();
        self.modes = infos.iter().map(|i| i.mode.clone()).collect();
        self.mode_origins = infos.into_iter().map(|i| i.origin).collect();
        self.models = self.client.list_models().await.unwrap_or_default();
        if let Ok(providers) = self.client.list_providers().await {
            self.default_model = providers.default_model;
            self.default_thinking_level = providers.default_thinking_level;
        }
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
        self.push_model_health();
        // Each mode's effective default model, so a new-chat mode change
        // applies the same mode > global precedence as thread creation.
        ui::set_mode_model_indices(
            &self.ui,
            self.modes
                .iter()
                .map(|m| {
                    preferred_model_index(
                        &self.models,
                        m.default_model.as_deref(),
                        Some(self.default_model.as_str()),
                    )
                })
                .collect(),
        );
        self.push_picker_indices();
        // The offline banner depends on the (server-filtered) model list:
        // local models keep prompts usable, an empty list blocks them.
        self.push_connectivity();
    }

    /// Align the cached provider-level subscription state with every model
    /// row. Providers without an applicable health record get empty entries,
    /// which hides both the row annotation and the selected-model pill.
    fn push_model_health(&self) {
        let by_provider: HashMap<&str, &trouve_protocol::SubscriptionHealth> = self
            .subscription_health
            .iter()
            .map(|health| (health.provider_id.as_str(), health))
            .collect();
        let health = self
            .models
            .iter()
            .map(|model| {
                model
                    .id
                    .split_once('/')
                    .and_then(|(provider, _)| by_provider.get(provider).copied())
                    .map(model_health_view)
                    .unwrap_or_default()
            })
            .collect();
        ui::set_model_health(&self.ui, health);
    }

    /// Whether prompt entry and prompt-adjacent mutations are blocked: the
    /// server is unreachable, or it is offline with nothing runnable. One
    /// predicate feeds both the banner/input gate and the command-loop
    /// rejection in [`Self::handle`], so the two can't disagree.
    fn connectivity_blocked(&self) -> bool {
        self.server_unreachable || (self.offline && self.models.is_empty())
    }

    /// Push the connectivity banner + input gate. A lost client→server
    /// connection outranks the server's own internet state — nothing works
    /// while the server is gone, so that banner is red and blocks
    /// everything. Otherwise: offline with local models available keeps
    /// prompt entry usable (restricted to those models); offline with
    /// nothing usable blocks it and says why.
    fn push_connectivity(&self) {
        if self.server_unreachable {
            let warning = if self.server_failed {
                "The trouve server stopped and could not be restarted — \
                 please restart the app."
                    .into()
            } else if self.embedded_server.is_some() {
                "Lost the connection to the trouve server — reconnecting…".into()
            } else {
                format!(
                    "Can't reach the trouve server at {} — check your \
                     connection. Retrying…",
                    self.server_url
                )
            };
            ui::set_connectivity(&self.ui, true, warning, true);
            return;
        }
        let blocked = self.connectivity_blocked();
        let warning = if blocked {
            "You're offline and no local models are available — prompts are \
             disabled until the connection returns. To work offline in the \
             future, set up a model under Settings → Local Models."
        } else if self.offline {
            "You're offline — only local models are available until the \
             connection returns."
        } else {
            ""
        };
        ui::set_connectivity(&self.ui, blocked, warning.into(), false);
    }

    /// Show a transient connectivity notice that clears itself after a few
    /// seconds (sequence-guarded so an older notice's timer can't clear a
    /// newer notice).
    fn show_connectivity_notice(&mut self, text: &str) {
        self.connectivity_notice_seq += 1;
        let seq = self.connectivity_notice_seq;
        ui::set_connectivity_notice(&self.ui, text.into());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            let _ = tx.send(UiCommand::ConnectivityNoticeExpired(seq));
        });
    }

    /// Drop any transient notice immediately (bad news must not sit next to
    /// a stale "back online" line).
    fn clear_connectivity_notice(&mut self) {
        self.connectivity_notice_seq += 1;
        ui::set_connectivity_notice(&self.ui, String::new());
    }

    /// React to a `server.connectivity_changed` event: refresh the model
    /// catalog (the server filters it while offline), regate the inputs,
    /// and on recovery show a short self-clearing notice.
    async fn apply_connectivity_change(&mut self, online: bool) {
        let was_offline = self.offline;
        self.offline = !online;
        self.reload_catalogs().await;
        if online && was_offline {
            self.show_connectivity_notice("Back online — the full model list is available again.");
        } else if !online {
            self.clear_connectivity_notice();
        }
    }

    /// Re-sync after the connection to the server came back. Event streams
    /// replay losslessly from their cursors, but the handler drops stale
    /// envelopes — a connectivity transition that happened during the gap
    /// would be lost, so the snapshot is refetched; catalogs and sessions
    /// reload for the same reason.
    async fn resync_after_reconnect(&mut self, notice: &str) {
        if let Ok(info) = self.client.info().await {
            self.offline = !info.online;
        }
        self.reload_catalogs().await; // re-pushes the connectivity banner
        self.refresh_subscriptions(SubscriptionRefresh::Force);
        let _ = self.reload_sessions().await;
        self.show_connectivity_notice(notice);
    }

    /// The embedded server task died: attempt one automatic restart on the
    /// same address/token (a fresh engine over the persisted store). A
    /// crash loop (dying again within a minute of a restart) or a failed
    /// rebind gives up and asks for an app restart.
    async fn handle_server_exited(&mut self, status: &str) {
        let Some(info) = self.embedded_server.clone() else {
            // Externally-managed server: the watchdog handles messaging.
            return;
        };
        tracing::warn!("embedded trouve-server exited ({status})");
        self.server_unreachable = true;
        self.clear_connectivity_notice();
        if self
            .last_respawn
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(60))
        {
            self.server_failed = true;
            self.push_connectivity();
            return;
        }
        self.last_respawn = Some(std::time::Instant::now());
        self.push_connectivity();
        let restarted = match spawn_embedded_server(info.addr, &info.token).await {
            Ok((_, handle)) => {
                if wait_server_ready(&self.client).await.is_ok() {
                    // Hand ownership to the watcher only once the server
                    // answers; an unready task must not linger unwatched.
                    watch_embedded_server(self.tx.clone(), handle);
                    true
                } else {
                    // Abort and join so the listener/engine tear down before retry.
                    handle.abort();
                    let _ = handle.await;
                    false
                }
            }
            Err(e) => {
                tracing::warn!("restarting embedded trouve-server: {e:#}");
                false
            }
        };
        if restarted {
            self.server_unreachable = false;
            self.server_failed = false;
            self.resync_after_reconnect("The trouve server stopped and was restarted.")
                .await;
        } else {
            self.server_failed = true;
            self.push_connectivity();
        }
    }

    /// Index of a provider-qualified model id in the models catalog.
    fn model_index_of(&self, model: Option<&str>) -> i32 {
        model
            .and_then(|id| self.models.iter().position(|m| m.id == id))
            .map(|i| i as i32)
            .unwrap_or(-1)
    }

    /// Initial picker positions for a fresh thread. This mirrors the
    /// server's mode-default > global-default model precedence while still
    /// falling back to the first runnable catalog entry if a configured
    /// default is currently unavailable (for example while offline).
    fn new_chat_default_indices(&self) -> (i32, i32) {
        let mode_index = default_mode_index(&self.modes);
        let mode_default = usize::try_from(mode_index)
            .ok()
            .and_then(|index| self.modes.get(index))
            .and_then(|mode| mode.default_model.as_deref());
        let model_index = preferred_model_index(
            &self.models,
            mode_default,
            Some(self.default_model.as_str()),
        );
        (mode_index, model_index)
    }

    async fn reload_sessions(&mut self) -> Result<()> {
        let current_id = self.current_session_id();
        self.workspaces = self.client.list_workspaces().await?;
        if reconcile_workspace_order(&mut self.workspace_order, &self.workspaces) {
            self.save_workspace_order();
        }
        self.sessions = self.client.list_sessions().await?;
        self.busy_sessions = self
            .sessions
            .iter()
            .filter(|s| s.active)
            .map(|s| s.id.clone())
            .collect();
        self.push_agents_running();
        self.current_session =
            current_id.and_then(|id| self.sessions.iter().position(|s| s.id == id));
        let session_ids: HashSet<String> = self.sessions.iter().map(|s| s.id.clone()).collect();
        self.nav_prs.retain(|id, _| session_ids.contains(id));
        self.unread_sessions.retain(|id| session_ids.contains(id));
        self.error_sessions.retain(|id| session_ids.contains(id));
        self.watched_sessions.retain(|id| session_ids.contains(id));
        let stale_threads: Vec<String> = self
            .thread_sessions
            .iter()
            .filter(|(_, session_id)| !session_ids.contains(*session_id))
            .map(|(thread_id, _)| thread_id.clone())
            .collect();
        for thread_id in stale_threads {
            self.stop_following_thread(&thread_id);
        }
        self.attention_by_session
            .retain(|session_id, _| session_ids.contains(session_id));
        // If the open session vanished (deleted in another window or by an
        // automation), drop its threads and selection too — otherwise
        // current_thread_id keeps returning a thread of a session that no
        // longer exists and the chat renders stale.
        if self.current_session.is_none() {
            self.threads.clear();
            self.current_thread = None;
        }
        self.push_nav();
        // A session waiting on an agent can block on an approval/question in
        // any thread. Watch all threads of active background sessions so the
        // sidebar can surface that state without requiring the user to open
        // the session first.
        let active: Vec<String> = self.busy_sessions.iter().cloned().collect();
        for session_id in active {
            self.watch_session_threads(session_id);
        }
        self.refresh_nav_prs(false);
        Ok(())
    }

    /// Keep the preferred workspace valid after the server's visible
    /// workspace set changes. Preserve it when still open; otherwise fall
    /// back to the first open workspace (or none).
    fn sync_home_workspace(&mut self) {
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.id == self.home_workspace_id)
        {
            return;
        }
        self.home_workspace_id = self
            .workspaces
            .first()
            .map(|workspace| workspace.id.clone())
            .unwrap_or_default();
    }

    /// Clear session-derived UI when its workspace is closed locally or by
    /// another client. Persisted session/thread data remains available if the
    /// workspace is reopened later.
    fn close_current_session_if_in_workspace(&mut self, workspace_id: &str) {
        let closes_current = self
            .current_session
            .and_then(|index| self.sessions.get(index))
            .is_some_and(|session| session.workspace_id == workspace_id);
        if !closes_current {
            return;
        }
        self.current_session = None;
        self.threads.clear();
        self.current_thread = None;
        self.resume.session_id.clear();
        crate::winstate::save_resume(&self.resume);
        self.push_threads();
        self.render_chat(true);
        self.push_context();
        self.push_queue();
        self.push_todos();
    }

    /// Build the session-specific part of one nav row.
    fn session_nav_row(&self, index: usize, archived: bool) -> NavRowData {
        let session = &self.sessions[index];
        let (pr_kind, pr_tooltip) = pr_badge(
            self.nav_prs
                .get(&session.id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );
        let (attention_kind, attention_tooltip) = self.session_attention(&session.id);
        NavRowData {
            kind: 1,
            title: session.title.clone(),
            subtitle: session.branch.clone(),
            session_index: index as i32,
            selected: self.current_session == Some(index),
            archived,
            busy: self.busy_sessions.contains(&session.id),
            unread: self.unread_sessions.contains(&session.id),
            error: self.error_sessions.contains(&session.id),
            pr_kind,
            pr_tooltip,
            attention_kind,
            attention_tooltip,
            ..Default::default()
        }
    }

    /// Read the incrementally-maintained pending request totals for a session.
    fn session_attention(&self, session_id: &str) -> (i32, String) {
        let counts = self
            .attention_by_session
            .get(session_id)
            .copied()
            .unwrap_or_default();
        attention_badge(counts.approvals, counts.questions)
    }

    /// Apply a thread's attention delta to its owning session. Returns true
    /// only when the aggregate changed and the sidebar needs rebuilding.
    fn update_session_attention(
        &mut self,
        thread_id: &str,
        before: AttentionCounts,
        after: AttentionCounts,
    ) -> bool {
        if before == after {
            return false;
        }
        let Some(session_id) = self.thread_sessions.get(thread_id).cloned() else {
            return false;
        };
        let empty = {
            let total = self
                .attention_by_session
                .entry(session_id.clone())
                .or_default();
            total.approvals = total.approvals.saturating_sub(before.approvals) + after.approvals;
            total.questions = total.questions.saturating_sub(before.questions) + after.questions;
            *total == AttentionCounts::default()
        };
        if empty {
            self.attention_by_session.remove(&session_id);
        }
        true
    }

    /// Stop and forget a thread follower whose session no longer exists.
    fn stop_following_thread(&mut self, thread_id: &str) -> bool {
        if let Some(task) = self.follower_tasks.remove(thread_id) {
            task.abort();
        }
        self.followed.remove(thread_id);
        let before = self
            .vms
            .get(thread_id)
            .map(thread_attention)
            .unwrap_or_default();
        let attention_changed =
            self.update_session_attention(thread_id, before, AttentionCounts::default());
        self.vms.remove(thread_id);
        self.thread_sessions.remove(thread_id);
        attention_changed
    }

    /// Mark completed work unread unless this thread is currently visible in
    /// the focused window. Returns whether the session badge changed.
    fn mark_thread_unread_if_hidden(&mut self, thread_id: &str, failed: bool) -> bool {
        let focused = self
            .window_focused
            .load(std::sync::atomic::Ordering::Relaxed);
        let viewed = focused && self.current_thread_id().as_deref() == Some(thread_id);
        if viewed {
            return false;
        }
        let Some(session_id) = self.thread_sessions.get(thread_id).cloned() else {
            return false;
        };
        let unread_changed = self.unread_sessions.insert(session_id.clone());
        let error_changed = failed && self.error_sessions.insert(session_id);
        unread_changed || error_changed
    }

    /// Rebuild the grouped left-nav rows and the row → entry map.
    fn push_nav(&mut self) {
        let mut rows = Vec::new();
        let mut nav = Vec::new();
        let workspace_count = self.workspace_order.len() as i32;
        for (workspace_position, workspace_id) in self.workspace_order.iter().enumerate() {
            let Some(wi) = self.workspaces.iter().position(|ws| ws.id == *workspace_id) else {
                continue;
            };
            let ws = &self.workspaces[wi];
            let expanded = !self.collapsed_workspaces.contains(&ws.id);
            let show_archived = self.show_archived.contains(&ws.id);
            rows.push(NavRowData {
                kind: 0,
                title: ws.name.clone(),
                workspace_id: ws.id.clone(),
                workspace_position: workspace_position as i32,
                workspace_count,
                expanded,
                show_archived,
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
                rows.push(self.session_nav_row(i, false));
                nav.push(NavEntry::Session(i));
            }
            if archived_count > 0 && show_archived {
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
                        rows.push(self.session_nav_row(i, true));
                        nav.push(NavEntry::Session(i));
                    }
                }
            }
        }
        self.nav = nav;
        ui::set_nav(&self.ui, rows);
    }

    fn save_workspace_order(&mut self) {
        crate::winstate::save_workspace_order(&self.workspace_order);
    }

    fn drop_workspace(&mut self, workspace_id: &str, target_id: &str, after: bool) {
        if reorder_id(&mut self.workspace_order, workspace_id, target_id, after) {
            self.save_workspace_order();
            self.push_nav();
        }
    }

    fn move_workspace(&mut self, workspace_id: &str, offset: i32) {
        let Some(source) = self
            .workspace_order
            .iter()
            .position(|id| id == workspace_id)
        else {
            return;
        };
        let Some(last) = self.workspace_order.len().checked_sub(1) else {
            return;
        };
        let target = (source as i64 + i64::from(offset)).clamp(0, last as i64) as usize;
        if target == source {
            return;
        }
        let target_id = self.workspace_order[target].clone();
        self.drop_workspace(workspace_id, &target_id, target > source);
    }

    /// Push the server-authoritative number of active sessions to the UI
    /// (feeds the quit-confirmation dialog) and, when a deferred quit is
    /// armed, leave as soon as that count reaches zero.
    fn push_agents_running(&mut self) {
        // Cached view models can retain an old `turn_running` bit. Session
        // activity is seeded by the sessions endpoint and updated by the
        // global event stream, so it is the source of truth for app quit.
        let running = self.busy_sessions.len() as i32;
        ui::set_agents_running(&self.ui, running);
        if self
            .quit_when_idle
            .load(std::sync::atomic::Ordering::SeqCst)
            && running == 0
        {
            ui::quit(&self.ui);
        }
    }

    /// Pop a desktop notification for events the user would miss: the
    /// window is unfocused or the thread isn't the one on screen. Followers
    /// replay each thread's history from cursor 0, so anything but a fresh
    /// event (by append timestamp) is skipped.
    fn maybe_notify(&self, thread_id: &str, envelope: &EventEnvelope) {
        if !self.notify.enabled {
            return;
        }
        let focused = self
            .window_focused
            .load(std::sync::atomic::Ordering::Relaxed);
        let visible = self.current_thread_id().as_deref() == Some(thread_id);
        if focused && visible {
            return;
        }
        // A future ts (clock skew) errors in elapsed(); treat it as fresh.
        let fresh = std::time::SystemTime::from(envelope.ts)
            .elapsed()
            .map(|age| age < std::time::Duration::from_secs(10))
            .unwrap_or(true);
        if !fresh {
            return;
        }

        use trouve_protocol::Event;
        let (summary, detail) = match &envelope.event {
            Event::TurnCompleted { .. } if self.notify.on_finish => {
                ("Agent finished".to_string(), None)
            }
            Event::TurnFailed { error, .. } if self.notify.on_fail => {
                let mut error = error.trim().to_string();
                if error.len() > 120 {
                    error.truncate(120);
                    error.push('…');
                }
                ("Turn failed".to_string(), Some(error))
            }
            Event::ApprovalRequested { .. } if self.notify.on_attention => {
                ("Approval needed".to_string(), None)
            }
            Event::QuestionRequested { title, .. } if self.notify.on_attention => (
                "The agent has a question".to_string(),
                title.clone().filter(|t| !t.is_empty()),
            ),
            _ => return,
        };

        let session_id = self
            .thread_sessions
            .get(thread_id)
            .cloned()
            .unwrap_or_default();
        let session_title = self
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let body = match detail {
            Some(detail) if session_title.is_empty() => detail,
            Some(detail) => format!("{session_title}\n{detail}"),
            None => session_title,
        };
        crate::notify::show(
            crate::notify::Toast {
                summary,
                body,
                sound: self.notify.sound,
                session_id,
                thread_id: thread_id.to_string(),
            },
            self.tx.clone(),
        );
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
        self.current_thread
            .and_then(|i| self.threads.get(i))
            .map(|t| t.id.clone())
    }

    /// The question request behind a wizard row: its request id and the
    /// questions it poses.
    fn question_at(&self, row: usize) -> Option<(String, Vec<trouve_protocol::Question>)> {
        let request_id = self.row_call_ids.get(row)?.clone()?;
        let vm = self.vms.get(&self.current_thread_id()?)?;
        vm.items.iter().find_map(|item| match item {
            trouve_client_core::viewmodel::ChatItem::Questions {
                request_id: r,
                questions,
                ..
            } if *r == request_id => Some((request_id.clone(), questions.clone())),
            _ => None,
        })
    }

    fn open_thread_index(&mut self, index: usize, force_tail: bool) {
        if index >= self.threads.len() {
            return;
        }
        if let Some(session_id) = self.current_session_id()
            && (self.unread_sessions.remove(&session_id) | self.error_sessions.remove(&session_id))
        {
            self.push_nav();
        }
        if self.current_thread == Some(index) && self.new_chat.is_none() {
            if force_tail {
                self.apply_scroll_intent(true);
            }
            return;
        }
        // Clicking a real tab while the provisional "New Thread" tab is up
        // dismisses the form (its tab disappears).
        if self.new_chat.is_some() {
            self.close_new_chat();
        }
        self.current_thread = Some(index);
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat(false);
        self.push_context();
        self.push_queue();
        self.push_todos();
        self.remember_position();
        self.apply_scroll_intent(force_tail);
    }

    async fn select_session(&mut self, index: usize) -> Result<()> {
        if index >= self.sessions.len() {
            return Ok(());
        }
        if self.current_session_id().as_deref() == Some(self.sessions[index].id.as_str()) {
            if self.unread_sessions.remove(&self.sessions[index].id)
                | self.error_sessions.remove(&self.sessions[index].id)
            {
                self.push_nav();
            }
            return Ok(());
        }
        self.current_session = Some(index);
        self.unread_sessions.remove(&self.sessions[index].id);
        self.error_sessions.remove(&self.sessions[index].id);
        self.close_new_chat();
        self.push_nav();
        let session_id = self.sessions[index].id.clone();
        self.threads = self.client.list_threads(&session_id).await?;
        for t in &self.threads {
            self.thread_sessions
                .insert(t.id.clone(), session_id.clone());
        }
        self.watched_sessions.insert(session_id.clone());
        let thread_ids: Vec<String> = self.threads.iter().map(|t| t.id.clone()).collect();
        for thread_id in thread_ids {
            self.follow_thread(thread_id, session_id.clone());
        }
        // Reopen the thread the user last had open in this session; first
        // thread when there's no bookmark (or it was deleted).
        self.current_thread = self
            .resume
            .session_threads
            .get(&session_id)
            .and_then(|tid| self.threads.iter().position(|t| t.id == *tid))
            .or(if self.threads.is_empty() {
                None
            } else {
                Some(0)
            });
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat(false);
        self.push_context();
        self.push_queue();
        self.push_todos();
        self.remember_position();
        self.apply_scroll_intent(false);
        self.refresh_usage_text().await;
        let _ = self.load_files().await;
        let _ = self.refresh_diff().await;
        self.prs.clear();
        self.pr_selected = 0;
        self.refresh_prs();
        self.refresh_session_mcp();
        // The terminal tab always reflects the open session: attach (or
        // reuse the running attachment) when it's showing.
        if self.right_tab == TERMINAL_TAB {
            self.ensure_terminal().await;
        } else {
            self.push_term();
        }
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

    /// Apply the navigation policy for the open thread. Attention at the
    /// tail wins over a parked history bookmark: explicit notification
    /// navigation, a running turn, or visible queued prompts all open at
    /// the bottom. Idle threads restore a rendered-row anchor when present.
    fn apply_scroll_intent(&mut self, force_tail: bool) {
        let Some(thread_id) = self.current_thread_id() else {
            self.restoring_thread = None;
            return;
        };
        let vm = self.vms.get(&thread_id);
        let attention_at_tail = should_open_chat_at_tail(
            force_tail,
            vm.is_some_and(|vm| vm.turn_running),
            vm.is_some_and(|vm| !vm.queue.is_empty()),
        );
        let bookmark = self.resume.thread_scroll.get(&thread_id).copied();
        if attention_at_tail || bookmark.is_none() {
            self.restoring_thread = None;
            // The tail is now the last position the user saw; do not revive
            // an older parked-history anchor after the turn later goes idle.
            if self.resume.thread_scroll.remove(&thread_id).is_some() {
                crate::winstate::save_resume(&self.resume);
            }
            ui::scroll_chat_to_end(&self.ui);
        } else if let Some(bookmark) = bookmark {
            self.restoring_thread = Some(thread_id);
            ui::restore_chat_position(&self.ui, bookmark.row, bookmark.offset);
        }
    }

    /// A first-time follower starts with an empty view model and then folds
    /// persisted history. If that replay reveals tail attention while a
    /// row bookmark is still converging, switch to the tail exactly once.
    fn follow_tail_for_open_attention(&mut self, thread_id: &str) {
        if self.restoring_thread.as_deref() == Some(thread_id)
            && self.vms.get(thread_id).is_some_and(|vm| {
                should_open_chat_at_tail(false, vm.turn_running, !vm.queue.is_empty())
            })
        {
            self.apply_scroll_intent(false);
        }
    }

    fn push_threads(&self) {
        let mut tabs: Vec<(String, String, String)> = self
            .threads
            .iter()
            .map(|t| {
                let mode = self
                    .modes
                    .iter()
                    .find(|m| m.id == t.mode)
                    .map(|m| mode_display_name(&m.display_name, &m.id))
                    .unwrap_or_else(|| mode_display_name("", &t.mode));
                // Agent-spawned children carry a fork marker so users can
                // tell delegated work from their own tabs at a glance.
                let marker = if t.spawned { "⑂ " } else { "" };
                let completed = t
                    .todos
                    .iter()
                    .filter(|todo| todo.status == TodoStatus::Completed)
                    .count();
                let progress = if t.todos.is_empty() {
                    String::new()
                } else {
                    format!("{completed}/{}", t.todos.len())
                };
                (
                    t.id.clone(),
                    format!("{marker}{} · {}", mode, short_model(&t.model)),
                    progress,
                )
            })
            .collect();
        // The new-thread form lives in a provisional tab so the previous
        // tab stays one click away; `current_thread` is untouched
        // underneath, making cancel a pure UI dismissal.
        let selected = if matches!(self.new_chat, Some(NewChat::Thread)) {
            tabs.push((String::new(), "New Thread".into(), String::new()));
            (tabs.len() - 1) as i32
        } else {
            self.current_thread.map(|i| i as i32).unwrap_or(-1)
        };
        ui::set_threads(&self.ui, tabs, selected);
    }

    /// Push the active thread's current todo snapshot to both persistent UI
    /// surfaces. Selecting a thread with no todos removes the conditional
    /// right-side tab; if it was selected, fall back to Diff first.
    fn push_todos(&mut self) {
        let todos = self
            .current_thread
            .and_then(|i| self.threads.get(i))
            .map(|thread| thread.todos.clone())
            .unwrap_or_default();
        if todos.is_empty() && self.right_tab == TODOS_TAB {
            self.right_tab = 0;
            ui::set_right_tab(&self.ui, 0);
        }
        let completed = todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Completed)
            .count();
        let progress = if todos.is_empty() {
            String::new()
        } else {
            format!("{completed}/{} complete", todos.len())
        };
        let current = todos
            .iter()
            .find(|todo| todo.status == TodoStatus::InProgress)
            .map(|todo| todo.content.clone())
            .unwrap_or_default();
        let rows = todos
            .into_iter()
            .map(|todo| {
                let status = match todo.status {
                    TodoStatus::Pending => 0,
                    TodoStatus::InProgress => 1,
                    TodoStatus::Completed => 2,
                    TodoStatus::Cancelled => 3,
                };
                (todo.content, status)
            })
            .collect();
        ui::set_todos(&self.ui, rows, progress, current);
    }

    /// Apply a todo snapshot event to the protocol Thread copy used for tab
    /// badges and initial state. The per-thread view model folds it too.
    fn capture_todos(&mut self, thread_id: &str, envelope: &EventEnvelope) -> bool {
        let trouve_protocol::Event::TodosUpdated { todos } = &envelope.event else {
            return false;
        };
        if let Some(thread) = self
            .threads
            .iter_mut()
            .find(|thread| thread.id == thread_id)
        {
            thread.todos = todos.clone();
        }
        true
    }

    /// Composer pickers mirror the current thread's mode/model.
    fn push_picker_indices(&mut self) {
        let (mode, model, permission) = match self.current_thread.and_then(|i| self.threads.get(i))
        {
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
                permission_index_of(Some(thread.permission_mode)),
            ),
            None => (-1, -1, -1),
        };
        ui::set_picker_indices(&self.ui, mode, model);
        ui::set_permission_index(&self.ui, permission);
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
            .or_else(|| {
                thread?
                    .model_options
                    .get("thinking_level")?
                    .as_str()
                    .map(String::from)
            })
            .filter(|current| values.iter().any(|value| value == current))
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

    fn push_new_chat_knobs(&mut self, mode_index: usize, model_index: usize) {
        let thinking = self
            .models
            .get(model_index)
            .and_then(|model| thinking_property(&model.options_schema));
        let (key, values, default) = match thinking {
            Some((key, values, default)) => (Some(key), values, default),
            None => (None, Vec::new(), None),
        };
        let selected = preferred_thinking_index(
            &values,
            self.modes
                .get(mode_index)
                .and_then(|mode| mode.default_thinking_level.as_deref()),
            self.default_thinking_level.as_deref(),
            default.as_deref(),
        );
        self.new_chat_thinking_key = key;
        self.new_chat_thinking_values = values.clone();
        ui::set_new_chat_knobs(
            &self.ui,
            values.iter().map(|value| level_label(value)).collect(),
            selected,
        );
    }

    /// Start following the current thread's event stream (idempotent).
    fn follow_current(&mut self) {
        let Some(thread_id) = self.current_thread_id() else {
            return;
        };
        let Some(session_id) = self.current_session_id() else {
            return;
        };
        self.follow_thread(thread_id, session_id);
    }

    /// Discover all threads in an active background session. The request is
    /// asynchronous so a slow server cannot stall UI command handling.
    fn watch_session_threads(&mut self, session_id: String) {
        if !self.watched_sessions.insert(session_id.clone()) {
            return;
        }
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_threads(&session_id)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiCommand::SessionThreadsLoaded(session_id, result));
        });
    }

    /// Follow one thread for chat rendering and session-list state.
    fn follow_thread(&mut self, thread_id: String, session_id: String) {
        self.thread_sessions.insert(thread_id.clone(), session_id);
        if !self.followed.insert(thread_id.clone()) {
            return;
        }
        let mut vm = ThreadViewModel::new();
        if let Some(thread) = self.threads.iter().find(|thread| thread.id == thread_id) {
            vm.todos = thread.todos.clone();
        }
        self.vms.insert(thread_id.clone(), vm);
        let client = self.client.clone();
        let tx = self.tx.clone();
        // Reconnect for the lifetime of the app. The server ends the stream
        // on a store error or its own restart; without this the thread's
        // chat would silently freeze (no deltas, tool cards, or approvals)
        // until relaunch. Resume from the last cursor delivered — tracked in
        // the closure so an error path (which loses the return value) still
        // knows where to continue — so no event is replayed or dropped.
        let follower_id = thread_id.clone();
        let task = tokio::spawn(async move {
            use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
            let cursor = std::sync::Arc::new(AtomicU64::new(0));
            let replay = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let replay_flush_scheduled = std::sync::Arc::new(AtomicBool::new(false));
            // Startup history predates this app run and is already viewed;
            // persisted envelopes after the stream has gone live are a
            // reconnect backlog and can represent unseen background work.
            let live_seen = std::sync::Arc::new(AtomicBool::new(false));
            loop {
                let id = thread_id.clone();
                let seen = cursor.clone();
                let replay = replay.clone();
                let flush_scheduled = replay_flush_scheduled.clone();
                let live_seen = live_seen.clone();
                let event_tx = tx.clone();
                let after = cursor.load(Ordering::Relaxed);
                let result = client
                    .follow_thread_events(&thread_id, after, |envelope| {
                        seen.store(envelope.cursor, Ordering::Relaxed);
                        let persisted_replay = std::time::SystemTime::from(envelope.ts)
                            .elapsed()
                            .is_ok_and(|age| age > std::time::Duration::from_secs(2));
                        if persisted_replay {
                            replay.lock().unwrap().push(envelope);
                            if !flush_scheduled.swap(true, Ordering::AcqRel) {
                                let replay = replay.clone();
                                let flush_scheduled = flush_scheduled.clone();
                                let event_tx = event_tx.clone();
                                let id = id.clone();
                                let mark_unread = live_seen.load(Ordering::Relaxed);
                                tokio::spawn(async move {
                                    // Let the local replay producer fill the
                                    // batch before handing it to the
                                    // controller's unbounded command queue.
                                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                                    flush_scheduled.store(false, Ordering::Release);
                                    let batch = std::mem::take(&mut *replay.lock().unwrap());
                                    if !batch.is_empty() {
                                        let _ = event_tx.send(UiCommand::Events(
                                            id,
                                            batch,
                                            mark_unread,
                                        ));
                                    }
                                });
                            }
                        } else {
                            let mark_unread = live_seen.load(Ordering::Relaxed);
                            let batch = std::mem::take(&mut *replay.lock().unwrap());
                            if !batch.is_empty() {
                                let _ = event_tx.send(UiCommand::Events(
                                    id.clone(),
                                    batch,
                                    mark_unread,
                                ));
                            }
                            live_seen.store(true, Ordering::Relaxed);
                            let _ = event_tx.send(UiCommand::Event(id.clone(), Box::new(envelope)));
                        }
                        std::ops::ControlFlow::Continue(())
                    })
                    .await;
                match result {
                    Ok(last) => {
                        cursor.fetch_max(last, Ordering::Relaxed);
                    }
                    Err(e) => {
                        tracing::warn!("event stream for {thread_id} reconnecting: {e:#}");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        self.follower_tasks.insert(follower_id, task);
    }

    /// Refresh the composer's "@"-mention path list for the open session, at
    /// most once per TTL (the popup pings on every keystroke, and renders
    /// ping here too — the walk is a full worktree scan, so throttle it).
    fn refresh_at_files(&mut self) {
        const TTL: std::time::Duration = std::time::Duration::from_secs(10);
        let session_id = self
            .current_thread
            .and_then(|i| self.threads.get(i))
            .map(|t| t.session_id.clone())
            .or_else(|| self.current_session_id());
        let Some(session_id) = session_id else { return };
        if let Some((sid, at)) = &self.at_files_fetched
            && *sid == session_id
            && at.elapsed() < TTL
        {
            return;
        }
        self.at_files_fetched = Some((session_id.clone(), std::time::Instant::now()));
        let client = self.client.clone();
        let ui = self.ui.clone();
        tokio::spawn(async move {
            match client.session_paths(&session_id).await {
                Ok(paths) => ui::set_at_files(&ui, paths),
                Err(e) => tracing::warn!("worktree path list for @-mentions failed: {e:#}"),
            }
        });
    }

    /// Re-fold the current thread into chat rows. `scroll` jumps the list to
    /// the end — wanted when content arrives or threads switch, jarring for
    /// in-place toggles (tool details, raw view).
    fn render_chat(&mut self, scroll: bool) {
        let Some(thread_id) = self.current_thread_id() else {
            self.row_call_ids.clear();
            ui::set_chat(&self.ui, Vec::new(), String::new(), false);
            ui::set_composer_enabled(&self.ui, false);
            ui::set_composer_turn_running(&self.ui, false);
            ui::set_slash_commands(&self.ui, Vec::new());
            return;
        };
        // Keep the "@" mention paths roughly current while a thread is open
        // (agents create files mid-turn); the helper self-throttles.
        self.refresh_at_files();
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
        let vm = self.vms.entry(thread_id.clone()).or_default();
        ui::set_composer_turn_running(&self.ui, vm.turn_running);
        // Wizard state tracks the thread's pending question requests: fresh
        // state when one appears, dropped once it resolves.
        for item in &vm.items {
            if let trouve_client_core::viewmodel::ChatItem::Questions {
                request_id,
                questions,
                answers,
                ..
            } = item
            {
                if answers.is_none() {
                    self.wizards
                        .entry(request_id.clone())
                        .or_insert_with(|| render::WizardState::new(questions.len()));
                } else {
                    self.wizards.remove(request_id);
                }
            }
        }
        let (rows, call_ids) = render::chat_rows(
            vm,
            &self.expanded_tools,
            &raw_turns,
            &collapsed,
            &self.wizards,
        );
        self.row_call_ids = call_ids;
        ui::set_slash_commands(
            &self.ui,
            vm.commands
                .iter()
                .map(|c| (c.name.clone(), c.description.clone()))
                .collect(),
        );
        ui::set_chat(&self.ui, rows, thread_id, scroll);
        ui::set_composer_enabled(&self.ui, true);
    }

    /// Push the current thread's prompt queue to the composer's queue panel.
    /// "Send now" shows when the thread is idle: queues never auto-run
    /// after a restart or a failed turn — resuming is the user's call.
    fn push_queue(&mut self) {
        let Some(thread_id) = self.current_thread_id() else {
            ui::set_queue(&self.ui, Vec::new(), Vec::new(), false);
            return;
        };
        let vm = self.vms.entry(thread_id).or_default();
        let prompts = vm.queue.iter().map(|p| p.content.clone()).collect();
        // Shown beside the row text (not part of it — rows are editable).
        let badges = vm
            .queue
            .iter()
            .map(|p| match p.attachments.len() {
                0 => String::new(),
                n => format!("📎{n}"),
            })
            .collect();
        ui::set_queue(&self.ui, prompts, badges, !vm.turn_running);
    }

    /// Mirror the staged attachments as composer chips.
    fn push_attachments(&self) {
        let chips = self
            .pending_attachments
            .iter()
            .map(|a| {
                // Base64 is 4 chars per 3 bytes; close enough for a label.
                let bytes = a.data.len() * 3 / 4;
                (
                    a.name.clone(),
                    format!("{} · {}", a.mime, human_size(bytes)),
                )
            })
            .collect();
        ui::set_composer_attachments(&self.ui, chips);
    }

    /// Server id of the current thread's queued prompt shown at `index`.
    fn queued_prompt_id(&self, index: usize) -> Option<String> {
        let thread_id = self.current_thread_id()?;
        self.vms
            .get(&thread_id)?
            .queue
            .get(index)
            .map(|p| p.id.clone())
    }

    // --- terminal tab -----------------------------------------------------

    /// The current session's attached, still-running terminal.
    fn term_attached(&self) -> Option<(String, &TermState)> {
        let state = self.terms.get(&self.current_session_id()?)?;
        if state.exited {
            return None;
        }
        Some((state.terminal_id.clone(), state))
    }

    fn current_term_mut(&mut self) -> Option<&mut TermState> {
        let session_id = self.current_session_id()?;
        self.terms.get_mut(&session_id)
    }

    /// Attach the current session's terminal: reuse the running attachment,
    /// otherwise open (or re-open after exit) server-side and start a
    /// follower task streaming output back as [`UiCommand::TermOutput`].
    async fn ensure_terminal(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.push_term();
            return;
        };
        if self.terms.get(&session_id).is_some_and(|s| !s.exited) {
            self.push_term();
            return;
        }
        let (cols, rows) = self.term_view;
        let info = match self.client.open_terminal(&session_id, cols, rows).await {
            Ok(info) => info,
            Err(e) => {
                self.error(&format!("terminal: {e:#}"));
                return;
            }
        };
        // Size the screen model like the view; the server PTY follows on
        // the next resize event if it disagrees.
        let mut state = TermState {
            terminal_id: info.id.clone(),
            grid: slint_terminal::GridState::new(rows, cols, TERM_SCROLLBACK),
            offset: 0,
            exited: info.exited,
        };
        if (info.cols, info.rows) != (cols, rows) {
            let _ = self.client.terminal_resize(&info.id, cols, rows).await;
        }
        state.grid.resize(rows, cols);
        self.terms.insert(session_id.clone(), state);
        self.push_term();

        // Follower: replays the backlog, then streams live output until the
        // shell exits or the terminal is killed. A dropped/lagged stream
        // reconnects from the last offset (the server replays its backlog).
        // Output goes through the command channel so all screen state stays
        // on the controller.
        let client = self.client.clone();
        let tx = self.tx.clone();
        let terminal_id = info.id.clone();
        tokio::spawn(async move {
            let mut after = 0u64;
            loop {
                let result = client
                    .follow_terminal(&terminal_id, after, |offset, bytes| {
                        if tx
                            .send(UiCommand::TermOutput {
                                session_id: session_id.clone(),
                                terminal_id: terminal_id.clone(),
                                offset,
                                bytes,
                            })
                            .is_err()
                        {
                            return std::ops::ControlFlow::Break(());
                        }
                        std::ops::ControlFlow::Continue(())
                    })
                    .await;
                match result {
                    Ok((_, true)) => break,
                    Ok((offset, false)) => {
                        after = offset;
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    // Gone (killed / server restart): stop following.
                    Err(e) => {
                        tracing::debug!("terminal follower ended: {e:#}");
                        break;
                    }
                }
            }
            let _ = tx.send(UiCommand::TermEnded {
                session_id,
                terminal_id,
            });
        });
    }

    /// Render the current session's terminal screen into the UI (or the
    /// detached placeholder when there is none).
    fn push_term(&mut self) {
        let Some(state) = self
            .current_session_id()
            .and_then(|sid| self.terms.get(&sid))
        else {
            ui::set_term(&self.ui, Vec::new(), None, 0, String::new(), false);
            return;
        };
        let (fg, bg) = render::term_colors();
        let rows = state.grid.rows(fg, bg);
        let status = if state.exited {
            "shell exited".to_string()
        } else {
            String::new()
        };
        ui::set_term(
            &self.ui,
            rows,
            state.grid.cursor(),
            state.grid.scrollback_offset(),
            status,
            true,
        );
    }

    /// Push the context dial: last turn's input tokens vs the model window.
    fn push_context(&mut self) {
        let Some(thread) = self.current_thread.and_then(|i| self.threads.get(i)) else {
            ui::set_context(&self.ui, 0.0, false, "no thread selected".into());
            return;
        };
        let catalog_window = self
            .models
            .iter()
            .find(|m| m.id == thread.model)
            .map(|m| m.context_window);
        let vm = self.vms.entry(thread.id.clone()).or_default();
        // A window reported live by the provider (codex sends the real one
        // with token usage) beats the static catalog guess.
        let window = vm
            .last_usage
            .as_ref()
            .and_then(|u| u.context_window)
            .or(catalog_window);
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

    /// Fetch the session diff and repaint only when it actually changed
    /// (the auto-refresh poller calls this every couple of seconds).
    /// Collapsed state carries over by file path.
    async fn refresh_diff(&mut self) -> Result<()> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(());
        };
        let diff = self.client.session_diff(&session_id).await?;
        if diff.diff == self.diff_raw {
            return Ok(());
        }
        let collapsed_paths: HashSet<String> = self
            .diff_files
            .iter()
            .zip(&self.diff_collapsed)
            .filter(|(_, c)| **c)
            .map(|(f, _)| f.path.clone())
            .collect();
        self.diff_files = slint_diff_view::parse_unified_diff(&diff.diff);
        self.diff_collapsed = self
            .diff_files
            .iter()
            .map(|f| collapsed_paths.contains(&f.path))
            .collect();
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

    fn shared_prs_for_session(&self, session: &Session) -> Vec<trouve_protocol::PrInfo> {
        project_session_prs(
            session,
            self.pr_dash.values().flat_map(|list| list.prs.iter()),
            self.nav_prs
                .get(&session.id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
    }

    /// Fold the shared account snapshots into sidebar icons and the current
    /// session's right panel. `clear_missing` is used after a full refresh so
    /// PRs that left the account feed do not remain as stale indicators.
    fn sync_shared_prs(&mut self, clear_missing: bool) {
        let updates: Vec<_> = self
            .sessions
            .iter()
            .map(|session| (session.id.clone(), self.shared_prs_for_session(session)))
            .collect();
        for (session_id, prs) in updates {
            if clear_missing || !prs.is_empty() {
                self.nav_prs.insert(session_id, prs);
            }
        }
        if let Some(session_id) = self.current_session_id()
            && let Some(shared) = self.nav_prs.get(&session_id).cloned()
            && (clear_missing || !shared.is_empty())
        {
            let keep = self
                .prs
                .get(self.pr_selected)
                .and_then(|cur| shared.iter().position(|pr| pr.number == cur.number));
            self.prs = shared;
            self.pr_selected = keep.unwrap_or(0);
            self.pr_error.clear();
            self.push_prs();
        }
        self.push_nav();
    }

    /// Populate the right panel immediately from known account data, then run
    /// the authoritative session lookup. The account feed cannot discover
    /// cross-branch associations by itself and must never broaden them.
    fn refresh_prs(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.prs.clear();
            self.pr_selected = 0;
            self.pr_error.clear();
            self.push_prs();
            return;
        };
        if !self.github_configured {
            self.push_prs();
            return;
        }
        let shared = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| self.shared_prs_for_session(session))
            .unwrap_or_default();
        if !shared.is_empty() {
            self.prs = shared.clone();
            self.nav_prs.insert(session_id.clone(), shared);
            self.pr_selected = self.pr_selected.min(self.prs.len().saturating_sub(1));
            self.pr_error.clear();
            self.push_prs();
            self.push_nav();
        } else if self.prs.is_empty() {
            self.pr_error = "looking for pull requests…".into();
        }
        self.push_prs();
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .session_prs(&session_id)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiCommand::PrsLoaded(session_id, result));
        });
    }

    /// Sidebar PR summaries are projections of the shared account feed; they
    /// never issue per-session GitHub requests.
    fn refresh_nav_prs(&mut self, force: bool) {
        if !self.github_configured {
            if !self.nav_prs.is_empty() {
                self.nav_prs.clear();
                self.push_nav();
            }
            return;
        }
        if force {
            self.nav_prs.clear();
        }
        self.sync_shared_prs(false);
    }

    /// Kick off a background fetch of the session's effective MCP config
    /// (the merged view a turn would see); lands as `SessionMcpLoaded`.
    fn refresh_session_mcp(&self) {
        let Some(session_id) = self.current_session_id() else {
            ui::set_session_mcp(&self.ui, Vec::new(), String::new());
            return;
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .session_mcp_servers(&session_id)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiCommand::SessionMcpLoaded(session_id, result));
        });
    }

    fn push_prs(&self) {
        let labels = self
            .prs
            .iter()
            .map(|pr| format!("#{} · {} ({})", pr.number, pr.title, pr.state))
            .collect();
        let items = self
            .prs
            .iter()
            .map(|pr| ui::PrView {
                title: pr.title.clone(),
                state: pr.state.clone(),
                meta: format!(
                    "#{}{} · {} → {}",
                    pr.number,
                    if pr.draft { " · draft" } else { "" },
                    pr.head,
                    pr.base,
                ),
                url: pr.url.clone(),
                checks: format_checks(&pr.checks),
                reviews: format_reviews(&pr.reviews),
            })
            .collect();
        ui::set_prs(
            &self.ui,
            self.github_configured,
            &self.pr_error,
            labels,
            items,
            self.pr_selected,
        );
    }

    // --- PR dashboard --------------------------------------------------------

    /// Kick off one account-level refresh across all configured instances.
    fn refresh_pr_dashboard(&mut self) {
        if self.github_configured
            && !self.offline
            && !self.server_unreachable
            && self.pr_dash_loading.insert("github".into())
        {
            self.pr_dash_errors.clear();
            let client = self.client.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = client
                    .refresh_github_prs()
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(UiCommand::PrDashRefreshFinished("github".into(), result));
            });
        }
        self.push_pr_dashboard();
    }

    /// Rebuild the dashboard view: reconcile the persisted group order,
    /// classify every fetched PR into exactly one group, and push.
    fn push_pr_dashboard(&mut self) {
        if reconcile_pr_group_order(&mut self.pr_group_order) {
            crate::winstate::save_pr_group_order(&self.pr_group_order);
        }
        let mut repository_keys: Vec<String> = self
            .pr_dash
            .values()
            .flat_map(|list| list.prs.iter())
            .map(|pr| format!("{}/{}", pr.host, pr.repository))
            .collect();
        repository_keys.sort();
        repository_keys.dedup();
        if self
            .pr_dash_filter
            .as_ref()
            .is_some_and(|selected| !repository_keys.contains(selected))
        {
            self.pr_dash_filter = None;
        }

        let mut projects = vec!["All projects".to_string()];
        projects.extend(repository_keys.iter().cloned());
        let filter_index = self
            .pr_dash_filter
            .as_ref()
            .and_then(|id| repository_keys.iter().position(|key| key == id))
            .map(|i| i as i32 + 1)
            .unwrap_or(0);

        let now = chrono::Utc::now();
        // Rows carry a sort key: merge recency for the merged group, PR
        // number (a proxy for recency) elsewhere — newest first either way.
        let mut rows: HashMap<&'static str, Vec<(i64, ui::PrRowView)>> = HashMap::new();
        for list in self.pr_dash.values() {
            for pr in &list.prs {
                let repo_key = format!("{}/{}", pr.host, pr.repository);
                if self
                    .pr_dash_filter
                    .as_ref()
                    .is_some_and(|id| *id != repo_key)
                {
                    continue;
                }
                let Some(group) = classify_pr(pr, &list.viewer, now) else {
                    continue;
                };
                let (check_kind, check_label) = check_pill(&pr.checks);
                let (approval_kind, approval_label) = approval_pill(pr);
                let (merge_kind, merge_label) = merge_pill(pr);
                let has_chat = self
                    .sessions
                    .iter()
                    .any(|s| s.workspace_id == pr.workspace_id && s.branch == pr.head);
                let sort_key = match pr.merged_at {
                    Some(at) if group == "recently-merged" => -at.timestamp(),
                    _ => -(pr.number as i64),
                };
                rows.entry(group).or_default().push((
                    sort_key,
                    ui::PrRowView {
                        workspace_id: pr.workspace_id.clone(),
                        app_name: pr.repository.clone(),
                        number_label: format!("#{}", pr.number),
                        title: pr.title.clone(),
                        branch: pr.head.clone(),
                        check_kind,
                        check_label: check_label.into(),
                        approval_kind,
                        approval_label: approval_label.into(),
                        merge_kind,
                        merge_label: merge_label.into(),
                        comments_label: match pr.comments {
                            1 => "1 comment".into(),
                            n => format!("{n} comments"),
                        },
                        last_comment: match (pr.comments, pr.last_comment_at) {
                            (_, Some(at)) => format!("last comment {}", human_age(at, now)),
                            (0, None) => "no comments yet".into(),
                            (_, None) => "last comment time unavailable".into(),
                        },
                        url: pr.url.clone(),
                        has_chat,
                    },
                ));
            }
        }

        let group_count = self.pr_group_order.len() as i32;
        let groups = self
            .pr_group_order
            .iter()
            .enumerate()
            .filter_map(|(position, key)| {
                let def = PR_GROUPS.iter().find(|d| d.key == key)?;
                Some(ui::PrGroupView {
                    key: def.key.into(),
                    title: def.title.into(),
                    description: def.description.into(),
                    kind: def.kind,
                    icon: def.icon.into(),
                    position: position as i32,
                    group_count,
                    collapsed: self.pr_collapsed.contains(def.key),
                    empty_text: def.empty.into(),
                    prs: {
                        let mut prs = rows.remove(def.key).unwrap_or_default();
                        prs.sort_by_key(|(key, _)| *key);
                        prs.into_iter().map(|(_, row)| row).collect()
                    },
                })
            })
            .collect();

        let mut status_parts = Vec::new();
        if !self.pr_dash_loading.is_empty() {
            status_parts.push("looking for pull requests…".to_string());
        }
        for e in self.pr_dash_errors.values() {
            status_parts.push(e.clone());
        }

        ui::set_pr_dashboard(
            &self.ui,
            groups,
            projects,
            filter_index,
            status_parts.join("\n"),
            !self.workspaces.is_empty(),
        );
    }

    fn drop_pr_group(&mut self, key: &str, target_key: &str, after: bool) {
        if reorder_id(&mut self.pr_group_order, key, target_key, after) {
            crate::winstate::save_pr_group_order(&self.pr_group_order);
            self.push_pr_dashboard();
        }
    }

    fn move_pr_group(&mut self, key: &str, offset: i32) {
        let Some(source) = self.pr_group_order.iter().position(|k| k == key) else {
            return;
        };
        let Some(last) = self.pr_group_order.len().checked_sub(1) else {
            return;
        };
        let target = (source as i64 + i64::from(offset)).clamp(0, last as i64) as usize;
        if target == source {
            return;
        }
        let target_key = self.pr_group_order[target].clone();
        self.drop_pr_group(key, &target_key, target > source);
    }

    /// The dashboard's chat button: jump to the session that owns the PR's
    /// branch, or offer a new chat for it (workspace preselected, the PR
    /// branch as base ref) when none exists.
    async fn open_pr_chat(&mut self, workspace_id: &str, branch: &str) -> Result<()> {
        if let Some(index) = self
            .sessions
            .iter()
            .position(|s| s.workspace_id == workspace_id && s.branch == branch)
        {
            return self.select_session(index).await;
        }
        let workspace = self.workspaces.iter().position(|w| w.id == workspace_id);
        self.open_new_session_screen(workspace).await?;
        // Start the new chat from the PR's code: preselect its head branch
        // when the local checkout knows it (falls back to HEAD otherwise).
        if let Some(i) = self.branches.iter().position(|b| b == branch) {
            ui::set_branches(&self.ui, self.branches.clone(), i as i32);
        }
        Ok(())
    }

    /// Record a fresh integration snapshot: per-host state plus the "any
    /// host works" flag the PR tab keys off.
    fn apply_github_integration(&mut self, gh: trouve_protocol::GithubIntegration) {
        self.github_configured = gh.hosts.iter().any(|h| h.configured) || gh.configured;
        self.github_hosts = gh.hosts;
    }

    fn push_github_integration(&self) {
        let hosts = self
            .github_hosts
            .iter()
            .map(|h| ui::GithubHostView {
                host: h.host.clone(),
                configured: h.configured,
                source: h.source.clone(),
                oauth_available: h.oauth_available,
                removable: h.removable,
            })
            .collect();
        ui::set_github_integration(&self.ui, hosts);
    }

    /// Fetch subscription health in the background (the Codex query may
    /// spawn its app-server, which takes a moment).
    fn refresh_subscriptions(&mut self, freshness: SubscriptionRefresh) {
        let Some(generation) = self
            .subscription_refresh
            .begin(std::time::Instant::now(), freshness)
        else {
            return;
        };
        let client = self.client.clone();
        let tx = self.tx.clone();
        ui::set_subscriptions(&self.ui, Vec::new(), "checking subscription usage…".into());
        tokio::spawn(async move {
            let result = client
                .subscription_health()
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiCommand::SubscriptionsLoaded { generation, result });
        });
    }

    /// Refresh after a completed turn unless another followed thread already
    /// triggered the same provider-side work within the throttle window.
    fn refresh_subscriptions_after_turn(&mut self) {
        self.refresh_subscriptions(SubscriptionRefresh::IfStale);
    }

    /// Reload the MCP server list in the background: a quick unprobed list
    /// paints immediately, then health probes fill in (they spawn every
    /// server, which can take seconds). Lists all registered workspaces,
    /// grouped by scope, so nothing is hidden behind the current session.
    fn refresh_mcp(&self) {
        ui::set_mcp_workspaces(
            &self.ui,
            self.workspaces.iter().map(|w| w.name.clone()).collect(),
            self.workspaces.iter().map(|w| w.id.clone()).collect(),
        );
        let client = self.client.clone();
        let tx = self.tx.clone();
        let ui = self.ui.clone();
        tokio::spawn(async move {
            match client.list_mcp_servers(None, false).await {
                Ok(list) => {
                    let _ = tx.send(UiCommand::McpLoaded(list, false));
                }
                Err(e) => {
                    ui::set_mcp_status(&ui, format!("failed to load MCP servers: {e:#}"));
                    return;
                }
            }
            match client.list_mcp_servers(None, true).await {
                Ok(list) => {
                    let _ = tx.send(UiCommand::McpLoaded(list, true));
                }
                Err(e) => ui::set_mcp_status(&ui, format!("health check failed: {e:#}")),
            }
        });
    }

    /// Reload the Files tree from scratch (session switch, refresh).
    async fn load_files(&mut self) -> Result<()> {
        self.file_children.clear();
        self.file_expanded.clear();
        let Some(session_id) = self.current_session_id() else {
            self.file_rows.clear();
            ui::set_file_list(&self.ui, Vec::new());
            return Ok(());
        };
        let root = self.client.session_files(&session_id, ".").await?;
        self.file_children.insert(".".into(), root);
        self.push_file_tree();
        Ok(())
    }

    /// Flatten the cached tree (expanded dirs only) into display rows.
    fn push_file_tree(&mut self) {
        fn walk(
            dir: &str,
            depth: i32,
            children: &HashMap<String, Vec<DirEntry>>,
            expanded: &HashSet<String>,
            out: &mut Vec<FileRow>,
        ) {
            let Some(entries) = children.get(dir) else {
                return;
            };
            for entry in entries {
                let path = if dir == "." {
                    entry.name.clone()
                } else {
                    format!("{dir}/{}", entry.name)
                };
                let is_expanded = entry.is_dir && expanded.contains(&path);
                out.push(FileRow {
                    path: path.clone(),
                    name: entry.name.clone(),
                    is_dir: entry.is_dir,
                    depth,
                    expanded: is_expanded,
                });
                if is_expanded {
                    walk(&path, depth + 1, children, expanded, out);
                }
            }
        }
        let mut rows = Vec::new();
        walk(".", 0, &self.file_children, &self.file_expanded, &mut rows);
        ui::set_file_list(
            &self.ui,
            rows.iter()
                .map(|r| (r.name.clone(), r.is_dir, r.depth, r.expanded))
                .collect(),
        );
        self.file_rows = rows;
    }

    // --- new-chat screens ----------------------------------------------------

    /// `workspace`: pre-selected workspace index (the per-workspace "+"),
    /// or None to default to the current session's / home workspace.
    async fn open_new_session_screen(&mut self, workspace: Option<usize>) -> Result<()> {
        self.new_chat = Some(NewChat::Session);
        let (mode_index, model_index) = self.new_chat_default_indices();
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
            mode_index,
            model_index,
        );
        self.push_new_chat_knobs(
            usize::try_from(mode_index).unwrap_or(0),
            usize::try_from(model_index).unwrap_or(0),
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
        let (mode_index, model_index) = self.new_chat_default_indices();
        ui::set_new_chat(
            &self.ui,
            Vec::new(),
            -1,
            Vec::new(),
            -1,
            mode_index,
            model_index,
        );
        self.push_new_chat_knobs(
            usize::try_from(mode_index).unwrap_or(0),
            usize::try_from(model_index).unwrap_or(0),
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

    async fn start_new_chat(&mut self, selection: NewChatSelection, prompt: String) -> Result<()> {
        let mut model_options = serde_json::Map::new();
        if let (Some(key), Some(value)) = (
            self.new_chat_thinking_key.clone(),
            self.new_chat_thinking_values
                .get(selection.thinking_idx)
                .cloned(),
        ) {
            model_options.insert(key, serde_json::Value::String(value));
        }
        let permission_mode = match selection.permission_idx {
            1 => Some(PermissionMode::Ask),
            2 => Some(PermissionMode::AllowList),
            3 => Some(PermissionMode::Yolo),
            _ => None,
        };
        match self.new_chat {
            Some(NewChat::Thread) => {
                self.close_new_chat();
                self.create_thread(
                    selection.mode_idx,
                    selection.model_idx,
                    model_options.clone(),
                    permission_mode,
                )
                .await?;
                if let Some(thread_id) = self.current_thread_id() {
                    let uploads = std::mem::take(&mut self.pending_attachments);
                    self.push_attachments();
                    self.client
                        .send_message_with(&thread_id, &prompt, uploads)
                        .await?;
                }
            }
            _ => {
                let workspace = self
                    .workspaces
                    .get(selection.workspace_idx)
                    .context("no workspace selected")?
                    .clone();
                let session = self
                    .client
                    .create_session(&CreateSessionRequest {
                        workspace_id: workspace.id,
                        title: Some(trouve_client_core::title::summarize_session_title(&prompt)),
                        base_ref: self.branches.get(selection.branch_idx).cloned(),
                        fetch_latest: selection.fetch_latest,
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
                self.create_thread(
                    selection.mode_idx,
                    selection.model_idx,
                    model_options,
                    permission_mode,
                )
                .await?;
                if let Some(thread_id) = self.current_thread_id() {
                    let uploads = std::mem::take(&mut self.pending_attachments);
                    self.push_attachments();
                    self.client
                        .send_message_with(&thread_id, &prompt, uploads)
                        .await?;
                }
            }
        }
        Ok(())
    }

    // --- settings --------------------------------------------------------------

    async fn refresh_settings(&mut self) {
        if let Ok(gh) = self.client.github_integration().await {
            self.apply_github_integration(gh);
            self.push_github_integration();
        }
        let providers = match self.client.list_providers().await {
            Ok(p) => p,
            Err(e) => {
                ui::set_settings_status(&self.ui, format!("failed to load: {e:#}"));
                return;
            }
        };
        let model_ids: Vec<String> = self.models.iter().map(|m| m.id.clone()).collect();
        let default_model = providers.default_model.clone();
        let default_thinking_level = providers.default_thinking_level.clone();
        self.default_model = default_model.clone();
        self.default_thinking_level = default_thinking_level.clone();
        let default_index = model_ids
            .iter()
            .position(|m| *m == default_model)
            .map(|i| i as i32)
            .unwrap_or(-1);
        let thinking_views: Vec<ui::ModelThinkingView> = self
            .models
            .iter()
            .map(|model| {
                let (_, values, schema_default) =
                    thinking_property(&model.options_schema).unwrap_or_default();
                let configured_index = default_thinking_level
                    .as_ref()
                    .and_then(|level| values.iter().position(|value| value == level))
                    .or_else(|| {
                        schema_default
                            .as_ref()
                            .and_then(|level| values.iter().position(|value| value == level))
                    })
                    .map(|i| i as i32)
                    .unwrap_or_else(|| if values.is_empty() { -1 } else { 0 });
                ui::ModelThinkingView {
                    names: values.iter().map(|value| level_label(value)).collect(),
                    values,
                    configured_index,
                }
            })
            .collect();
        let default_thinking_index = usize::try_from(default_index)
            .ok()
            .and_then(|i| thinking_views.get(i))
            .map(|item| item.configured_index)
            .unwrap_or(-1);
        ui::set_settings_data(
            &self.ui,
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
                        p.category,
                        p.experimental,
                    )
                })
                .collect(),
            model_ids.clone(),
            thinking_views,
            default_index,
            default_thinking_index,
            permission_index_of(Some(providers.default_permission_mode)),
        );
        let mode_views = self
            .modes
            .iter()
            .zip(&self.mode_origins)
            .map(|(m, origin)| ui::ModeView {
                id: m.id.clone(),
                display_name: m.display_name.clone(),
                origin: origin.clone(),
                read_only: m.read_only,
                system_prompt: m.system_prompt.clone(),
                allowed_tools: m.allowed_tools.join(", "),
                permission_index: permission_index_of(m.default_permission_mode),
                model_index: self.model_index_of(m.default_model.as_deref()),
                thinking_index: m
                    .default_thinking_level
                    .as_ref()
                    .and_then(|level| {
                        let model_id = m.default_model.as_deref().unwrap_or(&default_model);
                        self.models
                            .iter()
                            .find(|model| model.id == model_id)
                            .and_then(|model| thinking_property(&model.options_schema))
                            .and_then(|(_, values, _)| {
                                values.iter().position(|value| value == level)
                            })
                    })
                    .map(|i| i as i32)
                    .unwrap_or(-1),
            })
            .collect();
        ui::set_settings_modes(&self.ui, mode_views, model_ids);
        // Preset catalog is static server data; fetch alongside the rest.
        if let Ok(known) = self.client.known_providers().await {
            ui::set_known_providers(&self.ui, known);
        }
        self.refresh_clis().await;
        self.refresh_local();
    }

    /// Estimated bytes/sec for an in-flight download, from consecutive
    /// progress polls (exponentially smoothed so the label doesn't jitter).
    /// Returns None until there are two samples far enough apart.
    fn download_rate(&mut self, key: &str, bytes: u64) -> Option<f64> {
        let now = std::time::Instant::now();
        match self.download_rates.get_mut(key) {
            // Same download progressing: fold the newest interval in.
            Some(s) if bytes >= s.bytes => {
                let dt = now.duration_since(s.at).as_secs_f64();
                // Two pollers can sample the same download back-to-back;
                // a near-zero interval would just amplify noise.
                if dt < 0.3 {
                    return (s.rate > 0.0).then_some(s.rate);
                }
                let inst = (bytes - s.bytes) as f64 / dt;
                s.rate = if s.rate > 0.0 {
                    0.6 * s.rate + 0.4 * inst
                } else {
                    inst
                };
                s.bytes = bytes;
                s.at = now;
                (s.rate > 0.0).then_some(s.rate)
            }
            // First sample, or the byte count went backwards (a restarted
            // download): start fresh.
            _ => {
                self.download_rates.insert(
                    key.to_string(),
                    RateSample {
                        bytes,
                        at: now,
                        rate: 0.0,
                    },
                );
                None
            }
        }
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
                    let origin = if source == "managed" {
                        "managed"
                    } else {
                        "system"
                    };
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
            if install.as_ref().map(|s| s.status.as_str()) != Some("pending") {
                self.download_rates.remove(&format!("cli:{}", cli.id));
            }
            let (status, busy, progress) = match install.as_ref().map(|s| s.status.as_str()) {
                Some("pending") => {
                    let s = install.as_ref().unwrap();
                    let label = match &s.version {
                        Some(v) => format!("downloading {v}"),
                        None => "downloading".to_string(),
                    };
                    let rate = self.download_rate(&format!("cli:{}", cli.id), s.received_bytes);
                    let (text, pct) =
                        download_progress(&label, s.received_bytes, s.total_bytes, rate);
                    (text, true, pct)
                }
                Some("failed") => (
                    format!(
                        "install failed: {}",
                        install
                            .as_ref()
                            .and_then(|s| s.error.clone())
                            .unwrap_or_default()
                    ),
                    false,
                    -1,
                ),
                _ => (String::new(), false, -1),
            };
            rows.push(ui::CliView {
                id: cli.id,
                display_name: cli.display_name,
                version_label,
                action_label,
                status,
                busy,
                progress,
                managed: cli.source == "managed",
            });
        }
        ui::set_clis(&self.ui, rows);
    }

    /// Kick off a background fetch of local-model state (hardware, runtime,
    /// downloads); lands as `LocalLoaded`.
    fn refresh_local(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = async {
                let status = client.local_status().await.map_err(|e| format!("{e:#}"))?;
                let install = client
                    .cli_install_status("llama-server")
                    .await
                    .map_err(|e| format!("{e:#}"))?;
                Ok::<_, String>((status, install))
            }
            .await;
            let _ = tx.send(UiCommand::LocalLoaded(result));
        });
    }

    /// Render local-model state into the settings section.
    fn push_local(
        &mut self,
        status: &trouve_protocol::LocalStatus,
        install: &trouve_protocol::CliInstallStatus,
    ) {
        let mut hw_line = format!("{} RAM", human_gb(status.ram_bytes));
        if status.gpus.is_empty() {
            hw_line.push_str(" · no GPU detected (models run on the CPU)");
        }
        for gpu in &status.gpus {
            hw_line.push_str(&format!(
                " · {} {} VRAM",
                gpu.name,
                human_gb(gpu.vram_bytes)
            ));
        }
        let mut runtime_label = match (&status.runtime_installed, &status.runtime_version) {
            (true, Some(v)) => format!("installed ({v})"),
            (true, None) => "installed".to_string(),
            (false, _) => "not installed".to_string(),
        };
        if status.runtime_update_available
            && let Some(latest) = &status.runtime_latest_version
        {
            runtime_label.push_str(&format!(" — {latest} available"));
        }
        if install.status != "pending" {
            self.download_rates.remove("cli:llama-server");
        }
        let (runtime_busy, runtime_status, runtime_progress) = match install.status.as_str() {
            "pending" => {
                let rate = self.download_rate("cli:llama-server", install.received_bytes);
                let (text, pct) = download_progress(
                    "downloading llama.cpp",
                    install.received_bytes,
                    install.total_bytes,
                    rate,
                );
                (true, text, pct)
            }
            "failed" => (
                false,
                format!(
                    "install failed: {}",
                    install.error.clone().unwrap_or_default()
                ),
                -1,
            ),
            _ => (false, String::new(), -1),
        };
        let runtime_action = if status.runtime_installed {
            String::new()
        } else {
            "Install".to_string()
        };
        let (server_line, server_busy) =
            match (&status.running_model, status.server_status.as_str()) {
                (Some(model), "starting") => (format!("llama-server is loading {model}…"), true),
                (Some(model), _) => (format!("llama-server is running {model}"), false),
                (None, _) => (String::new(), false),
            };
        // Two sections: models the user has (downloaded, downloading, or
        // added themselves) and the untouched curated recommendations.
        let mut yours: Vec<ui::LocalModelView> = Vec::new();
        let mut recommended: Vec<ui::LocalModelView> = Vec::new();
        for m in &status.models {
            let mut meta = String::new();
            if !m.params.is_empty() {
                meta.push_str(&m.params);
                meta.push_str(" · ");
            }
            meta.push_str(&human_gb(m.size_bytes));
            let fit_label = match m.fit.as_str() {
                "gpu" => "fits your GPU",
                "cpu" => "runs on CPU (slower)",
                _ => "needs more memory",
            };
            let progress = (m.download_bytes * 100)
                .checked_div(m.size_bytes)
                .map_or(0, |p| p.min(99) as i32);
            let rate_key = format!("model:{}", m.id);
            let download_line = if m.download_status == "pending" {
                let rate = self.download_rate(&rate_key, m.download_bytes);
                download_progress("downloading", m.download_bytes, m.size_bytes, rate).0
            } else {
                self.download_rates.remove(&rate_key);
                String::new()
            };
            let view = ui::LocalModelView {
                id: m.id.clone(),
                name: m.display_name.clone(),
                header: String::new(),
                meta,
                fit: m.fit.clone(),
                fit_label: fit_label.into(),
                notes: m.notes.clone(),
                downloaded: m.downloaded,
                downloading: m.download_status == "pending",
                progress,
                download_line,
                error: m.download_error.clone(),
                custom: m.custom,
            };
            let mine = m.downloaded
                || m.custom
                || m.download_status == "pending"
                || !view.error.is_empty();
            if mine {
                yours.push(view);
            } else {
                recommended.push(view);
            }
        }
        let header = |text: &str| ui::LocalModelView {
            id: String::new(),
            name: String::new(),
            header: text.to_string(),
            meta: String::new(),
            fit: String::new(),
            fit_label: String::new(),
            notes: String::new(),
            downloaded: false,
            downloading: false,
            progress: 0,
            download_line: String::new(),
            error: String::new(),
            custom: false,
        };
        let mut models = Vec::new();
        if !yours.is_empty() {
            models.push(header("YOUR MODELS"));
            models.extend(yours);
        }
        if !recommended.is_empty() {
            models.push(header("RECOMMENDED"));
            models.extend(recommended);
        }
        ui::set_local(
            &self.ui,
            ui::LocalView {
                enabled: status.enabled,
                hw_line,
                runtime_label,
                runtime_action,
                runtime_busy,
                runtime_progress,
                runtime_managed: status.runtime_managed,
                runtime_update: status.runtime_update_available,
                runtime_status,
                server_line,
                server_busy,
                models,
            },
        );
    }

    /// Re-fetch the automations list in the background.
    fn refresh_automations(&self) {
        let client = self.client.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = client
                .list_automations()
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(UiCommand::AutomationsLoaded(result));
        });
    }

    /// Render the cached automations into the screen, with the workspace
    /// picker arrays it needs.
    fn push_automations(&self) {
        let names: Vec<String> = self.workspaces.iter().map(|w| w.name.clone()).collect();
        let ids: Vec<String> = self.workspaces.iter().map(|w| w.id.clone()).collect();
        let rows = self
            .automations
            .iter()
            .map(|a| {
                let ws_name = self
                    .workspaces
                    .iter()
                    .find(|w| w.id == a.workspace_id)
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| a.workspace_id.clone());
                let next_line = a
                    .next_run_at
                    .as_deref()
                    .and_then(fmt_local_ts)
                    .map(|t| format!("next run {t}"))
                    .unwrap_or_default();
                let awaiting_approval = a.last_error == "awaiting approval";
                let last_line = if awaiting_approval {
                    "waiting for approval".to_string()
                } else if !a.last_error.is_empty() {
                    format!("last run failed: {}", a.last_error)
                } else {
                    a.last_run_at
                        .as_deref()
                        .and_then(fmt_local_ts)
                        .map(|t| format!("last run {t}"))
                        .unwrap_or_default()
                };
                let mut days = vec![false; 7];
                for d in &a.schedule.days {
                    if let Some(flag) = days.get_mut(*d as usize) {
                        *flag = true;
                    }
                }
                ui::AutomationView {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    schedule_line: format!("{} · {ws_name}", schedule_summary(&a.schedule)),
                    next_line,
                    last_line,
                    last_failed: !a.last_error.is_empty() && !awaiting_approval,
                    enabled: a.enabled,
                    prompt: a.prompt.clone(),
                    workspace_index: ids.iter().position(|id| *id == a.workspace_id).unwrap_or(0)
                        as i32,
                    kind: a.schedule.kind.clone(),
                    minute_text: a.schedule.minute.to_string(),
                    permission_index: match a.permission_mode {
                        PermissionMode::Ask => 0,
                        PermissionMode::AllowList => 1,
                        PermissionMode::Yolo => 2,
                    },
                    time: if a.schedule.time.is_empty() {
                        "09:00".into()
                    } else {
                        a.schedule.time.clone()
                    },
                    days,
                }
            })
            .collect();
        ui::set_automations(&self.ui, rows, names, ids);
    }

    /// Render the cached template catalog into the screen.
    fn push_automation_templates(&self) {
        let templates = self
            .automation_templates
            .iter()
            .map(|t| {
                let mut days = vec![false; 7];
                for d in &t.schedule.days {
                    if let Some(flag) = days.get_mut(*d as usize) {
                        *flag = true;
                    }
                }
                ui::AutomationTemplateView {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    description: t.description.clone(),
                    schedule_line: schedule_summary(&t.schedule),
                    prompt: t.prompt.clone(),
                    kind: t.schedule.kind.clone(),
                    minute_text: t.schedule.minute.to_string(),
                    time: if t.schedule.time.is_empty() {
                        "09:00".into()
                    } else {
                        t.schedule.time.clone()
                    },
                    days,
                }
            })
            .collect();
        ui::set_automation_templates(&self.ui, templates);
    }

    /// Fold server-scope state events during replay; edge-triggered lifecycle
    /// events only act when fresh so reconnects do not trigger reload storms.
    async fn handle_server_event(&mut self, envelope: trouve_protocol::EventEnvelope) {
        use trouve_protocol::Event;

        // Dashboard snapshots are folded even during replay: unlike the
        // lifecycle events below, the persisted payload is the state itself
        // and reconstructs the cache after launch/reconnect.
        if let Event::GithubPullRequestsUpdated { pull_requests } = &envelope.event {
            self.pr_dash
                .insert(pull_requests.host.clone(), pull_requests.clone());
            self.sync_shared_prs(false);
            self.push_pr_dashboard();
            return;
        }

        let fresh = std::time::SystemTime::from(envelope.ts)
            .elapsed()
            .map(|age| age.as_secs() < 20)
            .unwrap_or(true);
        if !fresh {
            return;
        }
        match &envelope.event {
            // An automation ran: its last/next fields changed, and a
            // successful run created a session this UI hasn't seen.
            Event::AutomationFired { .. } => {
                self.refresh_automations();
                let _ = self.reload_sessions().await;
            }
            // Background session changes (this UI's own actions already
            // reload explicitly; a second reload is cheap and idempotent).
            Event::SessionCreated { .. } | Event::SessionDeleted { .. } => {
                let _ = self.reload_sessions().await;
            }
            // Workspace lifecycle is server-scoped so another app instance
            // can keep its sidebar in sync with opens and closes here.
            Event::WorkspaceRegistered { .. } => {
                if self.reload_sessions().await.is_ok() {
                    self.sync_home_workspace();
                }
            }
            Event::WorkspaceClosed { workspace_id } => {
                self.close_current_session_if_in_workspace(workspace_id);
                if self.reload_sessions().await.is_ok() {
                    self.sync_home_workspace();
                }
            }
            // A session started or finished processing prompts: light up or
            // dim its sidebar indicator.
            Event::SessionActivity {
                session_id, active, ..
            } => {
                let changed = if *active {
                    self.busy_sessions.insert(session_id.clone())
                } else {
                    self.busy_sessions.remove(session_id)
                };
                if *active {
                    self.watch_session_threads(session_id.clone());
                } else if changed {
                    let focused = self
                        .window_focused
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let viewed = focused
                        && self.current_session_id().as_deref() == Some(session_id.as_str());
                    if !viewed {
                        self.unread_sessions.insert(session_id.clone());
                    }
                }
                if changed {
                    self.push_nav();
                    self.push_agents_running();
                }
            }
            Event::ThreadCreated {
                thread_id,
                session_id,
            } if self.watched_sessions.contains(session_id) => {
                self.follow_thread(thread_id.clone(), session_id.clone());
            }
            Event::ThreadCreated {
                thread_id: _,
                session_id: _,
            } => {
                // Only selected or active sessions are watched. Their thread
                // lists are discovered explicitly, so unrelated global
                // thread events must not create permanent SSE followers.
            }
            // The server's internet reachability flipped: refresh the model
            // list (filtered while offline), regate prompt entry, announce
            // recovery.
            Event::ConnectivityChanged { online } => {
                self.apply_connectivity_change(*online).await;
            }
            _ => {}
        }
    }

    /// Render the cached HuggingFace search results into the settings UI.
    fn push_local_search(&self, status: String) {
        // A repo stays visible while any of its files lands in an enabled
        // fit category; its full file list stays intact once shown.
        let (gpu, cpu, large) = self.local_search_fits;
        let visible: Vec<_> = self
            .local_search
            .iter()
            .filter(|r| {
                r.files.iter().any(|f| match f.fit.as_str() {
                    "gpu" => gpu,
                    "cpu" => cpu,
                    _ => large,
                })
            })
            .collect();
        let status = if status.is_empty() && visible.is_empty() && !self.local_search.is_empty() {
            format!(
                "all {} results hidden by the fit filters",
                self.local_search.len()
            )
        } else {
            status
        };
        let results = visible
            .into_iter()
            .map(|r| ui::LocalSearchView {
                repo: r.repo.clone(),
                meta: format!(
                    "{} downloads · {} likes",
                    human_count(r.downloads),
                    human_count(r.likes)
                ),
                file_labels: r
                    .files
                    .iter()
                    .map(|f| {
                        let label = if f.quant.is_empty() {
                            f.file.rsplit('/').next().unwrap_or(&f.file).to_string()
                        } else {
                            f.quant.clone()
                        };
                        format!("{label} · {}", human_gb(f.size_bytes))
                    })
                    .collect(),
                file_names: r.files.iter().map(|f| f.file.clone()).collect(),
                file_fits: r.files.iter().map(|f| f.fit.clone()).collect(),
                file_fit_labels: r
                    .files
                    .iter()
                    .map(|f| {
                        match f.fit.as_str() {
                            "gpu" => "fits your GPU",
                            "cpu" => "runs on CPU (slower)",
                            _ => "needs more memory",
                        }
                        .to_string()
                    })
                    .collect(),
                file_added: r.files.iter().map(|f| f.added).collect(),
                recommended: r.recommended as i32,
            })
            .collect();
        ui::set_local_search(&self.ui, results, status);
    }

    // --- command dispatch --------------------------------------------------------

    async fn handle(&mut self, command: UiCommand) -> Result<()> {
        // The UI disables these controls while blocked, but a command that
        // was already queued when connectivity flipped (or a click racing
        // the banner) still lands here — the gate must be authoritative on
        // this side, not just cosmetic in the UI.
        if self.connectivity_blocked() {
            let reason = if self.server_unreachable {
                "the trouve server is unreachable"
            } else {
                "you're offline with no local models"
            };
            match &command {
                UiCommand::SendMessage(_)
                | UiCommand::StartNewChat { .. }
                | UiCommand::QueueEdit { .. }
                | UiCommand::QueueDelete(_)
                | UiCommand::QueueMove { .. }
                | UiCommand::QueueReorder { .. }
                | UiCommand::QueueSendNow
                | UiCommand::QueueSendNowAt(_)
                | UiCommand::ComposerModeChanged(_)
                | UiCommand::ComposerModelChanged(_)
                | UiCommand::ComposerThinkingChanged(_)
                | UiCommand::ComposerPermissionChanged(_)
                | UiCommand::ComposerContextChanged(_)
                | UiCommand::ComposerFastToggled(_) => {
                    self.error(&format!("Can't do that right now — {reason}."));
                    return Ok(());
                }
                UiCommand::SaveAutomation { .. }
                | UiCommand::AutomationToggled(..)
                | UiCommand::RunAutomation(_)
                | UiCommand::DeleteAutomation(_) => {
                    ui::set_automations_status(
                        &self.ui,
                        format!("Can't do that right now — {reason}."),
                    );
                    return Ok(());
                }
                _ => {}
            }
        }
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
            UiCommand::WorkspaceDropped {
                workspace_id,
                target_id,
                after,
            } => self.drop_workspace(&workspace_id, &target_id, after),
            UiCommand::WorkspaceMoved {
                workspace_id,
                offset,
            } => self.move_workspace(&workspace_id, offset),
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
            UiCommand::ToggleArchivedFilter { row } => {
                if let Some(NavEntry::Workspace(wi)) = self.nav.get(row)
                    && let Some(ws) = self.workspaces.get(*wi)
                {
                    let id = ws.id.clone();
                    if !self.show_archived.remove(&id) {
                        self.show_archived.insert(id);
                    }
                    self.push_nav();
                }
            }
            UiCommand::CloseWorkspace { row } => {
                if let Some(NavEntry::Workspace(wi)) = self.nav.get(row)
                    && let Some(ws) = self.workspaces.get(*wi)
                {
                    let id = ws.id.clone();
                    self.client.close_workspace(&id).await?;
                    self.collapsed_workspaces.remove(&id);
                    self.show_archived.remove(&id);
                    self.archived_expanded.remove(&id);
                    self.close_current_session_if_in_workspace(&id);
                    self.reload_sessions().await?;
                    self.sync_home_workspace();
                }
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
                self.refresh_mcp();
                self.refresh_subscriptions(SubscriptionRefresh::IfStale);
                ui::set_center_screen(&self.ui, 3);
            }
            UiCommand::CloseSettings => {
                // Back to whatever the center showed before settings.
                ui::set_center_screen(
                    &self.ui,
                    match self.new_chat {
                        None => 0,
                        Some(NewChat::Session) => 1,
                        Some(NewChat::Thread) => 2,
                    },
                );
                // The visit may have (un)configured GitHub; the PR tab
                // reflects it immediately. No-op when unconfigured.
                self.refresh_prs();
            }
            UiCommand::AppearanceChanged => {
                // Chat rows bake syntax-highlight and inline-code colors at
                // conversion time; drop the diff cache and re-fold so they
                // pick up the new theme.
                ui::invalidate_chat_cache(&self.ui);
                self.render_chat(false);
                // Same for the open file's highlight segments (and the
                // markdown preview rows behind it).
                if let (Some(path), Some(session_id)) =
                    (self.open_file.clone(), self.current_session_id())
                    && let Ok(file) = self.client.session_file(&session_id, &path).await
                {
                    let lines = render::highlight_file(&file.path, &file.content);
                    ui::set_file_view(&self.ui, path, file.content, lines);
                }
            }
            UiCommand::NewSession => self.open_new_session_screen(None).await?,
            UiCommand::NewThread => self.open_new_thread_screen(),
            UiCommand::CancelNewChat => {
                self.close_new_chat();
                self.render_chat(false);
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
                    let (mode_index, model_index) = self.new_chat_default_indices();
                    ui::set_new_chat(
                        &self.ui,
                        self.workspaces.iter().map(|w| w.name.clone()).collect(),
                        index as i32,
                        Vec::new(),
                        -1,
                        mode_index,
                        model_index,
                    );
                    self.push_new_chat_knobs(
                        usize::try_from(mode_index).unwrap_or(0),
                        usize::try_from(model_index).unwrap_or(0),
                    );
                    self.load_branches(index).await;
                }
            }
            UiCommand::StartNewChat {
                workspace_idx,
                branch_idx,
                fetch_latest,
                mode_idx,
                model_idx,
                thinking_idx,
                permission_idx,
                prompt,
            } => {
                self.start_new_chat(
                    NewChatSelection {
                        workspace_idx,
                        branch_idx,
                        fetch_latest,
                        mode_idx,
                        model_idx,
                        thinking_idx,
                        permission_idx,
                    },
                    prompt,
                )
                .await?
            }
            UiCommand::NewChatModelChanged {
                mode_idx,
                model_idx,
            } => self.push_new_chat_knobs(mode_idx, model_idx),
            UiCommand::SelectThread(i) => {
                self.open_thread_index(i, false);
                // i == threads.len() is the provisional tab itself: no-op.
            }
            UiCommand::ChatPositionChanged {
                thread_id,
                row,
                offset,
                at_bottom,
            } => {
                if self.restoring_thread.as_deref() == Some(&thread_id) {
                    self.restoring_thread = None;
                }
                let changed = if at_bottom {
                    self.resume.thread_scroll.remove(&thread_id).is_some()
                } else if offset.is_finite() && offset >= 0.0 {
                    let bookmark = crate::winstate::ChatScrollBookmark { row, offset };
                    if self.resume.thread_scroll.get(&thread_id) == Some(&bookmark) {
                        false
                    } else {
                        self.resume.thread_scroll.insert(thread_id, bookmark);
                        true
                    }
                } else {
                    false
                };
                if changed {
                    crate::winstate::save_resume(&self.resume);
                }
            }
            UiCommand::SendMessage(text) => {
                if let Some(thread_id) = self.current_thread_id() {
                    let uploads = std::mem::take(&mut self.pending_attachments);
                    self.push_attachments();
                    if let Err(e) = self
                        .client
                        .send_message_with(&thread_id, &text, uploads.clone())
                        .await
                    {
                        // Restage so a transient failure doesn't eat the files.
                        self.pending_attachments = uploads;
                        self.push_attachments();
                        return Err(e);
                    }
                    self.apply_scroll_intent(true);
                }
            }
            UiCommand::CancelTurn => {
                if let Some(thread_id) = self.current_thread_id() {
                    self.client.cancel_turn(&thread_id).await?;
                }
            }
            UiCommand::RefreshAtFiles => self.refresh_at_files(),
            UiCommand::AttachFileDialog => {
                // The portal dialog can stay open indefinitely; run it off
                // the command loop so events keep flowing meanwhile.
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Attach files to the prompt")
                        .pick_files()
                        .await;
                    for file in picked.unwrap_or_default() {
                        let path = file.path().to_path_buf();
                        let name = file.file_name();
                        match tokio::fs::read(&path).await {
                            Ok(bytes) => {
                                let mime = mime_guess::from_path(&path)
                                    .first_or_octet_stream()
                                    .essence_str()
                                    .to_string();
                                let _ = tx.send(UiCommand::AddAttachment { name, mime, bytes });
                            }
                            Err(e) => tracing::warn!("attach {name}: {e}"),
                        }
                    }
                });
            }
            UiCommand::AddAttachment { name, mime, bytes } => {
                const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
                if bytes.len() > MAX_ATTACHMENT_BYTES {
                    self.error(&format!("{name} is larger than the 10 MB attachment limit"));
                } else {
                    use base64::Engine as _;
                    self.pending_attachments
                        .push(trouve_protocol::AttachmentUpload {
                            name,
                            mime,
                            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                        });
                    self.push_attachments();
                }
            }
            UiCommand::AttachmentRemoved(index) => {
                if index < self.pending_attachments.len() {
                    self.pending_attachments.remove(index);
                    self.push_attachments();
                }
            }
            UiCommand::QueueEdit { index, content } => {
                if let Some(id) = self.queued_prompt_id(index)
                    && let Err(e) = self.client.update_queued_prompt(&id, &content).await
                {
                    self.error(&format!("{e:#}"));
                }
            }
            UiCommand::QueueDelete(index) => {
                if let Some(id) = self.queued_prompt_id(index)
                    && let Err(e) = self.client.delete_queued_prompt(&id).await
                {
                    self.error(&format!("{e:#}"));
                }
            }
            UiCommand::QueueMove { index, delta } => {
                if let Some(thread_id) = self.current_thread_id() {
                    let ids: Vec<String> = self
                        .vms
                        .get(&thread_id)
                        .map(|vm| vm.queue.iter().map(|p| p.id.clone()).collect())
                        .unwrap_or_default();
                    let to = index as i64 + delta as i64;
                    if index < ids.len() && to >= 0 && (to as usize) < ids.len() {
                        let mut ids = ids;
                        ids.swap(index, to as usize);
                        if let Err(e) = self.client.reorder_queue(&thread_id, &ids).await {
                            self.error(&format!("{e:#}"));
                        }
                    }
                }
            }
            UiCommand::QueueReorder { from, to } => {
                if let Some(thread_id) = self.current_thread_id() {
                    let ids: Vec<String> = self
                        .vms
                        .get(&thread_id)
                        .map(|vm| vm.queue.iter().map(|p| p.id.clone()).collect())
                        .unwrap_or_default();
                    if from < ids.len() && to < ids.len() && from != to {
                        let mut ids = ids;
                        let id = ids.remove(from);
                        ids.insert(to, id);
                        if let Err(e) = self.client.reorder_queue(&thread_id, &ids).await {
                            self.error(&format!("{e:#}"));
                        }
                    }
                }
            }
            UiCommand::QueueSendNow => {
                if let Some(thread_id) = self.current_thread_id() {
                    match self.client.dispatch_queue(&thread_id).await {
                        Ok(_) => self.apply_scroll_intent(true),
                        Err(e) => self.error(&format!("{e:#}")),
                    }
                }
            }
            UiCommand::QueueSendNowAt(index) => {
                if let Some(thread_id) = self.current_thread_id() {
                    let (mut ids, idle): (Vec<String>, bool) = self
                        .vms
                        .get(&thread_id)
                        .map(|vm| {
                            (
                                vm.queue.iter().map(|p| p.id.clone()).collect(),
                                !vm.turn_running,
                            )
                        })
                        .unwrap_or_default();
                    if index < ids.len() {
                        let id = ids.remove(index);
                        ids.insert(0, id);
                        self.client.reorder_queue(&thread_id, &ids).await?;
                        if idle {
                            self.client.dispatch_queue(&thread_id).await?;
                        }
                    }
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
            UiCommand::QuestionOption { row, option } => {
                if let Some((request_id, questions)) = self.question_at(row)
                    && let Some(w) = self.wizards.get_mut(&request_id)
                    && w.step < questions.len()
                {
                    let q = &questions[w.step];
                    let id = if option >= q.options.len() {
                        render::OTHER_ID.to_string()
                    } else {
                        q.options[option].id.clone()
                    };
                    let sel = &mut w.selections[w.step];
                    if q.allow_multiple {
                        match sel.iter().position(|s| *s == id) {
                            Some(pos) => {
                                sel.remove(pos);
                            }
                            None => sel.push(id),
                        }
                    } else if sel.first() == Some(&id) {
                        sel.clear();
                    } else {
                        *sel = vec![id];
                    }
                    self.render_chat(false);
                }
            }
            UiCommand::QuestionOtherEdited { row, text } => {
                if let Some((request_id, questions)) = self.question_at(row)
                    && let Some(w) = self.wizards.get_mut(&request_id)
                    && w.step < questions.len()
                {
                    let was_empty = w.other_texts[w.step].trim().is_empty();
                    w.other_texts[w.step] = text;
                    // Only the Next button's enabled state depends on
                    // this text; re-render just when it flips so the
                    // input isn't reset mid-typing.
                    if was_empty != w.other_texts[w.step].trim().is_empty() {
                        self.render_chat(false);
                    }
                }
            }
            UiCommand::QuestionBack(row) => {
                if let Some((request_id, _)) = self.question_at(row)
                    && let Some(w) = self.wizards.get_mut(&request_id)
                {
                    w.step = w.step.saturating_sub(1);
                    self.render_chat(false);
                }
            }
            UiCommand::QuestionNext(row) => {
                if let Some((request_id, questions)) = self.question_at(row)
                    && let Some(w) = self.wizards.get_mut(&request_id)
                {
                    if w.step < questions.len() {
                        w.step += 1;
                        self.render_chat(false);
                    } else {
                        // Review page: submit. The wizard state stays
                        // until question.resolved lands and prunes it.
                        let answers = w.answers(&questions);
                        self.client
                            .resolve_question(&request_id, Some(answers))
                            .await?;
                    }
                }
            }
            UiCommand::QuestionSkip(row) => {
                if let Some((request_id, _)) = self.question_at(row) {
                    self.client.resolve_question(&request_id, None).await?;
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
                let selected_mode = self.modes.get(i).cloned();
                let mode = selected_mode.as_ref().map(|m| m.id.clone());
                // Switching modes also applies the mode's default model,
                // and thinking level, when present; the user can still
                // re-pick either afterwards.
                let model = selected_mode
                    .as_ref()
                    .and_then(|m| m.default_model.clone())
                    .filter(|m| self.models.iter().any(|known| known.id == *m));
                let mut model_options = if model.is_some() {
                    serde_json::Map::new()
                } else {
                    self.current_model_options()
                };
                for key in ["thinking_level", "reasoning_effort", "effort", "reasoning"] {
                    model_options.remove(key);
                }
                if let Some(level) = selected_mode
                    .and_then(|m| m.default_thinking_level)
                    .or_else(|| self.default_thinking_level.clone())
                {
                    model_options.insert("thinking_level".into(), serde_json::Value::String(level));
                }
                self.update_current_thread(UpdateThreadRequest {
                    mode,
                    model_options: Some(model_options),
                    model,
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
            UiCommand::ComposerPermissionChanged(i) => {
                if let Some(permission_mode) = permission_mode_of(i as i32) {
                    self.update_current_thread(UpdateThreadRequest {
                        permission_mode: Some(permission_mode),
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
            UiCommand::RightTabChanged(tab) => {
                self.right_tab = if tab == TODOS_TAB
                    && self
                        .current_thread
                        .and_then(|i| self.threads.get(i))
                        .is_none_or(|thread| thread.todos.is_empty())
                {
                    ui::set_right_tab(&self.ui, 0);
                    0
                } else {
                    tab
                };
                if self.right_tab == TERMINAL_TAB {
                    self.ensure_terminal().await;
                }
            }
            UiCommand::TermKey { text, ctrl, alt } => {
                let Some((id, bytes)) = self.term_attached().and_then(|(id, state)| {
                    slint_terminal::encode_key(&text, ctrl, alt, state.grid.application_cursor())
                        .map(|b| (id, b))
                }) else {
                    return Ok(());
                };
                // Typing always snaps back to the live tail.
                if let Some(state) = self.current_term_mut()
                    && state.grid.scrollback_offset() > 0
                {
                    state.grid.scroll_to_live();
                    self.push_term();
                }
                if let Err(e) = self.client.terminal_input(&id, &bytes).await {
                    self.error(&format!("terminal input: {e:#}"));
                }
            }
            UiCommand::TermPaste(text) => {
                let Some((id, bracketed)) = self
                    .term_attached()
                    .map(|(id, state)| (id, state.grid.bracketed_paste()))
                else {
                    return Ok(());
                };
                let bytes = slint_terminal::encode_paste(&text, bracketed);
                if let Err(e) = self.client.terminal_input(&id, &bytes).await {
                    self.error(&format!("terminal paste: {e:#}"));
                }
            }
            UiCommand::TermWheel(lines) => {
                if let Some(state) = self.current_term_mut() {
                    state.grid.scroll_lines(lines);
                    self.push_term();
                }
            }
            UiCommand::TermResized { cols, rows } => {
                self.term_view = (cols, rows);
                let Some((id, state)) = self
                    .current_session_id()
                    .and_then(|sid| self.terms.get_mut(&sid).map(|s| (s.terminal_id.clone(), s)))
                else {
                    return Ok(());
                };
                if state.grid.size() != (rows, cols) {
                    state.grid.resize(rows, cols);
                    self.push_term();
                    if let Err(e) = self.client.terminal_resize(&id, cols, rows).await {
                        tracing::warn!("terminal resize: {e:#}");
                    }
                }
            }
            UiCommand::TermRestart => {
                let Some(session_id) = self.current_session_id() else {
                    return Ok(());
                };
                if let Some(state) = self.terms.remove(&session_id) {
                    // Kill server-side; the follower ends with the stream.
                    let _ = self.client.kill_terminal(&state.terminal_id).await;
                }
                self.ensure_terminal().await;
            }
            UiCommand::TermOutput {
                session_id,
                terminal_id,
                offset,
                bytes,
            } => {
                let visible = self.current_session_id().as_deref() == Some(session_id.as_str());
                // Guard against a stale follower racing a restart.
                let mut applied = false;
                if let Some(state) = self.terms.get_mut(&session_id)
                    && state.terminal_id == terminal_id
                {
                    state.grid.process(&bytes);
                    state.offset = offset;
                    applied = true;
                }
                if applied && visible {
                    let now = std::time::Instant::now();
                    let elapsed = self
                        .last_term_render
                        .map(|last| now.duration_since(last))
                        .unwrap_or(TERM_FRAME_INTERVAL);
                    if elapsed >= TERM_FRAME_INTERVAL {
                        self.term_render_pending = None;
                        self.push_term();
                        self.last_term_render = Some(now);
                    } else {
                        let pending = (session_id.clone(), terminal_id.clone());
                        if self.term_render_pending.as_ref() != Some(&pending) {
                            self.term_render_pending = Some(pending);
                            let tx = self.tx.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(TERM_FRAME_INTERVAL - elapsed).await;
                                let _ = tx.send(UiCommand::FlushTerm {
                                    session_id,
                                    terminal_id,
                                });
                            });
                        }
                    }
                }
            }
            UiCommand::FlushTerm {
                session_id,
                terminal_id,
            } => {
                let expected = (session_id.clone(), terminal_id.clone());
                if self.term_render_pending.as_ref() == Some(&expected) {
                    self.term_render_pending = None;
                    let visible = self.current_session_id().as_deref() == Some(session_id.as_str());
                    let current = self
                        .terms
                        .get(&session_id)
                        .is_some_and(|state| state.terminal_id == terminal_id);
                    if visible && current {
                        self.push_term();
                        self.last_term_render = Some(std::time::Instant::now());
                    }
                }
            }
            UiCommand::TermEnded {
                session_id,
                terminal_id,
            } => {
                let visible = self.current_session_id().as_deref() == Some(session_id.as_str());
                let mut applied = false;
                if let Some(state) = self.terms.get_mut(&session_id)
                    && state.terminal_id == terminal_id
                {
                    state.exited = true;
                    applied = true;
                }
                if applied && visible {
                    self.term_render_pending = None;
                    self.push_term();
                    self.last_term_render = Some(std::time::Instant::now());
                }
            }
            // Background poll: swallow transient errors rather than flashing
            // the error banner every tick.
            UiCommand::RefreshDiff => {
                let _ = self.refresh_diff().await;
            }
            UiCommand::ToggleDiffFile(i) => {
                if let Some(flag) = self.diff_collapsed.get_mut(i) {
                    *flag = !*flag;
                    self.push_diff();
                }
            }
            UiCommand::FileActivated(i) => {
                let Some(row) = self.file_rows.get(i).cloned() else {
                    return Ok(());
                };
                if row.is_dir {
                    // Toggle the folder; children are fetched lazily on the
                    // first expand and kept for later re-expands.
                    if !self.file_expanded.remove(&row.path) {
                        self.file_expanded.insert(row.path.clone());
                        if !self.file_children.contains_key(&row.path)
                            && let Some(session_id) = self.current_session_id()
                        {
                            let entries = self.client.session_files(&session_id, &row.path).await?;
                            self.file_children.insert(row.path.clone(), entries);
                        }
                    }
                    self.push_file_tree();
                } else if let Some(session_id) = self.current_session_id() {
                    let file = self.client.session_file(&session_id, &row.path).await?;
                    let lines = render::highlight_file(&file.path, &file.content);
                    self.open_file = Some(row.path.clone());
                    ui::set_file_view(&self.ui, row.path, file.content, lines);
                    // A browser open carries no line range to highlight.
                    ui::set_file_selection(&self.ui, -1, -1);
                }
            }
            UiCommand::OpenFileExternally(path) => {
                // Hand the absolute worktree path to the system's default
                // handler (xdg-open / open / start) — whatever editor the
                // user has associated with the file type.
                if let Some(index) = self.current_session {
                    let full = std::path::Path::new(&self.sessions[index].worktree_path)
                        .join(path.trim_start_matches('/'));
                    crate::opener::open(&full);
                }
            }
            UiCommand::OpenChatFile(path, from, to) => {
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
                        self.open_file = Some(rel.clone());
                        ui::set_file_view(&self.ui, rel, file.content, lines);
                        // Preselect the lines the tool covered (1-based in
                        // the args, 0-based in the view).
                        if from > 0 {
                            ui::set_file_selection(&self.ui, from - 1, to.max(from) - 1);
                        } else {
                            ui::set_file_selection(&self.ui, -1, -1);
                        }
                        ui::set_right_tab(&self.ui, 1);
                    }
                    Err(e) => self.error(&format!("could not open {rel}: {e}")),
                }
            }
            UiCommand::Undo => {
                if let Some(session_id) = self.current_session_id() {
                    self.client.undo(&session_id).await?;
                    self.refresh_diff().await?;
                }
            }
            UiCommand::Redo => {
                if let Some(session_id) = self.current_session_id() {
                    self.client.redo(&session_id).await?;
                    self.refresh_diff().await?;
                }
            }
            UiCommand::CreatePr => {
                if let (Some(session_id), Some(index)) =
                    (self.current_session_id(), self.current_session)
                {
                    let title = self.sessions[index].title.clone();
                    self.client
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
                    self.refresh_pr_dashboard();
                    self.refresh_prs();
                }
            }
            UiCommand::RefreshPrs => {
                self.refresh_pr_dashboard();
                self.refresh_prs();
            }
            UiCommand::SelectPr(i) => {
                if i < self.prs.len() {
                    self.pr_selected = i;
                    self.push_prs();
                }
            }
            UiCommand::OpenPrUrl(url) => {
                if !url.is_empty() {
                    crate::opener::open(&url);
                }
            }
            UiCommand::PrsLoaded(session_id, result) => {
                match result {
                    Ok(prs) => {
                        self.nav_prs.insert(session_id.clone(), prs.clone());
                        if self.current_session_id().as_deref() == Some(&session_id) {
                            // Keep the selection on the same PR across
                            // refreshes when it still exists.
                            let keep = self
                                .prs
                                .get(self.pr_selected)
                                .and_then(|cur| prs.iter().position(|p| p.number == cur.number));
                            self.prs = prs;
                            self.pr_selected = keep.unwrap_or(0);
                            self.pr_error.clear();
                            self.push_prs();
                        }
                        self.push_nav();
                    }
                    Err(e) => {
                        // The right panel owns errors for its selected
                        // session; an old response must not replace it.
                        if self.current_session_id().as_deref() == Some(&session_id) {
                            self.prs.clear();
                            self.pr_selected = 0;
                            self.pr_error = e;
                            self.push_prs();
                        }
                    }
                }
            }
            UiCommand::OpenIntegrationsSettings => {
                ui::set_settings_section(&self.ui, 3);
                self.refresh_settings().await;
                self.refresh_mcp();
                self.refresh_subscriptions(SubscriptionRefresh::IfStale);
                ui::set_center_screen(&self.ui, 3);
            }
            UiCommand::RefreshMcp => self.refresh_mcp(),
            UiCommand::McpLoaded(servers, probed) => {
                let items = servers
                    .into_iter()
                    .map(|s| ui::McpView {
                        name: s.name,
                        scope: s.scope,
                        workspace_id: s.workspace_id,
                        workspace_name: s.workspace_name,
                        command_line: render_command_line(&s.command, &s.args),
                        env_lines: s
                            .env
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        health: if probed || s.health == "disabled" {
                            s.health
                        } else {
                            "checking".into()
                        },
                        detail: s.detail,
                    })
                    .collect();
                ui::set_mcp_servers(&self.ui, items);
                ui::set_mcp_status(
                    &self.ui,
                    if probed {
                        String::new()
                    } else {
                        "checking server health…".into()
                    },
                );
            }
            UiCommand::RefreshSessionMcp => self.refresh_session_mcp(),
            UiCommand::SessionMcpLoaded(session_id, result) => {
                // Stale fetch (session switched underneath): drop it.
                if self.current_session_id().as_deref() == Some(session_id.as_str()) {
                    match result {
                        Ok(servers) => {
                            let items = servers
                                .into_iter()
                                .map(|s| ui::McpView {
                                    name: s.name,
                                    scope: s.scope,
                                    workspace_id: s.workspace_id,
                                    workspace_name: s.workspace_name,
                                    command_line: render_command_line(&s.command, &s.args),
                                    env_lines: s
                                        .env
                                        .iter()
                                        .map(|(k, v)| format!("{k}={v}"))
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                    health: s.health,
                                    detail: s.detail,
                                })
                                .collect();
                            ui::set_session_mcp(&self.ui, items, String::new());
                        }
                        Err(e) => ui::set_session_mcp(
                            &self.ui,
                            Vec::new(),
                            format!("failed to load MCP config: {e}"),
                        ),
                    }
                }
            }
            UiCommand::SaveMcpServer {
                name,
                scope,
                command_line,
                env_lines,
                workspace_id,
            } => match parse_mcp_form(&command_line, &env_lines) {
                Ok((command, args, env)) => {
                    let req = trouve_protocol::UpsertMcpServerRequest {
                        workspace_id: (scope == "workspace" && !workspace_id.is_empty())
                            .then_some(workspace_id),
                        scope,
                        command,
                        args,
                        env,
                    };
                    match self.client.upsert_mcp_server(&name, &req).await {
                        Ok(()) => {
                            self.refresh_mcp();
                            self.refresh_session_mcp();
                        }
                        Err(e) => ui::set_mcp_status(&self.ui, format!("{e:#}")),
                    }
                }
                Err(e) => ui::set_mcp_status(&self.ui, e),
            },
            UiCommand::DeleteMcpServer {
                name,
                scope,
                workspace_id,
            } => {
                let workspace_id =
                    (scope == "workspace" && !workspace_id.is_empty()).then_some(workspace_id);
                match self
                    .client
                    .delete_mcp_server(&name, &scope, workspace_id.as_deref())
                    .await
                {
                    Ok(()) => {
                        self.refresh_mcp();
                        self.refresh_session_mcp();
                    }
                    Err(e) => ui::set_mcp_status(&self.ui, format!("{e:#}")),
                }
            }
            UiCommand::McpLogs(name) => match self.client.mcp_server_logs(&name).await {
                Ok(logs) => ui::set_mcp_logs(&self.ui, name, logs.lines.join("\n")),
                Err(e) => self.error(&format!("loading MCP logs: {e:#}")),
            },
            UiCommand::SubscriptionsLoaded { generation, result } => {
                if self.subscription_refresh.is_current(generation) {
                    match result {
                        Ok(subs) => {
                            let items = subs
                                .iter()
                                .map(|s| ui::SubscriptionView {
                                    provider: s.provider_id.clone(),
                                    status: s.status.clone(),
                                    plan: s.plan.clone(),
                                    credits: s.credits.clone(),
                                    note: s.note.clone(),
                                    windows: s
                                        .windows
                                        .iter()
                                        .map(|w| {
                                            (w.label.clone(), w.used_percent, w.resets.clone())
                                        })
                                        .collect(),
                                })
                                .collect();
                            self.subscription_health = subs;
                            ui::set_subscriptions(&self.ui, items, String::new());
                            self.push_model_health();
                        }
                        Err(error) => ui::set_subscriptions(
                            &self.ui,
                            Vec::new(),
                            format!("failed to load subscription usage: {error}"),
                        ),
                    }
                }
            }
            UiCommand::AddGithubHost(host, client_id) => {
                match self.client.add_github_host(&host, &client_id).await {
                    Ok(integration) => {
                        self.apply_github_integration(integration);
                        self.push_github_integration();
                        self.refresh_pr_dashboard();
                        self.refresh_prs();
                        self.refresh_nav_prs(true);
                    }
                    Err(e) => self.error(&format!("adding GitHub host: {e:#}")),
                }
            }
            UiCommand::RemoveGithubHost(host) => {
                match self.client.remove_github_host(&host).await {
                    Ok(integration) => {
                        self.apply_github_integration(integration);
                        self.push_github_integration();
                        self.refresh_pr_dashboard();
                        self.refresh_prs();
                        self.refresh_nav_prs(true);
                    }
                    Err(e) => self.error(&format!("removing GitHub host: {e:#}")),
                }
            }
            UiCommand::RefreshSettings => {
                // Sent after a login or CLI install completes — both can
                // unlock backend models, so refresh the pickers too.
                self.reload_catalogs().await;
                self.refresh_settings().await;
                self.refresh_subscriptions(SubscriptionRefresh::Force);
            }
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
                            &self.ui,
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
                        self.refresh_subscriptions(SubscriptionRefresh::Force);
                    }
                    Err(e) => {
                        ui::set_settings_status(&self.ui, format!("{e:#}"));
                    }
                }
            }
            UiCommand::DeleteProvider(id) => match self.client.delete_provider(&id).await {
                Ok(()) => {
                    ui::set_settings_status(&self.ui, format!("removed {id}"));
                    self.reload_catalogs().await;
                    self.refresh_settings().await;
                    self.refresh_subscriptions(SubscriptionRefresh::Force);
                }
                Err(e) => {
                    ui::set_settings_status(&self.ui, format!("{e:#}"));
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
                    ui::set_settings_status(&self.ui, msg);
                    if !started.verification_url.is_empty() {
                        open_in_browser(&started.verification_url);
                    }
                    // Poll the login in the background so the UI stays live;
                    // report the outcome and refresh the provider list.
                    let client = self.client.clone();
                    let settings_ui = self.ui.clone();
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
                    ui::set_settings_status(&self.ui, format!("{e:#}"));
                }
            },
            UiCommand::SetDefaultModel(i, thinking_level) => {
                if let Some(model) = self.models.get(i) {
                    match self
                        .client
                        .set_default_model(&model.id, thinking_level.as_deref())
                        .await
                    {
                        Ok(()) => {
                            ui::set_settings_status(
                                &self.ui,
                                match &thinking_level {
                                    Some(level) => format!(
                                        "defaults: {} · {} thinking",
                                        model.id,
                                        level_label(level)
                                    ),
                                    None => format!("default model: {}", model.id),
                                },
                            );
                            self.refresh_settings().await;
                        }
                        Err(e) => {
                            ui::set_settings_status(&self.ui, format!("{e:#}"));
                        }
                    }
                }
            }
            UiCommand::SetDefaultPermission(i) => {
                if let Some(mode) = permission_mode_of(i) {
                    match self.client.set_default_permission_mode(mode).await {
                        Ok(()) => {
                            ui::set_settings_status(
                                &self.ui,
                                format!("default permissions: {}", permission_label(mode)),
                            );
                            self.refresh_settings().await;
                        }
                        Err(e) => {
                            ui::set_settings_status(&self.ui, format!("{e:#}"));
                        }
                    }
                }
            }
            UiCommand::SaveMode(
                id,
                display,
                prompt,
                tools,
                read_only,
                perm,
                model,
                thinking_level,
            ) => {
                let req = UpsertModeRequest {
                    display_name: display,
                    system_prompt: prompt,
                    allowed_tools: tools
                        .split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .collect(),
                    read_only,
                    default_permission_mode: permission_mode_of(perm),
                    default_model: usize::try_from(model)
                        .ok()
                        .and_then(|i| self.models.get(i))
                        .map(|m| m.id.clone()),
                    default_thinking_level: thinking_level,
                };
                match self.client.upsert_mode(&id, &req).await {
                    Ok(()) => {
                        ui::set_settings_status(&self.ui, format!("saved mode {id}"));
                        self.reload_catalogs().await;
                        self.refresh_settings().await;
                    }
                    Err(e) => ui::set_settings_status(&self.ui, format!("{e:#}")),
                }
            }
            UiCommand::DeleteMode(id) => match self.client.delete_mode(&id).await {
                Ok(()) => {
                    ui::set_settings_status(&self.ui, format!("removed mode override {id}"));
                    self.reload_catalogs().await;
                    self.refresh_settings().await;
                }
                Err(e) => ui::set_settings_status(&self.ui, format!("{e:#}")),
            },
            UiCommand::SetModeModel(id, model_idx) => {
                // PUT replaces the whole mode file, so carry the current
                // fields and only swap the default model.
                if let Some(mode) = self.modes.iter().find(|m| m.id == id).cloned() {
                    let req = UpsertModeRequest {
                        display_name: mode.display_name,
                        system_prompt: mode.system_prompt,
                        allowed_tools: mode.allowed_tools,
                        read_only: mode.read_only,
                        default_permission_mode: mode.default_permission_mode,
                        default_model: usize::try_from(model_idx)
                            .ok()
                            .and_then(|i| self.models.get(i))
                            .map(|m| m.id.clone()),
                        // Thinking choices are model-specific; changing the
                        // mode's model resets this override to the global
                        // default, just like changing a thread model clears
                        // its model options.
                        default_thinking_level: None,
                    };
                    match self.client.upsert_mode(&id, &req).await {
                        Ok(()) => {
                            ui::set_settings_status(
                                &self.ui,
                                match &req.default_model {
                                    Some(m) => format!("{id} default model: {m}"),
                                    None => format!("{id} uses the global default model"),
                                },
                            );
                            self.reload_catalogs().await;
                            self.refresh_settings().await;
                        }
                        Err(e) => ui::set_settings_status(&self.ui, format!("{e:#}")),
                    }
                }
            }
            UiCommand::SetModeThinking(id, thinking_level) => {
                // PUT replaces the whole mode file, so carry every field and
                // only swap the thinking-level override.
                if let Some(mode) = self.modes.iter().find(|m| m.id == id).cloned() {
                    let req = UpsertModeRequest {
                        display_name: mode.display_name,
                        system_prompt: mode.system_prompt,
                        allowed_tools: mode.allowed_tools,
                        read_only: mode.read_only,
                        default_permission_mode: mode.default_permission_mode,
                        default_model: mode.default_model,
                        default_thinking_level: thinking_level,
                    };
                    match self.client.upsert_mode(&id, &req).await {
                        Ok(()) => {
                            ui::set_settings_status(
                                &self.ui,
                                match &req.default_thinking_level {
                                    Some(level) => {
                                        format!("{id} default thinking: {}", level_label(level))
                                    }
                                    None => {
                                        format!("{id} uses the global default thinking level")
                                    }
                                },
                            );
                            self.reload_catalogs().await;
                            self.refresh_settings().await;
                        }
                        Err(e) => ui::set_settings_status(&self.ui, format!("{e:#}")),
                    }
                }
            }
            UiCommand::RefreshLocal => {
                self.local_polling = false;
                self.refresh_local();
            }
            UiCommand::LocalLoaded(result) => match result {
                Ok((status, install)) => {
                    self.push_local(&status, &install);
                    // A finished download/delete changes the model catalog:
                    // reload the composer pickers when the count moves.
                    let downloaded = status.models.iter().filter(|m| m.downloaded).count();
                    if self.local_downloaded != Some(downloaded) {
                        if self.local_downloaded.is_some() {
                            self.reload_catalogs().await;
                        }
                        self.local_downloaded = Some(downloaded);
                    }
                    // Keep polling while something is in flight (downloads,
                    // runtime install, or a model loading after restart).
                    let busy = install.status == "pending"
                        || status.server_status == "starting"
                        || status.models.iter().any(|m| m.download_status == "pending");
                    if busy && !self.local_polling {
                        self.local_polling = true;
                        let tx = self.tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                            let _ = tx.send(UiCommand::RefreshLocal);
                        });
                    }
                }
                Err(e) => ui::set_local_status(&self.ui, format!("failed to load: {e}")),
            },
            UiCommand::LocalDownload(id) => {
                match self.client.start_local_model_download(&id).await {
                    Ok(()) => ui::set_local_status(&self.ui, String::new()),
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
            }
            UiCommand::LocalCancelDownload(id) => {
                match self.client.cancel_local_model_download(&id).await {
                    Ok(()) => ui::set_local_status(&self.ui, String::new()),
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
            }
            UiCommand::LocalDeleteModel(id) => {
                match self.client.delete_local_model(&id).await {
                    Ok(()) => ui::set_local_status(&self.ui, String::new()),
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
            }
            UiCommand::LocalStopServer => {
                let _ = self.client.stop_local_server().await;
                self.refresh_local();
            }
            UiCommand::LocalRestartServer => {
                match self.client.restart_local_server().await {
                    Ok(()) => ui::set_local_status(&self.ui, String::new()),
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
            }
            UiCommand::LocalEnabledToggled(enabled) => {
                match self.client.set_local_enabled(enabled).await {
                    Ok(()) => ui::set_local_status(&self.ui, String::new()),
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
                // local/* models appear or disappear from the pickers.
                self.reload_catalogs().await;
            }
            UiCommand::LocalAddModel { repo, file } => {
                match self
                    .client
                    .add_local_model(&AddLocalModelRequest {
                        repo: repo.clone(),
                        file: file.clone(),
                        display_name: None,
                    })
                    .await
                {
                    Ok(()) => {
                        ui::set_local_status(&self.ui, String::new());
                        // Flip the just-added file to "✓ added" in the
                        // search results without re-running the search
                        // (and keep that file selected when rows rebuild).
                        for result in &mut self.local_search {
                            if result.repo.eq_ignore_ascii_case(&repo) {
                                for (i, f) in result.files.iter_mut().enumerate() {
                                    if f.file.eq_ignore_ascii_case(&file) {
                                        f.added = true;
                                        result.recommended = i as u32;
                                    }
                                }
                            }
                        }
                        self.push_local_search(String::new());
                    }
                    Err(e) => ui::set_local_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_local();
            }
            UiCommand::LocalSearchFilters { gpu, cpu, large } => {
                self.local_search_fits = (gpu, cpu, large);
                self.push_local_search(String::new());
            }
            UiCommand::LocalSearch(query) => {
                ui::set_local_search(&self.ui, Vec::new(), "searching HuggingFace…".into());
                let client = self.client.clone();
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let result = client
                        .search_local_models(&query)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(UiCommand::LocalSearchLoaded(result));
                });
            }
            UiCommand::LocalSearchLoaded(result) => match result {
                Ok(results) => {
                    let status = if results.is_empty() {
                        "no repos with single-file GGUFs matched".to_string()
                    } else {
                        String::new()
                    };
                    self.local_search = results;
                    self.push_local_search(status);
                }
                Err(e) => {
                    self.local_search = Vec::new();
                    self.push_local_search(format!("search failed: {e}"));
                }
            },
            UiCommand::OpenPullRequests => {
                self.refresh_pr_dashboard();
                ui::set_center_screen(&self.ui, 5);
            }
            UiCommand::ClosePullRequests => {
                ui::set_center_screen(
                    &self.ui,
                    match self.new_chat {
                        None => 0,
                        Some(NewChat::Session) => 1,
                        Some(NewChat::Thread) => 2,
                    },
                );
            }
            UiCommand::RefreshPullRequests => self.refresh_pr_dashboard(),
            UiCommand::GithubRefreshTick => self.refresh_pr_dashboard(),
            UiCommand::PrDashRefreshFinished(workspace_id, result) => {
                self.pr_dash_loading.remove(&workspace_id);
                let succeeded = result.is_ok();
                match result {
                    Ok(()) => {
                        self.pr_dash_errors.remove(&workspace_id);
                    }
                    Err(error) => {
                        self.pr_dash_errors.insert(workspace_id, error);
                    }
                }
                // The command result owns progress/error state only; the PR
                // snapshot itself is folded exclusively from the SSE event.
                if succeeded {
                    self.sync_shared_prs(true);
                }
                self.push_pr_dashboard();
            }
            UiCommand::PrDashFilterPicked(index) => {
                let mut repositories: Vec<String> = self
                    .pr_dash
                    .values()
                    .flat_map(|list| list.prs.iter())
                    .map(|pr| format!("{}/{}", pr.host, pr.repository))
                    .collect();
                repositories.sort();
                repositories.dedup();
                self.pr_dash_filter = usize::try_from(index - 1)
                    .ok()
                    .and_then(|index| repositories.get(index).cloned());
                self.push_pr_dashboard();
            }
            UiCommand::PrGroupToggled(key) => {
                if !self.pr_collapsed.remove(&key) {
                    self.pr_collapsed.insert(key);
                }
                self.push_pr_dashboard();
            }
            UiCommand::PrGroupDropped {
                key,
                target_key,
                after,
            } => self.drop_pr_group(&key, &target_key, after),
            UiCommand::PrGroupMoved { key, offset } => self.move_pr_group(&key, offset),
            UiCommand::PrChatClicked {
                workspace_id,
                branch,
            } => {
                self.open_pr_chat(&workspace_id, &branch).await?;
            }
            UiCommand::OpenAutomations => {
                ui::set_automations_status(&self.ui, String::new());
                self.push_automations(); // last known list while the fetch runs
                self.refresh_automations();
                // Templates are a static catalog: fetch once, silently — the
                // screen just has no template section if this fails.
                if self.automation_templates.is_empty() {
                    let client = self.client.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        if let Ok(templates) = client.automation_templates().await {
                            let _ = tx.send(UiCommand::AutomationTemplatesLoaded(templates));
                        }
                    });
                }
                ui::set_center_screen(&self.ui, 4);
            }
            UiCommand::CloseAutomations => {
                ui::set_center_screen(
                    &self.ui,
                    match self.new_chat {
                        None => 0,
                        Some(NewChat::Session) => 1,
                        Some(NewChat::Thread) => 2,
                    },
                );
            }
            UiCommand::RefreshAutomations => self.refresh_automations(),
            UiCommand::AutomationsLoaded(result) => match result {
                Ok(automations) => {
                    self.automations = automations;
                    self.push_automations();
                }
                Err(e) => ui::set_automations_status(&self.ui, format!("loading failed: {e}")),
            },
            UiCommand::AutomationTemplatesLoaded(templates) => {
                self.automation_templates = templates;
                self.push_automation_templates();
            }
            UiCommand::SaveAutomation {
                id,
                name,
                prompt,
                workspace_id,
                kind,
                minute,
                time,
                days,
                permission_index,
                enabled,
            } => {
                let minute: u8 = match minute.trim().parse() {
                    Ok(m) if m <= 59 => m,
                    _ if kind == "hourly" => {
                        ui::set_automations_status(&self.ui, "minute must be 0-59".into());
                        return Ok(());
                    }
                    _ => 0,
                };
                let req = trouve_protocol::UpsertAutomationRequest {
                    name,
                    prompt,
                    workspace_id,
                    mode: None,
                    model: None,
                    permission_mode: match permission_index {
                        1 => PermissionMode::AllowList,
                        2 => PermissionMode::Yolo,
                        _ => PermissionMode::Ask,
                    },
                    schedule: trouve_protocol::AutomationSchedule {
                        kind,
                        minute,
                        time: time.trim().to_string(),
                        days: days
                            .split(',')
                            .filter_map(|d| d.trim().parse().ok())
                            .collect(),
                    },
                    enabled,
                };
                let result = if id.is_empty() {
                    self.client.create_automation(&req).await.map(|_| ())
                } else {
                    self.client.update_automation(&id, &req).await.map(|_| ())
                };
                match result {
                    Ok(()) => {
                        ui::set_automations_status(&self.ui, String::new());
                        ui::close_automation_form(&self.ui);
                        self.refresh_automations();
                    }
                    Err(e) => ui::set_automations_status(&self.ui, format!("{e:#}")),
                }
            }
            UiCommand::AutomationToggled(id, enabled) => {
                let Some(automation) = self.automations.iter().find(|a| a.id == id) else {
                    return Ok(());
                };
                let req = trouve_protocol::UpsertAutomationRequest {
                    name: automation.name.clone(),
                    prompt: automation.prompt.clone(),
                    workspace_id: automation.workspace_id.clone(),
                    mode: automation.mode.clone(),
                    model: automation.model.clone(),
                    permission_mode: automation.permission_mode,
                    schedule: automation.schedule.clone(),
                    enabled,
                };
                match self.client.update_automation(&id, &req).await {
                    Ok(_) => self.refresh_automations(),
                    Err(e) => ui::set_automations_status(&self.ui, format!("{e:#}")),
                }
            }
            UiCommand::RunAutomation(id) => match self.client.run_automation(&id).await {
                Ok(()) => ui::set_automations_status(
                    &self.ui,
                    "run started — a new session will appear in a moment".into(),
                ),
                Err(e) => ui::set_automations_status(&self.ui, format!("{e:#}")),
            },
            UiCommand::DeleteAutomation(id) => match self.client.delete_automation(&id).await {
                Ok(()) => self.refresh_automations(),
                Err(e) => ui::set_automations_status(&self.ui, format!("{e:#}")),
            },
            UiCommand::ServerEvent(envelope) => {
                self.handle_server_event(*envelope).await;
            }
            UiCommand::ConnectivityNoticeExpired(seq) => {
                if seq == self.connectivity_notice_seq {
                    ui::set_connectivity_notice(&self.ui, String::new());
                }
            }
            // The watchdog and the child watcher enqueue independently, so
            // a queued transition can be stale by the time it runs (a
            // Restored overtaken by a newer exit, a Lost overtaken by a
            // successful respawn). Both handlers revalidate against the
            // server before applying, so an outdated message can neither
            // unblock a dead server nor re-block a recovered one.
            UiCommand::ServerConnectionLost => {
                if self.client.info().await.is_err() {
                    self.server_unreachable = true;
                    self.clear_connectivity_notice();
                    self.push_connectivity();
                }
            }
            UiCommand::ServerConnectionRestored => {
                if self.client.info().await.is_ok() {
                    self.server_unreachable = false;
                    self.server_failed = false;
                    self.resync_after_reconnect("Reconnected to the trouve server.")
                        .await;
                }
            }
            UiCommand::ServerExited(status) => {
                self.handle_server_exited(&status).await;
            }
            UiCommand::CliInstall(id) => match self.client.start_cli_install(&id).await {
                Ok(()) => {
                    ui::set_settings_status(&self.ui, format!("installing {id}…"));
                    self.refresh_clis().await;
                    if id == "llama-server" {
                        self.refresh_local();
                    }
                    // Poll until the install settles, re-rendering the rows
                    // each tick so the progress bar moves.
                    let client = self.client.clone();
                    let settings_ui = self.ui.clone();
                    let tx = self.tx.clone();
                    tokio::spawn(async move {
                        for _ in 0..1200 {
                            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                            let Ok(status) = client.cli_install_status(&id).await else {
                                return;
                            };
                            match status.status.as_str() {
                                "pending" => {
                                    let _ = tx.send(UiCommand::RefreshClis);
                                    continue;
                                }
                                "success" => {
                                    ui::set_settings_status(
                                        &settings_ui,
                                        format!(
                                            "installed {id} {}",
                                            status.version.unwrap_or_default()
                                        ),
                                    );
                                }
                                // Cancelled installs clear back to "none".
                                "none" => {
                                    ui::set_settings_status(&settings_ui, String::new());
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
                            if id == "llama-server" {
                                let _ = tx.send(UiCommand::RefreshLocal);
                            }
                            return;
                        }
                    });
                }
                Err(e) => {
                    ui::set_settings_status(&self.ui, format!("{e:#}"));
                }
            },
            UiCommand::CliCancel(id) => {
                if let Err(e) = self.client.cancel_cli_install(&id).await {
                    ui::set_settings_status(&self.ui, format!("{e:#}"));
                }
                // The install task notices at its next chunk; the poll loop
                // (or the local section's poller) picks up the cleared state.
            }
            UiCommand::CliUninstall(id) => {
                match self.client.uninstall_cli(&id).await {
                    Ok(()) => ui::set_settings_status(&self.ui, format!("uninstalled {id}")),
                    Err(e) => ui::set_settings_status(&self.ui, format!("{e:#}")),
                }
                self.refresh_clis().await;
                if id == "llama-server" {
                    self.refresh_local();
                }
                // Backends may have fallen back to a PATH binary (or gone
                // away): refresh the model pickers.
                self.reload_catalogs().await;
            }
            UiCommand::RefreshClis => {
                self.refresh_clis().await;
            }
            UiCommand::Events(thread_id, envelopes, mark_unread) => {
                // A follower can be aborted while its already-queued
                // commands are still waiting in this channel.
                if !self.followed.contains(&thread_id) {
                    return Ok(());
                }
                let before = self
                    .vms
                    .get(&thread_id)
                    .map(thread_attention)
                    .unwrap_or_default();
                let mut changed = false;
                let mut completed = false;
                let mut finished = false;
                let mut failed = false;
                let mut todos_changed = false;
                for envelope in &envelopes {
                    todos_changed |= self.capture_todos(&thread_id, envelope);
                    changed |= self
                        .vms
                        .entry(thread_id.clone())
                        .or_default()
                        .apply(envelope)
                        .is_some();
                    completed |=
                        matches!(envelope.event, trouve_protocol::Event::TurnCompleted { .. });
                    finished |= matches!(
                        envelope.event,
                        trouve_protocol::Event::TurnCompleted { .. }
                            | trouve_protocol::Event::TurnFailed { .. }
                    );
                    failed |= matches!(envelope.event, trouve_protocol::Event::TurnFailed { .. });
                    self.maybe_notify(&thread_id, envelope);
                }
                let after = self
                    .vms
                    .get(&thread_id)
                    .map(thread_attention)
                    .unwrap_or_default();
                let attention_changed = self.update_session_attention(&thread_id, before, after);
                if self.current_thread_id().as_deref() == Some(&thread_id) {
                    self.push_context();
                    self.push_queue();
                    if todos_changed {
                        self.push_todos();
                    }
                    if changed {
                        self.render_chat(false);
                        self.last_delta_render = None;
                    }
                    self.follow_tail_for_open_attention(&thread_id);
                    if completed {
                        let _ = self.refresh_diff().await;
                        self.refresh_usage_text().await;
                    }
                }
                if completed {
                    self.refresh_subscriptions_after_turn();
                }
                let unread_changed = mark_unread
                    && finished
                    && self.mark_thread_unread_if_hidden(&thread_id, failed);
                if attention_changed || unread_changed {
                    self.push_nav();
                }
                if todos_changed {
                    self.push_threads();
                }
                self.push_agents_running();
            }
            UiCommand::Event(thread_id, envelope) => {
                if !self.followed.contains(&thread_id) {
                    return Ok(());
                }
                let completed =
                    matches!(envelope.event, trouve_protocol::Event::TurnCompleted { .. });
                let before = self
                    .vms
                    .get(&thread_id)
                    .map(thread_attention)
                    .unwrap_or_default();
                let todos_changed = self.capture_todos(&thread_id, &envelope);
                let changed = self
                    .vms
                    .entry(thread_id.clone())
                    .or_default()
                    .apply(&envelope);
                let after = self
                    .vms
                    .get(&thread_id)
                    .map(thread_attention)
                    .unwrap_or_default();
                let attention_changed = self.update_session_attention(&thread_id, before, after);
                let finished = matches!(
                    envelope.event,
                    trouve_protocol::Event::TurnCompleted { .. }
                        | trouve_protocol::Event::TurnFailed { .. }
                );
                let failed = matches!(envelope.event, trouve_protocol::Event::TurnFailed { .. });
                if self.current_thread_id().as_deref() == Some(&thread_id) {
                    // Compaction/usage state can change without a chat row
                    // changing, so the dial refreshes on every event.
                    self.push_context();
                    // Queue contents and the idle flag both ride the event
                    // stream (queue_updated / turn.started / turn ends).
                    self.push_queue();
                    if todos_changed {
                        self.push_todos();
                    }
                    if changed.is_some() {
                        // Coalesce streaming deltas: re-folding the whole
                        // transcript per token is O(n^2) over a turn. Render
                        // at most every 50ms for deltas; every other event
                        // (including the finalized assistant.message and
                        // turn.completed) renders immediately, so the last
                        // token is never left unshown.
                        let is_delta = matches!(
                            envelope.event,
                            trouve_protocol::Event::AssistantDelta { .. }
                                | trouve_protocol::Event::AssistantThinking { .. }
                        );
                        let now = std::time::Instant::now();
                        let throttled = is_delta
                            && self
                                .last_delta_render
                                .is_some_and(|t| now.duration_since(t).as_millis() < 50);
                        if !throttled {
                            self.render_chat(false);
                            self.last_delta_render = if is_delta { Some(now) } else { None };
                        }
                    }
                    self.follow_tail_for_open_attention(&thread_id);
                    if completed {
                        let _ = self.refresh_diff().await;
                        self.refresh_usage_text().await;
                    }
                }
                if todos_changed {
                    self.push_threads();
                }
                // Subscription windows are provider-side state, so the
                // completed-turn event is the best point to replace the
                // startup snapshot without polling continuously.
                if completed {
                    self.refresh_subscriptions_after_turn();
                }
                self.maybe_notify(&thread_id, &envelope);
                let mut nav_changed = attention_changed;
                if finished {
                    nav_changed |= self.mark_thread_unread_if_hidden(&thread_id, failed);
                }
                if nav_changed {
                    self.push_nav();
                }
                self.push_agents_running();
            }
            UiCommand::NotifyPrefsChanged(prefs) => self.notify = prefs,
            UiCommand::WindowFocusChanged(focused) => {
                if focused
                    && let Some(session_id) = self.current_session_id()
                    && (self.unread_sessions.remove(&session_id)
                        | self.error_sessions.remove(&session_id))
                {
                    self.push_nav();
                }
            }
            UiCommand::NotifyTest => {
                crate::notify::show(
                    crate::notify::Toast {
                        summary: "Test notification".into(),
                        body: "Notifications are working.".into(),
                        sound: self.notify.sound,
                        session_id: self.current_session_id().unwrap_or_default(),
                        thread_id: self.current_thread_id().unwrap_or_default(),
                    },
                    self.tx.clone(),
                );
            }
            UiCommand::NotificationActivated {
                session_id,
                thread_id,
            } => {
                ui::raise_window(&self.ui);
                if !session_id.is_empty()
                    && self.current_session_id() != Some(session_id.clone())
                    && let Some(i) = self.sessions.iter().position(|s| s.id == session_id)
                {
                    self.select_session(i).await?;
                }
                if let Some(i) = self.threads.iter().position(|t| t.id == thread_id) {
                    // Notifications point at newest actionable state
                    // (approval/question/completion), so they always reveal
                    // the tail even when this is already the open thread.
                    self.open_thread_index(i, true);
                }
            }
            UiCommand::SessionThreadsLoaded(session_id, result) => match result {
                Ok(threads) => {
                    if !self.watched_sessions.contains(&session_id) {
                        return Ok(());
                    }
                    for thread in threads {
                        self.follow_thread(thread.id, session_id.clone());
                    }
                }
                Err(e) => {
                    self.watched_sessions.remove(&session_id);
                    tracing::warn!("watching session {session_id} threads: {e}");
                }
            },
            UiCommand::QuitWhenIdle => {
                self.quit_when_idle
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                ui::set_quit_when_idle(&self.ui, true);
                self.push_agents_running();
            }
            UiCommand::CancelQuitWhenIdle => {
                self.quit_when_idle
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                ui::finish_cancel_quit_when_idle(&self.ui);
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
        let Some(thread_id) = self.threads.get(index).map(|t| t.id.clone()) else {
            return;
        };
        match self.client.update_thread(&thread_id, &req).await {
            Ok(thread) => {
                if let Some(slot) = self.threads.get_mut(index) {
                    *slot = thread;
                }
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

    async fn create_thread(
        &mut self,
        mode_idx: usize,
        model_idx: usize,
        model_options: serde_json::Map<String, serde_json::Value>,
        permission_mode: Option<PermissionMode>,
    ) -> Result<()> {
        let Some(session_id) = self.current_session_id() else {
            return Ok(());
        };
        let thread = self
            .client
            .create_thread(&CreateThreadRequest {
                session_id,
                mode: self.modes.get(mode_idx).map(|m| m.id.clone()),
                model: self.models.get(model_idx).map(|m| m.id.clone()),
                model_options,
                permission_mode,
            })
            .await?;
        self.thread_sessions
            .insert(thread.id.clone(), thread.session_id.clone());
        self.threads.push(thread);
        self.current_thread = Some(self.threads.len() - 1);
        self.push_threads();
        self.push_picker_indices();
        self.follow_current();
        self.render_chat(false);
        self.push_context();
        self.push_queue();
        self.push_todos();
        self.remember_position();
        self.apply_scroll_intent(true);
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
        "on" => "On".into(),
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

/// Resolve the model a new thread should display and submit. Configured
/// defaults that are absent from the runnable catalog are skipped so the
/// form remains usable when provider availability changes.
fn preferred_model_index(
    models: &[ModelInfo],
    mode_default: Option<&str>,
    global_default: Option<&str>,
) -> i32 {
    [mode_default, global_default]
        .into_iter()
        .flatten()
        .find_map(|preferred| {
            models
                .iter()
                .position(|model| model.id == preferred)
                .map(|index| index as i32)
        })
        .unwrap_or(if models.is_empty() { -1 } else { 0 })
}

/// Resolve the thinking selection with the same inheritance as thread
/// creation, followed by the model schema's own default.
fn preferred_thinking_index(
    values: &[String],
    mode_default: Option<&str>,
    global_default: Option<&str>,
    schema_default: Option<&str>,
) -> i32 {
    [mode_default.or(global_default), schema_default]
        .into_iter()
        .flatten()
        .find_map(|preferred| {
            values
                .iter()
                .position(|value| value == preferred)
                .map(|index| index as i32)
        })
        .unwrap_or(if values.is_empty() { -1 } else { 0 })
}

/// "512 B" / "34 KB" / "2.4 MB", for attachment chip labels.
fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
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
    crate::opener::open(url);
}

/// One line per check run: status glyph, name, conclusion.
fn format_checks(checks: &[trouve_protocol::CheckRun]) -> String {
    checks
        .iter()
        .map(|c| match c.conclusion.as_deref() {
            Some("success") => format!("✓ {}", c.name),
            Some(conclusion @ ("failure" | "timed_out" | "startup_failure")) => {
                format!("✗ {} — {conclusion}", c.name)
            }
            Some(conclusion) => format!("• {} — {conclusion}", c.name),
            None => format!("… {} — {}", c.name, c.status),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line per review: reviewer and their verdict.
/// Settings-picker index for an optional permission mode: -1 = global
/// default, then ask / allow-list / yolo.
fn permission_index_of(mode: Option<PermissionMode>) -> i32 {
    match mode {
        None => -1,
        Some(PermissionMode::Ask) => 0,
        Some(PermissionMode::AllowList) => 1,
        Some(PermissionMode::Yolo) => 2,
    }
}

fn permission_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::AllowList => "Allow list",
        PermissionMode::Yolo => "Yolo",
    }
}

/// Inverse of [`permission_index_of`]; out-of-range means global default.
fn permission_mode_of(index: i32) -> Option<PermissionMode> {
    match index {
        0 => Some(PermissionMode::Ask),
        1 => Some(PermissionMode::AllowList),
        2 => Some(PermissionMode::Yolo),
        _ => None,
    }
}

/// Compact attention kind + accessible hover/focus detail.
fn attention_badge(approvals: usize, questions: usize) -> (i32, String) {
    let plural = |count: usize, singular: &str, plural: &str| {
        format!("{count} {}", if count == 1 { singular } else { plural })
    };
    match (approvals, questions) {
        (0, 0) => (0, String::new()),
        (a, 0) => (1, format!("{} pending", plural(a, "approval", "approvals"))),
        (0, q) => (
            2,
            format!("{} awaiting an answer", plural(q, "question", "questions")),
        ),
        (a, q) => (
            3,
            format!(
                "{} and {} need attention",
                plural(a, "approval", "approvals"),
                plural(q, "question", "questions")
            ),
        ),
    }
}

/// Pull-request badge kind + detail. A single PR inherits its status color;
/// multiple PRs intentionally collapse to the neutral kind while the tooltip
/// preserves each individual status.
fn pr_badge(prs: &[trouve_protocol::PrInfo]) -> (i32, String) {
    if prs.is_empty() {
        return (0, String::new());
    }
    let state = |pr: &trouve_protocol::PrInfo| {
        if pr.draft {
            "Draft"
        } else {
            match pr.state.as_str() {
                "open" => "Open",
                "merged" => "Merged",
                "closed" => "Closed",
                _ => "Pull request",
            }
        }
    };
    let lines = prs
        .iter()
        .map(|pr| format!("#{} · {}", pr.number, state(pr)))
        .collect::<Vec<_>>()
        .join("\n");
    if prs.len() > 1 {
        return (5, format!("{} pull requests\n{lines}", prs.len()));
    }
    let pr = &prs[0];
    let kind = if pr.draft {
        2
    } else {
        match pr.state.as_str() {
            "open" => 1,
            "merged" => 3,
            "closed" => 4,
            _ => 5,
        }
    };
    (kind, format!("Pull request\n{lines}"))
}

fn format_reviews(reviews: &[trouve_protocol::PrReview]) -> String {
    reviews
        .iter()
        .map(|r| format!("{} — {}", r.reviewer, r.state))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Command + args as one shell-quoted line (round-trips through the MCP
/// edit form and `parse_mcp_form`).
/// Human-readable size in decimal gigabytes ("4.7 GB").
fn human_gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

fn human_mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

/// Human summary of an automation schedule ("Hourly at :15",
/// "Daily at 09:00", "Mon, Wed at 09:00"). Mirrors the server's model.
fn schedule_summary(s: &trouve_protocol::AutomationSchedule) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    match s.kind.as_str() {
        "hourly" => format!("Hourly at :{:02}", s.minute),
        "daily" => format!("Daily at {}", s.time),
        "weekly" => {
            let mut days: Vec<u8> = s.days.clone();
            days.sort_unstable();
            days.dedup();
            if days.len() == 7 {
                return format!("Daily at {}", s.time);
            }
            let names: Vec<&str> = days
                .iter()
                .filter_map(|d| DAYS.get(*d as usize).copied())
                .collect();
            format!("{} at {}", names.join(", "), s.time)
        }
        other => other.to_string(),
    }
}

/// RFC3339 → "Jul 13 09:00" in this machine's time zone.
fn fmt_local_ts(rfc: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(rfc).ok().map(|t| {
        t.with_timezone(&chrono::Local)
            .format("%b %d %H:%M")
            .to_string()
    })
}

/// "1.2M" / "180k" / "42" for download/like counts.
fn human_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// Compact and expanded presentations of one provider's subscription state.
/// The compact row deliberately uses only the highest reported percentage:
/// it is the window closest to its cap, not a promise that requests below
/// 100% will succeed.
fn model_health_view(health: &trouve_protocol::SubscriptionHealth) -> ui::ModelHealthView {
    let plan = display_plan(&health.plan);
    let constrained = health
        .windows
        .iter()
        .max_by_key(|window| window.used_percent);
    let note_lower = health.note.to_ascii_lowercase();

    let (summary, tone) = match health.status.as_str() {
        "ok" => {
            if let Some(window) = constrained {
                let pct = window.used_percent.clamp(0, 100);
                let summary = if plan.is_empty() {
                    format!("{pct}% used")
                } else {
                    format!("{plan} · {pct}% used")
                };
                let tone = if pct >= 90 {
                    3
                } else if pct >= 70 {
                    2
                } else {
                    1
                };
                (summary, tone)
            } else if !plan.is_empty() {
                (plan.clone(), 1)
            } else if !health.credits.is_empty() {
                (health.credits.clone(), 1)
            } else {
                ("usage available".into(), 1)
            }
        }
        "unavailable" => {
            let label = if note_lower.contains("login") || note_lower.contains("logged in") {
                "login required"
            } else {
                "usage unavailable"
            };
            (label.into(), 3)
        }
        "unsupported" => {
            let label = if note_lower.contains("api key") || note_lower.contains("usage-billed") {
                "API billed"
            } else {
                "usage unavailable"
            };
            (label.into(), 0)
        }
        _ => ("usage unavailable".into(), 0),
    };

    let title = if plan.is_empty() {
        health.provider_id.clone()
    } else {
        format!("{} · {plan}", health.provider_id)
    };
    let mut lines = vec![title];
    if !health.windows.is_empty() {
        lines.push(String::new());
        lines.extend(health.windows.iter().map(|window| {
            let pct = window.used_percent.clamp(0, 100);
            if window.resets.is_empty() {
                format!("{}: {pct}% used", window.label)
            } else {
                format!("{}: {pct}% used · {}", window.label, window.resets)
            }
        }));
    }
    if !health.credits.is_empty() {
        lines.push(health.credits.clone());
    }
    if !health.note.is_empty() {
        lines.push(String::new());
        lines.push(health.note.clone());
    }
    if health.status == "ok" {
        lines.push(String::new());
        lines.push(
            "Highest reported usage is shown in the picker. Provider limits may change before the next refresh."
                .into(),
        );
    }

    ui::ModelHealthView {
        summary,
        detail: lines.join("\n"),
        tone,
    }
}

fn display_plan(plan: &str) -> String {
    let mut chars = plan.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().chain(chars).collect()
}

/// Download status line + bar percent: "label… 45 MB / 120 MB (37%)", or
/// just the received count with -1 (no bar) when the total is unknown.
fn download_progress(label: &str, received: u64, total: u64, rate: Option<f64>) -> (String, i32) {
    let speed = rate.map(human_rate).unwrap_or_default();
    if total == 0 {
        return (format!("{label}… {}{speed}", human_mb(received)), -1);
    }
    let pct = ((received * 100) / total).min(100) as i32;
    (
        format!(
            "{label}… {} / {} ({pct}%){speed}",
            human_mb(received),
            human_mb(total)
        ),
        pct,
    )
}

/// " · 12.3 MB/s" — transfer-rate suffix for download status lines.
fn human_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1e6 {
        format!(" · {:.1} MB/s", bytes_per_sec / 1e6)
    } else if bytes_per_sec >= 1e3 {
        format!(" · {:.0} kB/s", bytes_per_sec / 1e3)
    } else {
        format!(" · {bytes_per_sec:.0} B/s")
    }
}

fn render_command_line(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|part| {
            shlex::try_quote(part)
                .map(|q| q.into_owned())
                .unwrap_or_else(|_| part.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split the MCP form's command line and KEY=VALUE env block.
#[allow(clippy::type_complexity)]
fn parse_mcp_form(
    command_line: &str,
    env_lines: &str,
) -> Result<
    (
        String,
        Vec<String>,
        std::collections::BTreeMap<String, String>,
    ),
    String,
> {
    let mut parts = shlex::split(command_line)
        .ok_or_else(|| "command line has unbalanced quotes".to_string())?;
    if parts.is_empty() {
        return Err("command is required".to_string());
    }
    let command = parts.remove(0);
    let mut env = std::collections::BTreeMap::new();
    for line in env_lines.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("environment line '{line}' is not KEY=VALUE"))?;
        if key.trim().is_empty() {
            return Err(format!("environment line '{line}' has an empty key"));
        }
        env.insert(key.trim().to_string(), value.to_string());
    }
    Ok((command, parts, env))
}

/// Drop stale/duplicate ids from a saved order and append newly registered
/// workspaces in the order returned by the server. Returns true if the order
/// was modified.
fn reconcile_workspace_order(order: &mut Vec<String>, workspaces: &[Workspace]) -> bool {
    let live: HashSet<&str> = workspaces.iter().map(|ws| ws.id.as_str()).collect();
    let mut seen = HashSet::new();
    let original_len = order.len();
    order.retain(|id| live.contains(id.as_str()) && seen.insert(id.clone()));
    let removed_or_deduped = order.len() != original_len;
    let mut added = false;
    for workspace in workspaces {
        if seen.insert(workspace.id.clone()) {
            order.push(workspace.id.clone());
            added = true;
        }
    }
    removed_or_deduped || added
}

/// Move `id` immediately before/after `target_id` in an ordered id list
/// (workspace sidebar order, PR dashboard group order).
fn reorder_id(order: &mut Vec<String>, id: &str, target_id: &str, after: bool) -> bool {
    if id == target_id {
        return false;
    }
    let Some(source) = order.iter().position(|entry| entry == id) else {
        return false;
    };
    if !order.iter().any(|entry| entry == target_id) {
        return false;
    }

    let moved = order.remove(source);
    let target = order
        .iter()
        .position(|entry| entry == target_id)
        .expect("target was validated before removing a different id");
    let destination = target + usize::from(after);
    order.insert(destination, moved);
    source != destination
}

/// Static definition of one PR dashboard group.
struct PrGroupDef {
    key: &'static str,
    title: &'static str,
    description: &'static str,
    icon: &'static str,
    /// Icon tint (see PrGroupItem.kind in the Slint screen).
    kind: i32,
    empty: &'static str,
}

/// The dashboard's groups in default order. `key` is the stable id the
/// persisted order, collapse state, and reorder callbacks use.
const PR_GROUPS: [PrGroupDef; 7] = [
    PrGroupDef {
        key: "review-requested",
        title: "Review Requested",
        description: "Pull requests where your review has been requested.",
        icon: "◉",
        kind: 0,
        empty: "No reviews waiting on you.",
    },
    PrGroupDef {
        key: "drafts",
        title: "Drafts",
        description: "Open pull requests still marked as drafts.",
        icon: "✎",
        kind: 1,
        empty: "No draft pull requests right now.",
    },
    PrGroupDef {
        key: "needs-reviewers",
        title: "Needs Reviewers",
        description: "Open pull requests that do not have any reviewers yet.",
        icon: "＋",
        kind: 2,
        empty: "Every open pull request has a reviewer.",
    },
    PrGroupDef {
        key: "pending-review",
        title: "Pending Review",
        description: "Open pull requests waiting for review or approval.",
        icon: "◐",
        kind: 2,
        empty: "Nothing is waiting on review.",
    },
    PrGroupDef {
        key: "ready-to-merge",
        title: "Ready to Merge",
        description: "Fully approved pull requests with every check passing.",
        icon: "✓",
        kind: 3,
        empty: "Nothing is ready to merge yet.",
    },
    PrGroupDef {
        key: "needs-attention",
        title: "Needs Attention",
        description: "Pull requests with merge conflicts, failing checks, or changes requested.",
        icon: "⚠",
        kind: 4,
        empty: "Nothing needs attention — all clear.",
    },
    PrGroupDef {
        key: "recently-merged",
        title: "Recently Merged",
        description: "Pull requests merged in the last 24 hours.",
        icon: "⇥",
        kind: 5,
        empty: "Nothing merged in the last 24 hours.",
    },
];

/// Drop unknown keys from a saved dashboard group order and append any
/// missing groups in canonical order. Returns true if the order changed.
fn reconcile_pr_group_order(order: &mut Vec<String>) -> bool {
    let mut seen = HashSet::new();
    let original_len = order.len();
    order.retain(|key| PR_GROUPS.iter().any(|d| d.key == key) && seen.insert(key.clone()));
    let removed = order.len() != original_len;
    let mut added = false;
    for def in &PR_GROUPS {
        if seen.insert(def.key.to_string()) {
            order.push(def.key.to_string());
            added = true;
        }
    }
    removed || added
}

/// Review states arrive as octocrab's lowercased Debug names, so
/// ChangesRequested shows up as "changesrequested".
fn normalized_review_state(state: &str) -> &'static str {
    match state {
        "approved" => "approved",
        "changesrequested" | "changes_requested" => "changes_requested",
        "dismissed" => "dismissed",
        _ => "other",
    }
}

/// Latest verdict per reviewer: true = approved, false = changes
/// requested. Comments don't change a verdict; a dismissal clears it.
fn review_verdicts(reviews: &[trouve_protocol::PrReview]) -> HashMap<&str, bool> {
    let mut latest: HashMap<&str, bool> = HashMap::new();
    for review in reviews {
        match normalized_review_state(&review.state) {
            "approved" => {
                latest.insert(&review.reviewer, true);
            }
            "changes_requested" => {
                latest.insert(&review.reviewer, false);
            }
            "dismissed" => {
                latest.remove(review.reviewer.as_str());
            }
            _ => {}
        }
    }
    latest
}

/// Check pill: (kind, label) with kind 0 no checks / 1 passing /
/// 2 running / 3 failing.
fn check_pill(checks: &[trouve_protocol::CheckRun]) -> (i32, &'static str) {
    if checks.is_empty() {
        return (0, "no checks");
    }
    let failing = checks.iter().any(|c| {
        matches!(
            c.conclusion.as_deref(),
            Some(
                "failure"
                    | "timed_out"
                    | "cancelled"
                    | "action_required"
                    | "startup_failure"
                    | "stale"
            )
        )
    });
    if failing {
        return (3, "checks failing");
    }
    if checks
        .iter()
        .any(|c| c.status != "completed" || c.conclusion.is_none())
    {
        return (2, "checks running");
    }
    (1, "checks passing")
}

/// Merge pill: (kind, label) with kind 0 unknown / 1 clean / 3 conflicts.
/// Merged and closed PRs get an empty label (the row hides the pill) —
/// mergeability only means something while the PR is open.
fn merge_pill(pr: &trouve_protocol::PrInfo) -> (i32, &'static str) {
    if pr.state != "open" {
        return (0, "");
    }
    match pr.mergeable {
        Some(true) => (1, "no conflicts"),
        Some(false) => (3, "merge conflicts"),
        None => (0, "merge unknown"),
    }
}

/// Approval pill: (kind, label) with kind 0 no reviews / 1 approved /
/// 2 pending / 3 changes requested. "Approved" means at least one
/// approval, no changes requested, and no outstanding review requests.
fn approval_pill(pr: &trouve_protocol::PrInfo) -> (i32, &'static str) {
    let verdicts = review_verdicts(&pr.reviews);
    let approvals = verdicts.values().filter(|approved| **approved).count();
    if verdicts.values().any(|approved| !approved) {
        (3, "changes requested")
    } else if approvals > 0 && pr.requested_reviewers.is_empty() {
        (1, "approved")
    } else if approvals > 0 || !pr.requested_reviewers.is_empty() {
        (2, "review pending")
    } else {
        (0, "no reviews")
    }
}

/// Which dashboard group a PR lands in (None = not shown). Merged PRs
/// first, then merge conflicts (except on drafts), then the viewer's
/// own review inbox, then drafts, then the remaining problem states —
/// each PR appears in exactly one group.
fn classify_pr(
    pr: &trouve_protocol::PrInfo,
    viewer: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<&'static str> {
    if pr.state == "merged" {
        let recent = pr
            .merged_at
            .is_some_and(|at| now.signed_duration_since(at) <= chrono::Duration::hours(24));
        return recent.then_some("recently-merged");
    }
    if pr.state != "open" {
        return None;
    }
    // Conflicts must be resolved before anything else can happen to the
    // PR, so an unmergeable one needs attention — even one sitting in
    // the viewer's review inbox. Drafts are exempt: they aren't going
    // anywhere yet, so they stay in the drafts group (their merge pill
    // still shows the conflict).
    if pr.mergeable == Some(false) && !pr.draft {
        return Some("needs-attention");
    }
    if !viewer.is_empty() && pr.requested_reviewers.iter().any(|r| r == viewer) {
        return Some("review-requested");
    }
    if pr.draft {
        return Some("drafts");
    }
    let (check_kind, _) = check_pill(&pr.checks);
    let (approval_kind, _) = approval_pill(pr);
    if check_kind == 3 || approval_kind == 3 {
        return Some("needs-attention");
    }
    if approval_kind == 1 && check_kind == 1 {
        return Some("ready-to-merge");
    }
    if pr.requested_reviewers.is_empty() && pr.reviews.is_empty() {
        return Some("needs-reviewers");
    }
    // The account feed already limits the dashboard to PRs relevant to the
    // viewer. Any remaining open PR is waiting for an assigned review or
    // approval.
    Some("pending-review")
}

/// Coarse relative age for the dashboard's comment timestamps
/// ("just now", "45 mins ago", "1 day ago").
fn human_age(from: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let mins = now.signed_duration_since(from).num_minutes().max(0);
    match mins {
        0 => "just now".into(),
        1 => "1 min ago".into(),
        m if m < 60 => format!("{m} mins ago"),
        m if m < 120 => "1 hour ago".into(),
        m if m < 60 * 24 => format!("{} hours ago", m / 60),
        m if m < 60 * 48 => "1 day ago".into(),
        m => format!("{} days ago", m / (60 * 24)),
    }
}

/// Navigation policy independent of whether an idle bookmark exists.
/// Queued prompts count as tail attention even when the thread is not
/// currently running (for example, after a restart or a failed turn).
fn should_open_chat_at_tail(force_tail: bool, turn_running: bool, has_queue: bool) -> bool {
    force_tail || turn_running || has_queue
}

#[cfg(test)]
mod tests {
    use super::{
        SubscriptionRefresh, SubscriptionRefreshState, approval_pill, attention_badge, check_pill,
        classify_pr, download_progress, human_age, human_rate, merge_pill, model_health_view,
        pr_badge, preferred_model_index, preferred_thinking_index, project_session_prs,
        reconcile_pr_group_order, reconcile_workspace_order, reorder_id, should_open_chat_at_tail,
        thinking_property,
    };
    use chrono::{Duration, TimeZone, Utc};
    use trouve_protocol::{
        CheckRun, ModelInfo, PrInfo, PrReview, Session, SubscriptionHealth, SubscriptionWindow,
        Workspace,
    };

    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            context_window: 1,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({}),
        }
    }

    #[test]
    fn new_chat_model_uses_mode_then_global_default() {
        let models = vec![
            model("first/model"),
            model("global/model"),
            model("mode/model"),
        ];

        assert_eq!(
            preferred_model_index(&models, Some("mode/model"), Some("global/model")),
            2
        );
        assert_eq!(
            preferred_model_index(&models, None, Some("global/model")),
            1
        );
        assert_eq!(
            preferred_model_index(&models, Some("missing/model"), Some("global/model")),
            1
        );
        assert_eq!(
            preferred_model_index(&models, Some("missing/model"), Some("also/missing")),
            0
        );
        assert_eq!(preferred_model_index(&[], None, None), -1);
    }

    #[test]
    fn new_chat_thinking_uses_inherited_then_schema_default() {
        let values = vec!["low".into(), "medium".into(), "high".into()];

        assert_eq!(
            preferred_thinking_index(&values, Some("high"), Some("low"), Some("medium")),
            2
        );
        assert_eq!(
            preferred_thinking_index(&values, None, Some("low"), Some("medium")),
            0
        );
        // An unsupported mode override takes precedence over the global
        // setting, then resolves to the model's own default like the core.
        assert_eq!(
            preferred_thinking_index(&values, Some("xhigh"), Some("high"), Some("medium")),
            1
        );
        assert_eq!(preferred_thinking_index(&[], None, None, None), -1);
    }

    fn workspaces(ids: &[&str]) -> Vec<Workspace> {
        ids.iter()
            .map(|id| Workspace {
                id: (*id).into(),
                name: (*id).into(),
                path: format!("/{id}"),
            })
            .collect()
    }

    #[test]
    fn saved_workspace_order_keeps_new_workspaces_at_the_end() {
        let list = workspaces(&["a", "b", "c", "d"]);
        let mut order = vec!["c".into(), "a".into(), "missing".into(), "c".into()];
        let changed = reconcile_workspace_order(&mut order, &list);
        assert_eq!(order, ["c", "a", "b", "d"]);
        assert!(changed); // Removed stale "missing" and duplicate "c", added "b" and "d"

        // Reconciling again with the same list should not change anything
        let mut order2 = vec!["c".into(), "a".into(), "b".into(), "d".into()];
        let changed2 = reconcile_workspace_order(&mut order2, &list);
        assert_eq!(order2, ["c", "a", "b", "d"]);
        assert!(!changed2); // No changes when order already matches
    }

    #[test]
    fn workspace_reorder_supports_before_and_after_drops() {
        let mut order = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        assert!(reorder_id(&mut order, "a", "c", true));
        assert_eq!(order, ["b", "c", "a", "d"]);
        assert!(reorder_id(&mut order, "d", "b", false));
        assert_eq!(order, ["d", "b", "c", "a"]);
        assert!(!reorder_id(&mut order, "d", "d", false));
    }

    #[test]
    fn pr_group_order_reconciles_saved_keys() {
        let mut order = vec![
            "ready-to-merge".into(),
            "missing".into(),
            "drafts".into(),
            "ready-to-merge".into(),
        ];
        assert!(reconcile_pr_group_order(&mut order));
        assert_eq!(
            order,
            [
                "ready-to-merge",
                "drafts",
                "review-requested",
                "needs-reviewers",
                "pending-review",
                "needs-attention",
                "recently-merged",
            ]
        );
        assert!(!reconcile_pr_group_order(&mut order));
    }

    fn pr() -> PrInfo {
        PrInfo {
            host: "github.com".into(),
            repository: "acme/app".into(),
            workspace_id: "ws_1".into(),
            number: 42,
            url: "https://github.com/acme/app/pull/42".into(),
            title: "Make it better".into(),
            state: "open".into(),
            draft: false,
            base: "main".into(),
            head: "feature".into(),
            checks: Vec::new(),
            reviews: Vec::new(),
            author: "author".into(),
            requested_reviewers: Vec::new(),
            comments: 0,
            last_comment_at: None,
            mergeable: None,
            merged_at: None,
        }
    }

    #[test]
    fn session_projection_keeps_only_branch_or_authoritative_associations() {
        let session = Session {
            id: "se_1".into(),
            workspace_id: "ws_1".into(),
            title: "session".into(),
            branch: "trouve/session".into(),
            worktree_path: "/tmp/session".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: Utc::now(),
        };

        let mut exact = pr();
        exact.number = 41;
        exact.head = session.branch.clone();

        let mut unrelated = pr();
        unrelated.number = 42;
        unrelated.head = "someone-elses-work".into();

        let mut associated = pr();
        associated.number = 43;
        associated.head = "created-through-gh".into();
        associated.title = "fresh dashboard details".into();

        let mut cached_associated = associated.clone();
        cached_associated.title = "older targeted details".into();
        let mut cached_missing = pr();
        cached_missing.number = 44;
        cached_missing.head = "created-through-graphql".into();

        let projected = project_session_prs(
            &session,
            [&exact, &unrelated, &associated],
            &[cached_associated, cached_missing],
        );
        assert_eq!(
            projected.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![44, 43, 41]
        );
        assert_eq!(projected[1].title, "fresh dashboard details");
        assert!(!projected.iter().any(|pr| pr.number == unrelated.number));
    }

    fn passing_check() -> CheckRun {
        CheckRun {
            name: "test".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
        }
    }

    #[test]
    fn pr_dashboard_classifies_each_actionable_state() {
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();

        let mut review_requested = pr();
        review_requested.requested_reviewers.push("viewer".into());
        assert_eq!(
            classify_pr(&review_requested, "viewer", now),
            Some("review-requested")
        );

        let mut draft = pr();
        draft.draft = true;
        assert_eq!(classify_pr(&draft, "viewer", now), Some("drafts"));

        let mut pending = pr();
        pending.requested_reviewers.push("reviewer".into());
        assert_eq!(classify_pr(&pending, "viewer", now), Some("pending-review"));

        let mut ready = pr();
        ready.checks.push(passing_check());
        ready.reviews.push(PrReview {
            reviewer: "reviewer".into(),
            state: "approved".into(),
        });
        assert_eq!(classify_pr(&ready, "viewer", now), Some("ready-to-merge"));

        let mut failing = pr();
        failing.checks.push(CheckRun {
            name: "test".into(),
            status: "completed".into(),
            conclusion: Some("failure".into()),
        });
        assert_eq!(
            classify_pr(&failing, "viewer", now),
            Some("needs-attention")
        );

        let mut merged = pr();
        merged.state = "merged".into();
        merged.merged_at = Some(now - Duration::hours(23));
        assert_eq!(classify_pr(&merged, "viewer", now), Some("recently-merged"));
        merged.merged_at = Some(now - Duration::hours(25));
        assert_eq!(classify_pr(&merged, "viewer", now), None);

        // A clean, passing PR with nobody assigned yet needs reviewers
        // (PR #101).
        let mut unassigned = pr();
        unassigned.mergeable = Some(true);
        unassigned.checks.push(passing_check());
        assert_eq!(
            classify_pr(&unassigned, "viewer", now),
            Some("needs-reviewers")
        );
    }

    #[test]
    fn ready_to_merge_requires_approval_and_passing_checks() {
        let mut approved = pr();
        approved.reviews.push(PrReview {
            reviewer: "reviewer".into(),
            state: "approved".into(),
        });
        assert_eq!(approval_pill(&approved), (1, "approved"));
        assert_eq!(check_pill(&approved.checks), (0, "no checks"));

        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        assert_eq!(
            classify_pr(&approved, "viewer", now),
            Some("pending-review")
        );
        approved.checks.push(passing_check());
        assert_eq!(
            classify_pr(&approved, "viewer", now),
            Some("ready-to-merge")
        );

        approved.checks[0].conclusion = Some("stale".into());
        assert_eq!(check_pill(&approved.checks), (3, "checks failing"));
    }

    #[test]
    fn changes_requested_wins_over_an_earlier_approval() {
        let mut reviewed = pr();
        reviewed.reviews.extend([
            PrReview {
                reviewer: "reviewer".into(),
                state: "approved".into(),
            },
            PrReview {
                reviewer: "reviewer".into(),
                state: "changesrequested".into(),
            },
        ]);
        assert_eq!(approval_pill(&reviewed), (3, "changes requested"));
    }

    #[test]
    fn merge_pill_reflects_mergeability_and_hides_after_merge() {
        let mut conflicted = pr();
        assert_eq!(merge_pill(&conflicted), (0, "merge unknown"));
        conflicted.mergeable = Some(true);
        assert_eq!(merge_pill(&conflicted), (1, "no conflicts"));
        conflicted.mergeable = Some(false);
        assert_eq!(merge_pill(&conflicted), (3, "merge conflicts"));

        let mut merged = pr();
        merged.state = "merged".into();
        assert_eq!(merge_pill(&merged), (0, ""));
    }

    #[test]
    fn unmergeable_prs_need_attention_unless_drafted() {
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();

        // Even a PR that would otherwise be ready to merge or one in the
        // viewer's review inbox lands in needs-attention.
        let mut ready = pr();
        ready.checks.push(passing_check());
        ready.reviews.push(PrReview {
            reviewer: "reviewer".into(),
            state: "approved".into(),
        });
        ready.mergeable = Some(false);
        assert_eq!(classify_pr(&ready, "viewer", now), Some("needs-attention"));

        // Drafts stay drafts — the merge pill carries the conflict.
        let mut draft = pr();
        draft.draft = true;
        draft.mergeable = Some(false);
        assert_eq!(classify_pr(&draft, "viewer", now), Some("drafts"));

        let mut review_requested = pr();
        review_requested.requested_reviewers.push("viewer".into());
        review_requested.mergeable = Some(false);
        assert_eq!(
            classify_pr(&review_requested, "viewer", now),
            Some("needs-attention")
        );

        // A cleanly mergeable PR keeps its usual group.
        let mut clean = pr();
        clean.mergeable = Some(true);
        clean.requested_reviewers.push("reviewer".into());
        assert_eq!(classify_pr(&clean, "viewer", now), Some("pending-review"));
    }

    #[test]
    fn relative_comment_age_uses_approachable_units() {
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        assert_eq!(human_age(now - Duration::minutes(45), now), "45 mins ago");
        assert_eq!(human_age(now - Duration::hours(24), now), "1 day ago");
    }
    fn nav_pr(number: u64, state: &str, draft: bool) -> trouve_protocol::PrInfo {
        trouve_protocol::PrInfo {
            host: "github.com".into(),
            repository: "acme/app".into(),
            workspace_id: String::new(),
            number,
            url: String::new(),
            title: format!("PR {number}"),
            state: state.into(),
            draft,
            base: "main".into(),
            head: "feature".into(),
            checks: Vec::new(),
            reviews: Vec::new(),
            author: String::new(),
            requested_reviewers: Vec::new(),
            comments: 0,
            last_comment_at: None,
            mergeable: None,
            merged_at: None,
        }
    }

    #[test]
    fn pr_badges_color_single_status_and_neutralize_multiple() {
        assert_eq!(pr_badge(&[]).0, 0);
        assert_eq!(pr_badge(&[nav_pr(1, "open", false)]).0, 1);
        assert_eq!(pr_badge(&[nav_pr(2, "open", true)]).0, 2);
        assert_eq!(pr_badge(&[nav_pr(3, "merged", false)]).0, 3);
        assert_eq!(pr_badge(&[nav_pr(4, "closed", false)]).0, 4);
        let (kind, tip) = pr_badge(&[nav_pr(5, "open", false), nav_pr(4, "merged", false)]);
        assert_eq!(kind, 5);
        assert!(tip.contains("#5 · Open"));
        assert!(tip.contains("#4 · Merged"));
    }

    #[test]
    fn attention_badges_distinguish_approval_question_and_both() {
        assert_eq!(attention_badge(0, 0).0, 0);
        assert_eq!(attention_badge(1, 0).0, 1);
        assert_eq!(attention_badge(0, 1).0, 2);
        let (kind, tip) = attention_badge(2, 1);
        assert_eq!(kind, 3);
        assert_eq!(tip, "2 approvals and 1 question need attention");
    }

    #[test]
    fn download_progress_includes_speed() {
        let (text, pct) = download_progress("downloading", 25_000_000, 100_000_000, Some(12.3e6));
        assert_eq!(text, "downloading… 25 MB / 100 MB (25%) · 12.3 MB/s");
        assert_eq!(pct, 25);
        // No rate yet (first poll) → no speed suffix.
        let (text, pct) = download_progress("downloading", 5_000_000, 0, None);
        assert_eq!(text, "downloading… 5 MB");
        assert_eq!(pct, -1);
    }

    #[test]
    fn human_rate_picks_sane_units() {
        assert_eq!(human_rate(12.34e6), " · 12.3 MB/s");
        assert_eq!(human_rate(850e3), " · 850 kB/s");
        assert_eq!(human_rate(120.0), " · 120 B/s");
    }
    #[test]
    fn thinking_picker_requires_a_model_advertised_enum() {
        let supported = serde_json::json!({
            "type": "object",
            "properties": {
                "reasoning_effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "default": "medium"
                }
            }
        });
        assert_eq!(
            thinking_property(&supported),
            Some((
                "reasoning_effort".into(),
                vec!["low".into(), "medium".into(), "high".into()],
                Some("medium".into())
            ))
        );
        assert!(
            thinking_property(&serde_json::json!({
                "type": "object",
                "properties": {"temperature": {"type": "number"}}
            }))
            .is_none()
        );
    }

    #[test]
    fn chat_navigation_reveals_running_and_queued_tail_attention() {
        assert!(should_open_chat_at_tail(false, true, false));
        assert!(should_open_chat_at_tail(false, false, true));
        assert!(should_open_chat_at_tail(true, false, false));
        assert!(!should_open_chat_at_tail(false, false, false));
    }

    #[test]
    fn model_health_uses_the_window_closest_to_its_cap() {
        let health = SubscriptionHealth {
            provider_id: "codex".into(),
            status: "ok".into(),
            plan: "pro".into(),
            windows: vec![
                SubscriptionWindow {
                    label: "5-hour window".into(),
                    used_percent: 42,
                    resets: "resets in 1h".into(),
                },
                SubscriptionWindow {
                    label: "Weekly".into(),
                    used_percent: 76,
                    resets: "resets Monday".into(),
                },
            ],
            credits: String::new(),
            note: String::new(),
        };

        let view = model_health_view(&health);
        assert_eq!(view.summary, "Pro · 76% used");
        assert_eq!(view.tone, 2);
        assert!(view.detail.contains("5-hour window: 42% used"));
        assert!(view.detail.contains("Weekly: 76% used"));
    }

    #[test]
    fn model_health_distinguishes_api_billing_from_login_failure() {
        let api = SubscriptionHealth {
            provider_id: "cursor".into(),
            status: "unsupported".into(),
            plan: String::new(),
            windows: Vec::new(),
            credits: String::new(),
            note: "usage-billed via API key".into(),
        };
        assert_eq!(model_health_view(&api).summary, "API billed");
        assert_eq!(model_health_view(&api).tone, 0);

        let login = SubscriptionHealth {
            provider_id: "claude-code".into(),
            status: "unavailable".into(),
            plan: String::new(),
            windows: Vec::new(),
            credits: String::new(),
            note: "subscription usage needs a claude.ai login".into(),
        };
        assert_eq!(model_health_view(&login).summary, "login required");
        assert_eq!(model_health_view(&login).tone, 3);
    }

    #[test]
    fn subscription_refreshes_share_freshness_and_invalidate_old_responses() {
        let now = std::time::Instant::now();
        let mut state = SubscriptionRefreshState::default();

        let first = state.begin(now, SubscriptionRefresh::IfStale).unwrap();
        assert!(state.is_current(first));
        assert_eq!(
            state.begin(
                now + std::time::Duration::from_secs(29),
                SubscriptionRefresh::IfStale
            ),
            None
        );

        let forced = state
            .begin(
                now + std::time::Duration::from_secs(29),
                SubscriptionRefresh::Force,
            )
            .unwrap();
        assert!(!state.is_current(first));
        assert!(state.is_current(forced));
        assert_eq!(
            state.begin(
                now + std::time::Duration::from_secs(58),
                SubscriptionRefresh::IfStale
            ),
            None
        );
        assert!(
            state
                .begin(
                    now + std::time::Duration::from_secs(59),
                    SubscriptionRefresh::IfStale
                )
                .is_some()
        );
    }
}
