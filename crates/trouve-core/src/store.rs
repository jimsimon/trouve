//! SQLite persistence: workspaces, sessions, threads, provider transcripts,
//! checkpoints, and the append-only event log.
//!
//! The event log is the UI/replay/audit source of truth (invariant 2). The
//! `messages` table is the provider-facing transcript — a faithful record of
//! what was sent to/received from the model, which the event taxonomy does
//! not try to encode.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::broadcast;
use trouve_protocol::{
    Event, EventEnvelope, GithubPrList, PermissionMode, Scope, Session, Thread, ThreadViewSnapshot,
    Workspace,
};
use trouve_thread_view::ThreadProjection;

const THREAD_VIEW_SCHEMA_VERSION: i64 = 1;
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
  tools_enabled INTEGER NOT NULL DEFAULT 1,
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
CREATE TABLE IF NOT EXISTS thread_view_cache (
  thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
  cursor INTEGER NOT NULL,
  schema_version INTEGER NOT NULL,
  state TEXT NOT NULL
);
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
CREATE TABLE IF NOT EXISTS store_migrations (
  id TEXT PRIMARY KEY,
  applied_at TEXT NOT NULL
);
-- The identity-based SQL names below are retained for compatibility with
-- preview databases; the Rust and wire APIs expose reviewer profiles.
CREATE TABLE IF NOT EXISTS code_review_repositories (
  repository TEXT PRIMARY KEY,
  installation_id INTEGER NOT NULL,
  private INTEGER NOT NULL DEFAULT 0,
  mode TEXT NOT NULL DEFAULT 'off',
  model TEXT,
  coordinator_thinking_level TEXT,
  router_model TEXT,
  router_thinking_level TEXT,
  prompt TEXT NOT NULL DEFAULT '',
  identity_ids TEXT NOT NULL DEFAULT '["correctness","security","concurrency","api-compatibility","testing"]',
  routing_mode TEXT NOT NULL DEFAULT 'additive',
  semantic_routing INTEGER NOT NULL DEFAULT 1,
  included_reviewer_ids TEXT NOT NULL DEFAULT '[]',
  excluded_reviewer_ids TEXT NOT NULL DEFAULT '[]',
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
  coordinator_thinking_level TEXT,
  router_model TEXT,
  router_thinking_level TEXT,
  prompt TEXT NOT NULL DEFAULT '',
  identities TEXT NOT NULL DEFAULT '[]',
  config_hash TEXT NOT NULL DEFAULT '',
  session_id TEXT,
  thread_id TEXT,
  review_url TEXT NOT NULL DEFAULT '',
  error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  started_at TEXT,
  completed_at TEXT,
  review_base_sha TEXT NOT NULL DEFAULT '',
  review_scope TEXT NOT NULL DEFAULT 'incremental',
  retry_of TEXT,
  retried_by TEXT,
  lifecycle_comment_url TEXT NOT NULL DEFAULT '',
  check_run_id INTEGER,
  check_run_url TEXT NOT NULL DEFAULT '',
  check_sync_error TEXT NOT NULL DEFAULT '',
  projection_retry_count INTEGER NOT NULL DEFAULT 0,
  projection_retry_at TEXT,
  projection_retryable INTEGER NOT NULL DEFAULT 1,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  completed_reviewers INTEGER NOT NULL DEFAULT 0,
  total_reviewers INTEGER NOT NULL DEFAULT 0,
  candidate_issue_count INTEGER NOT NULL DEFAULT 0,
  issue_count INTEGER NOT NULL DEFAULT 0,
  fixed_issue_count INTEGER NOT NULL DEFAULT 0,
  summary TEXT NOT NULL DEFAULT '',
  prompt_for_agents TEXT NOT NULL DEFAULT '',
  publication_claimed INTEGER NOT NULL DEFAULT 0,
  preparation_elapsed_ms INTEGER NOT NULL DEFAULT 0,
  reviewer_elapsed_ms INTEGER NOT NULL DEFAULT 0,
  coordinator_elapsed_ms INTEGER NOT NULL DEFAULT 0,
  publication_elapsed_ms INTEGER NOT NULL DEFAULT 0,
  routing_mode TEXT NOT NULL DEFAULT 'additive',
  semantic_routing INTEGER NOT NULL DEFAULT 1,
  included_reviewer_ids TEXT NOT NULL DEFAULT '[]',
  excluded_reviewer_ids TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS code_review_jobs_status ON code_review_jobs (status, created_at);
CREATE INDEX IF NOT EXISTS code_review_jobs_repository_history
  ON code_review_jobs (repository, status, completed_at, created_at);
CREATE TABLE IF NOT EXISTS code_review_tasks (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  role TEXT NOT NULL,
  reviewer_id TEXT,
  reviewer_name TEXT NOT NULL DEFAULT '',
  batch_index INTEGER NOT NULL DEFAULT 0,
  batch_count INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'queued',
  lifecycle_stage TEXT NOT NULL DEFAULT 'queued',
  model TEXT,
  session_id TEXT,
  thread_id TEXT,
  prompt TEXT NOT NULL DEFAULT '',
  output TEXT NOT NULL DEFAULT '',
  thinking TEXT NOT NULL DEFAULT '',
  tool_output TEXT NOT NULL DEFAULT '',
  candidate_issue_count INTEGER NOT NULL DEFAULT 0,
  confirmed_issue_count INTEGER NOT NULL DEFAULT 0,
  provider_wait_ms INTEGER NOT NULL DEFAULT 0,
  model_elapsed_ms INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  cached_input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  tool_call_count INTEGER NOT NULL DEFAULT 0,
  error TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  started_at TEXT,
  model_started_at TEXT,
  last_progress_at TEXT,
  completed_at TEXT
);
CREATE INDEX IF NOT EXISTS code_review_tasks_job
  ON code_review_tasks (job_id, role, reviewer_id, batch_index, created_at);
CREATE INDEX IF NOT EXISTS code_review_tasks_stats
  ON code_review_tasks (reviewer_id, model, status, completed_at);
CREATE TABLE IF NOT EXISTS code_review_routing_decisions (
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  batch_index INTEGER NOT NULL,
  reviewer_id TEXT NOT NULL,
  reviewer_name TEXT NOT NULL,
  selected INTEGER NOT NULL,
  reasons TEXT NOT NULL DEFAULT '[]',
  PRIMARY KEY (job_id, batch_index, reviewer_id)
);
CREATE TABLE IF NOT EXISTS code_review_findings (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  path TEXT NOT NULL,
  line INTEGER NOT NULL,
  side TEXT NOT NULL,
  severity TEXT NOT NULL,
  body TEXT NOT NULL,
  prompt_for_agents TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'open',
  github_comment_id INTEGER,
  github_comment_url TEXT NOT NULL DEFAULT '',
  github_thread_id TEXT,
  resolved_at TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS code_review_findings_job
  ON code_review_findings (job_id, status);
CREATE INDEX IF NOT EXISTS code_review_findings_open_pr
  ON code_review_findings (status, job_id, path, line);
CREATE TABLE IF NOT EXISTS code_review_finding_sources (
  finding_id TEXT NOT NULL REFERENCES code_review_findings(id),
  candidate_id TEXT NOT NULL,
  task_id TEXT NOT NULL DEFAULT '',
  reviewer_id TEXT NOT NULL,
  reviewer_name TEXT NOT NULL,
  PRIMARY KEY (finding_id, candidate_id)
);
CREATE TABLE IF NOT EXISTS code_review_candidate_rejections (
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  candidate_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  reviewer_id TEXT NOT NULL,
  reviewer_name TEXT NOT NULL,
  path TEXT NOT NULL,
  line INTEGER NOT NULL,
  side TEXT NOT NULL,
  severity TEXT NOT NULL,
  body TEXT NOT NULL,
  reason TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (job_id, candidate_id)
);
CREATE TABLE IF NOT EXISTS code_review_pr_state (
  repository TEXT NOT NULL,
  pull_number INTEGER NOT NULL,
  manual_requested INTEGER NOT NULL DEFAULT 0,
  manual_generation INTEGER NOT NULL DEFAULT 0,
  last_reviewed_head_sha TEXT NOT NULL DEFAULT '',
  last_reviewed_base_sha TEXT NOT NULL DEFAULT '',
  last_reviewed_at TEXT,
  lifecycle_comment_id INTEGER,
  lifecycle_comment_url TEXT NOT NULL DEFAULT '',
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

/// Repeat-safe migrations for databases created before a schema change.
/// `CREATE TABLE IF NOT EXISTS` won't touch existing tables, so column
/// additions are retried and "duplicate column" errors are ignored.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE workspaces ADD COLUMN closed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE queued_prompts ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE queued_prompts ADD COLUMN claimed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE queued_prompts ADD COLUMN tools_enabled INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE automations ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'ask'",
    "ALTER TABLE threads ADD COLUMN todos TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_repositories ADD COLUMN identity_ids TEXT NOT NULL DEFAULT '[\"correctness\",\"security\",\"concurrency\",\"api-compatibility\",\"testing\"]'",
    "ALTER TABLE code_review_repositories ADD COLUMN routing_mode TEXT NOT NULL DEFAULT 'core'",
    "ALTER TABLE code_review_repositories ADD COLUMN semantic_routing INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_repositories ADD COLUMN included_reviewer_ids TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_repositories ADD COLUMN excluded_reviewer_ids TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_repositories ADD COLUMN reviewer_overrides TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_repositories ADD COLUMN router_model TEXT",
    "ALTER TABLE code_review_repositories ADD COLUMN router_thinking_level TEXT",
    "ALTER TABLE code_review_repositories ADD COLUMN coordinator_thinking_level TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN identities TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_jobs ADD COLUMN config_hash TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN review_base_sha TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN review_scope TEXT NOT NULL DEFAULT 'incremental'",
    "ALTER TABLE code_review_jobs ADD COLUMN retry_of TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN retried_by TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN lifecycle_comment_url TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN check_run_id INTEGER",
    "ALTER TABLE code_review_jobs ADD COLUMN check_run_url TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN check_sync_error TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN projection_retry_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN projection_retry_at TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN projection_retryable INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE code_review_jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN completed_reviewers INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN total_reviewers INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN candidate_issue_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN issue_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN fixed_issue_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN summary TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN prompt_for_agents TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN publication_claimed INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN preparation_elapsed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN reviewer_elapsed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN coordinator_elapsed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN publication_elapsed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN routing_mode TEXT NOT NULL DEFAULT 'core'",
    "ALTER TABLE code_review_jobs ADD COLUMN semantic_routing INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_jobs ADD COLUMN included_reviewer_ids TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_jobs ADD COLUMN excluded_reviewer_ids TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE code_review_jobs ADD COLUMN router_model TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN router_thinking_level TEXT",
    "ALTER TABLE code_review_jobs ADD COLUMN coordinator_thinking_level TEXT",
    "DROP INDEX IF EXISTS code_review_routing_decisions_job",
    "ALTER TABLE code_review_pr_state ADD COLUMN last_reviewed_head_sha TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_pr_state ADD COLUMN last_reviewed_base_sha TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_pr_state ADD COLUMN last_reviewed_at TEXT",
    "ALTER TABLE code_review_pr_state ADD COLUMN lifecycle_comment_id INTEGER",
    "ALTER TABLE code_review_pr_state ADD COLUMN lifecycle_comment_url TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_identities ADD COLUMN thinking_level TEXT",
    "ALTER TABLE code_review_identities ADD COLUMN built_in INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN provider_wait_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN model_elapsed_ms INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN input_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN cached_input_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN tool_call_count INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_tasks ADD COLUMN lifecycle_stage TEXT NOT NULL DEFAULT 'queued'",
    "ALTER TABLE code_review_tasks ADD COLUMN model_started_at TEXT",
    "ALTER TABLE code_review_tasks ADD COLUMN last_progress_at TEXT",
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
    backfill_terminal_code_review_task_lifecycle(conn)?;
    migrate_backend_sessions(conn)?;
    migrate_automatic_code_review_routing(conn)?;
    Ok(())
}

/// Lifecycle columns were added after code-review tasks were already durable.
/// Repair terminal rows created by older builds without disturbing failed or
/// cancelled tasks, whose last active stage remains useful diagnostic state.
fn backfill_terminal_code_review_task_lifecycle(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE code_review_tasks
         SET lifecycle_stage = 'completed',
             last_progress_at = COALESCE(last_progress_at, completed_at, started_at, created_at)
         WHERE status IN ('succeeded', 'not_applicable')
           AND (lifecycle_stage != 'completed' OR last_progress_at IS NULL)",
        [],
    )?;
    Ok(())
}

/// Upgrade untouched default policies to automatic routing while preserving
/// customized persona selections as Core policies. Record the migration so a
/// later user choice to return to Core remains respected.
fn migrate_automatic_code_review_routing(conn: &Connection) -> Result<()> {
    const MIGRATION_ID: &str = "automatic-code-review-routing-v1";
    const LEGACY_DEFAULTS: &str = r#"["correctness","security","api-compatibility","testing"]"#;
    const CURRENT_DEFAULTS: &str =
        r#"["correctness","security","concurrency","api-compatibility","testing"]"#;

    let applied = conn
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        return Ok(());
    }

    conn.execute(
        "UPDATE code_review_repositories
         SET identity_ids = ?2, routing_mode = 'auto', semantic_routing = 1
         WHERE identity_ids = ?1",
        params![LEGACY_DEFAULTS, CURRENT_DEFAULTS],
    )?;
    conn.execute(
        "UPDATE code_review_repositories
         SET routing_mode = 'auto', semantic_routing = 1
         WHERE identity_ids = ?1 AND routing_mode = 'core'",
        [CURRENT_DEFAULTS],
    )?;
    conn.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn projection_retry_delay_seconds(attempt: u32) -> u64 {
    const BASE_SECONDS: u64 = 60;
    const MAX_SECONDS: u64 = 6 * 60 * 60;
    let exponent = attempt.saturating_sub(1).min(9);
    BASE_SECONDS
        .saturating_mul(1_u64 << exponent)
        .min(MAX_SECONDS)
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

fn code_review_routing_mode_from(value: &str) -> trouve_protocol::CodeReviewRoutingMode {
    match value {
        "additive" | "auto" => trouve_protocol::CodeReviewRoutingMode::Additive,
        "automatic" | "thorough" => trouve_protocol::CodeReviewRoutingMode::Automatic,
        _ => trouve_protocol::CodeReviewRoutingMode::Manual,
    }
}

fn code_review_routing_mode_str(value: trouve_protocol::CodeReviewRoutingMode) -> &'static str {
    match value {
        trouve_protocol::CodeReviewRoutingMode::Manual => "manual",
        trouve_protocol::CodeReviewRoutingMode::Additive => "additive",
        trouve_protocol::CodeReviewRoutingMode::Automatic => "automatic",
    }
}

fn parse_datetime(value: String) -> chrono::DateTime<chrono::Utc> {
    value.parse().unwrap_or_else(|_| chrono::Utc::now())
}

fn parse_optional_datetime(value: Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    value.and_then(|value| value.parse().ok())
}

fn code_review_scope_from(value: &str) -> trouve_protocol::CodeReviewJobScope {
    match value {
        "full" => trouve_protocol::CodeReviewJobScope::Full,
        _ => trouve_protocol::CodeReviewJobScope::Incremental,
    }
}

fn code_review_scope_str(value: trouve_protocol::CodeReviewJobScope) -> &'static str {
    match value {
        trouve_protocol::CodeReviewJobScope::Incremental => "incremental",
        trouve_protocol::CodeReviewJobScope::Full => "full",
    }
}

fn elapsed_ms(
    started: chrono::DateTime<chrono::Utc>,
    finished: chrono::DateTime<chrono::Utc>,
) -> u64 {
    finished
        .signed_duration_since(started)
        .num_milliseconds()
        .max(0) as u64
}

fn finalize_code_review_model_elapsed(
    recorded_model_elapsed_ms: i64,
    model_started_at: Option<String>,
    last_progress_at: Option<String>,
    finished: chrono::DateTime<chrono::Utc>,
) -> u64 {
    let recorded = recorded_model_elapsed_ms.max(0) as u64;
    let Some(started) = parse_optional_datetime(model_started_at) else {
        return recorded;
    };
    let progress_anchor = parse_optional_datetime(last_progress_at)
        .filter(|progress| *progress >= started)
        .unwrap_or(started);
    recorded.saturating_add(elapsed_ms(progress_anchor, finished))
}

fn job_elapsed_ms(
    status: &str,
    created_at: chrono::DateTime<chrono::Utc>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> (u64, u64) {
    let now = chrono::Utc::now();
    let pending_end = started_at.or(completed_at).unwrap_or(now);
    let pending = elapsed_ms(created_at, pending_end);
    let running = started_at
        .map(|started| elapsed_ms(started, completed_at.unwrap_or(now)))
        .unwrap_or(0);
    if status == "queued" {
        (elapsed_ms(created_at, now), 0)
    } else {
        (pending, running)
    }
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
        routing_mode: code_review_routing_mode_from(&r.get::<_, String>(7)?),
        semantic_routing: r.get(8)?,
        included_reviewer_ids: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(9)?)
            .unwrap_or_default(),
        excluded_reviewer_ids: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(10)?)
            .unwrap_or_default(),
        reviewer_overrides: serde_json::from_str::<Vec<trouve_protocol::ReviewerOverride>>(
            &r.get::<_, String>(11)?,
        )
        .unwrap_or_default(),
        router_model: r.get(12)?,
        router_thinking_level: r.get(13)?,
        coordinator_thinking_level: r.get(14)?,
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
    pub review_base_sha: String,
    pub base_ref: String,
    pub head_ref: String,
    pub scope: trouve_protocol::CodeReviewJobScope,
    pub trigger: String,
    pub retry_of: Option<String>,
    pub model: Option<String>,
    pub coordinator_thinking_level: Option<String>,
    pub router_model: Option<String>,
    pub router_thinking_level: Option<String>,
    pub prompt: String,
    pub reviewers: Vec<trouve_protocol::ReviewerProfile>,
    pub routing_mode: trouve_protocol::CodeReviewRoutingMode,
    pub semantic_routing: bool,
    pub included_reviewer_ids: Vec<String>,
    pub excluded_reviewer_ids: Vec<String>,
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
    pub summary: String,
    pub prompt_for_agents: String,
    pub publication_claimed: bool,
}

fn row_to_code_review_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<CodeReviewJobRecord> {
    let reviewers: Vec<trouve_protocol::ReviewerProfile> =
        serde_json::from_str(&r.get::<_, String>(13)?).unwrap_or_default();
    let created_at = parse_datetime(r.get(19)?);
    let started_at = parse_optional_datetime(r.get(20)?);
    let completed_at = parse_optional_datetime(r.get(21)?);
    let status: String = r.get(10)?;
    let completed_reviewers = r.get::<_, i64>(31)? as u64;
    let total_reviewers = r.get::<_, i64>(32)? as u64;
    let percent = completed_reviewers
        .saturating_mul(100)
        .checked_div(total_reviewers)
        .map(|value| value.min(100) as u8)
        .unwrap_or_else(|| {
            u8::from(matches!(
                status.as_str(),
                "succeeded" | "failed" | "cancelled" | "stale"
            )) * 100
        });
    let (pending_elapsed_ms, running_elapsed_ms) =
        job_elapsed_ms(&status, created_at, started_at, completed_at);
    let base_ref: String = r.get(7)?;
    let review_base_sha: String = r.get(22)?;
    Ok(CodeReviewJobRecord {
        job: trouve_protocol::CodeReviewJob {
            id: r.get(0)?,
            installation_id: r.get::<_, i64>(1)? as u64,
            repository: r.get(2)?,
            pull_number: r.get::<_, i64>(3)? as u64,
            pull_title: r.get(4)?,
            pull_url: r.get(5)?,
            head_sha: r.get(6)?,
            review_base_sha: if review_base_sha.is_empty() {
                base_ref.clone()
            } else {
                review_base_sha
            },
            base_ref,
            head_ref: r.get(8)?,
            scope: code_review_scope_from(&r.get::<_, String>(23)?),
            trigger: r.get(9)?,
            status,
            retry_of: r.get(24)?,
            retried_by: r.get(25)?,
            model: r.get(11)?,
            coordinator_thinking_level: r.get(49)?,
            router_model: r.get(47)?,
            router_thinking_level: r.get(48)?,
            reviewer_ids: reviewers
                .iter()
                .map(|reviewer| reviewer.id.clone())
                .collect(),
            routing_mode: code_review_routing_mode_from(&r.get::<_, String>(43)?),
            semantic_routing: r.get(44)?,
            included_reviewer_ids: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(45)?)
                .unwrap_or_default(),
            excluded_reviewer_ids: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(46)?)
                .unwrap_or_default(),
            session_id: r.get(15)?,
            thread_id: r.get(16)?,
            review_url: r.get(17)?,
            lifecycle_comment_url: r.get(26)?,
            check_run_id: r.get::<_, Option<i64>>(27)?.map(|value| value as u64),
            check_run_url: r.get(28)?,
            check_sync_error: r.get(29)?,
            cancel_requested: r.get(30)?,
            progress: trouve_protocol::CodeReviewProgress {
                completed_reviewers,
                total_reviewers,
                percent,
            },
            candidate_issue_count: r.get::<_, i64>(33)? as u64,
            issue_count: r.get::<_, i64>(34)? as u64,
            fixed_issue_count: r.get::<_, i64>(35)? as u64,
            error: r.get(18)?,
            created_at,
            started_at,
            completed_at,
            pending_elapsed_ms,
            running_elapsed_ms,
            preparation_elapsed_ms: r.get::<_, i64>(39)? as u64,
            reviewer_elapsed_ms: r.get::<_, i64>(40)? as u64,
            coordinator_elapsed_ms: r.get::<_, i64>(41)? as u64,
            publication_elapsed_ms: r.get::<_, i64>(42)? as u64,
        },
        prompt: r.get(12)?,
        reviewers,
        summary: r.get(36)?,
        prompt_for_agents: r.get(37)?,
        publication_claimed: r.get(38)?,
    })
}

