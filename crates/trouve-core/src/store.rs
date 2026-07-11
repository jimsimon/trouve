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
use rusqlite::{params, Connection, OptionalExtension};
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
  input_tokens INTEGER NOT NULL,
  output_tokens INTEGER NOT NULL,
  cached_input_tokens INTEGER NOT NULL,
  cost_usd REAL,
  PRIMARY KEY (thread_id, turn)
);
CREATE TABLE IF NOT EXISTS backend_sessions (
  thread_id TEXT PRIMARY KEY REFERENCES threads(id),
  backend_session_id TEXT NOT NULL
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
"#;

/// Additive migrations for databases created before a column existed.
/// `CREATE TABLE IF NOT EXISTS` won't touch existing tables, so each entry
/// is applied and "duplicate column" errors are ignored.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE queued_prompts ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
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
    Ok(())
}

/// Attachment metadata JSON from a queue row; a corrupt value degrades to
/// "no attachments" rather than failing the whole queue read.
fn parse_attachments(json: &str) -> Vec<trouve_protocol::Attachment> {
    serde_json::from_str(json).unwrap_or_default()
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
            "SELECT id, session_id, mode, model, permission_mode, model_options, created_at
             FROM threads WHERE id = ?1",
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
        let mut stmt = conn.prepare(
            "SELECT id, session_id, mode, model, permission_mode, model_options, created_at
             FROM threads WHERE session_id = ?1 ORDER BY created_at",
        )?;
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

    pub fn backend_session(&self, thread_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT backend_session_id FROM backend_sessions WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_backend_session(&self, thread_id: &str, backend_session_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO backend_sessions (thread_id, backend_session_id) VALUES (?1, ?2)
             ON CONFLICT(thread_id) DO UPDATE SET backend_session_id = excluded.backend_session_id",
            params![thread_id, backend_session_id],
        )?;
        Ok(())
    }

    // --- usage accounting -------------------------------------------------------

    pub fn record_usage(
        &self,
        session_id: &str,
        thread_id: &str,
        turn: u64,
        usage: &trouve_protocol::Usage,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR REPLACE INTO usage
             (thread_id, session_id, turn, input_tokens, output_tokens, cached_input_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                thread_id,
                session_id,
                turn as i64,
                usage.input_tokens as i64,
                usage.output_tokens as i64,
                usage.cached_input_tokens as i64,
                usage.cost_usd
            ],
        )?;
        Ok(())
    }

    /// Input tokens reported for the thread's most recent turn (context size
    /// proxy for the compaction trigger and the UI usage indicator).
    pub fn last_input_tokens(&self, thread_id: &str) -> Result<Option<u64>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT input_tokens + cached_input_tokens FROM usage
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
        created_at: r
            .get::<_, String>(7)?
            .parse()
            .unwrap_or_else(|_| chrono::Utc::now()),
    })
}

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
        };
        store
            .insert_thread(&thread, &serde_json::Map::new())
            .unwrap();
        // The vendor-resume link is what used to trip the FK constraint.
        store.set_backend_session("th_1", "vendor-abc").unwrap();
        store.enqueue_prompt("th_1", "pending", &[]).unwrap();

        store.delete_session("se_1").unwrap();
        assert!(store.session("se_1").unwrap().is_none());
        assert!(store.backend_session("th_1").unwrap().is_none());
        assert!(store.queued_prompts("th_1").unwrap().is_empty());
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
        assert!(!store
            .reorder_queued_prompts("th_1", std::slice::from_ref(&c.id))
            .unwrap());
        // ...and applies the given order when it matches.
        assert!(store
            .reorder_queued_prompts("th_1", &[c.id.clone(), b.id.clone()])
            .unwrap());

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
