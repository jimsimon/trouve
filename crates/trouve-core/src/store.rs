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
  schedule TEXT NOT NULL,       -- JSON trouve_protocol::AutomationSchedule
  enabled INTEGER NOT NULL DEFAULT 1,
  next_run_at TEXT,             -- RFC3339; NULL while disabled
  last_run_at TEXT,
  last_session_id TEXT,
  last_error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL
);
"#;

/// Additive migrations for databases created before a column existed.
/// `CREATE TABLE IF NOT EXISTS` won't touch existing tables, so each entry
/// is applied and "duplicate column" errors are ignored.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE queued_prompts ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
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
    let schedule_json: String = r.get(6)?;
    Ok(trouve_protocol::Automation {
        id: r.get(0)?,
        name: r.get(1)?,
        prompt: r.get(2)?,
        workspace_id: r.get(3)?,
        mode: r.get(4)?,
        model: r.get(5)?,
        schedule: serde_json::from_str(&schedule_json).unwrap_or(
            trouve_protocol::AutomationSchedule {
                kind: "daily".into(),
                minute: 0,
                time: "09:00".into(),
                days: vec![],
            },
        ),
        enabled: r.get(7)?,
        next_run_at: r.get(8)?,
        last_run_at: r.get(9)?,
        last_session_id: r.get(10)?,
        last_error: r.get(11)?,
        created_at: r.get(12)?,
    })
}

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
        let (events_tx, _) = broadcast::channel(4096);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            events_tx,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        // Match on-disk behavior so tests exercise the same constraints.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&conn)?;
        let (events_tx, _) = broadcast::channel(4096);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            events_tx,
        })
    }

    // --- event log --------------------------------------------------------

    /// The single append chokepoint: persist first, then publish, so a
    /// subscriber can never observe an event that wouldn't survive a crash.
    pub fn append_event(&self, scope: Scope, event: Event) -> Result<EventEnvelope> {
        let ts = chrono::Utc::now();
        let payload = serde_json::to_string(&event)?;
        let (kind, id) = scope_cols(&scope);
        let cursor: u64 = {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO events (scope_kind, scope_id, ts, payload) VALUES (?1, ?2, ?3, ?4)",
                params![kind, id, ts.to_rfc3339(), payload],
            )?;
            conn.last_insert_rowid() as u64
        };
        let envelope = EventEnvelope {
            cursor,
            scope,
            ts,
            event,
        };
        // Nobody listening is fine.
        let _ = self.events_tx.send(envelope.clone());
        Ok(envelope)
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
            out.push(EventEnvelope {
                cursor,
                scope: scope_from_cols(&kind, id),
                ts: ts.parse().unwrap_or_else(|_| chrono::Utc::now()),
                event: serde_json::from_str(&payload)?,
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
        let mut stmt = conn.prepare("SELECT id, name, path FROM workspaces ORDER BY created_at")?;
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
            "INSERT INTO threads (id, session_id, mode, model, permission_mode, model_options, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                t.id,
                t.session_id,
                t.mode,
                t.model,
                permission_mode_str(t.permission_mode),
                serde_json::to_string(model_options)?,
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
             WHERE thread_id = ?1 ORDER BY position",
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
            "UPDATE queued_prompts SET content = ?2 WHERE id = ?1",
            params![id, content],
        )?;
        Ok(n > 0)
    }

    pub fn delete_queued_prompt(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute("DELETE FROM queued_prompts WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Apply a full new order. `ids` must be exactly the thread's current
    /// queue; returns false (changing nothing) when it isn't, so a reorder
    /// racing a dispatch fails cleanly instead of corrupting positions.
    pub fn reorder_queued_prompts(&self, thread_id: &str, ids: &[String]) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut current: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT id FROM queued_prompts WHERE thread_id = ?1 ORDER BY position")?;
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
                "UPDATE queued_prompts SET position = ?2 WHERE id = ?1",
                params![id, (i + 1) as i64],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Remove and return the front of the thread's queue.
    pub fn pop_queued_prompt(
        &self,
        thread_id: &str,
    ) -> Result<Option<trouve_protocol::QueuedPrompt>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let front = tx
            .query_row(
                "SELECT id, position, content, attachments, created_at FROM queued_prompts
                 WHERE thread_id = ?1 ORDER BY position LIMIT 1",
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
            tx.execute("DELETE FROM queued_prompts WHERE id = ?1", params![p.id])?;
        }
        tx.commit()?;
        Ok(front)
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
            "INSERT INTO automations (id, name, prompt, workspace_id, mode, model, schedule,
                                      enabled, next_run_at, last_run_at, last_session_id,
                                      last_error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
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
                    model = ?6, schedule = ?7, enabled = ?8, next_run_at = ?9
             WHERE id = ?1",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
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
            "SELECT id, name, prompt, workspace_id, mode, model, schedule, enabled,
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
                "SELECT id, name, prompt, workspace_id, mode, model, schedule, enabled,
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
        Ok(conn
            .query_row(
                "SELECT backend_session_id, seen_messages FROM backend_sessions
                 WHERE thread_id = ?1 AND backend IN (?2, '')
                 ORDER BY backend DESC LIMIT 1",
                params![thread_id, backend],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
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
        let undo_pos: Option<i64> = conn.query_row(
            "SELECT undo_pos FROM sessions WHERE id = ?1",
            params![row.session_id],
            |r| r.get(0),
        )?;
        if let Some(pos) = undo_pos {
            conn.execute(
                "DELETE FROM checkpoints WHERE session_id = ?1 AND seq > ?2",
                params![row.session_id, pos],
            )?;
            conn.execute(
                "UPDATE sessions SET undo_pos = NULL WHERE id = ?1",
                params![row.session_id],
            )?;
        }
        conn.execute(
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
     created_at, EXISTS(SELECT 1 FROM spawned_threads st WHERE st.child_thread_id = threads.id)";

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
        };
        store.insert_thread(&child, &serde_json::Map::new()).unwrap();
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
                },
                &serde_json::Map::new(),
            )
            .unwrap();
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

        // Pop drains front-first in the new order.
        let p1 = store.pop_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(p1.content, "third");
        let p2 = store.pop_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(p2.content, "second v2");
        assert!(store.pop_queued_prompt("th_1").unwrap().is_none());

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

        // Edit: rename + disable clears the next fire time.
        let mut edited = auto.clone();
        edited.name = "Morning triage".into();
        edited.enabled = false;
        edited.next_run_at = None;
        assert!(store.update_automation(&edited).unwrap());
        let got = store.automation("auto_1").unwrap().unwrap();
        assert_eq!(got.name, "Morning triage");
        assert!(!got.enabled);
        assert!(got.next_run_at.is_none());

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
        let popped = store.pop_queued_prompt("th_1").unwrap().unwrap();
        assert_eq!(popped.attachments, vec![att]);
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
}