const CODE_REVIEW_JOB_COLUMNS: &str = "id, installation_id, repository, pull_number, pull_title, pull_url, head_sha, \
     base_ref, head_ref, trigger, status, model, prompt, identities, config_hash, session_id, thread_id, \
     review_url, error, created_at, started_at, completed_at, review_base_sha, review_scope, retry_of, \
     retried_by, lifecycle_comment_url, check_run_id, check_run_url, check_sync_error, cancel_requested, \
     completed_reviewers, total_reviewers, candidate_issue_count, issue_count, fixed_issue_count, summary, \
     prompt_for_agents, publication_claimed, preparation_elapsed_ms, reviewer_elapsed_ms, \
     coordinator_elapsed_ms, publication_elapsed_ms, routing_mode, semantic_routing, \
     included_reviewer_ids, excluded_reviewer_ids, router_model, router_thinking_level, \
     coordinator_thinking_level";

#[derive(Debug, Clone)]
pub struct NewCodeReviewTask {
    pub job_id: String,
    pub role: trouve_protocol::CodeReviewTaskRole,
    pub reviewer_id: Option<String>,
    pub reviewer_name: String,
    pub batch_index: u64,
    pub batch_count: u64,
    pub model: Option<String>,
    pub prompt: String,
}

#[derive(Debug, Clone, Default)]
pub struct CodeReviewTaskMetrics {
    pub model_elapsed_ms: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub tool_call_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeReviewModelTiming {
    Preserve,
    Reset,
    Started,
}

#[derive(Debug, Clone, Copy)]
pub enum CodeReviewJobPhase {
    Preparation,
    Reviewers,
    Coordinator,
    Publication,
}

fn code_review_task_role_str(role: trouve_protocol::CodeReviewTaskRole) -> &'static str {
    match role {
        trouve_protocol::CodeReviewTaskRole::Router => "router",
        trouve_protocol::CodeReviewTaskRole::Reviewer => "reviewer",
        trouve_protocol::CodeReviewTaskRole::Coordinator => "coordinator",
    }
}

fn code_review_task_role_from(value: &str) -> trouve_protocol::CodeReviewTaskRole {
    match value {
        "router" => trouve_protocol::CodeReviewTaskRole::Router,
        "coordinator" => trouve_protocol::CodeReviewTaskRole::Coordinator,
        _ => trouve_protocol::CodeReviewTaskRole::Reviewer,
    }
}

fn code_review_task_lifecycle_stage_str(
    stage: trouve_protocol::CodeReviewTaskLifecycleStage,
) -> &'static str {
    use trouve_protocol::CodeReviewTaskLifecycleStage;
    match stage {
        CodeReviewTaskLifecycleStage::Queued => "queued",
        CodeReviewTaskLifecycleStage::WaitingForCapacity => "waiting_for_capacity",
        CodeReviewTaskLifecycleStage::StartingModel => "starting_model",
        CodeReviewTaskLifecycleStage::RunningModel => "running_model",
        CodeReviewTaskLifecycleStage::RunningTool => "running_tool",
        CodeReviewTaskLifecycleStage::RepairingOutput => "repairing_output",
        CodeReviewTaskLifecycleStage::Completed => "completed",
    }
}

fn code_review_task_lifecycle_stage_from(
    value: &str,
) -> trouve_protocol::CodeReviewTaskLifecycleStage {
    use trouve_protocol::CodeReviewTaskLifecycleStage;
    match value {
        "waiting_for_capacity" => CodeReviewTaskLifecycleStage::WaitingForCapacity,
        "starting_model" => CodeReviewTaskLifecycleStage::StartingModel,
        "running_model" => CodeReviewTaskLifecycleStage::RunningModel,
        "running_tool" => CodeReviewTaskLifecycleStage::RunningTool,
        "repairing_output" => CodeReviewTaskLifecycleStage::RepairingOutput,
        "completed" => CodeReviewTaskLifecycleStage::Completed,
        _ => CodeReviewTaskLifecycleStage::Queued,
    }
}

