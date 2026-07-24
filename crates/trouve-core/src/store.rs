//! SQLite persistence: workspaces, sessions, threads, provider transcripts,
//! checkpoints, and the append-only event log.
//!
//! The event log is the UI/replay/audit source of truth (invariant 2). The
//! `messages` table is the provider-facing transcript — a faithful record of
//! what was sent to/received from the model, which the event taxonomy does
//! not try to encode.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::broadcast;
use trouve_protocol::{Event, EventEnvelope, PermissionMode, Scope, Session, Thread, Workspace};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  path TEXT NOT NULL UNIQUE,
  closed INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  title TEXT NOT NULL,
  branch TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  base_ref TEXT NOT NULL,
  undo_pos INTEGER,           -- NULL = at latest checkpoint
  archived INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  mode TEXT NOT NULL,
  model TEXT NOT NULL,
  permission_mode TEXT NOT NULL,
  model_options TEXT NOT NULL DEFAULT '{}',
  todos TEXT NOT NULL DEFAULT '[]',
  last_turn INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
  thread_id TEXT NOT NULL REFERENCES threads(id),
  seq INTEGER NOT NULL,
  payload TEXT NOT NULL,      -- JSON trouve_providers::Message
  PRIMARY KEY (thread_id, seq)
);
CREATE TABLE IF NOT EXISTS checkpoints (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  thread_id TEXT,             -- NULL for the session-creation checkpoint
  turn INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  commit_hash TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS checkpoints_session_seq ON checkpoints (session_id, seq);
CREATE TABLE IF NOT EXISTS usage (
  thread_id TEXT NOT NULL REFERENCES threads(id),
  session_id TEXT NOT NULL REFERENCES sessions(id),
  turn INTEGER NOT NULL,
  input_tokens INTEGER NOT NULL,      -- summed across the turn's requests (cost)
  output_tokens INTEGER NOT NULL,
  cached_input_tokens INTEGER NOT NULL,
  context_input_tokens INTEGER NOT NULL DEFAULT 0, -- last request's input (context size)
  cost_usd REAL,
  PRIMARY KEY (thread_id, turn)
);
CREATE TABLE IF NOT EXISTS backend_sessions (
  thread_id TEXT NOT NULL REFERENCES threads(id),
  backend TEXT NOT NULL,          -- provider id ("cursor", "claude", …)
  backend_session_id TEXT NOT NULL,
  -- Transcript length (messages) when this backend last ran a turn; lets
  -- a resumed vendor session be told what other models did in between.
  seen_messages INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (thread_id, backend)
);
CREATE TABLE IF NOT EXISTS queued_prompts (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id),
  position INTEGER NOT NULL,
  content TEXT NOT NULL,
  attachments TEXT NOT NULL DEFAULT '[]',  -- JSON [trouve_protocol::Attachment]
  claimed INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS queued_prompts_thread ON queued_prompts (thread_id, position);
CREATE TABLE IF NOT EXISTS attachments (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id),
  name TEXT NOT NULL,
  mime TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  path TEXT NOT NULL,         -- stored file, absolute
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
  cursor INTEGER PRIMARY KEY AUTOINCREMENT,
  scope_kind TEXT NOT NULL,
  scope_id TEXT NOT NULL,
  ts TEXT NOT NULL,
  payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_scope ON events (scope_kind, scope_id, cursor);
CREATE TABLE IF NOT EXISTS spawned_threads (
  child_thread_id TEXT PRIMARY KEY REFERENCES threads(id),
  parent_thread_id TEXT NOT NULL REFERENCES threads(id),
  kind TEXT NOT NULL             -- 'thread' (inline, same worktree) | 'session'
);
CREATE TABLE IF NOT EXISTS automations (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  prompt TEXT NOT NULL,
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  mode TEXT,
  model TEXT,
  permission_mode TEXT NOT NULL DEFAULT 'ask',
  schedule TEXT NOT NULL,       -- JSON trouve_protocol::AutomationSchedule
  enabled INTEGER NOT NULL DEFAULT 1,
  next_run_at TEXT,             -- RFC3339; NULL while disabled
  last_run_at TEXT,
  last_session_id TEXT,
  last_error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL
);
-- The identity-based SQL names below are retained for compatibility with
-- preview databases; the Rust and wire APIs expose reviewer profiles.
CREATE TABLE IF NOT EXISTS code_review_repositories (
  repository TEXT PRIMARY KEY,
  installation_id INTEGER NOT NULL,
  private INTEGER NOT NULL DEFAULT 0,
  mode TEXT NOT NULL DEFAULT 'off',
  model TEXT,
  prompt TEXT NOT NULL DEFAULT '',
  identity_ids TEXT NOT NULL DEFAULT '["correctness","security","api-compatibility","testing"]',
  reviewer_overrides TEXT NOT NULL DEFAULT '[]',
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS code_review_identities (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  prompt TEXT NOT NULL,
  model TEXT,
  thinking_level TEXT,
  built_in INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS code_review_jobs (
  id TEXT PRIMARY KEY,
  dedupe_key TEXT NOT NULL UNIQUE,
  installation_id INTEGER NOT NULL,
  repository TEXT NOT NULL,
  pull_number INTEGER NOT NULL,
  pull_title TEXT NOT NULL,
  pull_url TEXT NOT NULL,
  head_sha TEXT NOT NULL,
  base_ref TEXT NOT NULL,
  head_ref TEXT NOT NULL,
  trigger TEXT NOT NULL,
  status TEXT NOT NULL,
  model TEXT,
  prompt TEXT NOT NULL DEFAULT '',
  identities TEXT NOT NULL DEFAULT '[]',
  config_hash TEXT NOT NULL DEFAULT '',
  session_id TEXT,
  thread_id TEXT,
  review_url TEXT NOT NULL DEFAULT '',
  error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  started_at TEXT,
  completed_at TEXT
);
CREATE INDEX IF NOT EXISTS code_review_jobs_status ON code_review_jobs (status, created_at);
CREATE TABLE IF NOT EXISTS code_review_pr_state (
  repository TEXT NOT NULL,
  pull_number INTEGER NOT NULL,
  manual_requested INTEGER NOT NULL DEFAULT 0,
  manual_generation INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (repository, pull_number)
);
CREATE TABLE IF NOT EXISTS code_review_manual_requests (
  repository TEXT NOT NULL,
  pull_number INTEGER NOT NULL,
  trigger_key TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (repository, pull_number, trigger_key)
);
CREATE TABLE IF NOT EXISTS github_webhook_deliveries (
  delivery_id TEXT PRIMARY KEY,
  received_at TEXT NOT NULL
);
-- TODO(retention): use seen_at for periodic archival or cleanup, but retain a
-- durable per-repository watermark or equivalent tombstone so deleting old
-- rows can never make stale manual-review commands eligible again.
CREATE TABLE IF NOT EXISTS code_review_polled_comments (
  repository TEXT NOT NULL,
  comment_id INTEGER NOT NULL,
  seen_at TEXT NOT NULL,
  PRIMARY KEY (repository, comment_id)
);
"#;

/// Additive migrations for databases created before a column existed.
/// `CREATE TABLE IF NOT EXISTS` won't touch existing tables, so each entry
/// is applied and "duplicate column" errors are ignored.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE workspaces ADD COLUMN closed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE queued_prompts ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE queued_prompts ADD COLUMN claimed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE automations ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'ask'",
    "ALTER TABLE threads ADD COLUMN todos TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_repositories ADD COLUMN identity_ids TEXT NOT NULL DEFAULT '[\"correctness\",\"security\",\"api-compatibility\",\"testing\"]'",
    "ALTER TABLE code_review_repositories ADD COLUMN reviewer_overrides TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_jobs ADD COLUMN identities TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_jobs ADD COLUMN config_hash TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_identities ADD COLUMN thinking_level TEXT",
    "ALTER TABLE code_review_identities ADD COLUMN built_in INTEGER NOT NULL DEFAULT 0",
    // Context-size proxy for compaction/UI: the input tokens of the turn's
    // *last* request, not the sum over its iterations (see record_usage).
    "ALTER TABLE usage ADD COLUMN context_input_tokens INTEGER NOT NULL DEFAULT 0",
];

fn apply_migrations(conn: &Connection) -> Result<()> {
    for sql in MIGRATIONS {
        if let Err(e) = conn.execute_batch(sql) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e).context(format!("migration failed: {sql}"));
            }
        }
    }
    migrate_backend_sessions(conn)?;
    Ok(())
}

/// Rebuild `backend_sessions` for databases created before it was keyed by
/// (thread, backend) — adding a column to the primary key needs a new
/// table. Legacy rows (one vendor session per thread, vendor unrecorded)
/// migrate under backend '' and act as a fallback until a real turn
/// replaces them.
fn migrate_backend_sessions(conn: &Connection) -> Result<()> {
    let legacy = {
        let mut stmt = conn.prepare("PRAGMA table_info(backend_sessions)")?;
        let columns: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        !columns.is_empty() && !columns.iter().any(|c| c == "backend")
    };
    if legacy {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE backend_sessions_v2 (
               thread_id TEXT NOT NULL REFERENCES threads(id),
               backend TEXT NOT NULL,
               backend_session_id TEXT NOT NULL,
               seen_messages INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (thread_id, backend)
             );
             -- Legacy sessions have seen the whole transcript to date: their
             -- vendor session was the only history carrier under the old
             -- schema, so nothing needs handing off on the next resume.
             INSERT INTO backend_sessions_v2
                    (thread_id, backend, backend_session_id, seen_messages)
               SELECT bs.thread_id, '', bs.backend_session_id,
                      (SELECT COUNT(*) FROM messages m WHERE m.thread_id = bs.thread_id)
                 FROM backend_sessions bs;
             DROP TABLE backend_sessions;
             ALTER TABLE backend_sessions_v2 RENAME TO backend_sessions;
             COMMIT;",
        )
        .context("rekeying backend_sessions by (thread, backend)")?;
    }
    Ok(())
}

/// Attachment metadata JSON from a queue row; a corrupt value degrades to
/// "no attachments" rather than failing the whole queue read.
fn parse_attachments(json: &str) -> Vec<trouve_protocol::Attachment> {
    serde_json::from_str(json).unwrap_or_default()
}

/// One `automations` row (column order matches the SELECTs below).
fn row_to_automation(r: &rusqlite::Row<'_>) -> rusqlite::Result<trouve_protocol::Automation> {
    let permission_mode: String = r.get(6)?;
    let schedule_json: String = r.get(7)?;
    Ok(trouve_protocol::Automation {
        id: r.get(0)?,
        name: r.get(1)?,
        prompt: r.get(2)?,
        workspace_id: r.get(3)?,
        mode: r.get(4)?,
        model: r.get(5)?,
        permission_mode: permission_mode_from(&permission_mode),
        schedule: serde_json::from_str(&schedule_json).unwrap_or(
            trouve_protocol::AutomationSchedule {
                kind: "daily".into(),
                minute: 0,
                time: "09:00".into(),
                days: vec![],
            },
        ),
        enabled: r.get(8)?,
        next_run_at: r.get(9)?,
        last_run_at: r.get(10)?,
        last_session_id: r.get(11)?,
        last_error: r.get(12)?,
        created_at: r.get(13)?,
    })
}

fn code_review_mode_from(value: &str) -> trouve_protocol::CodeReviewMode {
    match value {
        "manual" => trouve_protocol::CodeReviewMode::Manual,
        "automatic" => trouve_protocol::CodeReviewMode::Automatic,
        _ => trouve_protocol::CodeReviewMode::Off,
    }
}

fn code_review_mode_str(value: trouve_protocol::CodeReviewMode) -> &'static str {
    match value {
        trouve_protocol::CodeReviewMode::Off => "off",
        trouve_protocol::CodeReviewMode::Manual => "manual",
        trouve_protocol::CodeReviewMode::Automatic => "automatic",
    }
}