fn row_to_code_review_task(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<trouve_protocol::CodeReviewTask> {
    let created_at = parse_datetime(r.get(25)?);
    let started_at = parse_optional_datetime(r.get(26)?);
    let model_started_at = parse_optional_datetime(r.get(27)?);
    let last_progress_at = parse_optional_datetime(r.get(28)?);
    let completed_at = parse_optional_datetime(r.get(29)?);
    let elapsed = started_at
        .map(|started| elapsed_ms(started, completed_at.unwrap_or_else(chrono::Utc::now)))
        .unwrap_or(0);
    Ok(trouve_protocol::CodeReviewTask {
        id: r.get(0)?,
        job_id: r.get(1)?,
        role: code_review_task_role_from(&r.get::<_, String>(2)?),
        reviewer_id: r.get(3)?,
        reviewer_name: r.get(4)?,
        batch_index: r.get::<_, i64>(5)? as u64,
        batch_count: r.get::<_, i64>(6)? as u64,
        status: r.get(7)?,
        lifecycle_stage: code_review_task_lifecycle_stage_from(&r.get::<_, String>(8)?),
        model: r.get(9)?,
        session_id: r.get(10)?,
        thread_id: r.get(11)?,
        prompt: r.get(12)?,
        output: r.get(13)?,
        thinking: r.get(14)?,
        tool_output: r.get(15)?,
        candidate_issue_count: r.get::<_, i64>(16)? as u64,
        confirmed_issue_count: r.get::<_, i64>(17)? as u64,
        provider_wait_ms: r.get::<_, i64>(18)? as u64,
        model_elapsed_ms: r.get::<_, i64>(19)? as u64,
        input_tokens: r.get::<_, i64>(20)? as u64,
        cached_input_tokens: r.get::<_, i64>(21)? as u64,
        output_tokens: r.get::<_, i64>(22)? as u64,
        tool_call_count: r.get::<_, i64>(23)? as u64,
        error: r.get(24)?,
        created_at,
        started_at,
        model_started_at,
        last_progress_at,
        completed_at,
        elapsed_ms: elapsed,
    })
}

const CODE_REVIEW_TASK_COLUMNS: &str = "id, job_id, role, reviewer_id, reviewer_name, batch_index, \
     batch_count, status, lifecycle_stage, model, session_id, thread_id, prompt, output, thinking, tool_output, \
     candidate_issue_count, confirmed_issue_count, provider_wait_ms, model_elapsed_ms, input_tokens, \
     cached_input_tokens, output_tokens, tool_call_count, error, created_at, started_at, model_started_at, \
     last_progress_at, completed_at";
const CODE_REVIEW_TASK_PROGRESS_COLUMNS: &str = "job_id, id, lifecycle_stage, provider_wait_ms, \
     model_elapsed_ms, input_tokens, cached_input_tokens, output_tokens, tool_call_count, \
     model_started_at, last_progress_at";
const CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN: &str = "task_insertion_order";

#[derive(Debug, Clone)]
pub struct CodeReviewTaskProgressRecord {
    pub job_id: String,
    pub task_id: String,
    pub progress: trouve_protocol::CodeReviewTaskProgress,
}

fn row_to_code_review_task_progress(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<CodeReviewTaskProgressRecord> {
    Ok(CodeReviewTaskProgressRecord {
        job_id: r.get(0)?,
        task_id: r.get(1)?,
        progress: trouve_protocol::CodeReviewTaskProgress {
            lifecycle_stage: code_review_task_lifecycle_stage_from(&r.get::<_, String>(2)?),
            provider_wait_ms: r.get::<_, i64>(3)? as u64,
            model_elapsed_ms: r.get::<_, i64>(4)? as u64,
            input_tokens: r.get::<_, i64>(5)? as u64,
            cached_input_tokens: r.get::<_, i64>(6)? as u64,
            output_tokens: r.get::<_, i64>(7)? as u64,
            tool_call_count: r.get::<_, i64>(8)? as u64,
            model_started_at: parse_optional_datetime(r.get(9)?),
            last_progress_at: parse_datetime(r.get(10)?),
        },
    })
}

/// Same wire shape as a full task, but without the potentially large retained
/// prompt/transcript fields. Deriving this projection from the full column list
/// keeps the order aligned with the shared row mapper.
fn code_review_task_summary_columns() -> String {
    CODE_REVIEW_TASK_COLUMNS
        .split(',')
        .map(str::trim)
        .map(|column| match column {
            "prompt" => "'' AS prompt",
            "output" => "'' AS output",
            "thinking" => "'' AS thinking",
            "tool_output" => "'' AS tool_output",
            _ => column,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone)]
struct CodeReviewTaskAttempt {
    task: trouve_protocol::CodeReviewTask,
    insertion_order: i64,
}

fn row_to_code_review_task_attempt(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<CodeReviewTaskAttempt> {
    Ok(CodeReviewTaskAttempt {
        task: row_to_code_review_task(r)?,
        insertion_order: r.get(CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN)?,
    })
}

fn row_to_code_review_routing_decision(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<trouve_protocol::CodeReviewRoutingDecision> {
    Ok(trouve_protocol::CodeReviewRoutingDecision {
        batch_index: r.get::<_, i64>(0)? as u64,
        reviewer_id: r.get(1)?,
        reviewer_name: r.get(2)?,
        selected: r.get(3)?,
        reasons: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
    })
}

fn latest_code_review_task_attempts(
    attempts: &[CodeReviewTaskAttempt],
) -> Vec<&CodeReviewTaskAttempt> {
    let mut latest = BTreeMap::new();
    for attempt in attempts {
        let task = &attempt.task;
        if task.role != trouve_protocol::CodeReviewTaskRole::Reviewer {
            continue;
        }
        let Some(reviewer_id) = task.reviewer_id.as_ref() else {
            continue;
        };
        let key = (reviewer_id.clone(), task.batch_index);
        let replace = latest
            .get(&key)
            .is_none_or(|current: &&CodeReviewTaskAttempt| {
                attempt.insertion_order > current.insertion_order
            });
        if replace {
            latest.insert(key, attempt);
        }
    }
    latest.into_values().collect()
}

#[derive(Debug, Clone)]
pub struct NewCodeReviewFinding {
    pub path: String,
    pub line: u64,
    pub side: String,
    pub severity: String,
    pub body: String,
    pub prompt_for_agents: String,
    pub sources: Vec<trouve_protocol::CodeReviewFindingSource>,
}

#[derive(Debug, Clone, Default)]
pub struct CodeReviewPullStateRecord {
    pub last_reviewed_head_sha: String,
    pub last_reviewed_base_sha: String,
    pub last_reviewed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub lifecycle_comment_id: Option<u64>,
    pub lifecycle_comment_url: String,
}

fn code_review_persona_results(
    attempts: &[CodeReviewTaskAttempt],
) -> Vec<trouve_protocol::CodeReviewPersonaResult> {
    let mut grouped: BTreeMap<String, Vec<&trouve_protocol::CodeReviewTask>> = BTreeMap::new();
    for attempt in latest_code_review_task_attempts(attempts) {
        let task = &attempt.task;
        if let Some(reviewer_id) = task.reviewer_id.as_ref() {
            grouped.entry(reviewer_id.clone()).or_default().push(task);
        }
    }
    grouped
        .into_iter()
        .map(|(reviewer_id, tasks)| {
            let mut models = BTreeSet::new();
            let mut started_at = None;
            let mut completed_at = None;
            let mut all_terminal = true;
            let mut has_running = false;
            let mut has_queued = false;
            let mut has_failed = false;
            let mut has_cancelled = false;
            let mut has_succeeded = false;
            let mut all_successful_or_not_applicable = true;
            let mut completed_batches = 0_u64;
            let mut total_batches = 0_u64;
            let mut candidate_issue_count = 0_u64;
            let mut confirmed_issue_count = 0_u64;
            let mut provider_wait_ms = 0_u64;
            let mut model_elapsed_ms = 0_u64;
            let mut input_tokens = 0_u64;
            let mut cached_input_tokens = 0_u64;
            let mut output_tokens = 0_u64;
            let mut tool_call_count = 0_u64;
            for task in &tasks {
                total_batches = total_batches.max(task.batch_count);
                if let Some(model) = &task.model {
                    models.insert(model.clone());
                }
                if let Some(started) = task.started_at {
                    started_at = Some(
                        started_at.map_or(started, |current: chrono::DateTime<chrono::Utc>| {
                            current.min(started)
                        }),
                    );
                }
                if let Some(completed) = task.completed_at {
                    completed_at = Some(
                        completed_at.map_or(completed, |current: chrono::DateTime<chrono::Utc>| {
                            current.max(completed)
                        }),
                    );
                }
                let terminal = matches!(
                    task.status.as_str(),
                    "succeeded" | "failed" | "cancelled" | "not_applicable"
                );
                if terminal {
                    completed_batches += 1;
                } else {
                    all_terminal = false;
                }
                has_running |= task.status == "running";
                has_queued |= task.status == "queued";
                has_failed |= task.status == "failed";
                has_cancelled |= task.status == "cancelled";
                has_succeeded |= task.status == "succeeded";
                all_successful_or_not_applicable &=
                    matches!(task.status.as_str(), "succeeded" | "not_applicable");
                candidate_issue_count += task.candidate_issue_count;
                confirmed_issue_count += task.confirmed_issue_count;
                provider_wait_ms += task.provider_wait_ms;
                model_elapsed_ms += task.model_elapsed_ms;
                input_tokens += task.input_tokens;
                cached_input_tokens += task.cached_input_tokens;
                output_tokens += task.output_tokens;
                tool_call_count += task.tool_call_count;
            }
            all_terminal &= completed_batches >= total_batches;
            if !all_terminal {
                completed_at = None;
            }
            let status = if has_running {
                "running"
            } else if has_queued {
                "queued"
            } else if all_successful_or_not_applicable && has_succeeded && all_terminal {
                "succeeded"
            } else if all_terminal && tasks.iter().all(|task| task.status == "not_applicable") {
                "not_applicable"
            } else if has_failed {
                "failed"
            } else if has_cancelled {
                "cancelled"
            } else {
                "queued"
            };
            let elapsed = started_at
                .map(|started| elapsed_ms(started, completed_at.unwrap_or_else(chrono::Utc::now)))
                .unwrap_or(0);
            trouve_protocol::CodeReviewPersonaResult {
                reviewer_id,
                reviewer_name: tasks
                    .first()
                    .map(|task| task.reviewer_name.clone())
                    .unwrap_or_default(),
                status: status.into(),
                models: models.into_iter().collect(),
                completed_batches,
                total_batches,
                candidate_issue_count,
                confirmed_issue_count,
                provider_wait_ms,
                model_elapsed_ms,
                input_tokens,
                cached_input_tokens,
                output_tokens,
                tool_call_count,
                started_at,
                completed_at,
                elapsed_ms: elapsed,
            }
        })
        .collect()
}

fn code_review_status_add(counts: &mut trouve_protocol::CodeReviewStatusCounts, status: &str) {
    match status {
        "queued" => counts.queued += 1,
        "running" => counts.running += 1,
        "succeeded" => counts.succeeded += 1,
        "failed" => counts.failed += 1,
        "cancelled" => counts.cancelled += 1,
        "stale" => counts.stale += 1,
        _ => {}
    }
}

fn code_review_duration_stats(mut samples: Vec<u64>) -> trouve_protocol::CodeReviewDurationStats {
    if samples.is_empty() {
        return trouve_protocol::CodeReviewDurationStats::default();
    }
    samples.sort_unstable();
    let sum = samples.iter().map(|value| u128::from(*value)).sum::<u128>();
    let percentile = |numerator: usize, denominator: usize| -> u64 {
        let index = ((samples.len() - 1) * numerator).div_ceil(denominator);
        samples[index.min(samples.len() - 1)]
    };
    trouve_protocol::CodeReviewDurationStats {
        samples: samples.len() as u64,
        average_ms: (sum / samples.len() as u128) as u64,
        p50_ms: percentile(50, 100),
        p95_ms: percentile(95, 100),
        maximum_ms: *samples.last().unwrap_or(&0),
    }
}

fn push_nonzero_duration(samples: &mut Vec<u64>, duration_ms: u64) {
    if duration_ms > 0 {
        samples.push(duration_ms);
    }
}

fn code_review_stats_start(
    range: trouve_protocol::CodeReviewStatsRange,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let duration = match range {
        trouve_protocol::CodeReviewStatsRange::Hour => chrono::Duration::hours(1),
        trouve_protocol::CodeReviewStatsRange::Day => chrono::Duration::days(1),
        trouve_protocol::CodeReviewStatsRange::Week => chrono::Duration::weeks(1),
        trouve_protocol::CodeReviewStatsRange::Month => chrono::Duration::days(30),
        trouve_protocol::CodeReviewStatsRange::Year => chrono::Duration::days(365),
        trouve_protocol::CodeReviewStatsRange::All => return None,
    };
    Some(now - duration)
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

/// One bounded slice of persisted event history.
///
/// `next_after` advances across every database row the page inspected,
/// including retired or forward-incompatible events that were skipped while
/// decoding. This lets stream consumers make progress without retaining the
/// whole replay in memory.
pub struct EventReplayPage {
    pub events: Vec<EventEnvelope>,
    pub next_after: u64,
    pub exhausted: bool,
}

/// Shared handle to the database plus the live event fan-out.
type ScopedEventSenders = Arc<Mutex<HashMap<(String, String), broadcast::Sender<EventEnvelope>>>>;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    events_tx: broadcast::Sender<EventEnvelope>,
    scoped_events: ScopedEventSenders,
    append_tx: std::sync::mpsc::Sender<AppendRequest>,
}

/// One serialized event, in flight to the writer thread.
struct PendingEvent {
    scope: Scope,
    ts: chrono::DateTime<chrono::Utc>,
    /// Serialized on the caller's task so an unserializable event fails
    /// there instead of poisoning a whole batch.
    payload: String,
    event: Event,
}

/// One caller's event batch, in flight to the writer thread.
struct AppendRequest {
    events: Vec<PendingEvent>,
    reply: AppendReply,
}

enum AppendReply {
    Sync(std::sync::mpsc::SyncSender<Result<Vec<EventEnvelope>>>),
    Async(tokio::sync::oneshot::Sender<Result<Vec<EventEnvelope>>>),
}

impl AppendReply {
    fn send(self, result: Result<Vec<EventEnvelope>>) {
        match self {
            Self::Sync(tx) => {
                let _ = tx.send(result);
            }
            Self::Async(tx) => {
                let _ = tx.send(result);
            }
        }
    }
}

/// Upper bound on events combined across callers in one writer transaction.
/// A single caller's atomic batch may be larger. This bounds how long the
/// earliest waiter can be held behind later arrivals.
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
        Scope::CodeReviewJob(id) => ("code_review_job", id.clone()),
    }
}

fn scope_from_cols(kind: &str, id: String) -> Scope {
    match kind {
        "session" => Scope::Session(id),
        "thread" => Scope::Thread(id),
        "code_review_job" => Scope::CodeReviewJob(id),
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
    scoped_events: ScopedEventSenders,
) -> std::sync::mpsc::Sender<AppendRequest> {
    let (tx, rx) = std::sync::mpsc::channel::<AppendRequest>();
    std::thread::Builder::new()
        .name("trouve-event-writer".into())
        .spawn(move || {
            let mut deferred = None;
            loop {
                let first = match deferred.take() {
                    Some(request) => request,
                    None => match rx.recv() {
                        Ok(request) => request,
                        Err(_) => break,
                    },
                };
                let mut event_count = first.events.len();
                let mut requests = vec![first];
                while event_count < APPEND_BATCH_MAX {
                    let Ok(request) = rx.try_recv() else {
                        break;
                    };
                    if event_count.saturating_add(request.events.len()) > APPEND_BATCH_MAX {
                        deferred = Some(request);
                        break;
                    }
                    event_count += request.events.len();
                    requests.push(request);
                }
                let wait_started = std::time::Instant::now();
                let (inserted, wait, elapsed) = {
                    let mut conn = conn.lock().unwrap();
                    let wait = wait_started.elapsed();
                    let started = std::time::Instant::now();
                    let inserted = insert_event_batch(
                        &mut conn,
                        requests.iter().flat_map(|request| request.events.iter()),
                        event_count,
                    );
                    (inserted, wait, started.elapsed())
                };
                if elapsed >= std::time::Duration::from_millis(20) {
                    tracing::warn!(
                        event_count,
                        request_count = requests.len(),
                        wait_ms = wait.as_millis(),
                        elapsed_ms = elapsed.as_millis(),
                        "slow event-log batch commit"
                    );
                } else {
                    tracing::trace!(
                        event_count,
                        request_count = requests.len(),
                        wait_us = wait.as_micros(),
                        elapsed_us = elapsed.as_micros(),
                        "event-log batch committed"
                    );
                }
                match inserted {
                    Ok(cursors) => {
                        let mut cursors = cursors.into_iter();
                        for request in requests {
                            let mut envelopes = Vec::with_capacity(request.events.len());
                            // One caller batch has one scope, so resolve its
                            // live sender without re-locking for every event.
                            let scoped_sender = request.events.first().and_then(|event| {
                                let (kind, id) = scope_cols(&event.scope);
                                scoped_events
                                    .lock()
                                    .unwrap()
                                    .get(&(kind.to_owned(), id))
                                    .cloned()
                            });
                            for event in request.events {
                                let envelope = EventEnvelope {
                                    cursor: cursors.next().expect("one cursor per inserted event"),
                                    scope: event.scope,
                                    ts: event.ts,
                                    event: event.event,
                                };
                                // Nobody listening is fine; a caller that gave
                                // up waiting is too.
                                let _ = events_tx.send(envelope.clone());
                                if let Some(sender) = &scoped_sender {
                                    let _ = sender.send(envelope.clone());
                                }
                                envelopes.push(envelope);
                            }
                            request.reply.send(Ok(envelopes));
                        }
                    }
                    Err(e) => {
                        // The transaction rolled back: every waiter's event
                        // was equally not persisted.
                        let message = format!("appending event batch: {e}");
                        for request in requests {
                            request.reply.send(Err(anyhow::anyhow!(message.clone())));
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
fn insert_event_batch<'a>(
    conn: &mut Connection,
    batch: impl IntoIterator<Item = &'a PendingEvent>,
    event_count: usize,
) -> Result<Vec<u64>> {
    let tx = conn.transaction()?;
    let mut cursors = Vec::with_capacity(event_count);
    let mut thread_events = Vec::new();
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO events (scope_kind, scope_id, ts, payload) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for event in batch {
            let (kind, id) = scope_cols(&event.scope);
            stmt.execute(params![kind, id, event.ts.to_rfc3339(), event.payload])?;
            let cursor = tx.last_insert_rowid() as u64;
            cursors.push(cursor);
            if let Scope::Thread(thread_id) = &event.scope {
                thread_events.push((
                    thread_id.clone(),
                    EventEnvelope {
                        cursor,
                        scope: event.scope.clone(),
                        ts: event.ts,
                        event: event.event.clone(),
                    },
                ));
            }
        }
    }
    update_thread_view_caches(&tx, &thread_events)?;
    tx.commit()?;
    Ok(cursors)
}

fn update_thread_view_caches(
    tx: &rusqlite::Transaction<'_>,
    events: &[(String, EventEnvelope)],
) -> Result<()> {
    // Rewriting a full folded transcript for every streamed delta would
    // amplify writes quadratically. Refresh an existing cache once at a turn
    // boundary instead; a snapshot request incrementally catches up an active
    // turn when necessary.
    let mut through_by_thread = HashMap::<String, u64>::new();
    for (thread_id, envelope) in events {
        if matches!(
            envelope.event,
            Event::TurnCompleted { .. } | Event::TurnFailed { .. } | Event::TurnCancelled { .. }
        ) {
            through_by_thread.insert(thread_id.clone(), envelope.cursor);
        }
    }
    for (thread_id, through) in through_by_thread {
        let cached = tx
            .query_row(
                "SELECT schema_version, state FROM thread_view_cache WHERE thread_id = ?1",
                params![&thread_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some(mut projection) = cached.and_then(|(version, state)| {
            (version == THREAD_VIEW_SCHEMA_VERSION)
                .then(|| serde_json::from_str::<ThreadProjection>(&state).ok())
                .flatten()
        }) else {
            continue;
        };
        let mut stmt = tx.prepare_cached(
            "SELECT cursor, ts, payload FROM events
             WHERE scope_kind = 'thread' AND scope_id = ?1
               AND cursor > ?2 AND cursor <= ?3
             ORDER BY cursor",
        )?;
        let rows = stmt.query_map(
            params![&thread_id, projection.cursor as i64, through as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        for row in rows {
            let (cursor, ts, payload) = row?;
            let event = match serde_json::from_str(&payload) {
                Ok(event) => event,
                Err(_) if is_retired_event(&payload) => {
                    projection.cursor = cursor;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        "skipping undeserializable event {cursor} while updating thread view: {error}"
                    );
                    projection.cursor = cursor;
                    continue;
                }
            };
            projection.apply(&EventEnvelope {
                cursor,
                scope: Scope::Thread(thread_id.clone()),
                ts: ts.parse().unwrap_or_else(|_| chrono::Utc::now()),
                event,
            });
        }
        tx.execute(
            "UPDATE thread_view_cache
             SET cursor = ?2, schema_version = ?3, state = ?4
             WHERE thread_id = ?1",
            params![
                thread_id,
                projection.cursor as i64,
                THREAD_VIEW_SCHEMA_VERSION,
                serde_json::to_string(&projection)?
            ],
        )?;
    }
    Ok(())
}

fn serialize_events(scope: Scope, events: Vec<Event>) -> Result<Vec<PendingEvent>> {
    let now = chrono::Utc::now();
    events
        .into_iter()
        .map(|event| {
            Ok(PendingEvent {
                scope: scope.clone(),
                ts: now,
                payload: serde_json::to_string(&event)?,
                event,
            })
        })
        .collect()
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
        let scoped_events = Arc::new(Mutex::new(HashMap::new()));
        let append_tx = spawn_event_writer(
            Arc::clone(&conn),
            events_tx.clone(),
            Arc::clone(&scoped_events),
        );
        Self {
            conn,
            events_tx,
            scoped_events,
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
        let (reply, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.append_tx
            .send(AppendRequest {
                events: serialize_events(scope, vec![event])?,
                reply: AppendReply::Sync(reply),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        let mut envelopes = reply_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))??;
        Ok(envelopes.pop().expect("single append returns one event"))
    }

    /// Persist a same-scope batch without blocking a Tokio worker thread.
    /// The writer commits and publishes the whole batch in input order before
    /// resolving this future, preserving the same durability contract as
    /// [`Self::append_event`].
    pub async fn append_events_async(
        &self,
        scope: Scope,
        events: Vec<Event>,
    ) -> Result<Vec<EventEnvelope>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.append_tx
            .send(AppendRequest {
                events: serialize_events(scope, events)?,
                reply: AppendReply::Async(reply),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
    }

    /// Newest persisted cursor for `scope`, or zero when the scope is empty.
    pub fn latest_event_cursor(&self, scope: &Scope) -> Result<u64> {
        let (kind, id) = scope_cols(scope);
        let conn = self.conn.lock().unwrap();
        let cursor = conn.query_row(
            "SELECT MAX(cursor) FROM events WHERE scope_kind = ?1 AND scope_id = ?2",
            params![kind, id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(cursor.unwrap_or(0) as u64)
    }

    /// Fold a thread's durable events into a cached current-state snapshot.
    ///
    /// Raw rows are captured through one cursor while holding the connection,
    /// then decoded and folded without blocking unrelated store work. A
    /// conditional cache write cannot overwrite a newer turn-boundary refresh.
    pub fn thread_view_snapshot(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<(u64, ThreadViewSnapshot)> {
        let (mut projection, cache_valid, rows) = {
            let conn = self.conn.lock().unwrap();
            let cached = conn
                .query_row(
                    "SELECT schema_version, state FROM thread_view_cache WHERE thread_id = ?1",
                    params![thread_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let mut cache_valid = false;
            let projection: ThreadProjection = cached
                .and_then(|(version, state)| {
                    (version == THREAD_VIEW_SCHEMA_VERSION)
                        .then(|| serde_json::from_str(&state).ok())
                        .flatten()
                })
                .inspect(|_| cache_valid = true)
                .unwrap_or_default();
            let through = conn.query_row(
                "SELECT MAX(cursor) FROM events WHERE scope_kind = 'thread' AND scope_id = ?1",
                params![thread_id],
                |row| row.get::<_, Option<i64>>(0),
            )?;
            let through = through.unwrap_or(0) as u64;
            let mut stmt = conn.prepare_cached(
                "SELECT cursor, ts, payload FROM events
                 WHERE scope_kind = 'thread' AND scope_id = ?1
                   AND cursor > ?2 AND cursor <= ?3
                 ORDER BY cursor",
            )?;
            let rows = stmt.query_map(
                params![thread_id, projection.cursor as i64, through as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            (
                projection,
                cache_valid,
                rows.collect::<rusqlite::Result<Vec<_>>>()?,
            )
        };
        let needs_write = !cache_valid || !rows.is_empty();
        for (cursor, ts, payload) in rows {
            let event = match serde_json::from_str(&payload) {
                Ok(event) => event,
                Err(_) if is_retired_event(&payload) => {
                    projection.cursor = cursor;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        "skipping undeserializable event {cursor} while folding thread view: {error}"
                    );
                    projection.cursor = cursor;
                    continue;
                }
            };
            projection.apply(&EventEnvelope {
                cursor,
                scope: Scope::Thread(thread_id.to_string()),
                ts: ts.parse().unwrap_or_else(|_| chrono::Utc::now()),
                event,
            });
        }
        if needs_write {
            projection.snapshot.item_offset = 0;
            projection.snapshot.total_items = 0;
            projection.snapshot.has_older = false;
            let state = serde_json::to_string(&projection)?;
            self.conn.lock().unwrap().execute(
                "INSERT INTO thread_view_cache (thread_id, cursor, schema_version, state)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(thread_id) DO UPDATE SET
                   cursor = excluded.cursor,
                   schema_version = excluded.schema_version,
                   state = excluded.state
                 WHERE thread_view_cache.cursor <= excluded.cursor",
                params![
                    thread_id,
                    projection.cursor as i64,
                    THREAD_VIEW_SCHEMA_VERSION,
                    state
                ],
            )?;
        }
        let mut snapshot = projection.snapshot;
        let total = snapshot.items.len();
        let end = before
            .and_then(|offset| usize::try_from(offset).ok())
            .unwrap_or(total)
            .min(total);
        let start = end.saturating_sub(limit.max(1));
        let items = snapshot.items.drain(start..end).collect();
        snapshot.items = items;
        snapshot.item_offset = start as u64;
        snapshot.total_items = total as u64;
        snapshot.has_older = start > 0;
        Ok((projection.cursor, snapshot))
    }

    /// Read at most `limit` persisted rows in `(after, through]`.
    ///
    /// The fixed upper cursor gives callers a stable replay snapshot while a
    /// live subscription captures concurrent appends. Pages release the
    /// SQLite connection between batches, bounding both heap use and lock
    /// hold time for large histories.
    pub fn event_replay_page(
        &self,
        scope: &Scope,
        after: u64,
        through: u64,
        limit: usize,
    ) -> Result<EventReplayPage> {
        if after >= through || limit == 0 {
            return Ok(EventReplayPage {
                events: Vec::new(),
                next_after: after,
                exhausted: true,
            });
        }
        let (kind, id) = scope_cols(scope);
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT cursor, scope_kind, scope_id, ts, payload FROM events
             WHERE scope_kind = ?1 AND scope_id = ?2
               AND cursor > ?3 AND cursor <= ?4
             ORDER BY cursor LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![kind, id, after as i64, through as i64, limit as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        let mut rows_seen = 0usize;
        let mut next_after = after;
        for row in rows {
            let (cursor, kind, id, ts, payload) = row?;
            rows_seen += 1;
            next_after = cursor;
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
        Ok(EventReplayPage {
            events: out,
            next_after,
            exhausted: next_after >= through || rows_seen < limit,
        })
    }

    /// Persisted events for a scope after `after` (exclusive), oldest first.
    ///
    /// Callers that consume events incrementally should prefer
    /// [`Self::event_replay_page`]. This compatibility helper still returns a
    /// single vector, but uses bounded database pages while assembling it.
    pub fn events_after(&self, scope: &Scope, after: u64) -> Result<Vec<EventEnvelope>> {
        const PAGE_SIZE: usize = 256;

        let through = self.latest_event_cursor(scope)?;
        let mut cursor = after;
        let mut out = Vec::new();
        while cursor < through {
            let page = self.event_replay_page(scope, cursor, through, PAGE_SIZE)?;
            let next = page.next_after;
            out.extend(page.events);
            if page.exhausted || next <= cursor {
                break;
            }
            cursor = next;
        }
        Ok(out)
    }

    /// Most recently persisted account PR snapshot for `host`.
    ///
    /// The scan runs newest-first in bounded pages and stops at the first
    /// matching host. Payloads are decoded after releasing the SQLite
    /// connection mutex so a cold or missing host cannot block unrelated
    /// store operations for the full server history.
    pub fn latest_github_pr_snapshot(&self, host: &str) -> Result<Option<GithubPrList>> {
        const PAGE_SIZE: usize = 64;

        let mut before = i64::MAX;
        loop {
            let page = {
                let conn = self.conn.lock().unwrap();
                let mut stmt = conn.prepare(
                    "SELECT cursor, payload FROM events
                     WHERE scope_kind = 'server' AND scope_id = '' AND cursor < ?1
                     ORDER BY cursor DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![before, PAGE_SIZE as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut page = Vec::new();
                for row in rows {
                    page.push(row?);
                }
                page
            };
            let Some((oldest, _)) = page.last() else {
                return Ok(None);
            };
            before = *oldest;
            let exhausted = page.len() < PAGE_SIZE;
            for (_, payload) in page {
                let Ok(Event::GithubPullRequestsUpdated { pull_requests }) =
                    serde_json::from_str::<Event>(&payload)
                else {
                    continue;
                };
                if pull_requests.host.eq_ignore_ascii_case(host) {
                    return Ok(Some(pull_requests));
                }
            }
            if exhausted {
                return Ok(None);
            }
        }
    }

    /// Live subscription to all events; callers filter by scope.
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events_tx.subscribe()
    }

    /// Live subscription to exactly one event scope. Persisted replay still
    /// comes from `events_after`; this channel only avoids waking every active
    /// thread and SSE follower for unrelated live events.
    pub fn subscribe_scope(&self, scope: &Scope) -> broadcast::Receiver<EventEnvelope> {
        let (kind, id) = scope_cols(scope);
        let key = (kind.to_owned(), id);
        let mut senders = self.scoped_events.lock().unwrap();
        if senders.len() > 1_024 {
            senders.retain(|_, sender| sender.receiver_count() > 0);
        }
        senders
            .entry(key)
            .or_insert_with(|| broadcast::channel(1_024).0)
            .subscribe()
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
        self.enqueue_prompt_with_tools(thread_id, content, attachments, true)
    }

    pub(crate) fn enqueue_prompt_with_tools(
        &self,
        thread_id: &str,
        content: &str,
        attachments: &[trouve_protocol::Attachment],
        tools_enabled: bool,
    ) -> Result<trouve_protocol::QueuedPrompt> {
        let conn = self.conn.lock().unwrap();
        let id = format!("qp_{}", uuid::Uuid::new_v4().simple());
        let created_at = chrono::Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(attachments)?;
        conn.execute(
            "INSERT INTO queued_prompts
               (id, thread_id, position, content, attachments, tools_enabled, created_at)
             VALUES (?1, ?2,
               (SELECT COALESCE(MAX(position), 0) + 1 FROM queued_prompts WHERE thread_id = ?2),
               ?3, ?4, ?5, ?6)",
            params![
                id,
                thread_id,
                content,
                attachments_json,
                tools_enabled,
                created_at
            ],
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

    pub(crate) fn queued_prompt_tools_enabled(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT tools_enabled FROM queued_prompts WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?)
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
            let repositories: Vec<(String, String, String, String, String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT repository, identity_ids, included_reviewer_ids,
                            excluded_reviewer_ids, reviewer_overrides, routing_mode
                     FROM code_review_repositories",
                )?;
                stmt.query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?
            };
            let built_in_ids = crate::reviewers::built_in_reviewers()
                .into_iter()
                .map(|reviewer| reviewer.id)
                .collect::<HashSet<_>>();
            let fallback_reviewer_id = crate::reviewers::default_reviewer_ids().into_iter().next();
            for (
                repository,
                encoded_ids,
                encoded_included,
                encoded_excluded,
                encoded_overrides,
                routing_mode,
            ) in repositories
            {
                let mut ids: Vec<String> = serde_json::from_str(&encoded_ids).unwrap_or_default();
                let mut included: Vec<String> =
                    serde_json::from_str(&encoded_included).unwrap_or_default();
                let mut excluded: Vec<String> =
                    serde_json::from_str(&encoded_excluded).unwrap_or_default();
                let mut overrides: Vec<trouve_protocol::ReviewerOverride> =
                    serde_json::from_str(&encoded_overrides).unwrap_or_default();
                let before_ids = ids.len();
                let before_included = included.len();
                let before_excluded = excluded.len();
                let before_overrides = overrides.len();
                ids.retain(|reviewer_id| reviewer_id != id);
                included.retain(|reviewer_id| reviewer_id != id);
                excluded.retain(|reviewer_id| reviewer_id != id);
                overrides.retain(|reviewer_override| reviewer_override.reviewer_id != id);
                let changed = ids.len() != before_ids
                    || included.len() != before_included
                    || excluded.len() != before_excluded
                    || overrides.len() != before_overrides;
                if changed
                    && !matches!(routing_mode.as_str(), "core" | "manual")
                    && built_in_ids
                        .iter()
                        .all(|reviewer_id| excluded.contains(reviewer_id))
                    && let Some(fallback_reviewer_id) = &fallback_reviewer_id
                {
                    excluded.retain(|reviewer_id| reviewer_id != fallback_reviewer_id);
                }
                if changed {
                    if ids.is_empty() {
                        ids = crate::reviewers::default_reviewer_ids();
                    }
                    tx.execute(
                        "UPDATE code_review_repositories
                         SET identity_ids = ?2, included_reviewer_ids = ?3,
                             excluded_reviewer_ids = ?4, reviewer_overrides = ?5,
                             updated_at = ?6
                         WHERE repository = ?1",
                        params![
                            repository,
                            serde_json::to_string(&ids)?,
                            serde_json::to_string(&included)?,
                            serde_json::to_string(&excluded)?,
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
                    identity_ids, routing_mode, semantic_routing,
                    included_reviewer_ids, excluded_reviewer_ids, reviewer_overrides,
                    router_model, router_thinking_level, coordinator_thinking_level
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
        let routing_mode = request.routing_mode.map(code_review_routing_mode_str);
        let included_reviewer_ids = request
            .included_reviewer_ids
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let excluded_reviewer_ids = request
            .excluded_reviewer_ids
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let default_reviewer_ids =
            serde_json::to_string(&crate::reviewers::default_reviewer_ids())?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_repositories
                    (repository, installation_id, private, mode, model, prompt,
                     identity_ids, routing_mode, semantic_routing,
                     included_reviewer_ids, excluded_reviewer_ids,
                     reviewer_overrides, router_model, router_thinking_level,
                     coordinator_thinking_level, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4, ?5,
                     COALESCE(?6, ?16), COALESCE(?7, 'additive'),
                     COALESCE(?8, 1), COALESCE(?9, '[]'),
                     COALESCE(?10, '[]'), COALESCE(?11, '[]'), ?12, ?13, ?14, ?15)
             ON CONFLICT(repository) DO UPDATE SET
               installation_id = excluded.installation_id,
               mode = excluded.mode,
               model = excluded.model,
               prompt = excluded.prompt,
               identity_ids = COALESCE(?6, code_review_repositories.identity_ids),
               routing_mode = COALESCE(?7, code_review_repositories.routing_mode),
               semantic_routing = COALESCE(?8, code_review_repositories.semantic_routing),
               included_reviewer_ids =
                   COALESCE(?9, code_review_repositories.included_reviewer_ids),
               excluded_reviewer_ids =
                   COALESCE(?10, code_review_repositories.excluded_reviewer_ids),
               reviewer_overrides =
                   COALESCE(?11, code_review_repositories.reviewer_overrides),
               router_model = excluded.router_model,
               router_thinking_level = excluded.router_thinking_level,
               coordinator_thinking_level = excluded.coordinator_thinking_level,
               updated_at = excluded.updated_at",
            params![
                request.repository,
                request.installation_id as i64,
                code_review_mode_str(request.mode),
                request.model,
                request.prompt,
                reviewer_ids,
                routing_mode,
                request.semantic_routing,
                included_reviewer_ids,
                excluded_reviewer_ids,
                reviewer_overrides,
                request.router_model,
                request.router_thinking_level,
                request.coordinator_thinking_level,
                chrono::Utc::now().to_rfc3339(),
                default_reviewer_ids,
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
        let included_reviewer_ids = serde_json::to_string(&new_job.included_reviewer_ids)?;
        let excluded_reviewer_ids = serde_json::to_string(&new_job.excluded_reviewer_ids)?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO code_review_jobs
                    (id, dedupe_key, installation_id, repository, pull_number, pull_title,
                     pull_url, head_sha, base_ref, head_ref, trigger, status, model, prompt,
                     identities, config_hash, created_at, review_base_sha, review_scope,
                     retry_of, total_reviewers, routing_mode, semantic_routing,
                     included_reviewer_ids, excluded_reviewer_ids, router_model,
                     router_thinking_level, coordinator_thinking_level)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'queued',
                     ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                     ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
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
                new_job.review_base_sha,
                code_review_scope_str(new_job.scope),
                new_job.retry_of,
                new_job.reviewers.len() as i64,
                code_review_routing_mode_str(new_job.routing_mode),
                new_job.semantic_routing,
                included_reviewer_ids,
                excluded_reviewer_ids,
                new_job.router_model,
                new_job.router_thinking_level,
                new_job.coordinator_thinking_level,
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
        self.list_code_review_jobs_filtered(limit, None, None)
    }

    pub fn list_code_review_jobs_filtered(
        &self,
        limit: usize,
        status: Option<&str>,
        repository: Option<&str>,
    ) -> Result<Vec<trouve_protocol::CodeReviewJob>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs
             WHERE (?1 IS NULL OR status = ?1)
               AND (?2 IS NULL OR repository = ?2)
             ORDER BY
               CASE status WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
               CASE WHEN status = 'queued' THEN created_at END ASC,
               CASE WHEN status NOT IN ('running', 'queued')
                    THEN COALESCE(completed_at, created_at) END DESC,
               created_at DESC
             LIMIT ?3"
        ))?;
        let rows = stmt.query_map(
            params![status, repository, limit as i64],
            row_to_code_review_job,
        )?;
        let records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(records.into_iter().map(|record| record.job).collect())
    }

    pub fn code_review_jobs_with_projection_errors(
        &self,
        limit: usize,
    ) -> Result<Vec<trouve_protocol::CodeReviewJob>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs
             WHERE check_sync_error != ''
               AND projection_retryable != 0
               AND (projection_retry_at IS NULL OR projection_retry_at <= ?1)
             ORDER BY projection_retry_at,
                      COALESCE(completed_at, started_at, created_at)
             LIMIT ?2"
        ))?;
        let rows = stmt.query_map(
            params![chrono::Utc::now().to_rfc3339(), limit as i64],
            row_to_code_review_job,
        )?;
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
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let now = chrono::Utc::now().to_rfc3339();
        let interrupted_reviewers = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks
                 WHERE status IN ('queued', 'running') AND role = 'reviewer'
                   AND job_id IN (
                     SELECT id FROM code_review_jobs WHERE status = 'running'
                   )
                 ORDER BY created_at, rowid"
            ))?;
            let rows = stmt.query_map([], row_to_code_review_task)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute(
            "UPDATE code_review_tasks
             SET status = 'failed', completed_at = ?1,
                 error = 'server restarted while task was running',
                 model_elapsed_ms = CASE
                   WHEN model_started_at IS NULL THEN model_elapsed_ms
                   ELSE model_elapsed_ms + MAX(
                     0,
                     CAST(
                       (julianday(?1) - MAX(
                         julianday(model_started_at),
                         julianday(COALESCE(last_progress_at, model_started_at))
                       )) * 86400000 AS INTEGER
                     )
                   )
                 END
             WHERE status IN ('queued', 'running')
               AND job_id IN (SELECT id FROM code_review_jobs WHERE status = 'running')",
            params![now],
        )?;
        // Resuming a job reuses successful reviewer outputs. Give every
        // interrupted reviewer batch a fresh queued attempt so the resume
        // path reruns it instead of treating the crash marker above as an
        // intentional persona failure.
        for task in interrupted_reviewers {
            tx.execute(
                "INSERT INTO code_review_tasks
                        (id, job_id, role, reviewer_id, reviewer_name, batch_index,
                         batch_count, status, model, prompt, created_at)
                 VALUES (?1, ?2, 'reviewer', ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9)",
                params![
                    crate::new_id("rvt"),
                    task.job_id,
                    task.reviewer_id,
                    task.reviewer_name,
                    task.batch_index as i64,
                    task.batch_count as i64,
                    task.model,
                    task.prompt,
                    now,
                ],
            )?;
        }
        tx.execute(
            "UPDATE code_review_jobs
             SET status = 'queued', started_at = NULL, cancel_requested = 0,
                 publication_claimed = 0,
                 error = 'server restarted while review was running'
             WHERE status = 'running'",
            [],
        )?;
        tx.commit()?;
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
            "UPDATE code_review_jobs
             SET status = 'running', started_at = ?2, completed_at = NULL,
                 cancel_requested = 0, publication_claimed = 0, error = ''
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

    pub fn set_code_review_job_review_base(&self, id: &str, review_base_sha: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET review_base_sha = ?2
             WHERE id = ?1 AND status = 'running'",
            params![id, review_base_sha],
        )? > 0)
    }

    pub fn set_code_review_job_phase_elapsed(
        &self,
        id: &str,
        phase: CodeReviewJobPhase,
        elapsed_ms: u64,
    ) -> Result<()> {
        let column = match phase {
            CodeReviewJobPhase::Preparation => "preparation_elapsed_ms",
            CodeReviewJobPhase::Reviewers => "reviewer_elapsed_ms",
            CodeReviewJobPhase::Coordinator => "coordinator_elapsed_ms",
            CodeReviewJobPhase::Publication => "publication_elapsed_ms",
        };
        self.conn.lock().unwrap().execute(
            &format!(
                "UPDATE code_review_jobs SET {column} = ?2
                 WHERE id = ?1 AND status = 'running'"
            ),
            params![id, elapsed_ms as i64],
        )?;
        Ok(())
    }

    pub fn code_review_routing_decisions(
        &self,
        job_id: &str,
    ) -> Result<Vec<trouve_protocol::CodeReviewRoutingDecision>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT batch_index, reviewer_id, reviewer_name, selected, reasons
             FROM code_review_routing_decisions
             WHERE job_id = ?1
             ORDER BY batch_index, reviewer_id",
        )?;
        let rows = stmt.query_map([job_id], row_to_code_review_routing_decision)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Persist the complete routing snapshot once. Interrupted executions and
    /// persona retries reuse it instead of making different routing choices.
    pub fn save_code_review_routing_decisions(
        &self,
        job_id: &str,
        decisions: &[trouve_protocol::CodeReviewRoutingDecision],
    ) -> Result<Vec<trouve_protocol::CodeReviewRoutingDecision>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: i64 = tx.query_row(
            "SELECT COUNT(*) FROM code_review_routing_decisions WHERE job_id = ?1",
            [job_id],
            |row| row.get(0),
        )?;
        if existing == 0 && !decisions.is_empty() {
            let running: bool = tx.query_row(
                "SELECT status = 'running' FROM code_review_jobs WHERE id = ?1",
                [job_id],
                |row| row.get(0),
            )?;
            if !running {
                anyhow::bail!("review job {job_id} is no longer running");
            }
            let mut insert = tx.prepare(
                "INSERT INTO code_review_routing_decisions
                        (job_id, batch_index, reviewer_id, reviewer_name, selected, reasons)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for decision in decisions {
                insert.execute(params![
                    job_id,
                    decision.batch_index as i64,
                    decision.reviewer_id,
                    decision.reviewer_name,
                    decision.selected,
                    serde_json::to_string(&decision.reasons)?,
                ])?;
            }
        }
        let routed = {
            let mut stmt = tx.prepare(
                "SELECT batch_index, reviewer_id, reviewer_name, selected, reasons
                 FROM code_review_routing_decisions
                 WHERE job_id = ?1
                 ORDER BY batch_index, reviewer_id",
            )?;
            stmt.query_map([job_id], row_to_code_review_routing_decision)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.commit()?;
        Ok(routed)
    }

    pub fn create_code_review_task(
        &self,
        task: &NewCodeReviewTask,
    ) -> Result<trouve_protocol::CodeReviewTask> {
        let id = crate::new_id("rvt");
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO code_review_tasks
                    (id, job_id, role, reviewer_id, reviewer_name, batch_index,
                     batch_count, status, model, prompt, created_at, last_progress_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?9, ?10, ?10
             WHERE EXISTS (
               SELECT 1 FROM code_review_jobs WHERE id = ?2 AND status = 'running'
             )",
            params![
                id,
                task.job_id,
                code_review_task_role_str(task.role),
                task.reviewer_id,
                task.reviewer_name,
                task.batch_index as i64,
                task.batch_count as i64,
                task.model,
                task.prompt,
                now,
            ],
        )?;
        if inserted == 0 {
            anyhow::bail!(
                "review job {} is no longer running; task creation was superseded",
                task.job_id
            );
        }
        conn.query_row(
            &format!("SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks WHERE id = ?1"),
            params![id],
            row_to_code_review_task,
        )
        .map_err(Into::into)
    }

    pub fn start_code_review_task(
        &self,
        id: &str,
        session_id: &str,
        thread_id: &str,
        model: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewTask>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let updated = conn.execute(
            "UPDATE code_review_tasks
             SET status = 'running', session_id = ?2, thread_id = ?3,
                 model = ?4, started_at = ?5, error = '',
                 lifecycle_stage = 'waiting_for_capacity', last_progress_at = ?5
             WHERE id = ?1 AND status = 'queued'",
            params![id, session_id, thread_id, model, now],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            &format!("SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks WHERE id = ?1"),
            params![id],
            row_to_code_review_task,
        )?))
    }

    pub fn skip_code_review_task(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewTask>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let updated = conn.execute(
            "UPDATE code_review_tasks
             SET status = 'not_applicable', completed_at = ?2, error = ?3,
                 lifecycle_stage = 'completed', last_progress_at = ?2
             WHERE id = ?1 AND status = 'queued'",
            params![id, now, reason],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            &format!("SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks WHERE id = ?1"),
            params![id],
            row_to_code_review_task,
        )?))
    }

    pub fn is_code_review_thread(&self, thread_id: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().query_row(
            "SELECT EXISTS(
                   SELECT 1 FROM code_review_tasks
                   WHERE thread_id = ?1 AND status IN ('running', 'queued')
                 )",
            params![thread_id],
            |row| row.get(0),
        )?)
    }

    pub fn set_code_review_task_provider_wait(
        &self,
        thread_id: &str,
        provider_wait_ms: u64,
    ) -> Result<Option<CodeReviewTaskProgressRecord>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let updated = conn.execute(
            "UPDATE code_review_tasks
             SET provider_wait_ms = provider_wait_ms + ?2,
                 lifecycle_stage = CASE
                   WHEN lifecycle_stage = 'waiting_for_capacity' THEN 'starting_model'
                   ELSE lifecycle_stage
                 END,
                 last_progress_at = ?3
             WHERE thread_id = ?1 AND status = 'running'",
            params![thread_id, provider_wait_ms as i64, now],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            &format!(
                "SELECT {CODE_REVIEW_TASK_PROGRESS_COLUMNS}
                 FROM code_review_tasks WHERE thread_id = ?1 AND status = 'running'"
            ),
            params![thread_id],
            row_to_code_review_task_progress,
        )?))
    }

    pub fn set_code_review_task_progress(
        &self,
        id: &str,
        lifecycle_stage: trouve_protocol::CodeReviewTaskLifecycleStage,
        metrics: &CodeReviewTaskMetrics,
        model_timing: CodeReviewModelTiming,
    ) -> Result<Option<CodeReviewTaskProgressRecord>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let (replace_model_started_at, model_started_at) = match model_timing {
            CodeReviewModelTiming::Preserve => (false, None),
            CodeReviewModelTiming::Reset => (true, None),
            CodeReviewModelTiming::Started => (true, Some(now.as_str())),
        };
        let updated = conn.execute(
            "UPDATE code_review_tasks
             SET model_elapsed_ms = ?2, input_tokens = ?3,
                 cached_input_tokens = ?4, output_tokens = ?5,
                 tool_call_count = ?6, lifecycle_stage = ?7,
                 model_started_at = CASE WHEN ?8 THEN ?9 ELSE model_started_at END,
                 last_progress_at = ?10
             WHERE id = ?1 AND status = 'running'",
            params![
                id,
                metrics.model_elapsed_ms as i64,
                metrics.input_tokens as i64,
                metrics.cached_input_tokens as i64,
                metrics.output_tokens as i64,
                metrics.tool_call_count as i64,
                code_review_task_lifecycle_stage_str(lifecycle_stage),
                replace_model_started_at,
                model_started_at,
                now,
            ],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            &format!(
                "SELECT {CODE_REVIEW_TASK_PROGRESS_COLUMNS}
                 FROM code_review_tasks WHERE id = ?1"
            ),
            params![id],
            row_to_code_review_task_progress,
        )?))
    }

    pub fn append_code_review_task_output(
        &self,
        id: &str,
        stream: trouve_protocol::CodeReviewOutputStream,
        text: &str,
    ) -> Result<bool> {
        let column = match stream {
            trouve_protocol::CodeReviewOutputStream::Assistant => "output",
            trouve_protocol::CodeReviewOutputStream::Thinking => "thinking",
            trouve_protocol::CodeReviewOutputStream::Tool => "tool_output",
        };
        let sql = format!(
            "UPDATE code_review_tasks SET {column} = {column} || ?2
             WHERE id = ?1 AND status = 'running'"
        );
        Ok(self.conn.lock().unwrap().execute(&sql, params![id, text])? > 0)
    }

    pub fn finish_code_review_task(
        &self,
        id: &str,
        status: &str,
        output: &str,
        candidate_issue_count: u64,
        error: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewTask>> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now();
        let current = conn
            .query_row(
                "SELECT model_started_at, model_elapsed_ms, lifecycle_stage, last_progress_at
                 FROM code_review_tasks WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((model_started_at, recorded_model_elapsed_ms, current_stage, last_progress_at)) =
            current
        else {
            return Ok(None);
        };
        let model_elapsed_ms = if matches!(status, "failed" | "cancelled") {
            finalize_code_review_model_elapsed(
                recorded_model_elapsed_ms,
                model_started_at,
                last_progress_at,
                now,
            )
        } else {
            recorded_model_elapsed_ms.max(0) as u64
        };
        let completed = matches!(status, "succeeded" | "not_applicable");
        let lifecycle_stage = if completed {
            code_review_task_lifecycle_stage_str(
                trouve_protocol::CodeReviewTaskLifecycleStage::Completed,
            )
        } else {
            current_stage.as_str()
        };
        let now = now.to_rfc3339();
        let terminal_progress_at = completed.then_some(now.as_str());
        let updated = conn.execute(
            "UPDATE code_review_tasks
             SET status = ?2,
                 output = CASE WHEN ?3 = '' THEN output ELSE ?3 END,
                 candidate_issue_count = ?4, error = ?5, completed_at = ?6,
                 model_elapsed_ms = ?7, lifecycle_stage = ?8,
                 last_progress_at = COALESCE(?9, last_progress_at)
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![
                id,
                status,
                output,
                candidate_issue_count as i64,
                error,
                now,
                model_elapsed_ms as i64,
                lifecycle_stage,
                terminal_progress_at,
            ],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        Ok(Some(conn.query_row(
            &format!("SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks WHERE id = ?1"),
            params![id],
            row_to_code_review_task,
        )?))
    }

    fn code_review_task_attempts(&self, job_id: &str) -> Result<Vec<CodeReviewTaskAttempt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CODE_REVIEW_TASK_COLUMNS},
                    rowid AS {CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN}
             FROM code_review_tasks
             WHERE job_id = ?1
             ORDER BY CASE role WHEN 'reviewer' THEN 0 ELSE 1 END,
                      reviewer_name, batch_index, created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![job_id], row_to_code_review_task_attempt)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn code_review_task(
        &self,
        job_id: &str,
        task_id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewTask>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks
                     WHERE job_id = ?1 AND id = ?2"
                ),
                params![job_id, task_id],
                row_to_code_review_task,
            )
            .optional()?)
    }

    fn code_review_task_summaries(&self, job_id: &str) -> Result<Vec<CodeReviewTaskAttempt>> {
        let conn = self.conn.lock().unwrap();
        let columns = code_review_task_summary_columns();
        let mut stmt = conn.prepare(&format!(
            "SELECT {columns},
                    rowid AS {CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN}
             FROM code_review_tasks
             WHERE job_id = ?1
             ORDER BY CASE role WHEN 'reviewer' THEN 0 ELSE 1 END,
                      reviewer_name, batch_index, created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![job_id], row_to_code_review_task_attempt)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn code_review_tasks(&self, job_id: &str) -> Result<Vec<trouve_protocol::CodeReviewTask>> {
        Ok(self
            .code_review_task_attempts(job_id)?
            .into_iter()
            .map(|attempt| attempt.task)
            .collect())
    }

    pub(crate) fn latest_code_review_reviewer_tasks(
        &self,
        job_id: &str,
    ) -> Result<Vec<trouve_protocol::CodeReviewTask>> {
        let attempts = self.code_review_task_attempts(job_id)?;
        Ok(latest_code_review_task_attempts(&attempts)
            .into_iter()
            .map(|attempt| attempt.task.clone())
            .collect())
    }

    pub fn set_code_review_job_progress(
        &self,
        id: &str,
        completed_reviewers: u64,
        total_reviewers: u64,
    ) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs
             SET completed_reviewers = ?2, total_reviewers = ?3
             WHERE id = ?1 AND status = 'running'
               AND (completed_reviewers != ?2 OR total_reviewers != ?3)",
            params![id, completed_reviewers as i64, total_reviewers as i64],
        )? > 0)
    }

    pub fn completed_code_review_personas(&self, job_id: &str) -> Result<u64> {
        let attempts = self.code_review_task_attempts(job_id)?;
        let mut grouped: BTreeMap<String, Vec<&trouve_protocol::CodeReviewTask>> = BTreeMap::new();
        for attempt in latest_code_review_task_attempts(&attempts) {
            let task = &attempt.task;
            if let Some(reviewer_id) = task.reviewer_id.as_ref() {
                grouped.entry(reviewer_id.clone()).or_default().push(task);
            }
        }
        Ok(grouped
            .into_values()
            .filter(|tasks| {
                tasks.iter().all(|task| {
                    matches!(
                        task.status.as_str(),
                        "succeeded" | "failed" | "cancelled" | "not_applicable"
                    )
                }) && tasks.len() as u64
                    >= tasks.iter().map(|task| task.batch_count).max().unwrap_or(0)
            })
            .count() as u64)
    }

    /// Requeue only failed or cancelled batches belonging to one reviewer
    /// persona.
    ///
    /// Successful task attempts remain durable and are reused when the job
    /// resumes. New queued rows retain each failed attempt for inspection
    /// while making the newest attempt authoritative for persona rollups.
    pub fn retry_code_review_persona(
        &self,
        id: &str,
        reviewer_id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewJob>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let old = tx
            .query_row(
                &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
                params![id],
                row_to_code_review_job,
            )
            .optional()?;
        let Some(old) = old else {
            tx.commit()?;
            return Ok(None);
        };
        if old.job.status != "failed" {
            anyhow::bail!("reviewer personas can only be retried after the review job fails");
        }
        if old.job.session_id.is_some() {
            anyhow::bail!("review session cleanup is still pending; retry shortly");
        }

        let attempts = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CODE_REVIEW_TASK_COLUMNS},
                        rowid AS {CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN}
                 FROM code_review_tasks
                 WHERE job_id = ?1 AND role = 'reviewer' AND reviewer_id = ?2
                 ORDER BY rowid"
            ))?;
            let rows = stmt.query_map(params![id, reviewer_id], row_to_code_review_task_attempt)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if attempts.is_empty() {
            anyhow::bail!("reviewer persona {reviewer_id} was not part of review job {id}");
        }
        let retryable = latest_code_review_task_attempts(&attempts)
            .into_iter()
            .map(|attempt| &attempt.task)
            .filter(|task| matches!(task.status.as_str(), "failed" | "cancelled"))
            .collect::<Vec<_>>();
        if retryable.is_empty() {
            anyhow::bail!(
                "reviewer persona {reviewer_id} has no failed or cancelled batches to retry"
            );
        }

        let now = chrono::Utc::now().to_rfc3339();
        for task in retryable {
            tx.execute(
                "INSERT INTO code_review_tasks
                        (id, job_id, role, reviewer_id, reviewer_name, batch_index,
                         batch_count, status, model, prompt, created_at)
                 VALUES (?1, ?2, 'reviewer', ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9)",
                params![
                    crate::new_id("rvt"),
                    id,
                    reviewer_id,
                    task.reviewer_name,
                    task.batch_index as i64,
                    task.batch_count as i64,
                    task.model,
                    task.prompt,
                    now,
                ],
            )?;
        }
        let updated = tx.execute(
            "UPDATE code_review_jobs
             SET status = 'queued', started_at = NULL, completed_at = NULL,
                 error = '', cancel_requested = 0, publication_claimed = 0,
                 completed_reviewers = MAX(completed_reviewers - 1, 0)
             WHERE id = ?1 AND status = 'failed'",
            params![id],
        )?;
        if updated == 0 {
            anyhow::bail!("review job changed before the reviewer retry was queued");
        }
        let retried = tx.query_row(
            &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
            params![id],
            row_to_code_review_job,
        )?;
        tx.commit()?;
        Ok(Some(retried.job))
    }

    pub fn save_code_review_result(
        &self,
        job_id: &str,
        summary: &str,
        prompt_for_agents: &str,
        candidate_issue_count: u64,
        findings: &[NewCodeReviewFinding],
        candidate_rejections: &[trouve_protocol::CodeReviewCandidateRejection],
    ) -> Result<Vec<trouve_protocol::CodeReviewFinding>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM code_review_finding_sources
             WHERE finding_id IN (SELECT id FROM code_review_findings WHERE job_id = ?1)",
            params![job_id],
        )?;
        tx.execute(
            "DELETE FROM code_review_findings WHERE job_id = ?1",
            params![job_id],
        )?;
        tx.execute(
            "DELETE FROM code_review_candidate_rejections WHERE job_id = ?1",
            params![job_id],
        )?;
        tx.execute(
            "UPDATE code_review_tasks
             SET confirmed_issue_count = 0
             WHERE job_id = ?1",
            params![job_id],
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        for finding in findings {
            let finding_id = crate::new_id("rvf");
            tx.execute(
                "INSERT INTO code_review_findings
                        (id, job_id, path, line, side, severity, body,
                         prompt_for_agents, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9)",
                params![
                    finding_id,
                    job_id,
                    finding.path,
                    finding.line as i64,
                    finding.side,
                    finding.severity,
                    finding.body,
                    finding.prompt_for_agents,
                    now,
                ],
            )?;
            for source in &finding.sources {
                tx.execute(
                    "INSERT INTO code_review_finding_sources
                            (finding_id, candidate_id, task_id, reviewer_id, reviewer_name)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        finding_id,
                        source.candidate_id,
                        source.task_id,
                        source.reviewer_id,
                        source.reviewer_name,
                    ],
                )?;
                if !source.task_id.is_empty() {
                    tx.execute(
                        "UPDATE code_review_tasks
                         SET confirmed_issue_count = confirmed_issue_count + 1
                         WHERE id = ?1 AND job_id = ?2",
                        params![source.task_id, job_id],
                    )?;
                }
            }
        }
        for rejection in candidate_rejections {
            tx.execute(
                "INSERT INTO code_review_candidate_rejections
                        (candidate_id, job_id, task_id, reviewer_id, reviewer_name,
                         path, line, side, severity, body, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    rejection.candidate_id,
                    job_id,
                    rejection.task_id,
                    rejection.reviewer_id,
                    rejection.reviewer_name,
                    rejection.path,
                    rejection.line as i64,
                    rejection.side,
                    rejection.severity,
                    rejection.body,
                    rejection.reason,
                    now,
                ],
            )?;
        }
        tx.execute(
            "UPDATE code_review_jobs
             SET summary = ?2, prompt_for_agents = ?3,
                 candidate_issue_count = ?4, issue_count = ?5
             WHERE id = ?1",
            params![
                job_id,
                summary,
                prompt_for_agents,
                candidate_issue_count as i64,
                findings.len() as i64
            ],
        )?;
        tx.commit()?;
        drop(conn);
        self.code_review_findings(job_id)
    }

    fn code_review_candidate_rejections(
        &self,
        job_id: &str,
    ) -> Result<Vec<trouve_protocol::CodeReviewCandidateRejection>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT candidate_id, task_id, reviewer_id, reviewer_name,
                    path, line, side, severity, body, reason
             FROM code_review_candidate_rejections
             WHERE job_id = ?1
             ORDER BY reviewer_name, path, line, candidate_id",
        )?;
        Ok(stmt
            .query_map(params![job_id], |row| {
                Ok(trouve_protocol::CodeReviewCandidateRejection {
                    candidate_id: row.get(0)?,
                    task_id: row.get(1)?,
                    reviewer_id: row.get(2)?,
                    reviewer_name: row.get(3)?,
                    path: row.get(4)?,
                    line: row.get::<_, i64>(5)? as u64,
                    side: row.get(6)?,
                    severity: row.get(7)?,
                    body: row.get(8)?,
                    reason: row.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_code_review_finding_publication(
        &self,
        id: &str,
        comment_id: Option<u64>,
        comment_url: &str,
        thread_id: Option<&str>,
    ) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET github_comment_id = ?2, github_comment_url = ?3,
                 github_thread_id = COALESCE(?4, github_thread_id)
             WHERE id = ?1",
            params![
                id,
                comment_id.map(|value| value as i64),
                comment_url,
                thread_id
            ],
        )? > 0)
    }

    pub fn resolve_code_review_finding(&self, id: &str, status: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET status = ?2, resolved_at = ?3
             WHERE id = ?1 AND status = 'open'",
            params![id, status, chrono::Utc::now().to_rfc3339()],
        )? > 0)
    }

    pub fn code_review_findings(
        &self,
        job_id: &str,
    ) -> Result<Vec<trouve_protocol::CodeReviewFinding>> {
        let conn = self.conn.lock().unwrap();
        let base_rows: Vec<trouve_protocol::CodeReviewFinding> = {
            let mut stmt = conn.prepare(
                "SELECT id, job_id, path, line, side, severity, body,
                        prompt_for_agents, status, github_comment_id,
                        github_comment_url, github_thread_id, resolved_at
                 FROM code_review_findings
                 WHERE job_id = ?1 ORDER BY path, line, id",
            )?;
            stmt.query_map(params![job_id], |row| {
                Ok(trouve_protocol::CodeReviewFinding {
                    id: row.get(0)?,
                    job_id: row.get(1)?,
                    path: row.get(2)?,
                    line: row.get::<_, i64>(3)? as u64,
                    side: row.get(4)?,
                    severity: row.get(5)?,
                    body: row.get(6)?,
                    prompt_for_agents: row.get(7)?,
                    status: row.get(8)?,
                    sources: Vec::new(),
                    github_comment_id: row.get::<_, Option<i64>>(9)?.map(|value| value as u64),
                    github_comment_url: row.get(10)?,
                    github_thread_id: row.get(11)?,
                    resolved_at: parse_optional_datetime(row.get(12)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut findings = Vec::with_capacity(base_rows.len());
        for mut finding in base_rows {
            let mut stmt = conn.prepare(
                "SELECT reviewer_id, reviewer_name, candidate_id, task_id
                 FROM code_review_finding_sources
                 WHERE finding_id = ?1 ORDER BY reviewer_name, candidate_id",
            )?;
            finding.sources = stmt
                .query_map(params![finding.id], |row| {
                    Ok(trouve_protocol::CodeReviewFindingSource {
                        reviewer_id: row.get(0)?,
                        reviewer_name: row.get(1)?,
                        candidate_id: row.get(2)?,
                        task_id: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            findings.push(finding);
        }
        Ok(findings)
    }

    pub fn open_code_review_findings(
        &self,
        repository: &str,
        pull_number: u64,
    ) -> Result<Vec<trouve_protocol::CodeReviewFinding>> {
        let job_ids: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT f.job_id
                 FROM code_review_findings f
                 JOIN code_review_jobs j ON j.id = f.job_id
                 WHERE j.repository = ?1 AND j.pull_number = ?2 AND f.status = 'open'
                 ORDER BY j.completed_at, f.job_id",
            )?;
            stmt.query_map(params![repository, pull_number as i64], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut findings = Vec::new();
        for job_id in job_ids {
            findings.extend(
                self.code_review_findings(&job_id)?
                    .into_iter()
                    .filter(|finding| finding.status == "open"),
            );
        }
        Ok(findings)
    }

    pub fn code_review_job_detail(
        &self,
        id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewJobDetail>> {
        self.code_review_job_detail_with_task_content(id, true)
    }

    pub fn code_review_job_overview(
        &self,
        id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewJobDetail>> {
        self.code_review_job_detail_with_task_content(id, false)
    }

    fn code_review_job_detail_with_task_content(
        &self,
        id: &str,
        include_task_content: bool,
    ) -> Result<Option<trouve_protocol::CodeReviewJobDetail>> {
        let event_cursor =
            self.latest_event_cursor(&trouve_protocol::Scope::CodeReviewJob(id.to_owned()))?;
        let Some(record) = self.code_review_job(id)? else {
            return Ok(None);
        };
        let attempts = if include_task_content {
            self.code_review_task_attempts(id)?
        } else {
            self.code_review_task_summaries(id)?
        };
        let personas = code_review_persona_results(&attempts);
        let tasks = attempts.into_iter().map(|attempt| attempt.task).collect();
        let findings = self.code_review_findings(id)?;
        let candidate_rejections = self.code_review_candidate_rejections(id)?;
        let routing_decisions = self.code_review_routing_decisions(id)?;
        Ok(Some(trouve_protocol::CodeReviewJobDetail {
            job: record.job,
            event_cursor,
            tasks,
            personas,
            findings,
            candidate_rejections,
            routing_decisions,
            summary: record.summary,
            prompt_for_agents: record.prompt_for_agents,
        }))
    }

    pub fn latest_code_review_for_pull(
        &self,
        repository: &str,
        pull_number: u64,
        head_sha: Option<&str>,
        bot_login: &str,
    ) -> Result<Option<trouve_protocol::FirstPartyCodeReview>> {
        let id: Option<String> = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM code_review_jobs
                 WHERE repository = ?1 AND pull_number = ?2
                   AND status = 'succeeded'
                   AND (?3 IS NULL OR head_sha = ?3)
                 ORDER BY completed_at DESC LIMIT 1",
                params![repository, pull_number as i64, head_sha],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        let detail = self
            .code_review_job_detail(&id)?
            .ok_or_else(|| anyhow::anyhow!("published review job disappeared"))?;
        Ok(Some(trouve_protocol::FirstPartyCodeReview {
            job_id: detail.job.id,
            bot_login: bot_login.to_owned(),
            status: detail.job.status,
            summary: detail.summary,
            prompt_for_agents: detail.prompt_for_agents,
            review_url: detail.job.review_url,
            findings: detail.findings,
        }))
    }

    pub fn code_review_stats(
        &self,
        range: trouve_protocol::CodeReviewStatsRange,
        repository: Option<&str>,
    ) -> Result<trouve_protocol::CodeReviewStats> {
        #[derive(Default)]
        struct RepositoryAccumulator {
            status: trouve_protocol::CodeReviewStatusCounts,
            issues: u64,
            pending: Vec<u64>,
            running: Vec<u64>,
            preparation: Vec<u64>,
            reviewers: Vec<u64>,
            coordinator: Vec<u64>,
            publication: Vec<u64>,
        }
        #[derive(Default)]
        struct BucketAccumulator {
            status: trouve_protocol::CodeReviewStatusCounts,
            issues: u64,
            pending: Vec<u64>,
            running: Vec<u64>,
        }
        struct JobStatsRow {
            repository: String,
            status: String,
            created_at: chrono::DateTime<chrono::Utc>,
            started_at: Option<chrono::DateTime<chrono::Utc>>,
            completed_at: Option<chrono::DateTime<chrono::Utc>>,
            issues: u64,
            preparation_ms: u64,
            reviewer_ms: u64,
            coordinator_ms: u64,
            publication_ms: u64,
        }
        #[derive(Default)]
        struct PersonaExecution {
            reviewer_name: String,
            task_count: u64,
            succeeded_tasks: u64,
            failed_tasks: u64,
            cancelled_tasks: u64,
            not_applicable_tasks: u64,
            terminal_tasks: u64,
            candidates: u64,
            confirmed: u64,
            provider_wait_ms: u64,
            model_elapsed_ms: u64,
            input_tokens: u64,
            cached_input_tokens: u64,
            output_tokens: u64,
            tool_call_count: u64,
            started_at: Option<chrono::DateTime<chrono::Utc>>,
            completed_at: Option<chrono::DateTime<chrono::Utc>>,
        }
        #[derive(Default)]
        struct PersonaAccumulator {
            reviewer_name: String,
            task_count: u64,
            succeeded: u64,
            failed: u64,
            cancelled: u64,
            not_applicable: u64,
            candidates: u64,
            confirmed: u64,
            durations: Vec<u64>,
            provider_wait_durations: Vec<u64>,
            model_durations: Vec<u64>,
            input_tokens: u64,
            cached_input_tokens: u64,
            output_tokens: u64,
            tool_call_count: u64,
        }

        let now = chrono::Utc::now();
        let requested_start = code_review_stats_start(range, now);
        let start_param = requested_start.map(|value| value.to_rfc3339());
        let earliest_created_at = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT MIN(created_at) FROM code_review_jobs
                 WHERE (?1 IS NULL OR repository = ?1)
                   AND (
                     status IN ('queued', 'running')
                     OR ?2 IS NULL
                     OR completed_at >= ?2
                   )",
                params![repository, start_param],
                |row| row.get::<_, Option<String>>(0),
            )?
            .and_then(|value| value.parse().ok())
        };

        let chart_start = requested_start
            .or(earliest_created_at)
            .unwrap_or(now - chrono::Duration::hours(1));
        let total_seconds = now.signed_duration_since(chart_start).num_seconds().max(1);
        let step_seconds = match range {
            trouve_protocol::CodeReviewStatsRange::Hour => 60,
            trouve_protocol::CodeReviewStatsRange::Day => 15 * 60,
            trouve_protocol::CodeReviewStatsRange::Week => 2 * 60 * 60,
            trouve_protocol::CodeReviewStatsRange::Month => 12 * 60 * 60,
            trouve_protocol::CodeReviewStatsRange::Year => 7 * 24 * 60 * 60,
            trouve_protocol::CodeReviewStatsRange::All => ((total_seconds + 119) / 120).max(60),
        };
        let bucket_count =
            ((total_seconds + step_seconds - 1) / step_seconds).clamp(1, 120) as usize;
        let mut bucket_accumulators: Vec<BucketAccumulator> = (0..bucket_count)
            .map(|_| BucketAccumulator::default())
            .collect();
        let mut overall_status = trouve_protocol::CodeReviewStatusCounts::default();
        let mut overall_pending = Vec::new();
        let mut overall_running = Vec::new();
        let mut overall_preparation = Vec::new();
        let mut overall_reviewers = Vec::new();
        let mut overall_coordinator = Vec::new();
        let mut overall_publication = Vec::new();
        let mut overall_issues = 0_u64;
        let mut repositories: BTreeMap<String, RepositoryAccumulator> = BTreeMap::new();

        {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT repository, status, created_at, started_at, completed_at,
                        issue_count, preparation_elapsed_ms, reviewer_elapsed_ms,
                        coordinator_elapsed_ms, publication_elapsed_ms
                 FROM code_review_jobs
                 WHERE (?1 IS NULL OR repository = ?1)
                   AND (
                     status IN ('queued', 'running')
                     OR ?2 IS NULL
                     OR completed_at >= ?2
                   )
                 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(params![repository, start_param], |row| {
                Ok(JobStatsRow {
                    repository: row.get(0)?,
                    status: row.get(1)?,
                    created_at: parse_datetime(row.get(2)?),
                    started_at: parse_optional_datetime(row.get(3)?),
                    completed_at: parse_optional_datetime(row.get(4)?),
                    issues: row.get::<_, i64>(5)? as u64,
                    preparation_ms: row.get::<_, i64>(6)? as u64,
                    reviewer_ms: row.get::<_, i64>(7)? as u64,
                    coordinator_ms: row.get::<_, i64>(8)? as u64,
                    publication_ms: row.get::<_, i64>(9)? as u64,
                })
            })?;
            for row in rows {
                let job = row?;
                code_review_status_add(&mut overall_status, &job.status);
                let repository_stats = repositories.entry(job.repository.clone()).or_default();
                code_review_status_add(&mut repository_stats.status, &job.status);
                overall_issues += job.issues;
                repository_stats.issues += job.issues;
                let terminal = !matches!(job.status.as_str(), "queued" | "running");
                if !terminal {
                    continue;
                }
                let (pending_ms, running_ms) = job_elapsed_ms(
                    &job.status,
                    job.created_at,
                    job.started_at,
                    job.completed_at,
                );
                overall_pending.push(pending_ms);
                repository_stats.pending.push(pending_ms);
                if job.started_at.is_some() {
                    overall_running.push(running_ms);
                    repository_stats.running.push(running_ms);
                }
                push_nonzero_duration(&mut overall_preparation, job.preparation_ms);
                push_nonzero_duration(&mut overall_reviewers, job.reviewer_ms);
                push_nonzero_duration(&mut overall_coordinator, job.coordinator_ms);
                push_nonzero_duration(&mut overall_publication, job.publication_ms);
                push_nonzero_duration(&mut repository_stats.preparation, job.preparation_ms);
                push_nonzero_duration(&mut repository_stats.reviewers, job.reviewer_ms);
                push_nonzero_duration(&mut repository_stats.coordinator, job.coordinator_ms);
                push_nonzero_duration(&mut repository_stats.publication, job.publication_ms);
                if let Some(completed_at) = job.completed_at {
                    let seconds = completed_at
                        .signed_duration_since(chart_start)
                        .num_seconds()
                        .max(0);
                    let index =
                        ((seconds / step_seconds) as usize).min(bucket_accumulators.len() - 1);
                    let bucket = &mut bucket_accumulators[index];
                    code_review_status_add(&mut bucket.status, &job.status);
                    bucket.issues += job.issues;
                    bucket.pending.push(pending_ms);
                    if job.started_at.is_some() {
                        bucket.running.push(running_ms);
                    }
                }
            }
        }

        let executions: Vec<(String, String, String, PersonaExecution)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT t.job_id, t.reviewer_id, MAX(t.reviewer_name),
                        COALESCE(t.model, 'unknown (legacy)'), COUNT(*),
                        SUM(CASE WHEN t.status = 'succeeded' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN t.status = 'failed' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN t.status = 'cancelled' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN t.status = 'not_applicable' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN t.status IN (
                          'succeeded', 'failed', 'cancelled', 'not_applicable'
                        ) THEN 1 ELSE 0 END),
                        SUM(t.candidate_issue_count), SUM(t.confirmed_issue_count),
                        SUM(t.provider_wait_ms), SUM(t.model_elapsed_ms),
                        SUM(t.input_tokens), SUM(t.cached_input_tokens),
                        SUM(t.output_tokens), SUM(t.tool_call_count),
                        MIN(t.started_at), MAX(t.completed_at)
                 FROM code_review_tasks t
                 JOIN code_review_jobs j ON j.id = t.job_id
                 WHERE t.role = 'reviewer' AND t.reviewer_id IS NOT NULL
                   AND (?1 IS NULL OR j.repository = ?1)
                   AND (
                     t.status IN ('queued', 'running')
                     OR ?2 IS NULL
                     OR t.completed_at >= ?2
                   )
                 GROUP BY t.job_id, t.reviewer_id, COALESCE(t.model, 'unknown (legacy)')
                 ORDER BY MIN(t.created_at)",
            )?;
            stmt.query_map(params![repository, start_param], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(3)?,
                    PersonaExecution {
                        reviewer_name: row.get(2)?,
                        task_count: row.get::<_, i64>(4)? as u64,
                        succeeded_tasks: row.get::<_, i64>(5)? as u64,
                        failed_tasks: row.get::<_, i64>(6)? as u64,
                        cancelled_tasks: row.get::<_, i64>(7)? as u64,
                        not_applicable_tasks: row.get::<_, i64>(8)? as u64,
                        terminal_tasks: row.get::<_, i64>(9)? as u64,
                        candidates: row.get::<_, i64>(10)? as u64,
                        confirmed: row.get::<_, i64>(11)? as u64,
                        provider_wait_ms: row.get::<_, i64>(12)? as u64,
                        model_elapsed_ms: row.get::<_, i64>(13)? as u64,
                        input_tokens: row.get::<_, i64>(14)? as u64,
                        cached_input_tokens: row.get::<_, i64>(15)? as u64,
                        output_tokens: row.get::<_, i64>(16)? as u64,
                        tool_call_count: row.get::<_, i64>(17)? as u64,
                        started_at: parse_optional_datetime(row.get(18)?),
                        completed_at: parse_optional_datetime(row.get(19)?),
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        let mut persona_stats: BTreeMap<(String, String), PersonaAccumulator> = BTreeMap::new();
        for (_, reviewer_id, model, execution) in executions {
            let persona = persona_stats.entry((reviewer_id, model)).or_default();
            persona.reviewer_name = execution.reviewer_name;
            persona.task_count += execution.task_count;
            persona.candidates += execution.candidates;
            persona.confirmed += execution.confirmed;
            persona
                .provider_wait_durations
                .push(execution.provider_wait_ms);
            persona.model_durations.push(execution.model_elapsed_ms);
            persona.input_tokens += execution.input_tokens;
            persona.cached_input_tokens += execution.cached_input_tokens;
            persona.output_tokens += execution.output_tokens;
            persona.tool_call_count += execution.tool_call_count;
            let succeeded_with_optional_skips = execution.succeeded_tasks > 0
                && execution
                    .succeeded_tasks
                    .saturating_add(execution.not_applicable_tasks)
                    == execution.task_count;
            if succeeded_with_optional_skips {
                persona.succeeded += 1;
            } else if execution.failed_tasks > 0 {
                persona.failed += 1;
            } else if execution.cancelled_tasks > 0 {
                persona.cancelled += 1;
            } else if execution.not_applicable_tasks == execution.task_count {
                persona.not_applicable += 1;
            }
            let all_terminal = execution.terminal_tasks == execution.task_count;
            if all_terminal
                && let (Some(started), Some(completed)) =
                    (execution.started_at, execution.completed_at)
            {
                persona.durations.push(elapsed_ms(started, completed));
            }
        }

        let buckets = bucket_accumulators
            .into_iter()
            .enumerate()
            .map(|(index, bucket)| {
                let started_at =
                    chart_start + chrono::Duration::seconds(step_seconds * index as i64);
                let completed_at = (started_at + chrono::Duration::seconds(step_seconds)).min(now);
                trouve_protocol::CodeReviewStatsBucket {
                    started_at,
                    completed_at,
                    status: bucket.status,
                    issue_count: bucket.issues,
                    pending_average_ms: code_review_duration_stats(bucket.pending).average_ms,
                    running_average_ms: code_review_duration_stats(bucket.running).average_ms,
                }
            })
            .collect();
        let personas = persona_stats
            .into_iter()
            .map(
                |((reviewer_id, model), stats)| trouve_protocol::CodeReviewPersonaModelStats {
                    reviewer_id,
                    reviewer_name: stats.reviewer_name,
                    model,
                    task_count: stats.task_count,
                    succeeded: stats.succeeded,
                    failed: stats.failed,
                    cancelled: stats.cancelled,
                    not_applicable: stats.not_applicable,
                    candidate_issue_count: stats.candidates,
                    confirmed_issue_count: stats.confirmed,
                    duration: code_review_duration_stats(stats.durations),
                    provider_wait_duration: code_review_duration_stats(
                        stats.provider_wait_durations,
                    ),
                    model_duration: code_review_duration_stats(stats.model_durations),
                    input_tokens: stats.input_tokens,
                    cached_input_tokens: stats.cached_input_tokens,
                    output_tokens: stats.output_tokens,
                    tool_call_count: stats.tool_call_count,
                },
            )
            .collect();
        let repositories = repositories
            .into_iter()
            .map(
                |(repository, stats)| trouve_protocol::CodeReviewRepositoryStats {
                    repository,
                    status: stats.status,
                    issue_count: stats.issues,
                    pending_duration: code_review_duration_stats(stats.pending),
                    running_duration: code_review_duration_stats(stats.running),
                    preparation_duration: code_review_duration_stats(stats.preparation),
                    reviewer_duration: code_review_duration_stats(stats.reviewers),
                    coordinator_duration: code_review_duration_stats(stats.coordinator),
                    publication_duration: code_review_duration_stats(stats.publication),
                },
            )
            .collect();

        Ok(trouve_protocol::CodeReviewStats {
            range,
            repository: repository.map(str::to_string),
            generated_at: now,
            status: overall_status,
            pending_duration: code_review_duration_stats(overall_pending),
            running_duration: code_review_duration_stats(overall_running),
            preparation_duration: code_review_duration_stats(overall_preparation),
            reviewer_duration: code_review_duration_stats(overall_reviewers),
            coordinator_duration: code_review_duration_stats(overall_coordinator),
            publication_duration: code_review_duration_stats(overall_publication),
            issue_count: overall_issues,
            buckets,
            personas,
            repositories,
        })
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

    pub fn request_code_review_job_cancel(
        &self,
        id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewJob>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let state: Option<(String, bool)> = tx
            .query_row(
                "SELECT status, publication_claimed FROM code_review_jobs WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, publication_claimed)) = state else {
            tx.commit()?;
            return Ok(None);
        };
        match status.as_str() {
            "queued" => {
                tx.execute(
                    "UPDATE code_review_jobs
                     SET status = 'cancelled', cancel_requested = 1,
                         completed_at = ?2, error = 'cancelled by user'
                     WHERE id = ?1 AND status = 'queued'",
                    params![id, chrono::Utc::now().to_rfc3339()],
                )?;
            }
            "running" if !publication_claimed => {
                tx.execute(
                    "UPDATE code_review_jobs SET cancel_requested = 1
                     WHERE id = ?1 AND status = 'running'",
                    params![id],
                )?;
            }
            "running" => {
                anyhow::bail!(
                    "review publication has already started; wait for it to finish before cancelling"
                );
            }
            _ => {}
        }
        let record = tx.query_row(
            &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
            params![id],
            row_to_code_review_job,
        )?;
        tx.commit()?;
        Ok(Some(record.job))
    }

    /// Atomically cancel an active predecessor and enqueue a linked
    /// replacement. Publication claiming and retrying serialize on the same
    /// transaction, so at most one execution may cross the publication gate.
    pub fn retry_code_review_job(
        &self,
        id: &str,
    ) -> Result<Option<trouve_protocol::CodeReviewJob>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let old = tx
            .query_row(
                &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
                params![id],
                row_to_code_review_job,
            )
            .optional()?;
        let Some(old) = old else {
            tx.commit()?;
            return Ok(None);
        };
        if old.publication_claimed && old.job.status == "running" {
            anyhow::bail!(
                "review publication has already started; wait for it to finish before retrying"
            );
        }
        if old.job.status == "queued" {
            tx.execute(
                "UPDATE code_review_jobs
                 SET status = 'cancelled', cancel_requested = 1,
                     completed_at = ?2, error = 'replaced by retry'
                 WHERE id = ?1 AND status = 'queued'",
                params![id, chrono::Utc::now().to_rfc3339()],
            )?;
        } else if old.job.status == "running" {
            tx.execute(
                "UPDATE code_review_jobs
                 SET cancel_requested = 1, error = 'replaced by retry'
                 WHERE id = ?1 AND status = 'running'",
                params![id],
            )?;
        }
        let new_id = crate::new_id("rv");
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO code_review_jobs
                    (id, dedupe_key, installation_id, repository, pull_number,
                     pull_title, pull_url, head_sha, base_ref, head_ref, trigger,
                     status, model, prompt, identities, config_hash, created_at,
                     review_base_sha, review_scope, retry_of, total_reviewers,
                     routing_mode, semantic_routing, included_reviewer_ids,
                     excluded_reviewer_ids, router_model, router_thinking_level,
                     coordinator_thinking_level)
             SELECT ?2, ?3, installation_id, repository, pull_number,
                    pull_title, pull_url, head_sha, base_ref, head_ref, 'retry',
                    'queued', model, prompt, identities, config_hash, ?4,
                    review_base_sha, review_scope, id, total_reviewers,
                    routing_mode, semantic_routing, included_reviewer_ids,
                    excluded_reviewer_ids, router_model, router_thinking_level,
                    coordinator_thinking_level
             FROM code_review_jobs WHERE id = ?1",
            params![id, new_id, format!("retry:{id}:{new_id}"), now],
        )?;
        tx.execute(
            "UPDATE code_review_jobs SET retried_by = ?2 WHERE id = ?1",
            params![id, new_id],
        )?;
        let replacement = tx.query_row(
            &format!("SELECT {CODE_REVIEW_JOB_COLUMNS} FROM code_review_jobs WHERE id = ?1"),
            params![new_id],
            row_to_code_review_job,
        )?;
        tx.commit()?;
        Ok(Some(replacement.job))
    }

    pub fn code_review_job_cancel_requested(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT cancel_requested FROM code_review_jobs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn cancel_active_code_review_tasks(&self, job_id: &str, error: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_tasks
             SET status = 'cancelled', completed_at = ?2, error = ?3,
                 model_elapsed_ms = CASE
                   WHEN model_started_at IS NULL THEN model_elapsed_ms
                   ELSE model_elapsed_ms + MAX(
                     0,
                     CAST(
                       (julianday(?2) - MAX(
                         julianday(model_started_at),
                         julianday(COALESCE(last_progress_at, model_started_at))
                       )) * 86400000 AS INTEGER
                     )
                   )
                 END
             WHERE job_id = ?1 AND status IN ('queued', 'running')",
            params![job_id, now, error],
        )?;
        Ok(())
    }

    pub fn claim_code_review_publication(&self, id: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET publication_claimed = 1
             WHERE id = ?1 AND status = 'running'
               AND cancel_requested = 0 AND publication_claimed = 0",
            params![id],
        )? > 0)
    }

    pub fn set_code_review_job_check_run(
        &self,
        id: &str,
        check_run_id: Option<u64>,
        check_run_url: &str,
        sync_error: &str,
    ) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs
             SET check_run_id = COALESCE(?2, check_run_id),
                 check_run_url = CASE WHEN ?3 = '' THEN check_run_url ELSE ?3 END,
                 check_sync_error = ?4,
                 projection_retry_count =
                   CASE WHEN ?4 = '' THEN 0 ELSE projection_retry_count END,
                 projection_retry_at =
                   CASE WHEN ?4 = '' THEN NULL ELSE projection_retry_at END,
                 projection_retryable =
                   CASE WHEN ?4 = '' THEN 1 ELSE projection_retryable END
             WHERE id = ?1",
            params![
                id,
                check_run_id.map(|value| value as i64),
                check_run_url,
                sync_error
            ],
        )? > 0)
    }

    pub fn record_code_review_projection_failure(
        &self,
        id: &str,
        error: &str,
        retryable: bool,
    ) -> Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let current_attempts = tx
            .query_row(
                "SELECT projection_retry_count FROM code_review_jobs WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(current_attempts) = current_attempts else {
            return Ok(false);
        };
        let attempts = u32::try_from(current_attempts)
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let retry_at = retryable.then(|| {
            let delay = projection_retry_delay_seconds(attempts);
            (chrono::Utc::now() + chrono::Duration::seconds(delay as i64)).to_rfc3339()
        });
        tx.execute(
            "UPDATE code_review_jobs
             SET check_sync_error = ?2,
                 projection_retry_count = ?3,
                 projection_retry_at = ?4,
                 projection_retryable = ?5
             WHERE id = ?1",
            params![id, error, i64::from(attempts), retry_at, retryable],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn set_code_review_job_lifecycle_comment_url(&self, id: &str, url: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET lifecycle_comment_url = ?2 WHERE id = ?1",
            params![id, url],
        )? > 0)
    }

    pub fn code_review_pull_state(
        &self,
        repository: &str,
        pull_number: u64,
    ) -> Result<CodeReviewPullStateRecord> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT last_reviewed_head_sha, last_reviewed_base_sha,
                        last_reviewed_at, lifecycle_comment_id, lifecycle_comment_url
                 FROM code_review_pr_state
                 WHERE repository = ?1 AND pull_number = ?2",
                params![repository, pull_number as i64],
                |row| {
                    Ok(CodeReviewPullStateRecord {
                        last_reviewed_head_sha: row.get(0)?,
                        last_reviewed_base_sha: row.get(1)?,
                        last_reviewed_at: parse_optional_datetime(row.get(2)?),
                        lifecycle_comment_id: row
                            .get::<_, Option<i64>>(3)?
                            .map(|value| value as u64),
                        lifecycle_comment_url: row.get(4)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn set_code_review_lifecycle_comment(
        &self,
        repository: &str,
        pull_number: u64,
        comment_id: u64,
        comment_url: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_pr_state
                    (repository, pull_number, lifecycle_comment_id, lifecycle_comment_url)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repository, pull_number) DO UPDATE SET
               lifecycle_comment_id = excluded.lifecycle_comment_id,
               lifecycle_comment_url = excluded.lifecycle_comment_url",
            params![
                repository,
                pull_number as i64,
                comment_id as i64,
                comment_url
            ],
        )?;
        Ok(())
    }

    pub fn mark_code_review_published(
        &self,
        repository: &str,
        pull_number: u64,
        base_sha: &str,
        head_sha: &str,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO code_review_pr_state
                    (repository, pull_number, last_reviewed_head_sha,
                     last_reviewed_base_sha, last_reviewed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(repository, pull_number) DO UPDATE SET
               last_reviewed_head_sha = excluded.last_reviewed_head_sha,
               last_reviewed_base_sha = excluded.last_reviewed_base_sha,
               last_reviewed_at = excluded.last_reviewed_at",
            params![
                repository,
                pull_number as i64,
                head_sha,
                base_sha,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn set_code_review_job_fixed_issue_count(&self, id: &str, count: u64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET fixed_issue_count = ?2 WHERE id = ?1",
            params![id, count as i64],
        )?;
        Ok(())
    }

    pub fn pending_code_review_job_cleanups(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id FROM code_review_jobs
             WHERE status IN ('succeeded', 'failed', 'cancelled', 'stale')
               AND session_id IS NOT NULL
             ORDER BY completed_at",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn complete_code_review_job_cleanup(&self, id: &str, session_id: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET session_id = NULL, thread_id = NULL
             WHERE id = ?1 AND status IN ('succeeded', 'failed', 'cancelled', 'stale')
               AND session_id = ?2",
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
    fn thread_view_snapshot_rebuilds_and_advances_with_event_appends() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_view");
        let scope = Scope::Thread("th_view".into());
        for event in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "test/model".into(),
            },
            Event::UserMessage {
                turn: 1,
                content: "hello".into(),
                attachments: Vec::new(),
            },
            Event::AssistantDelta {
                turn: 1,
                text: "wor".into(),
            },
            Event::AssistantDelta {
                turn: 1,
                text: "ld".into(),
            },
            Event::AssistantMessage {
                turn: 1,
                content: "world".into(),
            },
        ] {
            store.append_event(scope.clone(), event).unwrap();
        }

        let (first_cursor, first) = store.thread_view_snapshot("th_view", None, 256).unwrap();
        assert_eq!(first.items.len(), 3);
        assert_eq!(first.item_offset, 0);
        assert_eq!(first.total_items, 3);
        assert!(!first.has_older);
        assert!(first.turn_running);

        let (_, tail) = store.thread_view_snapshot("th_view", None, 2).unwrap();
        assert_eq!(tail.item_offset, 1);
        assert_eq!(tail.total_items, 3);
        assert!(tail.has_older);
        assert_eq!(tail.items, first.items[1..]);
        let (_, older) = store
            .thread_view_snapshot("th_view", Some(tail.item_offset), 2)
            .unwrap();
        assert_eq!(older.item_offset, 0);
        assert_eq!(older.total_items, 3);
        assert!(!older.has_older);
        assert_eq!(older.items, first.items[..1]);

        store
            .append_event(
                scope.clone(),
                Event::TurnCompleted {
                    turn: 1,
                    usage: Default::default(),
                    checkpoint_id: None,
                },
            )
            .unwrap();
        let cached_cursor = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT cursor FROM thread_view_cache WHERE thread_id = 'th_view'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap() as u64;
        assert!(cached_cursor > first_cursor);
        let (second_cursor, second) = store.thread_view_snapshot("th_view", None, 256).unwrap();
        assert!(second_cursor > first_cursor);
        assert!(!second.turn_running);
        assert_eq!(second.items.len(), 3);
        let changes_before = store.conn.lock().unwrap().total_changes();
        let (unchanged_cursor, unchanged) =
            store.thread_view_snapshot("th_view", None, 256).unwrap();
        let changes_after = store.conn.lock().unwrap().total_changes();
        assert_eq!(unchanged_cursor, second_cursor);
        assert_eq!(unchanged.items, second.items);
        assert_eq!(changes_after, changes_before);

        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM thread_view_cache WHERE thread_id = 'th_view'",
                [],
            )
            .unwrap();
        let (rebuilt_cursor, rebuilt) = store.thread_view_snapshot("th_view", None, 256).unwrap();
        assert_eq!(rebuilt_cursor, second_cursor);
        assert_eq!(rebuilt.items, second.items);

        for index in 0..300 {
            store
                .append_event(
                    scope.clone(),
                    Event::UserMessage {
                        turn: 2,
                        content: format!("historical message {index}"),
                        attachments: Vec::new(),
                    },
                )
                .unwrap();
        }
        let (_, bounded) = store.thread_view_snapshot("th_view", None, 256).unwrap();
        assert_eq!(bounded.items.len(), 256);
        assert_eq!(bounded.total_items, 303);
        assert_eq!(bounded.item_offset, 47);
        assert!(bounded.has_older);
    }

    #[test]
    fn code_review_task_summary_columns_preserve_full_row_shape() {
        let summary = code_review_task_summary_columns();
        let full_columns = CODE_REVIEW_TASK_COLUMNS.split(',').map(str::trim);
        let summary_columns = summary
            .split(',')
            .map(str::trim)
            .map(|column| column.strip_prefix("'' AS ").unwrap_or(column));

        assert!(full_columns.eq(summary_columns));
    }

    #[test]
    fn migration_removes_redundant_routing_decisions_index() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE INDEX code_review_routing_decisions_job
             ON code_review_routing_decisions (job_id, batch_index, reviewer_id)",
        )
        .unwrap();
        apply_migrations(&conn).unwrap();
        let redundant_indexes = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'code_review_routing_decisions_job'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();

        assert_eq!(redundant_indexes, 0);
    }

    #[test]
    fn migration_backfills_terminal_code_review_task_lifecycle_repeatably() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO code_review_jobs
                    (id, dedupe_key, installation_id, repository, pull_number, pull_title,
                     pull_url, head_sha, base_ref, head_ref, trigger, status, created_at)
             VALUES ('job', 'migration-job', 7, 'acme/widgets', 42, 'Widgets',
                     'https://github.com/acme/widgets/pull/42', 'head', 'main', 'ship',
                     'automatic', 'succeeded', '2026-01-01T00:00:00Z');
             INSERT INTO code_review_tasks
                    (id, job_id, role, status, lifecycle_stage, created_at, started_at,
                     completed_at)
             VALUES ('succeeded', 'job', 'reviewer', 'succeeded', 'queued',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z',
                     '2026-01-01T00:02:00Z');
             INSERT INTO code_review_tasks
                    (id, job_id, role, status, lifecycle_stage, created_at, started_at)
             VALUES ('not-applicable', 'job', 'reviewer', 'not_applicable', 'queued',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z');
             INSERT INTO code_review_tasks
                    (id, job_id, role, status, lifecycle_stage, created_at)
             VALUES ('failed', 'job', 'reviewer', 'failed', 'queued',
                     '2026-01-01T00:00:00Z');",
        )
        .unwrap();

        apply_migrations(&conn).unwrap();
        apply_migrations(&conn).unwrap();

        let succeeded: (String, Option<String>) = conn
            .query_row(
                "SELECT lifecycle_stage, last_progress_at
                 FROM code_review_tasks WHERE id = 'succeeded'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(succeeded.0, "completed");
        assert_eq!(succeeded.1.as_deref(), Some("2026-01-01T00:02:00Z"));
        let not_applicable: (String, Option<String>) = conn
            .query_row(
                "SELECT lifecycle_stage, last_progress_at
                 FROM code_review_tasks WHERE id = 'not-applicable'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(not_applicable.0, "completed");
        assert_eq!(not_applicable.1.as_deref(), Some("2026-01-01T00:01:00Z"));
        let failed: (String, Option<String>) = conn
            .query_row(
                "SELECT lifecycle_stage, last_progress_at
                 FROM code_review_tasks WHERE id = 'failed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(failed, ("queued".into(), None));
    }

    #[test]
    fn failed_model_elapsed_adds_only_time_since_the_last_persisted_snapshot() {
        let finished = "2026-01-01T00:00:20Z".parse().unwrap();
        assert_eq!(
            finalize_code_review_model_elapsed(
                5_000,
                Some("2026-01-01T00:00:00Z".into()),
                Some("2026-01-01T00:00:10Z".into()),
                finished,
            ),
            15_000
        );
    }

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
    fn replay_pages_are_bounded_and_advance_past_retired_rows() {
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
        for index in 0..3 {
            store
                .append_event(
                    Scope::Server,
                    Event::WorkspaceRegistered {
                        workspace_id: format!("ws_{index}"),
                        path: format!("/tmp/workspace-{index}"),
                    },
                )
                .unwrap();
        }

        let through = store.latest_event_cursor(&Scope::Server).unwrap();
        let first = store
            .event_replay_page(&Scope::Server, 0, through, 2)
            .unwrap();
        assert_eq!(first.events.len(), 1);
        assert!(!first.exhausted);
        assert_eq!(first.events[0].cursor, first.next_after);

        let second = store
            .event_replay_page(&Scope::Server, first.next_after, through, 2)
            .unwrap();
        assert_eq!(second.events.len(), 2);
        assert!(second.exhausted);
        assert_eq!(second.next_after, through);
    }

    #[test]
    fn latest_github_snapshot_is_scoped_to_host() {
        let store = Store::open_in_memory().unwrap();
        for (viewer, host) in [
            ("alice", "github.com"),
            ("enterprise", "github.example.com"),
        ] {
            store
                .append_event(
                    Scope::Server,
                    Event::GithubPullRequestsUpdated {
                        pull_requests: GithubPrList {
                            viewer: viewer.into(),
                            host: host.into(),
                            prs: Vec::new(),
                        },
                    },
                )
                .unwrap();
        }
        for index in 0..70 {
            store
                .append_event(
                    Scope::Server,
                    Event::WorkspaceRegistered {
                        workspace_id: format!("ws_{index}"),
                        path: format!("/tmp/workspace-{index}"),
                    },
                )
                .unwrap();
        }
        store
            .append_event(
                Scope::Server,
                Event::GithubPullRequestsUpdated {
                    pull_requests: GithubPrList {
                        viewer: "bob".into(),
                        host: "github.com".into(),
                        prs: Vec::new(),
                    },
                },
            )
            .unwrap();

        let github = store
            .latest_github_pr_snapshot("GITHUB.COM")
            .unwrap()
            .unwrap();
        assert_eq!(github.viewer, "bob");
        let enterprise = store
            .latest_github_pr_snapshot("github.example.com")
            .unwrap()
            .unwrap();
        assert_eq!(enterprise.viewer, "enterprise");
        assert!(
            store
                .latest_github_pr_snapshot("missing.example.com")
                .unwrap()
                .is_none()
        );
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

    #[tokio::test]
    async fn concurrent_async_batches_survive_slow_commit_with_exact_content_and_completion() {
        const WRITERS: usize = 5;
        const EVENTS_PER_WRITER: usize = 50;
        let store = Store::open_in_memory().unwrap();
        let mut live = store.subscribe();
        let mut scoped_live: Vec<_> = (0..WRITERS)
            .map(|writer| store.subscribe_scope(&Scope::Thread(format!("th_{writer}"))))
            .collect();

        // Hold the connection while every async caller queues its batch. This
        // models a slow fsync/commit without adding a production-only delay.
        let conn = Arc::clone(&store.conn);
        let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let blocker = std::thread::spawn(move || {
            let guard = conn.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(guard);
        });
        locked_rx.recv().unwrap();

        let writers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let store = store.clone();
                tokio::spawn(async move {
                    let mut events: Vec<_> = (0..EVENTS_PER_WRITER)
                        .map(|event| Event::AssistantDelta {
                            turn: 1,
                            text: format!("{writer}:{event}"),
                        })
                        .collect();
                    events.push(Event::TurnCompleted {
                        turn: 1,
                        usage: trouve_protocol::Usage::default(),
                        checkpoint_id: None,
                    });
                    store
                        .append_events_async(Scope::Thread(format!("th_{writer}")), events)
                        .await
                        .unwrap()
                })
            })
            .collect();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        release_tx.send(()).unwrap();
        blocker.join().unwrap();

        for writer in writers {
            let envelopes = writer.await.unwrap();
            assert_eq!(envelopes.len(), EVENTS_PER_WRITER + 1);
            assert!(
                envelopes
                    .windows(2)
                    .all(|pair| pair[0].cursor < pair[1].cursor)
            );
        }

        let mut last = 0;
        for _ in 0..WRITERS * (EVENTS_PER_WRITER + 1) {
            let envelope = live.recv().await.unwrap();
            assert!(
                envelope.cursor > last,
                "broadcasts must follow cursor order"
            );
            last = envelope.cursor;
        }
        for (writer, receiver) in scoped_live.iter_mut().enumerate() {
            for _ in 0..EVENTS_PER_WRITER + 1 {
                let envelope = receiver.recv().await.unwrap();
                assert_eq!(envelope.scope, Scope::Thread(format!("th_{writer}")));
            }
        }
        for writer in 0..WRITERS {
            let persisted = store
                .events_after(&Scope::Thread(format!("th_{writer}")), 0)
                .unwrap();
            assert_eq!(persisted.len(), EVENTS_PER_WRITER + 1);
            let text: String = persisted
                .iter()
                .filter_map(|envelope| match &envelope.event {
                    Event::AssistantDelta { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            let expected: String = (0..EVENTS_PER_WRITER)
                .map(|event| format!("{writer}:{event}"))
                .collect();
            assert_eq!(text, expected);
            assert!(matches!(
                persisted.last().map(|envelope| &envelope.event),
                Some(Event::TurnCompleted { turn: 1, .. })
            ));
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
        let tool_free = store
            .enqueue_prompt_with_tools("th_2", "other thread", &[], false)
            .unwrap();
        assert!(store.queued_prompt_tools_enabled(&a.id).unwrap());
        assert!(!store.queued_prompt_tools_enabled(&tool_free.id).unwrap());

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
        assert_eq!(
            discovered[0].reviewer_ids,
            crate::reviewers::default_reviewer_ids()
        );

        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Automatic,
                model: Some("openai/gpt-5".into()),
                coordinator_thinking_level: Some("high".into()),
                router_model: Some("anthropic/router".into()),
                router_thinking_level: Some("low".into()),
                prompt: "focus on concurrency".into(),
                reviewer_ids: Some(crate::reviewers::default_reviewer_ids()),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Additive),
                semantic_routing: Some(true),
                included_reviewer_ids: Some(vec!["reliability".into()]),
                excluded_reviewer_ids: Some(vec!["operations".into()]),
                reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                    reviewer_id: "security".into(),
                    model: Some("anthropic/security".into()),
                    thinking_level: Some("medium".into()),
                    prompt_mode: trouve_protocol::ReviewerPromptMode::Append,
                    prompt: "Focus on tenant boundaries.".into(),
                }]),
            })
            .unwrap();
        let configured = store.list_code_review_repositories().unwrap().remove(0);
        assert_eq!(configured.mode, trouve_protocol::CodeReviewMode::Automatic);
        assert_eq!(configured.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(
            configured.coordinator_thinking_level.as_deref(),
            Some("high")
        );
        assert_eq!(configured.router_model.as_deref(), Some("anthropic/router"));
        assert_eq!(configured.router_thinking_level.as_deref(), Some("low"));
        assert_eq!(configured.reviewer_overrides.len(), 1);
        assert_eq!(
            configured.reviewer_overrides[0].thinking_level.as_deref(),
            Some("medium")
        );

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
            review_base_sha: "0000000000000000000000000000000000000000".into(),
            base_ref: "0000000000000000000000000000000000000000".into(),
            head_ref: "ship".into(),
            scope: trouve_protocol::CodeReviewJobScope::Incremental,
            trigger: "automatic".into(),
            retry_of: None,
            model: configured.model,
            coordinator_thinking_level: configured.coordinator_thinking_level,
            router_model: configured.router_model,
            router_thinking_level: configured.router_thinking_level,
            prompt: configured.prompt,
            reviewers,
            routing_mode: configured.routing_mode,
            semantic_routing: configured.semantic_routing,
            included_reviewer_ids: configured.included_reviewer_ids,
            excluded_reviewer_ids: configured.excluded_reviewer_ids,
            config_hash: "config".into(),
        };
        let queued = store.enqueue_code_review_job(&new_job).unwrap().unwrap();
        assert_eq!(queued.status, "queued");
        assert_eq!(queued.reviewer_ids, configured.reviewer_ids);
        assert_eq!(queued.coordinator_thinking_level.as_deref(), Some("high"));
        assert_eq!(queued.router_model.as_deref(), Some("anthropic/router"));
        assert_eq!(queued.router_thinking_level.as_deref(), Some("low"));
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
            .set_code_review_job_phase_elapsed(&queued.id, CodeReviewJobPhase::Preparation, 100)
            .unwrap();
        store
            .set_code_review_job_phase_elapsed(&queued.id, CodeReviewJobPhase::Reviewers, 200)
            .unwrap();
        store
            .set_code_review_job_phase_elapsed(&queued.id, CodeReviewJobPhase::Coordinator, 300)
            .unwrap();
        store
            .set_code_review_job_phase_elapsed(&queued.id, CodeReviewJobPhase::Publication, 400)
            .unwrap();
        store
            .finish_code_review_job(&queued.id, "succeeded", "https://review", "")
            .unwrap();
        let completed = store.list_code_review_jobs(10).unwrap().remove(0);
        assert_eq!(completed.status, "succeeded");
        assert_eq!(completed.session_id.as_deref(), Some("se_review"));
        assert_eq!(completed.thread_id.as_deref(), Some("th_review"));
        assert_eq!(completed.preparation_elapsed_ms, 100);
        assert_eq!(completed.reviewer_elapsed_ms, 200);
        assert_eq!(completed.coordinator_elapsed_ms, 300);
        assert_eq!(completed.publication_elapsed_ms, 400);
        let stats = store
            .code_review_stats(trouve_protocol::CodeReviewStatsRange::All, None)
            .unwrap();
        assert_eq!(stats.preparation_duration.average_ms, 100);
        assert_eq!(stats.reviewer_duration.average_ms, 200);
        assert_eq!(stats.coordinator_duration.average_ms, 300);
        assert_eq!(stats.publication_duration.average_ms, 400);
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
    fn legacy_default_reviewers_gain_automatic_routing_only_once() {
        const LEGACY_DEFAULTS: &str = r#"["correctness","security","api-compatibility","testing"]"#;
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO code_review_repositories
                    (repository, installation_id, identity_ids, routing_mode,
                     semantic_routing, updated_at)
             VALUES ('acme/widgets', 7, ?1, 'core', 0, '2026-01-01T00:00:00Z')",
            [LEGACY_DEFAULTS],
        )
        .unwrap();

        migrate_automatic_code_review_routing(&conn).unwrap();
        let migrated: (String, String, bool) = conn
            .query_row(
                "SELECT identity_ids, routing_mode, semantic_routing
                 FROM code_review_repositories
                 WHERE repository = 'acme/widgets'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&migrated.0).unwrap(),
            crate::reviewers::default_reviewer_ids()
        );
        assert_eq!(migrated.1, "auto");
        assert!(migrated.2);

        // Once migrated, an explicit return to Core is a user choice and must
        // survive subsequent startups.
        conn.execute(
            "UPDATE code_review_repositories
             SET identity_ids = ?1, routing_mode = 'core', semantic_routing = 0
             WHERE repository = 'acme/widgets'",
            [LEGACY_DEFAULTS],
        )
        .unwrap();
        migrate_automatic_code_review_routing(&conn).unwrap();
        let retained: (String, String, bool) = conn
            .query_row(
                "SELECT identity_ids, routing_mode, semantic_routing
                 FROM code_review_repositories
                 WHERE repository = 'acme/widgets'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(retained.0, LEGACY_DEFAULTS);
        assert_eq!(retained.1, "core");
        assert!(!retained.2);
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
                    review_base_sha: "0000000000000000000000000000000000000000".into(),
                    base_ref: "0000000000000000000000000000000000000000".into(),
                    head_ref: "ship".into(),
                    scope: trouve_protocol::CodeReviewJobScope::Incremental,
                    trigger: "automatic".into(),
                    retry_of: None,
                    model: None,
                    coordinator_thinking_level: None,
                    router_model: None,
                    router_thinking_level: None,
                    prompt: String::new(),
                    reviewers: Vec::new(),
                    routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                    semantic_routing: false,
                    included_reviewer_ids: Vec::new(),
                    excluded_reviewer_ids: Vec::new(),
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
        let excluded_built_ins = crate::reviewers::built_in_reviewers()
            .into_iter()
            .map(|built_in| built_in.id)
            .collect::<Vec<_>>();
        let unrelated_exclusions = excluded_built_ins.clone();
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/widgets".into(),
                mode: trouve_protocol::CodeReviewMode::Automatic,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: String::new(),
                reviewer_ids: Some(vec![reviewer.id.clone()]),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Additive),
                semantic_routing: Some(true),
                included_reviewer_ids: Some(vec![reviewer.id.clone()]),
                excluded_reviewer_ids: Some(excluded_built_ins),
                reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                    reviewer_id: reviewer.id.clone(),
                    model: Some("anthropic/domain".into()),
                    thinking_level: Some("low".into()),
                    prompt_mode: trouve_protocol::ReviewerPromptMode::Replace,
                    prompt: "Use repository-specific invariants.".into(),
                }]),
            })
            .unwrap();
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/unrelated".into(),
                mode: trouve_protocol::CodeReviewMode::Off,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: String::new(),
                reviewer_ids: Some(crate::reviewers::default_reviewer_ids()),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Additive),
                semantic_routing: Some(true),
                included_reviewer_ids: Some(Vec::new()),
                excluded_reviewer_ids: Some(unrelated_exclusions.clone()),
                reviewer_overrides: Some(Vec::new()),
            })
            .unwrap();
        let repositories = store.list_code_review_repositories().unwrap();
        let repository = repositories
            .iter()
            .find(|repository| repository.repository == "acme/widgets")
            .unwrap();
        assert_eq!(
            repository.reviewer_ids.as_slice(),
            std::slice::from_ref(&reviewer.id)
        );
        assert_eq!(repository.reviewer_overrides.len(), 1);
        assert_eq!(repository.included_reviewer_ids, vec![reviewer.id.clone()]);

        assert!(store.delete_custom_reviewer_profile(&reviewer.id).unwrap());
        assert!(store.list_custom_reviewer_profiles().unwrap().is_empty());
        let repositories = store.list_code_review_repositories().unwrap();
        let repository = repositories
            .iter()
            .find(|repository| repository.repository == "acme/widgets")
            .unwrap();
        assert_eq!(
            repository.reviewer_ids,
            crate::reviewers::default_reviewer_ids()
        );
        assert!(repository.included_reviewer_ids.is_empty());
        assert!(
            !repository
                .excluded_reviewer_ids
                .contains(&"correctness".into())
        );
        assert!(repository.reviewer_overrides.is_empty());
        let unrelated = repositories
            .iter()
            .find(|repository| repository.repository == "acme/unrelated")
            .unwrap();
        assert_eq!(unrelated.excluded_reviewer_ids, unrelated_exclusions);
    }

    #[test]
    fn code_review_artifacts_progress_retry_and_watermark_are_durable() {
        let store = Store::open_in_memory().unwrap();
        let reviewers = crate::reviewers::built_in_reviewers()
            .into_iter()
            .take(2)
            .collect::<Vec<_>>();
        let queued = store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:artifacts".into(),
                installation_id: 7,
                repository: "acme/widgets".into(),
                pull_number: 42,
                pull_title: "Ship widgets".into(),
                pull_url: "https://github.com/acme/widgets/pull/42".into(),
                head_sha: "2222222222222222222222222222222222222222".into(),
                review_base_sha: "1111111111111111111111111111111111111111".into(),
                base_ref: "main".into(),
                head_ref: "ship".into(),
                scope: trouve_protocol::CodeReviewJobScope::Incremental,
                trigger: "automatic".into(),
                retry_of: None,
                model: Some("provider/default".into()),
                coordinator_thinking_level: Some("medium".into()),
                router_model: Some("provider/router".into()),
                router_thinking_level: Some("low".into()),
                prompt: "Review it".into(),
                reviewers,
                routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                semantic_routing: false,
                included_reviewer_ids: Vec::new(),
                excluded_reviewer_ids: Vec::new(),
                config_hash: "config".into(),
            })
            .unwrap()
            .unwrap();
        let running = store.claim_code_review_job().unwrap().unwrap();
        assert_eq!(running.job.id, queued.id);
        let routing_decisions = vec![trouve_protocol::CodeReviewRoutingDecision {
            batch_index: 0,
            reviewer_id: "correctness".into(),
            reviewer_name: "Correctness".into(),
            selected: true,
            reasons: vec![trouve_protocol::CodeReviewRoutingReason {
                source: trouve_protocol::CodeReviewRoutingSource::Core,
                detail: "selected by the repository's Manual persona set".into(),
            }],
        }];
        assert_eq!(
            store
                .save_code_review_routing_decisions(&queued.id, &routing_decisions)
                .unwrap(),
            routing_decisions
        );
        let replacement = vec![trouve_protocol::CodeReviewRoutingDecision {
            selected: false,
            reasons: Vec::new(),
            ..routing_decisions[0].clone()
        }];
        assert_eq!(
            store
                .save_code_review_routing_decisions(&queued.id, &replacement)
                .unwrap(),
            routing_decisions,
            "routing is a once-only snapshot and retries must reuse it"
        );

        let task = store
            .create_code_review_task(&NewCodeReviewTask {
                job_id: queued.id.clone(),
                role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                reviewer_id: Some("correctness".into()),
                reviewer_name: "Correctness".into(),
                batch_index: 0,
                batch_count: 2,
                model: Some("provider/model".into()),
                prompt: "Find defects".into(),
            })
            .unwrap();
        assert_eq!(
            task.lifecycle_stage,
            trouve_protocol::CodeReviewTaskLifecycleStage::Queued
        );
        assert!(task.last_progress_at.is_some());
        let started_task = store
            .start_code_review_task(&task.id, "se_review", "th_review", "provider/model")
            .unwrap()
            .unwrap();
        assert_eq!(
            started_task.lifecycle_stage,
            trouve_protocol::CodeReviewTaskLifecycleStage::WaitingForCapacity
        );
        let capacity = store
            .set_code_review_task_provider_wait("th_review", 17)
            .unwrap()
            .unwrap();
        assert_eq!(capacity.progress.provider_wait_ms, 17);
        assert_eq!(
            capacity.progress.lifecycle_stage,
            trouve_protocol::CodeReviewTaskLifecycleStage::StartingModel
        );
        let progress = store
            .set_code_review_task_progress(
                &task.id,
                trouve_protocol::CodeReviewTaskLifecycleStage::RunningTool,
                &CodeReviewTaskMetrics {
                    model_elapsed_ms: 23,
                    input_tokens: 100,
                    cached_input_tokens: 40,
                    output_tokens: 12,
                    tool_call_count: 2,
                },
                CodeReviewModelTiming::Started,
            )
            .unwrap()
            .unwrap();
        assert!(progress.progress.model_started_at.is_some());
        assert_eq!(progress.progress.model_elapsed_ms, 23);
        assert_eq!(progress.progress.tool_call_count, 2);
        let repair = store
            .set_code_review_task_progress(
                &task.id,
                trouve_protocol::CodeReviewTaskLifecycleStage::RepairingOutput,
                &CodeReviewTaskMetrics {
                    model_elapsed_ms: 23,
                    input_tokens: 100,
                    cached_input_tokens: 40,
                    output_tokens: 12,
                    tool_call_count: 2,
                },
                CodeReviewModelTiming::Reset,
            )
            .unwrap()
            .unwrap();
        assert!(repair.progress.model_started_at.is_none());
        let repair_capacity = store
            .set_code_review_task_provider_wait("th_review", 5)
            .unwrap()
            .unwrap();
        assert_eq!(repair_capacity.progress.provider_wait_ms, 22);
        assert_eq!(
            repair_capacity.progress.lifecycle_stage,
            trouve_protocol::CodeReviewTaskLifecycleStage::RepairingOutput
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
        let repair_started = store
            .set_code_review_task_progress(
                &task.id,
                trouve_protocol::CodeReviewTaskLifecycleStage::RepairingOutput,
                &CodeReviewTaskMetrics {
                    model_elapsed_ms: 23,
                    input_tokens: 100,
                    cached_input_tokens: 40,
                    output_tokens: 12,
                    tool_call_count: 2,
                },
                CodeReviewModelTiming::Started,
            )
            .unwrap()
            .unwrap();
        assert!(repair_started.progress.model_started_at.is_some());
        assert!(repair_started.progress.model_started_at > progress.progress.model_started_at);
        assert!(
            store
                .append_code_review_task_output(
                    &task.id,
                    trouve_protocol::CodeReviewOutputStream::Assistant,
                    "candidate output",
                )
                .unwrap()
        );
        let finished_task = store
            .finish_code_review_task(&task.id, "succeeded", "", 1, "")
            .unwrap()
            .unwrap();
        assert_eq!(
            finished_task.lifecycle_stage,
            trouve_protocol::CodeReviewTaskLifecycleStage::Completed
        );
        assert_eq!(finished_task.provider_wait_ms, 22);
        assert_eq!(finished_task.model_elapsed_ms, 23);
        assert_eq!(finished_task.tool_call_count, 2);
        let partial = store.code_review_job_detail(&queued.id).unwrap().unwrap();
        assert_eq!(partial.personas[0].status, "queued");
        assert_eq!(partial.personas[0].completed_batches, 1);
        assert_eq!(partial.personas[0].total_batches, 2);
        assert_eq!(store.completed_code_review_personas(&queued.id).unwrap(), 0);
        let second_task = store
            .create_code_review_task(&NewCodeReviewTask {
                job_id: queued.id.clone(),
                role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                reviewer_id: Some("correctness".into()),
                reviewer_name: "Correctness".into(),
                batch_index: 1,
                batch_count: 2,
                model: Some("provider/model".into()),
                prompt: "Find defects in the next batch".into(),
            })
            .unwrap();
        store
            .skip_code_review_task(
                &second_task.id,
                "No changed files or hunks matched this focused persona.",
            )
            .unwrap()
            .unwrap();
        assert_eq!(store.completed_code_review_personas(&queued.id).unwrap(), 1);
        assert!(
            store
                .set_code_review_job_progress(&queued.id, 1, 2)
                .unwrap()
        );
        let findings = store
            .save_code_review_result(
                &queued.id,
                "One issue",
                "Fix every confirmed issue.",
                1,
                &[NewCodeReviewFinding {
                    path: "src/lib.rs".into(),
                    line: 12,
                    side: "RIGHT".into(),
                    severity: "high".into(),
                    body: "The error is ignored.".into(),
                    prompt_for_agents: "Handle the error at src/lib.rs:12.".into(),
                    sources: vec![trouve_protocol::CodeReviewFindingSource {
                        reviewer_id: "correctness".into(),
                        reviewer_name: "Correctness".into(),
                        candidate_id: "candidate-1".into(),
                        task_id: task.id.clone(),
                    }],
                }],
                &[trouve_protocol::CodeReviewCandidateRejection {
                    candidate_id: "candidate-2".into(),
                    task_id: task.id.clone(),
                    reviewer_id: "correctness".into(),
                    reviewer_name: "Correctness".into(),
                    path: "src/lib.rs".into(),
                    line: 18,
                    side: "RIGHT".into(),
                    severity: "low".into(),
                    body: "This branch could be simplified.".into(),
                    reason: "This is a non-actionable style preference.".into(),
                }],
            )
            .unwrap();
        assert_eq!(findings.len(), 1);

        let detail = store.code_review_job_detail(&queued.id).unwrap().unwrap();
        assert_eq!(detail.job.progress.completed_reviewers, 1);
        assert_eq!(detail.tasks[0].output, "candidate output");
        assert_eq!(detail.personas[0].candidate_issue_count, 1);
        assert_eq!(detail.candidate_rejections.len(), 1);
        assert_eq!(
            detail.candidate_rejections[0].reason,
            "This is a non-actionable style preference."
        );
        assert_eq!(detail.personas[0].confirmed_issue_count, 1);
        assert_eq!(detail.personas[0].status, "succeeded");
        assert_eq!(detail.findings[0].sources[0].task_id, task.id);
        assert_eq!(detail.prompt_for_agents, "Fix every confirmed issue.");
        assert_eq!(detail.routing_decisions, routing_decisions);

        let overview = store.code_review_job_overview(&queued.id).unwrap().unwrap();
        assert_eq!(overview.tasks.len(), detail.tasks.len());
        assert_eq!(overview.tasks[0].id, task.id);
        assert_eq!(overview.tasks[0].status, "succeeded");
        assert_eq!(overview.tasks[0].candidate_issue_count, 1);
        assert!(overview.tasks[0].prompt.is_empty());
        assert!(overview.tasks[0].output.is_empty());
        assert!(overview.tasks[0].thinking.is_empty());
        assert!(overview.tasks[0].tool_output.is_empty());
        assert_eq!(overview.personas.len(), detail.personas.len());
        assert_eq!(
            overview.personas[0].confirmed_issue_count,
            detail.personas[0].confirmed_issue_count
        );
        assert_eq!(overview.findings.len(), detail.findings.len());
        assert_eq!(overview.findings[0].id, detail.findings[0].id);
        assert_eq!(overview.routing_decisions, routing_decisions);

        let retained_task = store
            .code_review_task(&queued.id, &task.id)
            .unwrap()
            .unwrap();
        assert_eq!(retained_task.prompt, "Find defects");
        assert_eq!(retained_task.output, "candidate output");
        assert!(
            store
                .code_review_task("another-job", &task.id)
                .unwrap()
                .is_none()
        );

        assert_eq!(projection_retry_delay_seconds(1), 60);
        assert_eq!(projection_retry_delay_seconds(20), 6 * 60 * 60);
        store
            .record_code_review_projection_failure(&queued.id, "temporary failure", true)
            .unwrap();
        assert!(
            store
                .code_review_jobs_with_projection_errors(10)
                .unwrap()
                .is_empty()
        );
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE code_review_jobs SET projection_retry_at = ?2 WHERE id = ?1",
                params![
                    queued.id,
                    (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()
                ],
            )
            .unwrap();
        let projection_errors = store.code_review_jobs_with_projection_errors(10).unwrap();
        assert_eq!(projection_errors.len(), 1);
        assert_eq!(projection_errors[0].id, queued.id);
        store
            .set_code_review_job_check_run(&queued.id, None, "", "")
            .unwrap();
        assert!(
            store
                .code_review_jobs_with_projection_errors(10)
                .unwrap()
                .is_empty()
        );
        store
            .record_code_review_projection_failure(&queued.id, "permanent failure", false)
            .unwrap();
        assert!(
            store
                .code_review_jobs_with_projection_errors(10)
                .unwrap()
                .is_empty()
        );

        let replacement = store.retry_code_review_job(&queued.id).unwrap().unwrap();
        assert_eq!(replacement.retry_of.as_deref(), Some(queued.id.as_str()));
        assert_eq!(
            replacement.coordinator_thinking_level.as_deref(),
            Some("medium")
        );
        assert_eq!(replacement.router_model.as_deref(), Some("provider/router"));
        assert_eq!(replacement.router_thinking_level.as_deref(), Some("low"));
        let jobs = store.list_code_review_jobs(10).unwrap();
        assert_eq!(jobs[0].id, queued.id);
        assert_eq!(jobs[0].status, "running");
        assert_eq!(jobs[1].id, replacement.id);
        assert_eq!(jobs[1].status, "queued");
        let superseded_task = store
            .create_code_review_task(&NewCodeReviewTask {
                job_id: replacement.id.clone(),
                role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                reviewer_id: Some("correctness".into()),
                reviewer_name: "Correctness".into(),
                batch_index: 0,
                batch_count: 1,
                model: None,
                prompt: String::new(),
            })
            .unwrap_err();
        assert!(superseded_task.to_string().contains("no longer running"));

        store
            .mark_code_review_published(
                "acme/widgets",
                42,
                "1111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222",
            )
            .unwrap();
        let watermark = store.code_review_pull_state("acme/widgets", 42).unwrap();
        assert_eq!(
            watermark.last_reviewed_head_sha,
            "2222222222222222222222222222222222222222"
        );
        assert!(watermark.last_reviewed_at.is_some());

        let stats = store
            .code_review_stats(
                trouve_protocol::CodeReviewStatsRange::All,
                Some("acme/widgets"),
            )
            .unwrap();
        assert_eq!(stats.status.running, 1);
        assert_eq!(stats.status.queued, 1);
        assert_eq!(stats.personas[0].candidate_issue_count, 1);
        assert_eq!(stats.personas[0].confirmed_issue_count, 1);
        assert_eq!(stats.personas[0].task_count, 2);
        assert_eq!(stats.personas[0].succeeded, 1);
        assert_eq!(
            stats.personas[0].succeeded
                + stats.personas[0].failed
                + stats.personas[0].cancelled
                + stats.personas[0].not_applicable,
            1
        );
        assert_eq!(stats.personas[0].not_applicable, 0);
    }

    #[test]
    fn reviewer_persona_retries_failed_and_cancelled_batches() {
        let store = Store::open_in_memory().unwrap();
        let reviewer = crate::reviewers::built_in_reviewers().remove(0);
        let queued = store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:persona-retry".into(),
                installation_id: 7,
                repository: "acme/widgets".into(),
                pull_number: 42,
                pull_title: "Ship widgets".into(),
                pull_url: "https://github.com/acme/widgets/pull/42".into(),
                head_sha: "2222222222222222222222222222222222222222".into(),
                review_base_sha: "1111111111111111111111111111111111111111".into(),
                base_ref: "main".into(),
                head_ref: "ship".into(),
                scope: trouve_protocol::CodeReviewJobScope::Incremental,
                trigger: "automatic".into(),
                retry_of: None,
                model: Some("provider/default".into()),
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: "Review it".into(),
                reviewers: vec![reviewer.clone()],
                routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                semantic_routing: false,
                included_reviewer_ids: Vec::new(),
                excluded_reviewer_ids: Vec::new(),
                config_hash: "config".into(),
            })
            .unwrap()
            .unwrap();
        store.claim_code_review_job().unwrap().unwrap();

        let finish_batch = |batch_index: u64, status: &str, output: &str, error: &str| {
            let task = store
                .create_code_review_task(&NewCodeReviewTask {
                    job_id: queued.id.clone(),
                    role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                    reviewer_id: Some(reviewer.id.clone()),
                    reviewer_name: reviewer.name.clone(),
                    batch_index,
                    batch_count: 3,
                    model: Some("provider/default".into()),
                    prompt: format!("Review batch {}", batch_index + 1),
                })
                .unwrap();
            store
                .start_code_review_task(
                    &task.id,
                    "se_review",
                    &format!("th_{batch_index}"),
                    "provider/default",
                )
                .unwrap()
                .unwrap();
            store
                .set_code_review_task_progress(
                    &task.id,
                    trouve_protocol::CodeReviewTaskLifecycleStage::RunningModel,
                    &CodeReviewTaskMetrics {
                        model_elapsed_ms: 11 + batch_index,
                        ..CodeReviewTaskMetrics::default()
                    },
                    CodeReviewModelTiming::Started,
                )
                .unwrap()
                .unwrap();
            let finished = store
                .finish_code_review_task(&task.id, status, output, 0, error)
                .unwrap()
                .unwrap();
            if matches!(status, "failed" | "cancelled") {
                assert_eq!(
                    finished.lifecycle_stage,
                    trouve_protocol::CodeReviewTaskLifecycleStage::RunningModel
                );
                assert!(finished.model_started_at.is_some());
                assert!(finished.model_elapsed_ms >= 11 + batch_index);
            }
            finished
        };
        let failed = finish_batch(0, "failed", "", "provider unavailable");
        let cancelled = finish_batch(1, "cancelled", "", "review timed out");
        let succeeded = finish_batch(2, "succeeded", r#"{"findings":[]}"#, "");
        store
            .set_code_review_job_progress(&queued.id, 1, 1)
            .unwrap();
        store
            .finish_code_review_job(&queued.id, "failed", "", "provider unavailable")
            .unwrap();

        let retried = store
            .retry_code_review_persona(&queued.id, &reviewer.id)
            .unwrap()
            .unwrap();
        assert_eq!(retried.id, queued.id);
        assert_eq!(retried.status, "queued");
        assert_eq!(retried.progress.completed_reviewers, 0);

        // Force identical timestamps and misleading lexical ids: the newer
        // row must still win by SQLite insertion order.
        let failed_retry_id = store
            .code_review_tasks(&queued.id)
            .unwrap()
            .into_iter()
            .find(|task| task.batch_index == 0 && task.status == "queued")
            .unwrap()
            .id;
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE code_review_tasks
                 SET id = CASE
                       WHEN id = ?2 THEN 'rvt_z_old'
                       WHEN id = ?3 THEN 'rvt_a_new'
                       ELSE id
                     END,
                     created_at = '2026-07-27T00:00:00Z'
                 WHERE job_id = ?1 AND id IN (?2, ?3)",
                params![queued.id, failed.id, failed_retry_id],
            )
            .unwrap();

        let latest_tasks = store.latest_code_review_reviewer_tasks(&queued.id).unwrap();
        assert_eq!(
            latest_tasks
                .iter()
                .find(|task| task.batch_index == 0)
                .unwrap()
                .id,
            "rvt_a_new"
        );
        let detail = store.code_review_job_detail(&queued.id).unwrap().unwrap();
        assert_eq!(detail.tasks.len(), 5);
        assert_eq!(detail.personas[0].status, "queued");
        assert_eq!(detail.personas[0].completed_batches, 1);
        assert_eq!(detail.personas[0].total_batches, 3);
        assert_eq!(
            detail
                .tasks
                .iter()
                .filter(|task| task.batch_index == 2)
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec![succeeded.id.as_str()]
        );

        store.claim_code_review_job().unwrap().unwrap();
        let retries = store
            .code_review_tasks(&queued.id)
            .unwrap()
            .into_iter()
            .filter(|task| task.status == "queued")
            .collect::<Vec<_>>();
        assert_eq!(
            retries
                .iter()
                .map(|task| task.batch_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        for retry in retries {
            assert_ne!(retry.id, "rvt_z_old");
            assert_ne!(retry.id, cancelled.id);
            store
                .start_code_review_task(
                    &retry.id,
                    "se_retry",
                    &format!("th_retry_{}", retry.batch_index),
                    "provider/default",
                )
                .unwrap()
                .unwrap();
            store
                .finish_code_review_task(&retry.id, "succeeded", r#"{"findings":[]}"#, 0, "")
                .unwrap()
                .unwrap();
        }

        let detail = store.code_review_job_detail(&queued.id).unwrap().unwrap();
        assert_eq!(detail.personas[0].status, "succeeded");
        assert_eq!(detail.personas[0].completed_batches, 3);
        assert_eq!(store.completed_code_review_personas(&queued.id).unwrap(), 1);
        assert!(detail.tasks.iter().any(|task| task.id == "rvt_z_old"));
        assert!(detail.tasks.iter().any(|task| task.id == cancelled.id));
    }

    #[test]
    fn recovering_a_review_requeues_only_interrupted_reviewer_batches() {
        let store = Store::open_in_memory().unwrap();
        let reviewer = crate::reviewers::built_in_reviewers().remove(0);
        let queued = store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:recover-reviewers".into(),
                installation_id: 7,
                repository: "acme/widgets".into(),
                pull_number: 42,
                pull_title: "Ship widgets".into(),
                pull_url: "https://github.com/acme/widgets/pull/42".into(),
                head_sha: "2222222222222222222222222222222222222222".into(),
                review_base_sha: "1111111111111111111111111111111111111111".into(),
                base_ref: "main".into(),
                head_ref: "ship".into(),
                scope: trouve_protocol::CodeReviewJobScope::Incremental,
                trigger: "automatic".into(),
                retry_of: None,
                model: Some("provider/default".into()),
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: "Review it".into(),
                reviewers: vec![reviewer.clone()],
                routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                semantic_routing: false,
                included_reviewer_ids: Vec::new(),
                excluded_reviewer_ids: Vec::new(),
                config_hash: "config".into(),
            })
            .unwrap()
            .unwrap();
        store.claim_code_review_job().unwrap().unwrap();
        let create_batch = |batch_index: u64| {
            store
                .create_code_review_task(&NewCodeReviewTask {
                    job_id: queued.id.clone(),
                    role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                    reviewer_id: Some(reviewer.id.clone()),
                    reviewer_name: reviewer.name.clone(),
                    batch_index,
                    batch_count: 2,
                    model: Some("provider/default".into()),
                    prompt: format!("Review batch {}", batch_index + 1),
                })
                .unwrap()
        };
        let succeeded = create_batch(0);
        store
            .start_code_review_task(&succeeded.id, "se_review", "th_done", "provider/default")
            .unwrap()
            .unwrap();
        store
            .finish_code_review_task(&succeeded.id, "succeeded", r#"{"findings":[]}"#, 0, "")
            .unwrap()
            .unwrap();
        let interrupted = create_batch(1);
        store
            .start_code_review_task(
                &interrupted.id,
                "se_review",
                "th_interrupted",
                "provider/default",
            )
            .unwrap()
            .unwrap();
        store
            .set_code_review_task_progress(
                &interrupted.id,
                trouve_protocol::CodeReviewTaskLifecycleStage::RunningModel,
                &CodeReviewTaskMetrics {
                    model_elapsed_ms: 1_200,
                    ..CodeReviewTaskMetrics::default()
                },
                CodeReviewModelTiming::Started,
            )
            .unwrap()
            .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE code_review_tasks
                 SET model_started_at = ?2, last_progress_at = ?3
                 WHERE id = ?1",
                params![
                    interrupted.id,
                    (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(),
                    (chrono::Utc::now() - chrono::Duration::seconds(3)).to_rfc3339(),
                ],
            )
            .unwrap();
        }

        store.recover_code_review_jobs().unwrap();

        let detail = store.code_review_job_detail(&queued.id).unwrap().unwrap();
        assert_eq!(detail.job.status, "queued");
        assert_eq!(detail.tasks.len(), 3);
        assert_eq!(detail.personas[0].status, "queued");
        assert_eq!(detail.personas[0].completed_batches, 1);
        let interrupted = detail
            .tasks
            .iter()
            .find(|task| task.id == interrupted.id)
            .unwrap();
        assert_eq!(interrupted.status, "failed");
        assert!(interrupted.model_elapsed_ms >= 4_000);
        let retry = detail
            .tasks
            .iter()
            .find(|task| task.status == "queued")
            .unwrap();
        assert_eq!(retry.batch_index, 1);
        assert_ne!(retry.id, interrupted.id);
    }

    #[test]
    fn review_publication_rejects_cancellation_with_a_client_facing_reason() {
        let store = Store::open_in_memory().unwrap();
        let job = store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:publishing".into(),
                installation_id: 7,
                repository: "acme/widgets".into(),
                pull_number: 42,
                pull_title: "Ship widgets".into(),
                pull_url: "https://github.com/acme/widgets/pull/42".into(),
                head_sha: "2222222222222222222222222222222222222222".into(),
                review_base_sha: "1111111111111111111111111111111111111111".into(),
                base_ref: "main".into(),
                head_ref: "ship".into(),
                scope: trouve_protocol::CodeReviewJobScope::Incremental,
                trigger: "automatic".into(),
                retry_of: None,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: String::new(),
                reviewers: Vec::new(),
                routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                semantic_routing: false,
                included_reviewer_ids: Vec::new(),
                excluded_reviewer_ids: Vec::new(),
                config_hash: "config".into(),
            })
            .unwrap()
            .unwrap();
        store.claim_code_review_job().unwrap().unwrap();
        assert!(store.claim_code_review_publication(&job.id).unwrap());

        let error = store.request_code_review_job_cancel(&job.id).unwrap_err();
        assert!(error.to_string().contains("before cancelling"));
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
                    review_base_sha: base_ref.into(),
                    base_ref: base_ref.into(),
                    head_ref: "ship".into(),
                    scope: trouve_protocol::CodeReviewJobScope::Incremental,
                    trigger: "automatic".into(),
                    retry_of: None,
                    model: None,
                    coordinator_thinking_level: None,
                    router_model: None,
                    router_thinking_level: None,
                    prompt: String::new(),
                    reviewers: Vec::new(),
                    routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                    semantic_routing: false,
                    included_reviewer_ids: Vec::new(),
                    excluded_reviewer_ids: Vec::new(),
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

    #[tokio::test]
    async fn scoped_live_subscriptions_only_receive_their_scope() {
        let store = Store::open_in_memory().unwrap();
        let first_scope = Scope::Thread("th_first".into());
        let second_scope = Scope::Thread("th_second".into());
        let mut first = store.subscribe_scope(&first_scope);
        let mut second = store.subscribe_scope(&second_scope);
        store
            .append_event(
                first_scope,
                Event::TurnCapacityAcquired {
                    turn: 1,
                    wait_ms: 7,
                    background: false,
                },
            )
            .unwrap();
        let received = first.recv().await.unwrap();
        assert!(matches!(
            received.event,
            Event::TurnCapacityAcquired {
                turn: 1,
                wait_ms: 7,
                background: false
            }
        ));
        assert!(matches!(
            second.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