fn parse_datetime(value: String) -> chrono::DateTime<chrono::Utc> {
    value.parse().unwrap_or_else(|_| chrono::Utc::now())
}

fn parse_optional_datetime(value: Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    value.and_then(|value| value.parse().ok())
}

fn row_to_code_review_repository(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<trouve_protocol::CodeReviewRepository> {
    let mode: String = r.get(3)?;
    Ok(trouve_protocol::CodeReviewRepository {
        repository: r.get(0)?,
        installation_id: r.get::<_, i64>(1)? as u64,
        private: r.get(2)?,
        mode: code_review_mode_from(&mode),
        model: r.get(4)?,
        prompt: r.get(5)?,
        reviewer_ids: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(6)?)
            .unwrap_or_else(|_| crate::reviewers::default_reviewer_ids()),
        reviewer_overrides: serde_json::from_str::<Vec<trouve_protocol::ReviewerOverride>>(
            &r.get::<_, String>(7)?,
        )
        .unwrap_or_default(),
    })
}

#[derive(Debug, Clone)]
pub struct NewCodeReviewJob {
    pub dedupe_key: String,
    pub installation_id: u64,
    pub repository: String,
    pub pull_number: u64,
    pub pull_title: String,
    pub pull_url: String,
    pub head_sha: String,
    pub base_ref: String,
    pub head_ref: String,
    pub trigger: String,
    pub model: Option<String>,
    pub prompt: String,
    pub reviewers: Vec<trouve_protocol::ReviewerProfile>,
    pub config_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeReviewManualRequest {
    pub pull_number: u64,
    pub trigger_key: String,
}

#[derive(Debug, Clone)]
pub struct CodeReviewJobRecord {
    pub job: trouve_protocol::CodeReviewJob,
    pub prompt: String,
    pub reviewers: Vec<trouve_protocol::ReviewerProfile>,
}

fn row_to_code_review_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<CodeReviewJobRecord> {
    let reviewers: Vec<trouve_protocol::ReviewerProfile> =
        serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or_default();
    Ok(CodeReviewJobRecord {
        job: trouve_protocol::CodeReviewJob {
            id: r.get(0)?,
            installation_id: r.get::<_, i64>(1)? as u64,
            repository: r.get(2)?,
            pull_number: r.get::<_, i64>(3)? as u64,
            pull_title: r.get(4)?,
            pull_url: r.get(5)?,
            head_sha: r.get(6)?,
            base_ref: r.get(7)?,
            head_ref: r.get(8)?,
            trigger: r.get(9)?,
            status: r.get(10)?,
            model: r.get(11)?,
            reviewer_ids: reviewers
                .iter()
                .map(|reviewer| reviewer.id.clone())
                .collect(),
            session_id: r.get(15)?,
            thread_id: r.get(16)?,
            review_url: r.get(17)?,
            error: r.get(18)?,
            created_at: parse_datetime(r.get(19)?),
            started_at: parse_optional_datetime(r.get(20)?),
            completed_at: parse_optional_datetime(r.get(21)?),
        },
        prompt: r.get(12)?,
        reviewers,
    })
}

const CODE_REVIEW_JOB_COLUMNS: &str = "id, installation_id, repository, pull_number, pull_title, pull_url, head_sha, \
     base_ref, head_ref, trigger, status, model, prompt, identities, config_hash, session_id, thread_id, \
     review_url, error, created_at, started_at, completed_at";

pub enum UsageScope<'a> {
    Thread(&'a str),
    Session(&'a str),
}

#[derive(Debug, Clone)]
pub struct CheckpointRow {
    pub id: String,
    pub session_id: String,
    pub thread_id: Option<String>,
    pub turn: u64,
    pub seq: i64,
    pub commit_hash: String,
}

/// Shared handle to the database plus the live event fan-out.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    events_tx: broadcast::Sender<EventEnvelope>,
    append_tx: std::sync::mpsc::Sender<AppendRequest>,
}

/// One caller's event, in flight to the writer thread.
struct AppendRequest {
    scope: Scope,
    ts: chrono::DateTime<chrono::Utc>,
    /// Serialized on the caller's thread so an unserializable event fails
    /// there instead of poisoning a whole batch.
    payload: String,
    event: Event,
    reply: std::sync::mpsc::SyncSender<Result<EventEnvelope>>,
}

/// Upper bound on events committed per writer transaction. Bounds how long
/// the earliest waiter in a batch can be held behind later arrivals.
const APPEND_BATCH_MAX: usize = 256;

/// Event types intentionally removed by a protocol major-version bump. Their
/// persisted rows remain in the append-only log, but they no longer have a
/// meaningful representation in the current protocol.
const RETIRED_EVENT_TYPES: &[&str] = &["workspace.pull_requests_updated"];

fn is_retired_event(payload: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|event_type| RETIRED_EVENT_TYPES.contains(&event_type))
}

fn scope_cols(scope: &Scope) -> (&'static str, String) {
    match scope {
        Scope::Server => ("server", String::new()),
        Scope::Session(id) => ("session", id.clone()),
        Scope::Thread(id) => ("thread", id.clone()),
    }
}

fn scope_from_cols(kind: &str, id: String) -> Scope {
    match kind {
        "session" => Scope::Session(id),
        "thread" => Scope::Thread(id),
        _ => Scope::Server,
    }
}

/// The sole author of `events` rows and `events_tx` broadcasts. A single
/// writer assigning cursors and publishing in queue order upholds the
/// ordering invariant by construction: live SSE subscribers drop anything
/// with cursor <= the last they saw, so an out-of-order broadcast (6 before
/// 5) would lose event 5 permanently until reconnect.
///
/// The thread exits when every `Store` clone (each holding a request sender)
/// has been dropped.
fn spawn_event_writer(
    conn: Arc<Mutex<Connection>>,
    events_tx: broadcast::Sender<EventEnvelope>,
) -> std::sync::mpsc::Sender<AppendRequest> {
    let (tx, rx) = std::sync::mpsc::channel::<AppendRequest>();
    std::thread::Builder::new()
        .name("trouve-event-writer".into())
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut batch = vec![first];
                while batch.len() < APPEND_BATCH_MAX
                    && let Ok(req) = rx.try_recv()
                {
                    batch.push(req);
                }
                let inserted = {
                    let mut conn = conn.lock().unwrap();
                    insert_event_batch(&mut conn, &batch)
                };
                match inserted {
                    Ok(cursors) => {
                        for (req, cursor) in batch.into_iter().zip(cursors) {
                            let envelope = EventEnvelope {
                                cursor,
                                scope: req.scope,
                                ts: req.ts,
                                event: req.event,
                            };
                            // Nobody listening is fine; a caller that gave up
                            // waiting is too.
                            let _ = events_tx.send(envelope.clone());
                            let _ = req.reply.send(Ok(envelope));
                        }
                    }
                    Err(e) => {
                        // The transaction rolled back: every waiter's event
                        // was equally not persisted.
                        for req in batch {
                            let _ = req.reply.send(Err(anyhow::anyhow!("appending event: {e}")));
                        }
                    }
                }
            }
        })
        .expect("spawning event writer thread");
    tx
}

/// Insert a batch in queue order under one transaction, returning the
/// assigned cursors. All-or-nothing: on error the transaction rolls back.
fn insert_event_batch(conn: &mut Connection, batch: &[AppendRequest]) -> Result<Vec<u64>> {
    let tx = conn.transaction()?;
    let mut cursors = Vec::with_capacity(batch.len());
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO events (scope_kind, scope_id, ts, payload) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for req in batch {
            let (kind, id) = scope_cols(&req.scope);
            stmt.execute(params![kind, id, req.ts.to_rfc3339(), req.payload])?;
            cursors.push(tx.last_insert_rowid() as u64);
        }
    }
    tx.commit()?;
    Ok(cursors)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&conn)?;
        // Claims belong to dispatcher tasks in this process. After a crash
        // there is no worker to own them, so make the prompts visible and
        // explicitly dispatchable again instead of losing them.
        conn.execute(
            "UPDATE queued_prompts SET claimed = 0 WHERE claimed != 0",
            [],
        )?;
        Ok(Self::from_conn(conn))
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        // Match on-disk behavior so tests exercise the same constraints.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&conn)?;
        conn.execute(
            "UPDATE queued_prompts SET claimed = 0 WHERE claimed != 0",
            [],
        )?;
        Ok(Self::from_conn(conn))
    }

    fn from_conn(conn: Connection) -> Self {
        let conn = Arc::new(Mutex::new(conn));
        let (events_tx, _) = broadcast::channel(4096);
        let append_tx = spawn_event_writer(Arc::clone(&conn), events_tx.clone());
        Self {
            conn,
            events_tx,
            append_tx,
        }
    }

    // --- event log --------------------------------------------------------

    /// The single append chokepoint: persist first, then publish, so a
    /// subscriber can never observe an event that wouldn't survive a crash.
    ///
    /// Appends are executed by a dedicated writer thread that commits every
    /// request queued at that moment in one transaction, so concurrent turns
    /// pay one fsync per batch instead of one each, and never block each
    /// other on the connection mutex. This call still waits for durability:
    /// it returns once the batch containing this event has committed.
    pub fn append_event(&self, scope: Scope, event: Event) -> Result<EventEnvelope> {
        let payload = serde_json::to_string(&event)?;
        let (reply, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.append_tx
            .send(AppendRequest {
                scope,
                ts: chrono::Utc::now(),
                payload,
                event,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
    }

    /// Persisted events for a scope after `after` (exclusive), oldest first.
    pub fn events_after(&self, scope: &Scope, after: u64) -> Result<Vec<EventEnvelope>> {
        let (kind, id) = scope_cols(scope);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT cursor, scope_kind, scope_id, ts, payload FROM events
             WHERE scope_kind = ?1 AND scope_id = ?2 AND cursor > ?3 ORDER BY cursor",
        )?;
        let rows = stmt.query_map(params![kind, id, after as i64], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (cursor, kind, id, ts, payload) = row?;
            // Skip a row we can't deserialize (e.g. an event type written by
            // a newer build) rather than failing the whole scope's replay —
            // otherwise one unknown event makes the session/thread
            // permanently unloadable.
            let event = match serde_json::from_str(&payload) {
                Ok(e) => e,
                Err(_) if is_retired_event(&payload) => continue,
                Err(e) => {
                    tracing::warn!("skipping undeserializable event {cursor}: {e}");
                    continue;
                }
            };
            out.push(EventEnvelope {
                cursor,
                scope: scope_from_cols(&kind, id),
                ts: ts.parse().unwrap_or_else(|_| chrono::Utc::now()),
                event,
            });
        }
        Ok(out)
    }

    /// Live subscription to all events; callers filter by scope.
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events_tx.subscribe()
    }

    // --- workspaces ---------------------------------------------------------

    pub fn insert_workspace(&self, ws: &Workspace) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO workspaces (id, name, path, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![ws.id, ws.name, ws.path, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn set_workspace_closed(&self, id: &str, closed: bool) -> Result<bool> {
        let changed = self.conn.lock().unwrap().execute(
            "UPDATE workspaces SET closed = ?2 WHERE id = ?1 AND closed != ?2",
            params![id, closed],
        )?;
        Ok(changed != 0)
    }

    pub fn workspace(&self, id: &str) -> Result<Option<Workspace>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, path FROM workspaces WHERE id = ?1",
            params![id],
            |r| {
                Ok(Workspace {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    path: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// A workspace only while it is open and available for new activity.
    pub fn open_workspace(&self, id: &str) -> Result<Option<Workspace>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, path FROM workspaces WHERE id = ?1 AND closed = 0",
            params![id],
            |r| {
                Ok(Workspace {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    path: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn workspace_by_path(&self, path: &str) -> Result<Option<Workspace>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, name, path FROM workspaces WHERE path = ?1",
            params![path],
            |r| {
                Ok(Workspace {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    path: r.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, path FROM workspaces WHERE closed = 0 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Workspace {
                id: r.get(0)?,
                name: r.get(1)?,
                path: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- sessions -----------------------------------------------------------

    pub fn insert_session(&self, s: &Session) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO sessions (id, workspace_id, title, branch, worktree_path, base_ref, archived, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![s.id, s.workspace_id, s.title, s.branch, s.worktree_path, s.base_ref,
                    s.archived, s.created_at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, workspace_id, title, branch, worktree_path, base_ref, archived, created_at
             FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_sessions(&self, workspace_id: Option<&str>) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        match workspace_id {
            Some(ws) => {
                let mut stmt = conn.prepare(
                    "SELECT id, workspace_id, title, branch, worktree_path, base_ref, archived, created_at
                     FROM sessions WHERE workspace_id = ?1 ORDER BY created_at",
                )?;
                let rows = stmt.query_map(params![ws], row_to_session)?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, workspace_id, title, branch, worktree_path, base_ref, archived, created_at
                     FROM sessions ORDER BY created_at",
                )?;
                let rows = stmt.query_map([], row_to_session)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }

    /// Rename and/or (un)archive a session. `None` fields are unchanged.
    pub fn update_session(
        &self,
        id: &str,
        title: Option<&str>,
        archived: Option<bool>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if let Some(title) = title {
            conn.execute(
                "UPDATE sessions SET title = ?2 WHERE id = ?1",
                params![id, title],
            )?;
        }
        if let Some(archived) = archived {
            conn.execute(
                "UPDATE sessions SET archived = ?2 WHERE id = ?1",
                params![id, archived],
            )?;
        }
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // One transaction so a failure can't leave a half-deleted session.
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM events WHERE (scope_kind = 'session' AND scope_id = ?1)
             OR (scope_kind = 'thread' AND scope_id IN (SELECT id FROM threads WHERE session_id = ?1))",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM backend_sessions WHERE thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM queued_prompts WHERE thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM usage WHERE session_id = ?1", params![id])?;
        tx.execute("DELETE FROM checkpoints WHERE session_id = ?1", params![id])?;
        // attachments and spawned_threads both FK to threads(id); with
        // foreign_keys=ON, deleting threads while these rows exist fails the
        // whole transaction. Any session that ever took an attachment or used
        // spawn_thread/spawn_session hit this, leaving a session the engine
        // had already removed from disk still present in the DB.
        tx.execute(
            "DELETE FROM attachments WHERE thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute(
            "DELETE FROM spawned_threads WHERE child_thread_id IN (SELECT id FROM threads WHERE session_id = ?1)
             OR parent_thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM threads WHERE session_id = ?1", params![id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    // --- threads ------------------------------------------------------------

    pub fn insert_thread(
        &self,
        t: &Thread,
        model_options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO threads
                (id, session_id, mode, model, permission_mode, model_options, todos, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                t.id,
                t.session_id,
                t.mode,
                t.model,
                permission_mode_str(t.permission_mode),
                serde_json::to_string(model_options)?,
                serde_json::to_string(&t.todos)?,
                t.created_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn thread(&self, id: &str) -> Result<Option<Thread>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1"),
            params![id],
            row_to_thread,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn thread_model_options(
        &self,
        id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let text: String = conn.query_row(
            "SELECT model_options FROM threads WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn list_threads(&self, session_id: &str) -> Result<Vec<Thread>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {THREAD_COLUMNS} FROM threads WHERE session_id = ?1 ORDER BY created_at"
        ))?;
        let rows = stmt.query_map(params![session_id], row_to_thread)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Update thread settings between turns. `None` fields are unchanged.
    pub fn update_thread(
        &self,
        id: &str,
        mode: Option<&str>,
        model: Option<&str>,
        model_options: Option<&serde_json::Map<String, serde_json::Value>>,
        permission_mode: Option<PermissionMode>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        if let Some(mode) = mode {
            conn.execute(
                "UPDATE threads SET mode = ?2 WHERE id = ?1",
                params![id, mode],
            )?;
        }
        if let Some(model) = model {
            conn.execute(
                "UPDATE threads SET model = ?2 WHERE id = ?1",
                params![id, model],
            )?;
        }
        if let Some(options) = model_options {
            conn.execute(
                "UPDATE threads SET model_options = ?2 WHERE id = ?1",
                params![id, serde_json::to_string(options)?],
            )?;
        }
        if let Some(pm) = permission_mode {
            conn.execute(
                "UPDATE threads SET permission_mode = ?2 WHERE id = ?1",
                params![id, permission_mode_str(pm)],
            )?;
        }
        Ok(())
    }

    /// Replace the current todo snapshot for exactly one thread.
    pub fn update_thread_todos(&self, id: &str, todos: &[trouve_protocol::TodoItem]) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE threads SET todos = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(todos)?],
        )?;
        Ok(())
    }

    // --- spawned threads --------------------------------------------------
    // Parentage of agent-spawned children (spawn_thread / spawn_session
    // tools): drives the depth guard (children don't spawn grandchildren)
    // and the concurrency cap.

    pub fn insert_spawned(&self, child: &str, parent: &str, kind: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO spawned_threads (child_thread_id, parent_thread_id, kind)
             VALUES (?1, ?2, ?3)",
            params![child, parent, kind],
        )?;
        Ok(())
    }

    /// The parent thread id, when `child` was spawned by an agent.
    pub fn spawn_parent(&self, child: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT parent_thread_id FROM spawned_threads WHERE child_thread_id = ?1")?;
        let mut rows = stmt.query_map(params![child], |r| r.get(0))?;
        Ok(rows.next().transpose()?)
    }

    /// Every child the agent on `parent` has spawned (thread ids).
    pub fn spawned_children(&self, parent: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT child_thread_id FROM spawned_threads WHERE parent_thread_id = ?1")?;
        let rows = stmt.query_map(params![parent], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Atomically claim the next turn number for a thread.
    pub fn last_turn(&self, thread_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let turn: i64 = conn.query_row(
            "SELECT last_turn FROM threads WHERE id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(turn as u64)
    }

    pub fn next_turn(&self, thread_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE threads SET last_turn = last_turn + 1 WHERE id = ?1",
            params![thread_id],
        )?;
        let turn: i64 = conn.query_row(
            "SELECT last_turn FROM threads WHERE id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        Ok(turn as u64)
    }

    // --- queued prompts -------------------------------------------------------
    // Prompts submitted while a turn was running. Persisted so a restart or
    // crash doesn't lose them; drained in `position` order between turns.

    pub fn enqueue_prompt(
        &self,
        thread_id: &str,
        content: &str,
        attachments: &[trouve_protocol::Attachment],
    ) -> Result<trouve_protocol::QueuedPrompt> {
        let conn = self.conn.lock().unwrap();
        let id = format!("qp_{}", uuid::Uuid::new_v4().simple());
        let created_at = chrono::Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(attachments)?;
        conn.execute(
            "INSERT INTO queued_prompts (id, thread_id, position, content, attachments, created_at)
             VALUES (?1, ?2,
               (SELECT COALESCE(MAX(position), 0) + 1 FROM queued_prompts WHERE thread_id = ?2),
               ?3, ?4, ?5)",
            params![id, thread_id, content, attachments_json, created_at],
        )?;
        let position: i64 = conn.query_row(
            "SELECT position FROM queued_prompts WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(trouve_protocol::QueuedPrompt {
            id,
            thread_id: thread_id.to_string(),
            position: position as u64,
            content: content.to_string(),
            attachments: attachments.to_vec(),
            created_at,
        })
    }

    pub fn queued_prompts(&self, thread_id: &str) -> Result<Vec<trouve_protocol::QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, position, content, attachments, created_at FROM queued_prompts
             WHERE thread_id = ?1 AND claimed = 0 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![thread_id], |r| {
            Ok(trouve_protocol::QueuedPrompt {
                id: r.get(0)?,
                thread_id: thread_id.to_string(),
                position: r.get::<_, i64>(1)? as u64,
                content: r.get(2)?,
                attachments: parse_attachments(&r.get::<_, String>(3)?),
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Thread the queued prompt belongs to, if it still exists.
    pub fn queued_prompt_thread(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT thread_id FROM queued_prompts WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Returns false when the prompt no longer exists (already dispatched).
    pub fn update_queued_prompt(&self, id: &str, content: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE queued_prompts SET content = ?2 WHERE id = ?1 AND claimed = 0",
            params![id, content],
        )?;
        Ok(n > 0)
    }

    pub fn delete_queued_prompt(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM queued_prompts WHERE id = ?1 AND claimed = 0",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Apply a full new order. `ids` must be exactly the thread's current
    /// queue; returns false (changing nothing) when it isn't, so a reorder
    /// racing a dispatch fails cleanly instead of corrupting positions.
    pub fn reorder_queued_prompts(&self, thread_id: &str, ids: &[String]) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut current: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM queued_prompts
                     WHERE thread_id = ?1 AND claimed = 0 ORDER BY position",
            )?;
            let rows = stmt.query_map(params![thread_id], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        current.sort();
        let mut requested = ids.to_vec();
        requested.sort();
        if current != requested {
            return Ok(false);
        }
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE queued_prompts SET position = ?2 WHERE id = ?1 AND claimed = 0",
                params![id, (i + 1) as i64],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Hide and return the front prompt while a dispatcher prepares its
    /// durable turn start. The row is deleted only after the user message is
    /// persisted; setup failures release it back to the visible queue.
    pub fn claim_queued_prompt(
        &self,
        thread_id: &str,
    ) -> Result<Option<trouve_protocol::QueuedPrompt>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let front = tx
            .query_row(
                "SELECT id, position, content, attachments, created_at FROM queued_prompts
                 WHERE thread_id = ?1 AND claimed = 0 ORDER BY position LIMIT 1",
                params![thread_id],
                |r| {
                    Ok(trouve_protocol::QueuedPrompt {
                        id: r.get(0)?,
                        thread_id: thread_id.to_string(),
                        position: r.get::<_, i64>(1)? as u64,
                        content: r.get(2)?,
                        attachments: parse_attachments(&r.get::<_, String>(3)?),
                        created_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
        if let Some(p) = &front {
            tx.execute(
                "UPDATE queued_prompts SET claimed = 1 WHERE id = ?1 AND claimed = 0",
                params![p.id],
            )?;
        }
        tx.commit()?;
        Ok(front)
    }

    /// Return a claimed prompt to the visible queue after setup failed.
    pub fn release_queued_prompt(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE queued_prompts SET claimed = 0 WHERE id = ?1 AND claimed = 1",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Permanently consume a claimed prompt after its user message is
    /// durable in the event log and provider transcript.
    pub fn finish_queued_prompt(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM queued_prompts WHERE id = ?1 AND claimed = 1",
            params![id],
        )?;
        Ok(n > 0)
    }

    // --- attachments ------------------------------------------------------
    // Prompt uploads. Bytes live on disk (the engine writes them under
    // data_dir/attachments); this table is the id → file index plus the
    // metadata shown in transcripts.

    pub fn add_attachment(
        &self,
        thread_id: &str,
        attachment: &trouve_protocol::Attachment,
        path: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO attachments (id, thread_id, name, mime, size_bytes, path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                attachment.id,
                thread_id,
                attachment.name,
                attachment.mime,
                attachment.size_bytes as i64,
                path,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// On-disk paths of every attachment belonging to a session's threads
    /// (for cleaning up the files when the session is deleted).
    pub fn session_attachment_paths(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path FROM attachments
             WHERE thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
        )?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Metadata plus the stored file path.
    pub fn attachment(&self, id: &str) -> Result<Option<(trouve_protocol::Attachment, String)>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT name, mime, size_bytes, path FROM attachments WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        trouve_protocol::Attachment {
                            id: id.to_string(),
                            name: r.get(0)?,
                            mime: r.get(1)?,
                            size_bytes: r.get::<_, i64>(2)? as u64,
                        },
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?)
    }

    // --- automations ------------------------------------------------------------

    pub fn insert_automation(&self, a: &trouve_protocol::Automation) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO automations (id, name, prompt, workspace_id, mode, model,
                                      permission_mode, schedule, enabled, next_run_at,
                                      last_run_at, last_session_id, last_error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
                permission_mode_str(a.permission_mode),
                serde_json::to_string(&a.schedule)?,
                a.enabled,
                a.next_run_at,
                a.last_run_at,
                a.last_session_id,
                a.last_error,
                a.created_at,
            ],
        )?;
        Ok(())
    }

    /// Replace the user-editable fields plus the recomputed next fire time
    /// (run bookkeeping is `mark_automation_run`'s job).
    pub fn update_automation(&self, a: &trouve_protocol::Automation) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE automations SET name = ?2, prompt = ?3, workspace_id = ?4, mode = ?5,
                    model = ?6, permission_mode = ?7, schedule = ?8, enabled = ?9,
                    next_run_at = ?10
             WHERE id = ?1",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
                permission_mode_str(a.permission_mode),
                serde_json::to_string(&a.schedule)?,
                a.enabled,
                a.next_run_at,
            ],
        )?;
        Ok(n > 0)
    }

    /// Record one fire: when, what it created (or why it failed), and when
    /// it fires next.
    pub fn mark_automation_run(
        &self,
        id: &str,
        ran_at: &str,
        session_id: Option<&str>,
        error: &str,
        next_run_at: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE automations SET last_run_at = ?2, last_session_id = ?3, last_error = ?4,
                    next_run_at = ?5
             WHERE id = ?1",
            params![id, ran_at, session_id, error, next_run_at],
        )?;
        Ok(())
    }

    /// Update the outcome of the most recently dispatched run without
    /// changing its start time, session, or next scheduled occurrence.
    pub fn set_automation_result(&self, id: &str, error: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE automations SET last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// Reset the next fire time alone (startup recompute after downtime).
    pub fn set_automation_next_run(&self, id: &str, next_run_at: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE automations SET next_run_at = ?2 WHERE id = ?1",
            params![id, next_run_at],
        )?;
        Ok(())
    }

    pub fn list_automations(&self) -> Result<Vec<trouve_protocol::Automation>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt, workspace_id, mode, model, permission_mode, schedule, enabled,
                    next_run_at, last_run_at, last_session_id, last_error, created_at
             FROM automations ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], row_to_automation)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn automation(&self, id: &str) -> Result<Option<trouve_protocol::Automation>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT id, name, prompt, workspace_id, mode, model, permission_mode, schedule, enabled,
                        next_run_at, last_run_at, last_session_id, last_error, created_at
                 FROM automations WHERE id = ?1",
                params![id],
                row_to_automation,
            )
            .optional()?)
    }

    pub fn delete_automation(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM automations WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // --- automated code review ---------------------------------------------

    pub fn list_custom_reviewer_profiles(&self) -> Result<Vec<trouve_protocol::ReviewerProfile>> {
        self.list_reviewer_profiles(false)
    }

    pub fn list_built_in_reviewer_defaults(&self) -> Result<Vec<trouve_protocol::ReviewerProfile>> {
        self.list_reviewer_profiles(true)
    }

    fn list_reviewer_profiles(
        &self,
        built_in: bool,
    ) -> Result<Vec<trouve_protocol::ReviewerProfile>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt, model, thinking_level
             FROM code_review_identities
             WHERE built_in = ?1
             ORDER BY lower(name), id",
        )?;
        let rows = stmt.query_map([built_in], |row| {
            Ok(trouve_protocol::ReviewerProfile {
                id: row.get(0)?,
                name: row.get(1)?,
                prompt: row.get(2)?,
                model: row.get(3)?,
                default_thinking_level: row.get(4)?,
                built_in,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn upsert_reviewer_profile(
        &self,
        reviewer: &trouve_protocol::ReviewerProfile,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_identities
                    (id, name, prompt, model, thinking_level, built_in, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               prompt = excluded.prompt,
               model = excluded.model,
               thinking_level = excluded.thinking_level,
               built_in = excluded.built_in,
               updated_at = excluded.updated_at",
            params![
                reviewer.id,
                reviewer.name,
                reviewer.prompt,
                reviewer.model,
                reviewer.default_thinking_level,
                reviewer.built_in,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn delete_custom_reviewer_profile(&self, id: &str) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let deleted = tx.execute(
            "DELETE FROM code_review_identities WHERE id = ?1 AND built_in = 0",
            params![id],
        )?;
        if deleted > 0 {
            let repositories: Vec<(String, String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT repository, identity_ids, reviewer_overrides
                     FROM code_review_repositories",
                )?;
                stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for (repository, encoded_ids, encoded_overrides) in repositories {
                let mut ids: Vec<String> = serde_json::from_str(&encoded_ids).unwrap_or_default();
                let mut overrides: Vec<trouve_protocol::ReviewerOverride> =
                    serde_json::from_str(&encoded_overrides).unwrap_or_default();
                let before_ids = ids.len();
                let before_overrides = overrides.len();
                ids.retain(|reviewer_id| reviewer_id != id);
                overrides.retain(|reviewer_override| reviewer_override.reviewer_id != id);
                if ids.len() != before_ids || overrides.len() != before_overrides {
                    if ids.is_empty() {
                        ids = crate::reviewers::default_reviewer_ids();
                    }
                    tx.execute(
                        "UPDATE code_review_repositories
                         SET identity_ids = ?2, reviewer_overrides = ?3, updated_at = ?4
                         WHERE repository = ?1",
                        params![
                            repository,
                            serde_json::to_string(&ids)?,
                            serde_json::to_string(&overrides)?,
                            chrono::Utc::now().to_rfc3339(),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(deleted > 0)
    }

    pub fn upsert_discovered_code_review_repository(
        &self,
        installation_id: u64,
        repository: &str,
        private: bool,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_repositories
                    (repository, installation_id, private, mode, updated_at)
             VALUES (?1, ?2, ?3, 'off', ?4)
             ON CONFLICT(repository) DO UPDATE SET
               installation_id = excluded.installation_id,
               private = excluded.private,
               updated_at = excluded.updated_at",
            params![
                repository,
                installation_id as i64,
                private,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn list_code_review_repositories(
        &self,
    ) -> Result<Vec<trouve_protocol::CodeReviewRepository>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT repository, installation_id, private, mode, model, prompt,
                    identity_ids, reviewer_overrides
             FROM code_review_repositories ORDER BY repository",
        )?;
        let rows = stmt.query_map([], row_to_code_review_repository)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_code_review_repository(
        &self,
        request: &trouve_protocol::UpdateCodeReviewRepositoryRequest,
    ) -> Result<()> {
        let reviewer_ids = request
            .reviewer_ids
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let reviewer_overrides = request
            .reviewer_overrides
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_repositories
                    (repository, installation_id, private, mode, model, prompt,
                     identity_ids, reviewer_overrides, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4, ?5,
                     COALESCE(?6, '[\"correctness\",\"security\",\"api-compatibility\",\"testing\"]'),
                     COALESCE(?7, '[]'), ?8)
             ON CONFLICT(repository) DO UPDATE SET
               installation_id = excluded.installation_id,
               mode = excluded.mode,
               model = excluded.model,
               prompt = excluded.prompt,
               identity_ids = COALESCE(?6, code_review_repositories.identity_ids),
               reviewer_overrides = COALESCE(?7, code_review_repositories.reviewer_overrides),
               updated_at = excluded.updated_at",
            params![
                request.repository,
                request.installation_id as i64,
                code_review_mode_str(request.mode),
                request.model,
                request.prompt,
                reviewer_ids,
                reviewer_overrides,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn enqueue_code_review_job(
        &self,
        new_job: &NewCodeReviewJob,
    ) -> Result<Option<trouve_protocol::CodeReviewJob>> {
        let id = crate::new_id("rv");
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let reviewers = serde_json::to_string(&new_job.reviewers)?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO code_review_jobs
                    (id, dedupe_key, installation_id, repository, pull_number, pull_title,
                     pull_url, head_sha, base_ref, head_ref, trigger, status, model, prompt,
                     identities, config_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'queued',
                     ?12, ?13, ?14, ?15, ?16)",
            params![
                id,
                new_job.dedupe_key,
                new_job.installation_id as i64,
                new_job.repository,
                new_job.pull_number as i64,
                new_job.pull_title,
                new_job.pull_url,
                new_job.head_sha,
                new_job.base_ref,
                new_job.head_ref,
                new_job.trigger,
                new_job.model,
                new_job.prompt,
                reviewers,
                new_job.config_hash,
                now,
            ],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        Ok(Some(
            conn.query_row(
                &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
                params![id],
                row_to_code_review_job,
            )?
            .job,
        ))
    }

    pub fn supersede_code_review_jobs(
        &self,
        repository: &str,
        pull_number: u64,
        base_ref: &str,
        head_sha: &str,
        config_hash: &str,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM code_review_jobs
                 WHERE repository = ?1 AND pull_number = ?2
                   AND status IN ('queued', 'running')
                   AND (base_ref != ?3 OR head_sha != ?4 OR config_hash != ?5)
                 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(
                params![
                    repository,
                    pull_number as i64,
                    base_ref,
                    head_sha,
                    config_hash
                ],
                |row| row.get(0),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !ids.is_empty() {
            tx.execute(
                "UPDATE code_review_jobs
                 SET status = 'stale', review_url = '',
                     error = ?5, completed_at = ?6
                 WHERE repository = ?1 AND pull_number = ?2
                   AND status IN ('queued', 'running')
                   AND (base_ref != ?3 OR head_sha != ?4 OR config_hash != ?7)",
                params![
                    repository,
                    pull_number as i64,
                    base_ref,
                    head_sha,
                    format!(
                        "superseded by pull request revision {base_ref}..{head_sha} or review configuration"
                    ),
                    chrono::Utc::now().to_rfc3339(),
                    config_hash,
                ],
            )?;
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn list_code_review_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<trouve_protocol::CodeReviewJob>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs
             ORDER BY created_at DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map(params![limit as i64], row_to_code_review_job)?;
        let records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records.into_iter().map(|record| record.job).collect())
    }

    pub fn code_review_job(&self, id: &str) -> Result<Option<CodeReviewJobRecord>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
                params![id],
                row_to_code_review_job,
            )
            .optional()?)
    }

    pub fn code_review_job_exists(&self, dedupe_key: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT 1 FROM code_review_jobs WHERE dedupe_key = ?1",
                params![dedupe_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn recover_code_review_jobs(&self) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET status = 'queued', started_at = NULL,
                    error = 'server restarted while review was running'
             WHERE status = 'running'",
            [],
        )?;
        Ok(())
    }

    pub fn claim_code_review_job(&self) -> Result<Option<CodeReviewJobRecord>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let id: Option<String> = tx
            .query_row(
                "SELECT id FROM code_review_jobs WHERE status = 'queued'
                 ORDER BY created_at LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE code_review_jobs SET status = 'running', started_at = ?2, error = ''
             WHERE id = ?1 AND status = 'queued'",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        let record = tx.query_row(
            &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
            params![id],
            row_to_code_review_job,
        )?;
        tx.commit()?;
        Ok(Some(record))
    }

    pub fn set_code_review_job_session(
        &self,
        id: &str,
        session_id: &str,
        thread_id: &str,
    ) -> Result<bool> {
        let updated = self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET session_id = ?2, thread_id = ?3
             WHERE id = ?1 AND status = 'running'",
            params![id, session_id, thread_id],
        )?;
        Ok(updated > 0)
    }

    pub fn finish_code_review_job(
        &self,
        id: &str,
        status: &str,
        review_url: &str,
        error: &str,
    ) -> Result<bool> {
        let updated = self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET status = ?2, review_url = ?3, error = ?4,
                    completed_at = ?5
             WHERE id = ?1 AND status = 'running'",
            params![
                id,
                status,
                review_url,
                error,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(updated > 0)
    }

    pub fn pending_code_review_job_cleanups(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id FROM code_review_jobs
             WHERE status IN ('succeeded', 'failed', 'stale') AND session_id IS NOT NULL
             ORDER BY completed_at",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn complete_code_review_job_cleanup(&self, id: &str, session_id: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET session_id = NULL, thread_id = NULL
             WHERE id = ?1 AND status IN ('succeeded', 'failed', 'stale') AND session_id = ?2",
            params![id, session_id],
        )?;
        Ok(())
    }

    /// Update the polled bot-review request latch. Returns a new generation
    /// only on the false -> true transition, allowing a same-SHA re-request
    /// after the previous review cleared the request.
    pub fn code_review_manual_transition(
        &self,
        repository: &str,
        pull_number: u64,
        requested: bool,
    ) -> Result<Option<u64>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let previous: Option<(bool, i64)> = tx
            .query_row(
                "SELECT manual_requested, manual_generation FROM code_review_pr_state
                 WHERE repository = ?1 AND pull_number = ?2",
                params![repository, pull_number as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (was_requested, generation) = previous.unwrap_or((false, 0));
        let next_generation = generation + i64::from(requested && !was_requested);
        tx.execute(
            "INSERT INTO code_review_pr_state
                    (repository, pull_number, manual_requested, manual_generation)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repository, pull_number) DO UPDATE SET
               manual_requested = excluded.manual_requested,
               manual_generation = excluded.manual_generation",
            params![repository, pull_number as i64, requested, next_generation],
        )?;
        tx.commit()?;
        Ok((requested && !was_requested).then_some(next_generation as u64))
    }

    /// Claim one GitHub webhook delivery and, when present, durably record its
    /// manual review request in the same transaction. Duplicate delivery ids
    /// are ignored, which makes GitHub's at-least-once delivery safe to retry.
    pub fn claim_github_webhook_delivery(
        &self,
        delivery_id: &str,
        manual_request: Option<(&str, u64, &str)>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO github_webhook_deliveries (delivery_id, received_at)
             VALUES (?1, ?2)",
            params![delivery_id, chrono::Utc::now().to_rfc3339()],
        )?;
        if inserted > 0
            && let Some((repository, pull_number, trigger_key)) = manual_request
        {
            tx.execute(
                "INSERT OR IGNORE INTO code_review_manual_requests
                        (repository, pull_number, trigger_key, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    repository,
                    pull_number as i64,
                    trigger_key,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
        }
        tx.commit()?;
        Ok(inserted > 0)
    }

    pub fn pending_code_review_manual_requests(
        &self,
        repository: &str,
    ) -> Result<Vec<CodeReviewManualRequest>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT pull_number, trigger_key FROM code_review_manual_requests
             WHERE repository = ?1 ORDER BY created_at, trigger_key",
        )?;
        let rows = stmt.query_map(params![repository], |row| {
            Ok(CodeReviewManualRequest {
                pull_number: row.get::<_, i64>(0)? as u64,
                trigger_key: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn complete_code_review_manual_request(
        &self,
        repository: &str,
        pull_number: u64,
        trigger_key: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM code_review_manual_requests
             WHERE repository = ?1 AND pull_number = ?2 AND trigger_key = ?3",
            params![repository, pull_number as i64, trigger_key],
        )?;
        Ok(())
    }

    pub fn code_review_comment_poll_initialized(&self, repository: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM code_review_polled_comments
                 WHERE repository = ?1 LIMIT 1
             )",
            params![repository],
            |row| row.get(0),
        )?)
    }

    /// Claim a comment discovered by reconciliation and, when it is a manual
    /// review command, record the request in the same transaction. Keeping
    /// seen comments after their request is consumed prevents an old command
    /// from retriggering whenever the pull request head changes.
    pub fn claim_code_review_polled_comment(
        &self,
        repository: &str,
        comment_id: u64,
        manual_request: Option<(u64, &str)>,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO code_review_polled_comments
                    (repository, comment_id, seen_at)
             VALUES (?1, ?2, ?3)",
            params![
                repository,
                comment_id as i64,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        if inserted > 0
            && let Some((pull_number, trigger_key)) = manual_request
        {
            tx.execute(
                "INSERT OR IGNORE INTO code_review_manual_requests
                        (repository, pull_number, trigger_key, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    repository,
                    pull_number as i64,
                    trigger_key,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
        }
        tx.commit()?;
        Ok(inserted > 0)
    }

    // --- provider transcript --------------------------------------------------

    pub fn append_message(&self, thread_id: &str, payload: &serde_json::Value) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (thread_id, seq, payload)
             VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE thread_id = ?1), ?2)",
            params![thread_id, payload.to_string()],
        )?;
        Ok(())
    }

    pub fn messages(&self, thread_id: &str) -> Result<Vec<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT payload FROM messages WHERE thread_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![thread_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r?)?);
        }
        Ok(out)
    }

    /// Atomically replace a thread's provider transcript (context compaction).
    pub fn replace_messages(&self, thread_id: &str, payloads: &[serde_json::Value]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM messages WHERE thread_id = ?1",
            params![thread_id],
        )?;
        for (i, payload) in payloads.iter().enumerate() {
            tx.execute(
                "INSERT INTO messages (thread_id, seq, payload) VALUES (?1, ?2, ?3)",
                params![thread_id, (i + 1) as i64, payload.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // --- backend sessions ---------------------------------------------------
    // Vendor-agent session ids (Codex/Cursor/Claude Code) so external
    // backends resume the same conversation across turns and restarts.

    /// The vendor session to resume for this thread and backend, plus how
    /// many transcript messages that backend had seen when it last ran
    /// (anything after that happened under other models and needs handing
    /// off). Rows migrated from the pre-(thread, backend) schema live under
    /// backend '' and match any backend until a real turn writes a proper
    /// key.
    pub fn backend_session(&self, thread_id: &str, backend: &str) -> Result<Option<(String, u64)>> {
        let conn = self.conn.lock().unwrap();
        let row = conn
            .query_row(
                "SELECT backend_session_id, seen_messages FROM backend_sessions
                 WHERE thread_id = ?1 AND backend IN (?2, '')
                 ORDER BY backend DESC LIMIT 1",
                params![thread_id, backend],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        row.map(|(id, seen)| {
            let seen = u64::try_from(seen).context("backend seen_messages was negative")?;
            Ok((id, seen))
        })
        .transpose()
    }

    pub fn set_backend_session(
        &self,
        thread_id: &str,
        backend: &str,
        backend_session_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO backend_sessions (thread_id, backend, backend_session_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(thread_id, backend)
               DO UPDATE SET backend_session_id = excluded.backend_session_id",
            params![thread_id, backend, backend_session_id],
        )?;
        // A properly keyed row supersedes any migrated legacy fallback.
        conn.execute(
            "DELETE FROM backend_sessions WHERE thread_id = ?1 AND backend = ''",
            params![thread_id],
        )?;
        Ok(())
    }

    /// Record how far through the transcript a backend's vendor session is
    /// (called at the end of its turns). No-op when the backend never
    /// reported a session — with nothing to resume, the next turn hands
    /// off the whole history again anyway.
    pub fn mark_backend_seen(&self, thread_id: &str, backend: &str, seen: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let seen = i64::try_from(seen).context("backend seen_messages exceeds SQLite range")?;
        conn.execute(
            "UPDATE backend_sessions SET seen_messages = ?3
             WHERE thread_id = ?1 AND backend = ?2",
            params![thread_id, backend, seen],
        )?;
        Ok(())
    }

    // --- usage accounting -------------------------------------------------------

    /// Record a turn's usage. `usage` totals are summed across the turn's
    /// requests (correct for billing); `context_input_tokens` is the input
    /// size of the turn's *last* request — the only meaningful proxy for the
    /// current context size, since summing per-iteration inputs over a
    /// multi-tool turn inflates the figure many-fold and spuriously trips
    /// compaction.
    pub fn record_usage(
        &self,
        session_id: &str,
        thread_id: &str,
        turn: u64,
        usage: &trouve_protocol::Usage,
        context_input_tokens: u64,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO usage
             (thread_id, session_id, turn, input_tokens, output_tokens, cached_input_tokens, context_input_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                thread_id,
                session_id,
                turn as i64,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                usage.cached_input_tokens as i64,
                context_input_tokens as i64,
                usage.cost_usd
            ],
        )?;
        Ok(())
    }

    /// Context size (in tokens) of the thread's most recent turn: the last
    /// request's input, used by the compaction trigger and the UI usage
    /// indicator. Older rows recorded before this column existed report 0
    /// (the caller falls back to a character estimate).
    pub fn last_input_tokens(&self, thread_id: &str) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT context_input_tokens FROM usage
                 WHERE thread_id = ?1 ORDER BY turn DESC LIMIT 1",
                params![thread_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u64))
    }

    pub fn usage_summary(
        &self,
        scope_col: UsageScope<'_>,
    ) -> Result<trouve_protocol::UsageSummary> {
        let (col, id) = match scope_col {
            UsageScope::Thread(id) => ("thread_id", id),
            UsageScope::Session(id) => ("session_id", id),
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cached_input_tokens), 0), COALESCE(SUM(cost_usd), 0.0)
             FROM usage WHERE {col} = ?1"
        ))?;
        Ok(stmt.query_row(params![id], |r| {
            Ok(trouve_protocol::UsageSummary {
                turns: r.get::<_, i64>(0)? as u64,
                input_tokens: r.get::<_, i64>(1)? as u64,
                output_tokens: r.get::<_, i64>(2)? as u64,
                cached_input_tokens: r.get::<_, i64>(3)? as u64,
                cost_usd: r.get(4)?,
            })
        })?)
    }

    // --- checkpoints ----------------------------------------------------------

    /// Append a checkpoint, truncating any redo tail past the current undo
    /// position (standard undo-stack semantics).
    pub fn append_checkpoint(&self, row: &CheckpointRow) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // One transaction: truncating the redo tail, clearing undo_pos, and
        // inserting the checkpoint must be all-or-nothing, or a crash between
        // them loses the redo tail without recording the new checkpoint.
        let tx = conn.unchecked_transaction()?;
        let undo_pos: Option<i64> = tx.query_row(
            "SELECT undo_pos FROM sessions WHERE id = ?1",
            params![row.session_id],
            |r| r.get(0),
        )?;
        if let Some(pos) = undo_pos {
            tx.execute(
                "DELETE FROM checkpoints WHERE session_id = ?1 AND seq > ?2",
                params![row.session_id, pos],
            )?;
            tx.execute(
                "UPDATE sessions SET undo_pos = NULL WHERE id = ?1",
                params![row.session_id],
            )?;
        }
        tx.execute(
            "INSERT INTO checkpoints (id, session_id, thread_id, turn, seq, commit_hash, created_at)
             VALUES (?1, ?2, ?3, ?4,
                     (SELECT COALESCE(MAX(seq), -1) + 1 FROM checkpoints WHERE session_id = ?2),
                     ?5, ?6)",
            params![
                row.id,
                row.session_id,
                row.thread_id,
                row.turn as i64,
                row.commit_hash,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn checkpoint_at(&self, session_id: &str, seq: i64) -> Result<Option<CheckpointRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, session_id, thread_id, turn, seq, commit_hash FROM checkpoints
             WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq],
            row_to_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn latest_checkpoint_seq(&self, session_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT MAX(seq) FROM checkpoints WHERE session_id = ?1",
            params![session_id],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    }

    pub fn undo_pos(&self, session_id: &str) -> Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT undo_pos FROM sessions WHERE id = ?1",
            params![session_id],
            |r| r.get(0),
        )?)
    }

    pub fn set_undo_pos(&self, session_id: &str, pos: Option<i64>) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sessions SET undo_pos = ?2 WHERE id = ?1",
            params![session_id, pos],
        )?;
        Ok(())
    }
}

fn permission_mode_str(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "ask",
        PermissionMode::AllowList => "allow_list",
        PermissionMode::Yolo => "yolo",
    }
}

fn permission_mode_from(s: &str) -> PermissionMode {
    match s {
        "allow_list" => PermissionMode::AllowList,
        "yolo" => PermissionMode::Yolo,
        _ => PermissionMode::Ask,
    }
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        title: r.get(2)?,
        branch: r.get(3)?,
        worktree_path: r.get(4)?,
        base_ref: r.get(5)?,
        archived: r.get(6)?,
        // Activity is runtime state owned by the engine, not persisted.
        active: false,
        created_at: r
            .get::<_, String>(7)?
            .parse()
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

/// Columns matching `row_to_thread`, including the agent-spawned flag.
const THREAD_COLUMNS: &str = "id, session_id, mode, model, permission_mode, model_options, \
     created_at, EXISTS(SELECT 1 FROM spawned_threads st WHERE st.child_thread_id = threads.id), \
     todos";

fn row_to_thread(r: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: r.get(0)?,
        session_id: r.get(1)?,
        mode: r.get(2)?,
        model: r.get(3)?,
        permission_mode: permission_mode_from(&r.get::<_, String>(4)?),
        model_options: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
        created_at: r
            .get::<_, String>(6)?
            .parse()
            .unwrap_or_else(|_| chrono::Utc::now()),
        spawned: r.get(7)?,
        todos: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
    })
}

fn row_to_checkpoint(r: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointRow> {
    Ok(CheckpointRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        thread_id: r.get(2)?,
        turn: r.get::<_, i64>(3)? as u64,
        seq: r.get(4)?,
        commit_hash: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trouve_protocol::Event;

    #[test]
    fn append_and_replay_events() {
        let store = Store::open_in_memory().unwrap();
        let scope = Scope::Thread("th_1".into());
        for i in 0..3 {
            store
                .append_event(
                    scope.clone(),
                    Event::AssistantDelta {
                        turn: 1,
                        text: format!("d{i}"),
                    },
                )
                .unwrap();
        }
        // Unrelated scope must not leak into replay.
        store
            .append_event(
                Scope::Thread("th_2".into()),
                Event::AssistantDelta {
                    turn: 1,
                    text: "other".into(),
                },
            )
            .unwrap();

        let all = store.events_after(&scope, 0).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.windows(2).all(|w| w[0].cursor < w[1].cursor));

        let tail = store.events_after(&scope, all[0].cursor).unwrap();
        assert_eq!(tail.len(), 2);
    }

    #[test]
    fn replay_silently_skips_retired_events() {
        let store = Store::open_in_memory().unwrap();
        let retired_payload = serde_json::json!({
            "type": "workspace.pull_requests_updated",
            "workspace_id": "ws_1",
            "pull_requests": { "viewer": "octocat", "prs": [] },
        })
        .to_string();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO events (scope_kind, scope_id, ts, payload) VALUES ('server', '', ?1, ?2)",
                params![chrono::Utc::now().to_rfc3339(), retired_payload],
            )
            .unwrap();
        store
            .append_event(
                Scope::Server,
                Event::WorkspaceRegistered {
                    workspace_id: "ws_1".into(),
                    path: "/tmp/workspace".into(),
                },
            )
            .unwrap();

        let replayed = store.events_after(&Scope::Server, 0).unwrap();
        assert_eq!(replayed.len(), 1);
        assert!(matches!(
            replayed[0].event,
            Event::WorkspaceRegistered { .. }
        ));
    }

    #[test]
    fn concurrent_appends_persist_and_broadcast_in_cursor_order() {
        let store = Store::open_in_memory().unwrap();
        let mut rx = store.subscribe();
        let writers: Vec<_> = (0..4)
            .map(|t| {
                let store = store.clone();
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let env = store
                            .append_event(
                                Scope::Thread(format!("th_{t}")),
                                Event::AssistantDelta {
                                    turn: 1,
                                    text: format!("d{i}"),
                                },
                            )
                            .unwrap();
                        assert!(env.cursor > 0);
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        // Broadcast order must match cursor order even when appends from
        // different threads were committed in shared batches.
        let mut last = 0;
        for _ in 0..200 {
            let env = rx.try_recv().unwrap();
            assert!(env.cursor > last, "broadcast out of cursor order");
            last = env.cursor;
        }
        for t in 0..4 {
            let events = store
                .events_after(&Scope::Thread(format!("th_{t}")), 0)
                .unwrap();
            assert_eq!(events.len(), 50);
        }
    }

    #[test]
    fn append_returns_promptly_when_event_writer_exits() {
        let store = Store::open_in_memory().unwrap();
        let conn = Arc::clone(&store.conn);
        assert!(
            std::thread::spawn(move || {
                let _guard = conn.lock().unwrap();
                panic!("poison event-writer connection");
            })
            .join()
            .is_err()
        );

        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let append = std::thread::spawn(move || {
            result_tx
                .send(store.append_event(
                    Scope::Server,
                    Event::AssistantDelta {
                        turn: 1,
                        text: "unwritten".into(),
                    },
                ))
                .unwrap();
        });

        let result = result_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("append_event blocked after event writer exited");
        assert_eq!(
            result.unwrap_err().to_string(),
            "event writer thread has exited"
        );
        append.join().unwrap();
    }

    #[test]
    fn live_subscription_receives_appends() {
        let store = Store::open_in_memory().unwrap();
        let mut rx = store.subscribe();
        store
            .append_event(
                Scope::Server,
                Event::WorkspaceRegistered {
                    workspace_id: "ws_1".into(),
                    path: "/tmp/x".into(),
                },
            )
            .unwrap();
        let got = rx.try_recv().unwrap();
        assert_eq!(got.scope, Scope::Server);
    }

    #[test]
    fn session_rename_and_archive_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let ws = Workspace {
            id: "ws_1".into(),
            name: "x".into(),
            path: "/tmp/repo".into(),
        };
        store.insert_workspace(&ws).unwrap();
        let session = Session {
            id: "se_1".into(),
            workspace_id: ws.id.clone(),
            title: "before".into(),
            branch: "trouve/before".into(),
            worktree_path: "/tmp/wt".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();

        store
            .update_session("se_1", Some("after"), Some(true))
            .unwrap();
        let got = store.session("se_1").unwrap().unwrap();
        assert_eq!(got.title, "after");
        assert!(got.archived);

        // Partial update leaves the other field alone.
        store.update_session("se_1", None, Some(false)).unwrap();
        let got = store.session("se_1").unwrap().unwrap();
        assert_eq!(got.title, "after");
        assert!(!got.archived);
    }

    #[test]
    fn delete_session_clears_backend_session_links() {
        let store = Store::open_in_memory().unwrap();
        let ws = Workspace {
            id: "ws_1".into(),
            name: "x".into(),
            path: "/tmp/repo".into(),
        };
        store.insert_workspace(&ws).unwrap();
        let session = Session {
            id: "se_1".into(),
            workspace_id: ws.id.clone(),
            title: "t".into(),
            branch: "b".into(),
            worktree_path: "/tmp/wt".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_1".into(),
            session_id: "se_1".into(),
            mode: "code".into(),
            model: "p/m".into(),
            model_options: serde_json::Map::new(),
            permission_mode: PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store
            .insert_thread(&thread, &serde_json::Map::new())
            .unwrap();
        // The vendor-resume link is what used to trip the FK constraint.
        store
            .set_backend_session("th_1", "cursor", "vendor-abc")
            .unwrap();
        store.enqueue_prompt("th_1", "pending", &[]).unwrap();
        // Attachments and spawned-thread rows also FK to threads and would
        // otherwise fail the delete.
        store
            .add_attachment(
                "th_1",
                &trouve_protocol::Attachment {
                    id: "at_1".into(),
                    name: "shot.png".into(),
                    mime: "image/png".into(),
                    size_bytes: 3,
                },
                "/data/attachments/at_1.png",
            )
            .unwrap();
        let child = Thread {
            id: "th_child".into(),
            session_id: "se_1".into(),
            mode: "code".into(),
            model: "p/m".into(),
            model_options: serde_json::Map::new(),
            permission_mode: PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: true,
            todos: Vec::new(),
        };
        store
            .insert_thread(&child, &serde_json::Map::new())
            .unwrap();
        store.insert_spawned("th_child", "th_1", "thread").unwrap();

        store.delete_session("se_1").unwrap();
        assert!(store.session("se_1").unwrap().is_none());
        assert!(store.backend_session("th_1", "cursor").unwrap().is_none());
        assert!(store.queued_prompts("th_1").unwrap().is_empty());
        assert!(store.attachment("at_1").unwrap().is_none());
    }

    /// Vendor sessions are keyed per backend: swapping cursor → claude →
    /// cursor must not lose cursor's resume id. Rows migrated from the old
    /// one-per-thread schema (backend '') match any backend until a real
    /// turn writes a proper key.
    #[test]
    fn backend_sessions_keyed_per_backend_with_legacy_fallback() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_bs");

        store
            .set_backend_session("th_bs", "cursor", "cursor-sess")
            .unwrap();
        store
            .set_backend_session("th_bs", "claude", "claude-sess")
            .unwrap();
        store.mark_backend_seen("th_bs", "cursor", 4).unwrap();
        assert_eq!(
            store.backend_session("th_bs", "cursor").unwrap(),
            Some(("cursor-sess".into(), 4))
        );
        assert_eq!(
            store.backend_session("th_bs", "claude").unwrap(),
            Some(("claude-sess".into(), 0))
        );
        assert_eq!(store.backend_session("th_bs", "codex").unwrap(), None);
        // Marking an unknown (thread, backend) is a no-op, not an insert.
        store.mark_backend_seen("th_bs", "codex", 9).unwrap();
        assert_eq!(store.backend_session("th_bs", "codex").unwrap(), None);

        // Legacy fallback: a backend-less row (as migrated) matches any
        // backend, and the first properly keyed write clears it.
        seed_thread(&store, "th_legacy");
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO backend_sessions (thread_id, backend, backend_session_id)
                 VALUES ('th_legacy', '', 'old-sess')",
                [],
            )
            .unwrap();
        assert_eq!(
            store.backend_session("th_legacy", "cursor").unwrap(),
            Some(("old-sess".into(), 0))
        );
        store
            .set_backend_session("th_legacy", "cursor", "new-sess")
            .unwrap();
        assert_eq!(
            store.backend_session("th_legacy", "claude").unwrap(),
            None,
            "legacy fallback row should be gone after a keyed write"
        );
    }

    /// Opening a database created before backend_sessions was keyed by
    /// (thread, backend) rebuilds the table and keeps the rows.
    #[test]
    fn backend_sessions_migrates_legacy_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE threads (id TEXT PRIMARY KEY);
                 CREATE TABLE backend_sessions (
                   thread_id TEXT PRIMARY KEY REFERENCES threads(id),
                   backend_session_id TEXT NOT NULL
                 );
                 INSERT INTO threads (id) VALUES ('th_old');
                 INSERT INTO backend_sessions VALUES ('th_old', 'vendor-legacy');",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.backend_session("th_old", "anything").unwrap(),
            Some(("vendor-legacy".into(), 0))
        );
    }

    /// Workspace + session + thread rows so FK-checked inserts succeed.
    fn seed_thread(store: &Store, thread_id: &str) {
        if store.workspace("ws_q").unwrap().is_none() {
            store
                .insert_workspace(&Workspace {
                    id: "ws_q".into(),
                    name: "x".into(),
                    path: format!("/tmp/repo-{thread_id}"),
                })
                .unwrap();
            store
                .insert_session(&Session {
                    id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                    title: "t".into(),
                    branch: "b".into(),
                    worktree_path: "/tmp/wt".into(),
                    base_ref: "main".into(),
                    archived: false,
                    active: false,
                    created_at: chrono::Utc::now(),
                })
                .unwrap();
        }
        store
            .insert_thread(
                &Thread {
                    id: thread_id.into(),
                    session_id: "se_q".into(),
                    mode: "code".into(),
                    model: "p/m".into(),
                    model_options: serde_json::Map::new(),
                    permission_mode: PermissionMode::Ask,
                    created_at: chrono::Utc::now(),
                    spawned: false,
                    todos: Vec::new(),
                },
                &serde_json::Map::new(),
            )
            .unwrap();
    }

    #[test]
    fn thread_todos_round_trip_without_leaking_to_siblings() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_todo_1");
        seed_thread(&store, "th_todo_2");
        let todos = vec![trouve_protocol::TodoItem {
            id: "build".into(),
            content: "Build the feature".into(),
            status: trouve_protocol::TodoStatus::InProgress,
        }];

        store.update_thread_todos("th_todo_1", &todos).unwrap();

        assert_eq!(store.thread("th_todo_1").unwrap().unwrap().todos, todos);
        assert!(store.thread("th_todo_2").unwrap().unwrap().todos.is_empty());
    }

    #[test]
    fn queued_prompts_crud_pop_and_reorder() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_1");
        seed_thread(&store, "th_2");
        let a = store.enqueue_prompt("th_1", "first", &[]).unwrap();
        let b = store.enqueue_prompt("th_1", "second", &[]).unwrap();
        let c = store.enqueue_prompt("th_1", "third", &[]).unwrap();
        store.enqueue_prompt("th_2", "other thread", &[]).unwrap();

        let q = store.queued_prompts("th_1").unwrap();
        assert_eq!(
            q.iter().map(|p| p.content.as_str()).collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        assert_eq!(store.queued_prompt_thread(&a.id).unwrap().unwrap(), "th_1");

        // Edit and delete.
        assert!(store.update_queued_prompt(&b.id, "second v2").unwrap());
        assert!(store.delete_queued_prompt(&a.id).unwrap());
        assert!(!store.delete_queued_prompt(&a.id).unwrap());

        // Reorder requires the exact current id set...
        assert!(
            !store
                .reorder_queued_prompts("th_1", std::slice::from_ref(&c.id))
                .unwrap()
        );
        // ...and applies the given order when it matches.
        assert!(
            store
                .reorder_queued_prompts("th_1", &[c.id.clone(), b.id.clone()])
                .unwrap()
        );

        // Claim hides the prompt while a dispatcher prepares the turn;
        // releasing makes it visible again, finishing consumes it.
        let p1 = store.claim_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(p1.content, "third");
        assert_eq!(store.queued_prompts("th_1").unwrap().len(), 1);
        assert!(store.release_queued_prompt(&p1.id).unwrap());
        assert_eq!(store.queued_prompts("th_1").unwrap().len(), 2);
        let p1 = store.claim_queued_prompt("th_1").unwrap().unwrap();
        assert!(store.finish_queued_prompt(&p1.id).unwrap());
        let p2 = store.claim_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(p2.content, "second v2");
        assert!(store.finish_queued_prompt(&p2.id).unwrap());
        assert!(store.claim_queued_prompt("th_1").unwrap().is_none());

        // The other thread's queue is untouched.
        assert_eq!(store.queued_prompts("th_2").unwrap().len(), 1);
    }

    #[test]
    fn queued_prompts_survive_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("q.db");
        {
            let store = Store::open(&path).unwrap();
            seed_thread(&store, "th_1");
            store.enqueue_prompt("th_1", "keep me", &[]).unwrap();
            let claimed = store.claim_queued_prompt("th_1").unwrap().unwrap();
            assert_eq!(claimed.content, "keep me");
            assert!(store.queued_prompts("th_1").unwrap().is_empty());
        }
        let store = Store::open(&path).unwrap();
        let q = store.queued_prompts("th_1").unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].content, "keep me");
    }

    #[test]
    fn automations_round_trip_and_record_runs() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_workspace(&trouve_protocol::Workspace {
                id: "ws_1".into(),
                name: "proj".into(),
                path: "/tmp/proj".into(),
            })
            .unwrap();
        let auto = trouve_protocol::Automation {
            id: "auto_1".into(),
            name: "Nightly triage".into(),
            prompt: "Review open issues".into(),
            workspace_id: "ws_1".into(),
            mode: Some("code".into()),
            model: None,
            permission_mode: PermissionMode::Yolo,
            schedule: trouve_protocol::AutomationSchedule {
                kind: "weekly".into(),
                minute: 0,
                time: "09:00".into(),
                days: vec![0, 4],
            },
            enabled: true,
            next_run_at: Some("2026-07-13T09:00:00-04:00".into()),
            last_run_at: None,
            last_session_id: None,
            last_error: String::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        store.insert_automation(&auto).unwrap();

        let listed = store.list_automations().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].schedule, auto.schedule);
        assert_eq!(listed[0].mode.as_deref(), Some("code"));
        assert_eq!(listed[0].permission_mode, PermissionMode::Yolo);

        // Edit: rename + disable clears the next fire time.
        let mut edited = auto.clone();
        edited.name = "Morning triage".into();
        edited.enabled = false;
        edited.next_run_at = None;
        edited.permission_mode = PermissionMode::AllowList;
        assert!(store.update_automation(&edited).unwrap());
        let got = store.automation("auto_1").unwrap().unwrap();
        assert_eq!(got.name, "Morning triage");
        assert!(!got.enabled);
        assert!(got.next_run_at.is_none());
        assert_eq!(got.permission_mode, PermissionMode::AllowList);

        // A run records its outcome without touching the definition.
        store
            .mark_automation_run(
                "auto_1",
                "2026-07-13T09:00:01-04:00",
                Some("sess_9"),
                "",
                Some("2026-07-17T09:00:00-04:00"),
            )
            .unwrap();
        let got = store.automation("auto_1").unwrap().unwrap();
        assert_eq!(got.last_session_id.as_deref(), Some("sess_9"));
        assert_eq!(got.last_error, "");
        assert_eq!(got.name, "Morning triage");
        store
            .set_automation_result("auto_1", "provider failed")
            .unwrap();
        let got = store.automation("auto_1").unwrap().unwrap();
        assert_eq!(got.last_error, "provider failed");
        assert_eq!(got.last_session_id.as_deref(), Some("sess_9"));

        assert!(store.delete_automation("auto_1").unwrap());
        assert!(!store.delete_automation("auto_1").unwrap());
        assert!(store.list_automations().unwrap().is_empty());
    }

    #[test]
    fn attachments_round_trip_and_ride_the_queue() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_1");
        let att = trouve_protocol::Attachment {
            id: "at_1".into(),
            name: "shot.png".into(),
            mime: "image/png".into(),
            size_bytes: 42,
        };
        store
            .add_attachment("th_1", &att, "/data/attachments/at_1.png")
            .unwrap();
        let (meta, path) = store.attachment("at_1").unwrap().unwrap();
        assert_eq!(meta, att);
        assert_eq!(path, "/data/attachments/at_1.png");
        assert!(store.attachment("at_missing").unwrap().is_none());

        store
            .enqueue_prompt("th_1", "with file", std::slice::from_ref(&att))
            .unwrap();
        let q = store.queued_prompts("th_1").unwrap();
        assert_eq!(q[0].attachments, vec![att.clone()]);
        let claimed = store.claim_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(claimed.attachments, vec![att]);
    }

    #[test]
    fn replace_messages_swaps_transcript() {
        let store = Store::open_in_memory().unwrap();
        let ws = Workspace {
            id: "ws_1".into(),
            name: "x".into(),
            path: "/tmp/repo".into(),
        };
        store.insert_workspace(&ws).unwrap();
        let session = Session {
            id: "se_1".into(),
            workspace_id: ws.id.clone(),
            title: "t".into(),
            branch: "b".into(),
            worktree_path: "/tmp/wt".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_1".into(),
            session_id: "se_1".into(),
            mode: "code".into(),
            model: "p/m".into(),
            model_options: serde_json::Map::new(),
            permission_mode: PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };
        store
            .insert_thread(&thread, &serde_json::Map::new())
            .unwrap();

        for i in 0..3 {
            store
                .append_message("th_1", &serde_json::json!({"i": i}))
                .unwrap();
        }
        store
            .replace_messages("th_1", &[serde_json::json!({"summary": true})])
            .unwrap();
        let msgs = store.messages("th_1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["summary"], true);
        // Appending after a replace continues the sequence.
        store
            .append_message("th_1", &serde_json::json!({"i": 99}))
            .unwrap();
        assert_eq!(store.messages("th_1").unwrap().len(), 2);
    }

    #[test]
    fn checkpoint_undo_stack_truncates_redo_tail() {
        let store = Store::open_in_memory().unwrap();
        let ws = Workspace {
            id: "ws_1".into(),
            name: "x".into(),
            path: "/tmp/repo".into(),
        };
        store.insert_workspace(&ws).unwrap();
        let session = Session {
            id: "se_1".into(),
            workspace_id: ws.id.clone(),
            title: "t".into(),
            branch: "trouve/t".into(),
            worktree_path: "/tmp/wt".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        for (i, hash) in ["c0", "c1", "c2"].iter().enumerate() {
            store
                .append_checkpoint(&CheckpointRow {
                    id: format!("cp_{i}"),
                    session_id: "se_1".into(),
                    thread_id: None,
                    turn: i as u64,
                    seq: 0, // assigned by the store
                    commit_hash: hash.to_string(),
                })
                .unwrap();
        }
        assert_eq!(store.latest_checkpoint_seq("se_1").unwrap(), Some(2));
        // Simulate undo to seq 0, then a new checkpoint: seq 1-2 replaced.
        store.set_undo_pos("se_1", Some(0)).unwrap();
        store
            .append_checkpoint(&CheckpointRow {
                id: "cp_new".into(),
                session_id: "se_1".into(),
                thread_id: None,
                turn: 9,
                seq: 0,
                commit_hash: "c1b".into(),
            })
            .unwrap();
        assert_eq!(store.latest_checkpoint_seq("se_1").unwrap(), Some(1));
        assert_eq!(store.undo_pos("se_1").unwrap(), None);
        assert_eq!(
            store.checkpoint_at("se_1", 1).unwrap().unwrap().commit_hash,
            "c1b"
        );
    }

    #[test]
    fn closing_workspace_hides_it_without_deleting_it() {
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_close".into(),
            name: "close me".into(),
            path: "/tmp/close-me".into(),
        };
        store.insert_workspace(&workspace).unwrap();

        assert!(store.set_workspace_closed(&workspace.id, true).unwrap());
        assert!(store.list_workspaces().unwrap().is_empty());
        assert!(store.open_workspace(&workspace.id).unwrap().is_none());
        assert_eq!(
            store.workspace(&workspace.id).unwrap().unwrap().path,
            workspace.path
        );

        assert!(store.set_workspace_closed(&workspace.id, false).unwrap());
        assert!(store.open_workspace(&workspace.id).unwrap().is_some());
        let reopened = store.list_workspaces().unwrap();
        assert_eq!(reopened.len(), 1);
        assert_eq!(reopened[0].id, workspace.id);
    }

    #[test]
    fn code_review_policy_queue_and_manual_generations_are_durable() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_discovered_code_review_repository(7, "acme/widgets", true)
            .unwrap();
        let discovered = store.list_code_review_repositories().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].mode, trouve_protocol::CodeReviewMode::Off);
        assert!(discovered[0].private);

        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Automatic,
                model: Some("openai/gpt-5".into()),
                prompt: "focus on concurrency".into(),
                reviewer_ids: Some(crate::reviewers::default_reviewer_ids()),
                reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                    reviewer_id: "security".into(),
                    model: Some("anthropic/security".into()),
                    prompt_mode: trouve_protocol::ReviewerPromptMode::Append,
                    prompt: "Focus on tenant boundaries.".into(),
                }]),
            })
            .unwrap();
        let configured = store.list_code_review_repositories().unwrap().remove(0);
        assert_eq!(configured.mode, trouve_protocol::CodeReviewMode::Automatic);
        assert_eq!(configured.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(configured.reviewer_overrides.len(), 1);

        assert_eq!(
            store
                .code_review_manual_transition("acme/widgets", 42, true)
                .unwrap(),
            Some(1)
        );
        assert_eq!(
            store
                .code_review_manual_transition("acme/widgets", 42, true)
                .unwrap(),
            None
        );
        store
            .code_review_manual_transition("acme/widgets", 42, false)
            .unwrap();
        assert_eq!(
            store
                .code_review_manual_transition("acme/widgets", 42, true)
                .unwrap(),
            Some(2)
        );

        let mut reviewers: Vec<_> = crate::reviewers::built_in_reviewers()
            .into_iter()
            .filter(|reviewer| configured.reviewer_ids.contains(&reviewer.id))
            .collect();
        reviewers[0].default_thinking_level = Some("high".into());
        let new_job = NewCodeReviewJob {
            dedupe_key: "acme/widgets#42:head:automatic:config".into(),
            installation_id: 7,
            repository: "acme/widgets".into(),
            pull_number: 42,
            pull_title: "Ship widgets".into(),
            pull_url: "https://github.com/acme/widgets/pull/42".into(),
            head_sha: "1111111111111111111111111111111111111111".into(),
            base_ref: "0000000000000000000000000000000000000000".into(),
            head_ref: "ship".into(),
            trigger: "automatic".into(),
            model: configured.model,
            prompt: configured.prompt,
            reviewers,
            config_hash: "config".into(),
        };
        let queued = store.enqueue_code_review_job(&new_job).unwrap().unwrap();
        assert_eq!(queued.status, "queued");
        assert_eq!(queued.reviewer_ids, configured.reviewer_ids);
        assert!(store.enqueue_code_review_job(&new_job).unwrap().is_none());
        assert!(store.code_review_job_exists(&new_job.dedupe_key).unwrap());
        let running = store.claim_code_review_job().unwrap().unwrap();
        assert_eq!(running.job.id, queued.id);
        assert_eq!(running.job.status, "running");
        assert_eq!(
            running.reviewers[0].default_thinking_level.as_deref(),
            Some("high")
        );
        store
            .set_code_review_job_session(&queued.id, "se_review", "th_review")
            .unwrap();
        store
            .finish_code_review_job(&queued.id, "succeeded", "https://review", "")
            .unwrap();
        let completed = store.list_code_review_jobs(10).unwrap().remove(0);
        assert_eq!(completed.status, "succeeded");
        assert_eq!(completed.session_id.as_deref(), Some("se_review"));
        assert_eq!(completed.thread_id.as_deref(), Some("th_review"));
        assert_eq!(
            store.pending_code_review_job_cleanups().unwrap(),
            vec![(queued.id.clone(), "se_review".into())]
        );
        store
            .complete_code_review_job_cleanup(&queued.id, "se_review")
            .unwrap();
        let completed = store.list_code_review_jobs(10).unwrap().remove(0);
        assert!(completed.session_id.is_none());
        assert!(completed.thread_id.is_none());
        assert!(store.pending_code_review_job_cleanups().unwrap().is_empty());

        assert!(
            store
                .claim_github_webhook_delivery(
                    "delivery-1",
                    Some(("acme/widgets", 42, "comment:100")),
                )
                .unwrap()
        );
        assert!(
            !store
                .claim_github_webhook_delivery(
                    "delivery-1",
                    Some(("acme/widgets", 42, "comment:duplicate")),
                )
                .unwrap()
        );
        assert_eq!(
            store
                .pending_code_review_manual_requests("acme/widgets")
                .unwrap(),
            vec![CodeReviewManualRequest {
                pull_number: 42,
                trigger_key: "comment:100".into(),
            }]
        );
        store
            .complete_code_review_manual_request("acme/widgets", 42, "comment:100")
            .unwrap();
        assert!(
            store
                .pending_code_review_manual_requests("acme/widgets")
                .unwrap()
                .is_empty()
        );

        assert!(
            !store
                .code_review_comment_poll_initialized("acme/widgets")
                .unwrap()
        );
        assert!(
            store
                .claim_code_review_polled_comment(
                    "acme/widgets",
                    200,
                    Some((43, "manual:comment:200")),
                )
                .unwrap()
        );
        assert!(
            store
                .code_review_comment_poll_initialized("acme/widgets")
                .unwrap()
        );
        assert!(
            !store
                .claim_code_review_polled_comment(
                    "acme/widgets",
                    200,
                    Some((43, "manual:comment:duplicate")),
                )
                .unwrap()
        );
        assert_eq!(
            store
                .pending_code_review_manual_requests("acme/widgets")
                .unwrap(),
            vec![CodeReviewManualRequest {
                pull_number: 43,
                trigger_key: "manual:comment:200".into(),
            }]
        );
    }

    #[test]
    fn terminal_code_review_jobs_are_cleanup_eligible() {
        let store = Store::open_in_memory().unwrap();
        let finish_job = |status: &str, suffix: &str| {
            let queued = store
                .enqueue_code_review_job(&NewCodeReviewJob {
                    dedupe_key: format!("acme/widgets#42:{suffix}"),
                    installation_id: 7,
                    repository: "acme/widgets".into(),
                    pull_number: 42,
                    pull_title: "Ship widgets".into(),
                    pull_url: "https://github.com/acme/widgets/pull/42".into(),
                    head_sha: "1111111111111111111111111111111111111111".into(),
                    base_ref: "0000000000000000000000000000000000000000".into(),
                    head_ref: "ship".into(),
                    trigger: "automatic".into(),
                    model: None,
                    prompt: String::new(),
                    reviewers: Vec::new(),
                    config_hash: "config".into(),
                })
                .unwrap()
                .unwrap();
            assert_eq!(
                store.claim_code_review_job().unwrap().unwrap().job.id,
                queued.id
            );
            let session_id = format!("se_{suffix}");
            store
                .set_code_review_job_session(&queued.id, &session_id, &format!("th_{suffix}"))
                .unwrap();
            store
                .finish_code_review_job(&queued.id, status, "", status)
                .unwrap();
            (queued.id, session_id)
        };

        let mut expected = [
            finish_job("succeeded", "succeeded"),
            finish_job("failed", "failed"),
            finish_job("stale", "stale"),
        ];
        let mut pending = store.pending_code_review_job_cleanups().unwrap();
        expected.sort();
        pending.sort();
        assert_eq!(pending, expected);

        for (job_id, session_id) in expected {
            store
                .complete_code_review_job_cleanup(&job_id, &session_id)
                .unwrap();
            let job = store.code_review_job(&job_id).unwrap().unwrap().job;
            assert!(job.session_id.is_none());
            assert!(job.thread_id.is_none());
        }
        assert!(store.pending_code_review_job_cleanups().unwrap().is_empty());
    }

    #[test]
    fn custom_reviewer_profiles_are_durable_and_removed_from_policies() {
        let store = Store::open_in_memory().unwrap();
        let reviewer = trouve_protocol::ReviewerProfile {
            id: "custom:domain".into(),
            name: "Domain invariants".into(),
            prompt: "Check widget state transitions.".into(),
            model: Some("openai/gpt-5".into()),
            default_thinking_level: Some("high".into()),
            built_in: false,
        };
        store.upsert_reviewer_profile(&reviewer).unwrap();
        let reviewers = store.list_custom_reviewer_profiles().unwrap();
        assert_eq!(reviewers.as_slice(), std::slice::from_ref(&reviewer));
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Automatic,
                model: None,
                prompt: String::new(),
                reviewer_ids: Some(vec![reviewer.id.clone()]),
                reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                    reviewer_id: reviewer.id.clone(),
                    model: Some("anthropic/domain".into()),
                    prompt_mode: trouve_protocol::ReviewerPromptMode::Replace,
                    prompt: "Use repository-specific invariants.".into(),
                }]),
            })
            .unwrap();
        let repositories = store.list_code_review_repositories().unwrap();
        assert_eq!(
            repositories[0].reviewer_ids.as_slice(),
            std::slice::from_ref(&reviewer.id)
        );
        assert_eq!(repositories[0].reviewer_overrides.len(), 1);

        assert!(store.delete_custom_reviewer_profile(&reviewer.id).unwrap());
        assert!(store.list_custom_reviewer_profiles().unwrap().is_empty());
        assert_eq!(
            store.list_code_review_repositories().unwrap()[0].reviewer_ids,
            crate::reviewers::default_reviewer_ids()
        );
        assert!(
            store.list_code_review_repositories().unwrap()[0]
                .reviewer_overrides
                .is_empty()
        );
    }

    #[test]
    fn built_in_reviewer_defaults_are_durable_and_separate_from_custom_profiles() {
        let store = Store::open_in_memory().unwrap();
        let mut reviewer = crate::reviewers::built_in_reviewers().remove(0);
        reviewer.model = Some("anthropic/claude-sonnet".into());
        reviewer.default_thinking_level = Some("high".into());

        store.upsert_reviewer_profile(&reviewer).unwrap();

        assert!(store.list_custom_reviewer_profiles().unwrap().is_empty());
        assert_eq!(
            store.list_built_in_reviewer_defaults().unwrap(),
            vec![reviewer.clone()]
        );
        assert!(!store.delete_custom_reviewer_profile(&reviewer.id).unwrap());
        assert_eq!(
            store.list_built_in_reviewer_defaults().unwrap(),
            vec![reviewer]
        );
    }

    #[test]
    fn newer_pull_revision_supersedes_queued_and_running_jobs() {
        let store = Store::open_in_memory().unwrap();
        let enqueue = |suffix: &str, base_ref: &str, head_sha: &str, config_hash: &str| {
            store
                .enqueue_code_review_job(&NewCodeReviewJob {
                    dedupe_key: format!("acme/widgets#42:{suffix}"),
                    installation_id: 7,
                    repository: "acme/widgets".into(),
                    pull_number: 42,
                    pull_title: "Ship widgets".into(),
                    pull_url: "https://github.com/acme/widgets/pull/42".into(),
                    head_sha: head_sha.into(),
                    base_ref: base_ref.into(),
                    head_ref: "ship".into(),
                    trigger: "automatic".into(),
                    model: None,
                    prompt: String::new(),
                    reviewers: Vec::new(),
                    config_hash: config_hash.into(),
                })
                .unwrap()
                .unwrap()
        };

        let old_head = enqueue("old-head", "base-2", "head-1", "config");
        assert_eq!(
            store.claim_code_review_job().unwrap().unwrap().job.id,
            old_head.id
        );
        assert!(
            store
                .set_code_review_job_session(&old_head.id, "se_old", "th_old")
                .unwrap()
        );
        let old_base = enqueue("old-base", "base-1", "head-2", "config");
        let old_config = enqueue("old-config", "base-2", "head-2", "old-config");
        let current = enqueue("current", "base-2", "head-2", "config");

        let mut superseded = store
            .supersede_code_review_jobs("acme/widgets", 42, "base-2", "head-2", "config")
            .unwrap();
        let mut expected = vec![
            old_head.id.clone(),
            old_base.id.clone(),
            old_config.id.clone(),
        ];
        superseded.sort();
        expected.sort();
        assert_eq!(superseded, expected);
        assert_eq!(
            store
                .code_review_job(&old_head.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "stale"
        );
        assert_eq!(
            store
                .code_review_job(&old_base.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "stale"
        );
        assert_eq!(
            store
                .code_review_job(&current.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "queued"
        );
        assert!(
            !store
                .set_code_review_job_session(&old_base.id, "se_late", "th_late")
                .unwrap()
        );
        assert!(
            !store
                .finish_code_review_job(&old_head.id, "failed", "", "cancelled")
                .unwrap()
        );
        assert_eq!(
            store.pending_code_review_job_cleanups().unwrap(),
            vec![(old_head.id, "se_old".into())]
        );
    }
}
