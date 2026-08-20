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
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::broadcast;
use trouve_protocol::{
    Event, EventEnvelope, GithubPrList, PermissionMode, Scope, Session, SessionAttention,
    SessionOutcome, SessionSummariesSnapshot, SessionSummary, Thread, ThreadStatus,
    ThreadToolDetails, ThreadViewItem, ThreadViewSnapshot, Workspace,
};
use trouve_thread_view::{MaterializedThreadItem, ThreadProjection};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

// Version 2 retains server-measured per-tool execution durations. Treat the
// projection as a rebuildable cache so existing databases are upgraded by
// folding their durable event history again, without a storage migration.
// v3 retains the checkpoint id on completed folded turns so cached histories
// expose exact restore/fork actions after an upgrade.
// v4 stores completed folded rows independently and keeps only the live tail
// in the serialized projection cache.
// v5 adds durable TODO lifecycle rows. v6 keeps explicitly bounded provider
// thinking intact when tool lifecycle events interleave with its deltas. v7
// applies the same explicit-boundary rule when steering interleaves with them.
// v8 retains hidden turn boundaries after cancellation removes the visible
// status row, keeping turn-aligned pages bounded across cancelled histories.
// v9 terminalizes unmatched provider control-plane tool rows when their turn
// ends, so interrupted collaboration waits cannot replay as active forever.
const THREAD_VIEW_SCHEMA_VERSION: i64 = 9;
// A snapshot folds events without holding the SQLite connection. A terminal
// event can therefore advance the materialized cache before the snapshot
// reacquires the connection. Rebuild from that newer cache instead of mixing
// the stale in-memory projection with newer materialized rows.
const THREAD_VIEW_CACHE_RACE_RETRIES: usize = 3;
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
-- Provider-reported PR numbers are nominations, not authorization. Capture
-- the session worktree's coherent checked-out branch and exact HEAD when the
-- creator completes, then reconcile that immutable evidence with GitHub
-- asynchronously. The row survives process restarts and is deleted with its
-- owning session.
CREATE TABLE IF NOT EXISTS session_pr_verification_intents (
  session_id TEXT NOT NULL REFERENCES sessions(id),
  host TEXT NOT NULL,
  owner TEXT NOT NULL,
  repository TEXT NOT NULL,
  pull_number INTEGER NOT NULL,
  branch TEXT NOT NULL,
  head_sha TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  last_failure_class TEXT NOT NULL DEFAULT '',
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY (session_id, host, owner, repository, pull_number)
);
CREATE INDEX IF NOT EXISTS session_pr_verification_due
  ON session_pr_verification_intents (next_attempt_at, created_at);
CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id),
  title TEXT,
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
-- Durable intents for artifact deletion. These rows deliberately have no
-- foreign keys: session/attachment metadata is removed in the same
-- transaction, while the cleanup intent must survive until the executor
-- confirms the filesystem work.
CREATE TABLE IF NOT EXISTS artifact_cleanup_jobs (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  session_id TEXT,
  worktree_path TEXT,
  repository_path TEXT,
  attachment_paths TEXT NOT NULL DEFAULT '[]',
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  next_attempt_at TEXT,
  claim_until TEXT,
  claim_token TEXT,
  created_at TEXT NOT NULL
);
-- Persona deletion is a cross-boundary mutation: repository references stay
-- intact until the executor confirms the file removal, while this intent
-- makes a completed file mutation recoverable after a crash.
CREATE TABLE IF NOT EXISTS persona_cleanup_intents (
  persona_id TEXT PRIMARY KEY,
  attempts INTEGER NOT NULL DEFAULT 0,
  next_attempt_at TEXT,
  claim_until TEXT,
  claim_token TEXT,
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
CREATE TABLE IF NOT EXISTS thread_view_items (
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  item_index INTEGER NOT NULL,
  item TEXT NOT NULL,
  turn_start INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (thread_id, item_index)
);
CREATE TABLE IF NOT EXISTS thread_tool_details (
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  call_id TEXT NOT NULL,
  args TEXT NOT NULL,
  result TEXT,
  PRIMARY KEY (thread_id, call_id)
);
CREATE TABLE IF NOT EXISTS thread_statuses (
  thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  active INTEGER NOT NULL DEFAULT 0,
  attention TEXT NOT NULL DEFAULT 'none',
  last_outcome TEXT NOT NULL DEFAULT 'idle',
  latest_cursor INTEGER NOT NULL DEFAULT 0,
  started_at TEXT,
  completed_at TEXT
);
CREATE INDEX IF NOT EXISTS thread_statuses_session ON thread_statuses (session_id);
CREATE TABLE IF NOT EXISTS session_summaries (
  session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
  workspace_id TEXT NOT NULL,
  archived INTEGER NOT NULL DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 0,
  last_outcome TEXT NOT NULL DEFAULT 'idle',
  latest_thread_id TEXT,
  latest_cursor INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS session_summaries_workspace
  ON session_summaries (workspace_id, archived, updated_at, session_id);
CREATE TABLE IF NOT EXISTS session_summary_attention (
  kind TEXT NOT NULL,
  item_id TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  thread_id TEXT NOT NULL,
  PRIMARY KEY (kind, item_id, session_id)
);
CREATE INDEX IF NOT EXISTS session_summary_attention_session
  ON session_summary_attention (session_id, kind);
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
  thinking_level TEXT,
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
  review_watermark_sha TEXT NOT NULL DEFAULT '',
  review_batch_digest TEXT NOT NULL DEFAULT '',
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
  excluded_reviewer_ids TEXT NOT NULL DEFAULT '[]',
  publication_accepted INTEGER NOT NULL DEFAULT 0
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
CREATE TABLE IF NOT EXISTS code_review_pending_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  payload TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS code_review_pending_events_job
  ON code_review_pending_events (job_id, id);
CREATE TABLE IF NOT EXISTS code_review_findings (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL REFERENCES code_review_jobs(id),
  path TEXT NOT NULL,
  line INTEGER NOT NULL,
  side TEXT NOT NULL,
  severity TEXT NOT NULL,
  confidence TEXT NOT NULL DEFAULT 'medium',
  title TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL,
  prompt_for_agents TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'open',
  github_comment_id INTEGER,
  github_comment_url TEXT NOT NULL DEFAULT '',
  github_publication_status TEXT NOT NULL DEFAULT 'pending',
  github_thread_id TEXT,
  resolved_at TEXT,
  collapse_pending INTEGER NOT NULL DEFAULT 0,
  collapse_attempts INTEGER NOT NULL DEFAULT 0,
  collapse_next_attempt_at TEXT,
  created_at TEXT NOT NULL
);
-- The partial index on collapse_pending lives in MIGRATIONS only: SCHEMA
-- runs before migrations, so an index here would reference a column that
-- databases predating collapse_pending do not have yet and abort open.
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
  confidence TEXT NOT NULL DEFAULT 'medium',
  title TEXT NOT NULL DEFAULT '',
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
    "ALTER TABLE artifact_cleanup_jobs ADD COLUMN next_attempt_at TEXT",
    "ALTER TABLE artifact_cleanup_jobs ADD COLUMN claim_until TEXT",
    "ALTER TABLE artifact_cleanup_jobs ADD COLUMN claim_token TEXT",
    "CREATE TABLE IF NOT EXISTS persona_cleanup_intents (
       persona_id TEXT PRIMARY KEY,
       claim_until TEXT,
       claim_token TEXT,
       created_at TEXT NOT NULL
     )",
    "ALTER TABLE persona_cleanup_intents ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE persona_cleanup_intents ADD COLUMN next_attempt_at TEXT",
    "ALTER TABLE persona_cleanup_intents ADD COLUMN claim_until TEXT",
    "ALTER TABLE persona_cleanup_intents ADD COLUMN claim_token TEXT",
    "ALTER TABLE automations ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'ask'",
    "ALTER TABLE automations ADD COLUMN thinking_level TEXT",
    "ALTER TABLE threads ADD COLUMN todos TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE threads ADD COLUMN title TEXT",
    "ALTER TABLE thread_statuses ADD COLUMN started_at TEXT",
    "ALTER TABLE thread_statuses ADD COLUMN completed_at TEXT",
    "ALTER TABLE thread_view_items ADD COLUMN turn_start INTEGER NOT NULL DEFAULT 0",
    "CREATE INDEX IF NOT EXISTS thread_view_items_turn_start
       ON thread_view_items (thread_id, turn_start, item_index)",
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
    "ALTER TABLE code_review_jobs ADD COLUMN review_watermark_sha TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_jobs ADD COLUMN review_batch_digest TEXT NOT NULL DEFAULT ''",
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
    "ALTER TABLE code_review_jobs ADD COLUMN publication_accepted INTEGER NOT NULL DEFAULT 0",
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
    "ALTER TABLE code_review_findings ADD COLUMN github_publication_status TEXT NOT NULL DEFAULT 'pending'",
    "ALTER TABLE code_review_findings ADD COLUMN confidence TEXT NOT NULL DEFAULT 'medium'",
    "ALTER TABLE code_review_candidate_rejections ADD COLUMN confidence TEXT NOT NULL DEFAULT 'medium'",
    "ALTER TABLE code_review_findings ADD COLUMN title TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE code_review_candidate_rejections ADD COLUMN title TEXT NOT NULL DEFAULT ''",
    // Context-size proxy for compaction/UI: the input tokens of the turn's
    // *last* request, not the sum over its iterations (see record_usage).
    "ALTER TABLE usage ADD COLUMN context_input_tokens INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_findings ADD COLUMN collapse_pending INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_findings ADD COLUMN collapse_attempts INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE code_review_findings ADD COLUMN collapse_next_attempt_at TEXT",
    "ALTER TABLE session_pr_verification_intents ADD COLUMN last_failure_class TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE session_pr_verification_intents ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0",
    "CREATE INDEX IF NOT EXISTS code_review_findings_collapse_pending
       ON code_review_findings (collapse_pending) WHERE collapse_pending = 1",
];

fn apply_migrations(conn: &mut Connection) -> Result<()> {
    for sql in MIGRATIONS {
        if let Err(e) = conn.execute_batch(sql) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e).context(format!("migration failed: {sql}"));
            }
        }
    }
    backfill_code_review_watermarks(conn)?;
    backfill_terminal_code_review_task_lifecycle(conn)?;
    migrate_code_review_finding_publication_status(conn)?;
    migrate_general_persona_reviewer_references(conn)?;
    backfill_code_review_collapse_pending(conn)?;
    backfill_code_review_titles(conn)?;
    normalize_draft_stale_code_review_dedupe_keys(conn)?;
    migrate_backend_sessions(conn)?;
    migrate_automatic_code_review_routing(conn)?;
    migrate_session_summary_projection(conn)?;
    migrate_thread_status_projection(conn)?;
    recover_interrupted_session_summaries(conn)?;
    Ok(())
}

fn migrate_general_persona_reviewer_references(conn: &mut Connection) -> Result<()> {
    const MIGRATION_ID: &str = "general-persona-reviewer-references-v1";
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

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = tx
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        tx.commit()?;
        return Ok(());
    }
    tx.execute_batch(
        "UPDATE code_review_repositories SET
           identity_ids = CASE
             WHEN EXISTS (
               SELECT 1 FROM json_each(identity_ids)
               WHERE value IN ('code', 'plan', 'review')
             ) THEN CASE
               WHEN EXISTS (
                 SELECT 1 FROM json_each(identity_ids)
                 WHERE value NOT IN ('code', 'plan', 'review')
               ) THEN (
                 SELECT json_group_array(value) FROM json_each(identity_ids)
                 WHERE value NOT IN ('code', 'plan', 'review')
               )
               ELSE '[\"correctness\",\"security\",\"concurrency\",\"api-compatibility\",\"testing\"]'
             END
             ELSE identity_ids
           END,
           included_reviewer_ids = (
             SELECT json_group_array(value) FROM json_each(included_reviewer_ids)
             WHERE value NOT IN ('code', 'plan', 'review')
           ),
           excluded_reviewer_ids = (
             SELECT json_group_array(value) FROM json_each(excluded_reviewer_ids)
             WHERE value NOT IN ('code', 'plan', 'review')
           ),
           reviewer_overrides = (
             SELECT json_group_array(value) FROM json_each(reviewer_overrides)
             WHERE json_extract(value, '$.reviewer_id') NOT IN ('code', 'plan', 'review')
           )
         WHERE EXISTS (SELECT 1 FROM json_each(identity_ids) WHERE value IN ('code', 'plan', 'review'))
            OR EXISTS (SELECT 1 FROM json_each(included_reviewer_ids) WHERE value IN ('code', 'plan', 'review'))
            OR EXISTS (SELECT 1 FROM json_each(excluded_reviewer_ids) WHERE value IN ('code', 'plan', 'review'))
            OR EXISTS (
              SELECT 1 FROM json_each(reviewer_overrides)
              WHERE json_extract(value, '$.reviewer_id') IN ('code', 'plan', 'review')
            );",
    )?;
    tx.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

fn backfill_code_review_titles(conn: &mut Connection) -> Result<()> {
    for (migration_id, table) in [
        (
            "code-review-finding-title-backfill-v1",
            "code_review_findings",
        ),
        (
            "code-review-candidate-rejection-title-backfill-v1",
            "code_review_candidate_rejections",
        ),
    ] {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let applied = tx
            .query_row(
                "SELECT 1 FROM store_migrations WHERE id = ?1",
                [migration_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !applied {
            tx.execute(
                &format!(
                    "UPDATE {table}\n                     SET title = substr(replace(replace(trim(body), char(10), ' '), char(13), ' '), 1, 200)\n                     WHERE title = ''"
                ),
                [],
            )?;
            tx.execute(
                "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
                params![migration_id, chrono::Utc::now().to_rfc3339()],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// Jobs created before `review_watermark_sha` existed must recover the last
/// published head from pull state rather than the mutable effective diff base.
fn backfill_code_review_watermarks(conn: &mut Connection) -> Result<()> {
    const MIGRATION_ID: &str = "code-review-watermark-backfill-v1";
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = tx
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        tx.commit()?;
        return Ok(());
    }
    tx.execute(
        "UPDATE code_review_jobs
         SET review_watermark_sha = COALESCE((
             SELECT state.last_reviewed_head_sha
             FROM code_review_pr_state state
             WHERE state.repository = code_review_jobs.repository
               AND state.pull_number = code_review_jobs.pull_number
               AND state.last_reviewed_head_sha != ''
         ), review_watermark_sha)
         WHERE review_watermark_sha = ''",
        [],
    )?;
    tx.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Releases canonical automatic-review keys held by draft-stale jobs written
/// by an older binary. This intentionally runs on every startup so restoring a
/// current binary after a rollback also repairs rows created during rollback.
fn normalize_draft_stale_code_review_dedupe_keys(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE code_review_jobs
         SET dedupe_key = 'draft-stale:' || id || ':' || dedupe_key
         WHERE status = 'stale'
           AND trigger = 'automatic'
           AND error IN (
               'stale: pull request is a draft; automatic review stopped',
               'pull request is a draft; automatic review stopped'
           )
           AND dedupe_key NOT LIKE 'draft-stale:%'",
        [],
    )?;
    Ok(())
}

/// Arms the durable thread-collapse queue for findings closed before
/// `collapse_pending` existed: without this, historical conversations with a
/// published comment would stay uncollapsed forever. The backfill and its
/// marker commit atomically, so a failure leaves the marker absent and the
/// backfill retries on the next boot, while previously cleared collapses are
/// never re-armed once the marker is recorded.
fn backfill_code_review_collapse_pending(conn: &mut Connection) -> Result<()> {
    const MIGRATION_ID: &str = "code-review-collapse-pending-backfill-v1";
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

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = tx
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        tx.commit()?;
        return Ok(());
    }

    tx.execute(
        "UPDATE code_review_findings SET collapse_pending = 1
         WHERE status != 'open' AND github_comment_id IS NOT NULL",
        [],
    )?;
    tx.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Lifecycle columns were added after code-review tasks were already durable.
/// Repair terminal rows created by older builds without disturbing failed or
/// cancelled tasks, whose last active stage remains useful diagnostic state.
fn backfill_terminal_code_review_task_lifecycle(conn: &Connection) -> Result<()> {
    const MIGRATION_ID: &str = "code-review-terminal-task-lifecycle-v1";
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
        "UPDATE code_review_tasks
         SET lifecycle_stage = 'completed',
             last_progress_at = COALESCE(last_progress_at, completed_at, started_at, created_at)
         WHERE status IN ('succeeded', 'not_applicable')
           AND (lifecycle_stage != 'completed' OR last_progress_at IS NULL)",
        [],
    )?;
    conn.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

fn migrate_code_review_finding_publication_status(conn: &mut Connection) -> Result<()> {
    const MIGRATION_ID: &str = "code-review-finding-publication-status-v1";
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

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let applied = tx
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if applied {
        tx.commit()?;
        return Ok(());
    }

    tx.execute_batch(
        "UPDATE code_review_findings
         SET github_publication_status = 'published'
         WHERE github_comment_url != '' AND github_publication_status = 'pending';
         UPDATE code_review_findings
         SET github_publication_status = 'not_eligible'
         WHERE github_publication_status = 'pending'
           AND (trim(path) = '' OR line <= 0);",
    )?;
    tx.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

struct SessionSummaryChange {
    session_id: String,
    summary: Option<SessionSummary>,
    notification: Option<Event>,
}

struct ThreadStatusChange {
    statuses: Vec<ThreadStatus>,
}

/// Build the compact per-thread projection once for existing databases. New
/// writes update it in the same transaction as their source event.
fn migrate_thread_status_projection(conn: &Connection) -> Result<()> {
    // v2 rebuilds the projection once so databases created before timing was
    // retained gain accurate latest-turn timestamps from the durable log.
    const MIGRATION_ID: &str = "thread-status-projection-v2";
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

    let tx = write_transaction(conn)?;
    tx.execute("DELETE FROM thread_statuses", [])?;
    let threads_have_session_id = {
        let mut stmt = tx.prepare("PRAGMA table_info(threads)")?;
        stmt.query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|column| column == "session_id")
    };
    if threads_have_session_id {
        tx.execute(
            "INSERT INTO thread_statuses
               (thread_id, session_id, active, attention, last_outcome, latest_cursor)
             SELECT id, session_id, 0, 'none', 'idle', 0 FROM threads",
            [],
        )?;
    }
    let mut after = 0u64;
    loop {
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT cursor, scope_kind, scope_id, ts, payload
                 FROM events WHERE cursor > ?1 ORDER BY cursor LIMIT 512",
            )?;
            let mapped = stmt.query_map([after as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            mapped.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if rows.is_empty() {
            break;
        }
        for (cursor, kind, id, timestamp, payload) in &rows {
            after = *cursor;
            let Ok(event) = serde_json::from_str::<Event>(payload) else {
                continue;
            };
            let scope = scope_from_cols(kind, id.clone());
            let _ = project_thread_status(&tx, &scope, &event, *cursor, timestamp)?;
        }
    }
    tx.execute(
        "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
        params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(())
}

/// Build the projection once for existing databases. Afterwards the sole
/// event-writer transaction updates it and appends its durable replacement.
fn migrate_session_summary_projection(conn: &Connection) -> Result<()> {
    const MIGRATION_ID: &str = "session-summary-projection-v1";
    let applied = conn
        .query_row(
            "SELECT 1 FROM store_migrations WHERE id = ?1",
            [MIGRATION_ID],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !applied {
        let tx = write_transaction(conn)?;
        tx.execute("DELETE FROM session_summary_attention", [])?;
        tx.execute("DELETE FROM session_summaries", [])?;
        tx.execute(
            "INSERT INTO session_summaries
               (session_id, workspace_id, archived, active, last_outcome,
                latest_thread_id, latest_cursor, updated_at)
             SELECT id, workspace_id, archived, 0, 'idle', NULL, 0, created_at
             FROM sessions",
            [],
        )?;

        let mut after = 0u64;
        loop {
            let rows = {
                let mut stmt = tx.prepare(
                    "SELECT cursor, scope_kind, scope_id, ts, payload
                     FROM events WHERE cursor > ?1 ORDER BY cursor LIMIT 512",
                )?;
                let mapped = stmt.query_map([after as i64], |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if rows.is_empty() {
                break;
            }
            for (cursor, kind, id, timestamp, payload) in &rows {
                after = *cursor;
                let Ok(event) = serde_json::from_str::<Event>(payload) else {
                    continue;
                };
                let Ok(ts) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
                    continue;
                };
                let scope = scope_from_cols(kind, id.clone());
                let _ = project_session_summary(
                    &tx,
                    &scope,
                    &event,
                    *cursor,
                    ts.with_timezone(&chrono::Utc),
                )?;
            }
        }
        tx.execute(
            "INSERT INTO store_migrations (id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_ID, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
    }

    Ok(())
}

/// Process-owned turns and response channels do not survive restart. Record
/// recovery as durable source + replacement events so a resumed stream sees
/// the same cleared state as a fresh projection snapshot.
fn recover_interrupted_session_summaries(conn: &Connection) -> Result<()> {
    let interrupted = {
        let mut stmt = conn.prepare(
            "SELECT session_id, workspace_id FROM session_summaries AS summary
             WHERE active != 0 OR EXISTS (
               SELECT 1 FROM session_summary_attention AS attention
               WHERE attention.session_id = summary.session_id
             )
             ORDER BY session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if interrupted.is_empty() {
        return Ok(());
    }

    let now = chrono::Utc::now();
    let events = interrupted
        .into_iter()
        .map(|(session_id, workspace_id)| {
            let event = Event::SessionRecovered {
                session_id,
                workspace_id,
            };
            Ok(PendingEvent {
                scope: Scope::Server,
                ts: now,
                payload: serde_json::to_string(&event)?,
                event,
                mutation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let _ = insert_event_batch(conn, events.iter(), events.len(), std::iter::empty())?;
    Ok(())
}

fn ensure_session_summary(
    conn: &Connection,
    session_id: &str,
    cursor: u64,
    ts: &chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO session_summaries
           (session_id, workspace_id, archived, active, last_outcome,
            latest_thread_id, latest_cursor, updated_at)
         SELECT id, workspace_id, archived, 0, 'idle', NULL, ?2, ?3
         FROM sessions WHERE id = ?1",
        params![session_id, cursor as i64, ts.to_rfc3339()],
    )?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_summaries WHERE session_id = ?1)",
        [session_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn session_for_thread(conn: &Connection, thread_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT session_id FROM threads WHERE id = ?1",
        [thread_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn clear_thread_attention(conn: &Connection, thread_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM session_summary_attention WHERE thread_id = ?1",
        [thread_id],
    )?;
    Ok(())
}

fn touch_session_summary(
    conn: &Connection,
    session_id: &str,
    thread_id: Option<&str>,
    cursor: u64,
    ts: &chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE session_summaries
         SET latest_thread_id = COALESCE(?2, latest_thread_id),
             latest_cursor = ?3,
             updated_at = ?4
         WHERE session_id = ?1",
        params![session_id, thread_id, cursor as i64, ts.to_rfc3339()],
    )?;
    Ok(())
}

fn project_session_summary(
    conn: &Connection,
    scope: &Scope,
    event: &Event,
    cursor: u64,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<Option<SessionSummaryChange>> {
    let (session_id, thread_id) = match event {
        Event::SessionSummaryUpdated { .. } => return Ok(None),
        Event::SessionDeleted { session_id, .. } => {
            conn.execute(
                "DELETE FROM session_summary_attention WHERE session_id = ?1",
                [session_id],
            )?;
            conn.execute(
                "DELETE FROM session_summaries WHERE session_id = ?1",
                [session_id],
            )?;
            return Ok(Some(SessionSummaryChange {
                session_id: session_id.clone(),
                summary: None,
                notification: None,
            }));
        }
        Event::SessionCreated { session_id, .. }
        | Event::SessionUpdated { session_id, .. }
        | Event::SessionActivity { session_id, .. }
        | Event::SessionRecovered { session_id, .. } => (session_id.clone(), None),
        Event::ThreadCreated {
            session_id,
            thread_id,
        }
        | Event::ThreadUpdated {
            session_id,
            thread_id,
        } => (session_id.clone(), Some(thread_id.clone())),
        Event::TurnStarted { .. }
        | Event::TurnCompleted { .. }
        | Event::TurnFailed { .. }
        | Event::TurnCancelled { .. }
        | Event::ApprovalRequested { .. }
        | Event::ApprovalResolved { .. }
        | Event::ToolCompleted { .. }
        | Event::QuestionRequested { .. }
        | Event::QuestionResolved { .. } => {
            let Scope::Thread(thread_id) = scope else {
                return Ok(None);
            };
            let Some(session_id) = session_for_thread(conn, thread_id)? else {
                return Ok(None);
            };
            (session_id, Some(thread_id.clone()))
        }
        _ => return Ok(None),
    };

    if !ensure_session_summary(conn, &session_id, cursor, &ts)? {
        return Ok(None);
    }

    match event {
        Event::SessionUpdated { .. } => {
            conn.execute(
                "UPDATE session_summaries
                 SET archived = COALESCE(
                     (SELECT archived FROM sessions WHERE id = ?1), archived)
                 WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::SessionActivity { active, .. } => {
            conn.execute(
                "UPDATE session_summaries SET active = ?2 WHERE session_id = ?1",
                params![session_id, i64::from(*active)],
            )?;
        }
        Event::SessionRecovered { .. } => {
            conn.execute(
                "DELETE FROM session_summary_attention WHERE session_id = ?1",
                [&session_id],
            )?;
            conn.execute(
                "UPDATE session_summaries
                 SET active = 0, last_outcome = 'failed'
                 WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::ThreadCreated { .. } => {
            conn.execute(
                "UPDATE session_summaries SET last_outcome = 'idle' WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::TurnStarted { .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                clear_thread_attention(conn, thread_id)?;
            }
            conn.execute(
                "UPDATE session_summaries SET last_outcome = 'idle' WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::TurnCompleted { .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                clear_thread_attention(conn, thread_id)?;
            }
            conn.execute(
                "UPDATE session_summaries SET last_outcome = 'succeeded' WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::TurnFailed { .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                clear_thread_attention(conn, thread_id)?;
            }
            conn.execute(
                "UPDATE session_summaries SET last_outcome = 'failed' WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::TurnCancelled { .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                clear_thread_attention(conn, thread_id)?;
            }
            conn.execute(
                "UPDATE session_summaries SET last_outcome = 'idle' WHERE session_id = ?1",
                [&session_id],
            )?;
        }
        Event::ApprovalRequested { call_id, .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                let item_id = scoped_attention_item_id(thread_id, call_id);
                conn.execute(
                    "INSERT OR REPLACE INTO session_summary_attention
                       (kind, item_id, session_id, thread_id)
                     VALUES ('approval', ?1, ?2, ?3)",
                    params![item_id, session_id, thread_id],
                )?;
            }
        }
        Event::ApprovalResolved { call_id, .. } | Event::ToolCompleted { call_id, .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                let item_id = scoped_attention_item_id(thread_id, call_id);
                conn.execute(
                    "DELETE FROM session_summary_attention
                     WHERE kind = 'approval' AND item_id = ?1 AND session_id = ?2",
                    params![item_id, session_id],
                )?;
            }
        }
        Event::QuestionRequested { request_id, .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                let item_id = scoped_attention_item_id(thread_id, request_id);
                conn.execute(
                    "INSERT OR REPLACE INTO session_summary_attention
                       (kind, item_id, session_id, thread_id)
                     VALUES ('question', ?1, ?2, ?3)",
                    params![item_id, session_id, thread_id],
                )?;
            }
        }
        Event::QuestionResolved { request_id, .. } => {
            if let Some(thread_id) = thread_id.as_deref() {
                let item_id = scoped_attention_item_id(thread_id, request_id);
                conn.execute(
                    "DELETE FROM session_summary_attention
                     WHERE kind = 'question' AND item_id = ?1 AND session_id = ?2",
                    params![item_id, session_id],
                )?;
            }
        }
        _ => {}
    }

    let notification = session_notification_event(event, &session_id, thread_id.as_deref());
    touch_session_summary(conn, &session_id, thread_id.as_deref(), cursor, &ts)?;
    Ok(
        session_summary(conn, &session_id)?.map(|summary| SessionSummaryChange {
            session_id,
            summary: Some(summary),
            notification,
        }),
    )
}

fn scoped_attention_item_id(thread_id: &str, vendor_id: &str) -> String {
    // JSON string escaping makes the tuple unambiguous even if either vendor
    // identifier contains a delimiter chosen by a backend.
    serde_json::to_string(&(thread_id, vendor_id)).expect("string tuple serializes")
}

fn ensure_thread_status(conn: &Connection, thread_id: &str) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO thread_statuses
           (thread_id, session_id, active, attention, last_outcome, latest_cursor)
         SELECT id, session_id, 0, 'none', 'idle', 0
         FROM threads WHERE id = ?1",
        [thread_id],
    )?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM thread_statuses WHERE thread_id = ?1)",
        [thread_id],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn thread_attention(conn: &Connection, thread_id: &str) -> Result<SessionAttention> {
    let (approval, question) = conn.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM session_summary_attention
                  WHERE thread_id = ?1 AND kind = 'approval'),
           EXISTS(SELECT 1 FROM session_summary_attention
                  WHERE thread_id = ?1 AND kind = 'question')",
        [thread_id],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)? != 0)),
    )?;
    Ok(match (approval, question) {
        (false, false) => SessionAttention::None,
        (true, false) => SessionAttention::Approval,
        (false, true) => SessionAttention::Question,
        (true, true) => SessionAttention::Both,
    })
}

fn thread_status(conn: &Connection, thread_id: &str) -> Result<Option<ThreadStatus>> {
    conn.query_row(
        "SELECT session_id, active, attention, last_outcome, latest_cursor,
                started_at, completed_at
         FROM thread_statuses WHERE thread_id = ?1",
        [thread_id],
        |row| {
            let active = row.get::<_, i64>(1)? != 0;
            let attention = match row.get::<_, String>(2)?.as_str() {
                "approval" => SessionAttention::Approval,
                "question" => SessionAttention::Question,
                "both" => SessionAttention::Both,
                _ => SessionAttention::None,
            };
            let outcome = if active {
                SessionOutcome::Running
            } else {
                match row.get::<_, String>(3)?.as_str() {
                    "succeeded" => SessionOutcome::Succeeded,
                    "failed" => SessionOutcome::Failed,
                    _ => SessionOutcome::Idle,
                }
            };
            Ok(ThreadStatus {
                thread_id: thread_id.to_owned(),
                session_id: row.get(0)?,
                active,
                attention,
                outcome,
                latest_cursor: row.get::<_, i64>(4)? as u64,
                started_at: row
                    .get::<_, Option<String>>(5)?
                    .and_then(|value| value.parse().ok()),
                completed_at: row
                    .get::<_, Option<String>>(6)?
                    .and_then(|value| value.parse().ok()),
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn thread_statuses(conn: &Connection, session_id: &str) -> Result<Vec<ThreadStatus>> {
    let mut stmt = conn.prepare(
        "SELECT thread_id FROM thread_statuses
         WHERE session_id = ?1 ORDER BY thread_id",
    )?;
    let ids = stmt
        .query_map([session_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut statuses = Vec::with_capacity(ids.len());
    for thread_id in ids {
        if let Some(status) = thread_status(conn, &thread_id)? {
            statuses.push(status);
        }
    }
    Ok(statuses)
}

fn project_thread_status(
    conn: &Connection,
    scope: &Scope,
    event: &Event,
    cursor: u64,
    timestamp: &str,
) -> Result<Option<ThreadStatusChange>> {
    if matches!(event, Event::ThreadStatusUpdated { .. }) {
        return Ok(None);
    }
    if let Event::SessionRecovered { session_id, .. } = event {
        let interrupted_thread_ids = {
            let mut stmt = conn.prepare(
                "SELECT thread_id FROM thread_statuses
                 WHERE session_id = ?1 AND (active != 0 OR attention != 'none')
                 ORDER BY thread_id",
            )?;
            stmt.query_map([session_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if interrupted_thread_ids.is_empty() {
            return Ok(None);
        }
        conn.execute(
            "UPDATE thread_statuses
             SET completed_at = ?3, active = 0, attention = 'none',
                 last_outcome = 'failed', latest_cursor = ?2
             WHERE session_id = ?1 AND (active != 0 OR attention != 'none')",
            params![session_id, cursor as i64, timestamp],
        )?;
        let statuses = interrupted_thread_ids
            .into_iter()
            .map(|thread_id| {
                thread_status(conn, &thread_id)?.context("recovered thread status disappeared")
            })
            .collect::<Result<Vec<_>>>()?;
        return Ok(Some(ThreadStatusChange { statuses }));
    }

    let thread_id = match event {
        Event::ThreadCreated { thread_id, .. } => thread_id.clone(),
        Event::TurnStarted { .. }
        | Event::TurnCompleted { .. }
        | Event::TurnFailed { .. }
        | Event::TurnCancelled { .. }
        | Event::ApprovalRequested { .. }
        | Event::ApprovalResolved { .. }
        | Event::ToolCompleted { .. }
        | Event::QuestionRequested { .. }
        | Event::QuestionResolved { .. } => {
            let Scope::Thread(thread_id) = scope else {
                return Ok(None);
            };
            thread_id.clone()
        }
        _ => return Ok(None),
    };
    if !ensure_thread_status(conn, &thread_id)? {
        return Ok(None);
    }

    match event {
        Event::ThreadCreated { .. } => {
            conn.execute(
                "UPDATE thread_statuses
                 SET active = 0, attention = 'none', last_outcome = 'idle',
                     started_at = NULL, completed_at = NULL
                 WHERE thread_id = ?1",
                [&thread_id],
            )?;
        }
        Event::TurnStarted { .. } => {
            conn.execute(
                "UPDATE thread_statuses
                 SET active = 1, last_outcome = 'running', started_at = ?2,
                     completed_at = NULL
                 WHERE thread_id = ?1",
                params![thread_id, timestamp],
            )?;
        }
        Event::TurnCompleted { .. } => {
            conn.execute(
                "UPDATE thread_statuses
                 SET active = 0, last_outcome = 'succeeded', completed_at = ?2
                 WHERE thread_id = ?1",
                params![thread_id, timestamp],
            )?;
        }
        Event::TurnFailed { .. } => {
            conn.execute(
                "UPDATE thread_statuses
                 SET active = 0, last_outcome = 'failed', completed_at = ?2
                 WHERE thread_id = ?1",
                params![thread_id, timestamp],
            )?;
        }
        Event::TurnCancelled { .. } => {
            conn.execute(
                "UPDATE thread_statuses
                 SET active = 0, last_outcome = 'idle', completed_at = ?2
                 WHERE thread_id = ?1",
                params![thread_id, timestamp],
            )?;
        }
        _ => {}
    }
    let attention = match thread_attention(conn, &thread_id)? {
        SessionAttention::None => "none",
        SessionAttention::Approval => "approval",
        SessionAttention::Question => "question",
        SessionAttention::Both => "both",
    };
    conn.execute(
        "UPDATE thread_statuses SET attention = ?2, latest_cursor = ?3
         WHERE thread_id = ?1",
        params![thread_id, attention, cursor as i64],
    )?;
    Ok(
        thread_status(conn, &thread_id)?.map(|status| ThreadStatusChange {
            statuses: vec![status],
        }),
    )
}

const SESSION_NOTIFICATION_DETAIL_CHARS: usize = 120;

fn compact_session_notification_detail(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut chars = value.chars();
    let mut compact = chars
        .by_ref()
        .take(SESSION_NOTIFICATION_DETAIL_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        compact.push('…');
    }
    Some(compact)
}

fn session_notification_event(
    source: &Event,
    session_id: &str,
    thread_id: Option<&str>,
) -> Option<Event> {
    let thread_id = thread_id?;
    let (kind, detail) = match source {
        Event::TurnCompleted { .. } => (
            trouve_protocol::SessionNotificationKind::TurnCompleted,
            None,
        ),
        Event::TurnFailed { error, .. } => (
            trouve_protocol::SessionNotificationKind::TurnFailed,
            compact_session_notification_detail(error),
        ),
        Event::ApprovalRequested { .. } => (
            trouve_protocol::SessionNotificationKind::ApprovalRequested,
            None,
        ),
        Event::QuestionRequested { title, .. } => (
            trouve_protocol::SessionNotificationKind::QuestionRequested,
            title
                .as_deref()
                .and_then(compact_session_notification_detail),
        ),
        _ => return None,
    };
    Some(Event::SessionNotification {
        session_id: session_id.to_owned(),
        thread_id: thread_id.to_owned(),
        kind,
        detail,
    })
}

fn session_summary(conn: &Connection, session_id: &str) -> Result<Option<SessionSummary>> {
    let row = conn
        .query_row(
            "SELECT workspace_id, archived, active, last_outcome,
                    latest_thread_id, latest_cursor, updated_at
             FROM session_summaries WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((workspace_id, archived, active, last_outcome, latest_thread_id, cursor, ts)) = row
    else {
        return Ok(None);
    };
    let (approval, question) = conn.query_row(
        "SELECT
           EXISTS(SELECT 1 FROM session_summary_attention
                  WHERE session_id = ?1 AND kind = 'approval'),
           EXISTS(SELECT 1 FROM session_summary_attention
                  WHERE session_id = ?1 AND kind = 'question')",
        [session_id],
        |row| Ok((row.get::<_, i64>(0)? != 0, row.get::<_, i64>(1)? != 0)),
    )?;
    let attention = match (approval, question) {
        (false, false) => SessionAttention::None,
        (true, false) => SessionAttention::Approval,
        (false, true) => SessionAttention::Question,
        (true, true) => SessionAttention::Both,
    };
    let active = active != 0;
    let outcome = if active {
        SessionOutcome::Running
    } else {
        match last_outcome.as_str() {
            "idle" => SessionOutcome::Idle,
            "succeeded" => SessionOutcome::Succeeded,
            "failed" => SessionOutcome::Failed,
            // A downgrade may encounter a newer additive outcome literal.
            // Keep the writer transaction available and expose the neutral
            // state until a known event replaces the projection value.
            _ => SessionOutcome::Idle,
        }
    };
    let updated_at = chrono::DateTime::parse_from_rfc3339(&ts)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
    Ok(Some(SessionSummary {
        session_id: session_id.to_string(),
        workspace_id,
        archived: archived != 0,
        active,
        attention,
        outcome,
        latest_thread_id,
        latest_cursor: cursor as u64,
        updated_at,
    }))
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
    let permission_mode: String = r.get(7)?;
    let schedule_json: String = r.get(8)?;
    Ok(trouve_protocol::Automation {
        id: r.get(0)?,
        name: r.get(1)?,
        prompt: r.get(2)?,
        workspace_id: r.get(3)?,
        mode: r.get(4)?,
        model: r.get(5)?,
        thinking_level: r.get(6)?,
        permission_mode: permission_mode_from(&permission_mode),
        schedule: serde_json::from_str(&schedule_json).unwrap_or(
            trouve_protocol::AutomationSchedule {
                kind: "daily".into(),
                minute: 0,
                time: "09:00".into(),
                days: vec![],
            },
        ),
        enabled: r.get(9)?,
        next_run_at: r.get(10)?,
        last_run_at: r.get(11)?,
        last_session_id: r.get(12)?,
        last_error: r.get(13)?,
        created_at: r.get(14)?,
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

fn code_review_publication_status(
    value: &str,
) -> trouve_protocol::CodeReviewFindingPublicationStatus {
    match value {
        "published" => trouve_protocol::CodeReviewFindingPublicationStatus::Published,
        "not_eligible" => trouve_protocol::CodeReviewFindingPublicationStatus::NotEligible,
        "suppressed_by_policy" => {
            trouve_protocol::CodeReviewFindingPublicationStatus::SuppressedByPolicy
        }
        "failed" => trouve_protocol::CodeReviewFindingPublicationStatus::Failed,
        _ => trouve_protocol::CodeReviewFindingPublicationStatus::Pending,
    }
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
    pub publication_accepted: bool,
}

#[derive(Debug, Clone)]
pub struct CodeReviewBatchSnapshotUpdate {
    pub changed: bool,
}

#[derive(Debug, Clone)]
pub struct PendingCodeReviewEvent {
    pub id: i64,
    pub event: Event,
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
    let review_watermark_sha: String = r.get(50)?;
    let effective_review_base_sha = if review_base_sha.is_empty() {
        base_ref.clone()
    } else {
        review_base_sha
    };
    Ok(CodeReviewJobRecord {
        job: trouve_protocol::CodeReviewJob {
            id: r.get(0)?,
            installation_id: r.get::<_, i64>(1)? as u64,
            repository: r.get(2)?,
            pull_number: r.get::<_, i64>(3)? as u64,
            pull_title: r.get(4)?,
            pull_url: r.get(5)?,
            head_sha: r.get(6)?,
            review_base_sha: effective_review_base_sha.clone(),
            review_watermark_sha: if review_watermark_sha.is_empty() {
                effective_review_base_sha
            } else {
                review_watermark_sha
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
        publication_accepted: r.get(52)?,
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
     coordinator_thinking_level, review_watermark_sha, review_batch_digest, publication_accepted";

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
    latest
        .into_values()
        .filter(|attempt| attempt.task.status != "superseded")
        .collect()
}

fn completed_code_review_persona_count(attempts: &[CodeReviewTaskAttempt]) -> u64 {
    let mut grouped: BTreeMap<String, Vec<&trouve_protocol::CodeReviewTask>> = BTreeMap::new();
    for attempt in latest_code_review_task_attempts(attempts) {
        let task = &attempt.task;
        if let Some(reviewer_id) = task.reviewer_id.as_ref() {
            grouped.entry(reviewer_id.clone()).or_default().push(task);
        }
    }
    grouped
        .into_values()
        .filter(|tasks| {
            tasks.iter().all(|task| {
                matches!(
                    task.status.as_str(),
                    "succeeded" | "failed" | "cancelled" | "not_applicable"
                )
            }) && tasks.len() as u64 >= tasks.iter().map(|task| task.batch_count).max().unwrap_or(0)
        })
        .count() as u64
}

fn enqueue_code_review_pending_event(
    tx: &rusqlite::Transaction<'_>,
    job_id: &str,
    event: &Event,
) -> Result<()> {
    tx.execute(
        "INSERT INTO code_review_pending_events (job_id, payload, created_at)
         VALUES (?1, ?2, ?3)",
        params![
            job_id,
            serde_json::to_string(event)?,
            chrono::Utc::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

fn supersede_code_review_task(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    job_id: &str,
    completed_at: &str,
    recovery_message: &str,
) -> Result<bool> {
    let updated = tx.execute(
        "UPDATE code_review_tasks
         SET status = 'superseded',
             completed_at = COALESCE(completed_at, ?3),
             error = CASE
               WHEN status IN ('succeeded', 'failed', 'cancelled', 'not_applicable')
                 THEN error
               ELSE ?4
             END
         WHERE id = ?1 AND job_id = ?2 AND status != 'superseded'",
        params![task_id, job_id, completed_at, recovery_message],
    )?;
    if updated == 0 {
        return Ok(false);
    }
    let task = tx.query_row(
        &format!("SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks WHERE id = ?1"),
        [task_id],
        row_to_code_review_task,
    )?;
    enqueue_code_review_pending_event(
        tx,
        job_id,
        &Event::CodeReviewTaskUpdated {
            job_id: job_id.to_owned(),
            task: Box::new(task),
        },
    )?;
    Ok(true)
}

#[derive(Debug, Clone)]
pub struct NewCodeReviewFinding {
    pub path: String,
    pub line: u64,
    pub side: String,
    pub severity: String,
    pub confidence: String,
    pub title: String,
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

#[derive(Debug, Clone)]
pub(crate) struct ArtifactCleanupJob {
    pub id: String,
    pub claim_token: Option<String>,
    pub session_id: Option<String>,
    pub worktree_path: Option<String>,
    pub repository_path: Option<String>,
    pub attachment_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPrVerificationIntent {
    pub session_id: String,
    pub host: String,
    pub owner: String,
    pub repository: String,
    pub number: u64,
    pub branch: String,
    pub head_sha: String,
    pub attempts: u32,
    pub last_failure_class: String,
    pub consecutive_failures: u32,
    pub created_at: String,
}

impl ArtifactCleanupJob {
    pub(crate) fn deleted_session(
        session_id: String,
        worktree_path: String,
        repository_path: String,
        attachment_paths: Vec<String>,
    ) -> Self {
        Self {
            id: format!("acj_{}", uuid::Uuid::new_v4().simple()),
            claim_token: None,
            session_id: Some(session_id),
            worktree_path: Some(worktree_path),
            repository_path: Some(repository_path),
            attachment_paths,
        }
    }

    fn attachments(attachment_paths: Vec<String>) -> Self {
        Self {
            id: format!("acj_{}", uuid::Uuid::new_v4().simple()),
            claim_token: None,
            session_id: None,
            worktree_path: None,
            repository_path: None,
            attachment_paths,
        }
    }

    fn kind(&self) -> &'static str {
        if self.session_id.is_some() {
            "deleted_session"
        } else {
            "attachments"
        }
    }
}

const ARTIFACT_CLEANUP_CLAIM_MINUTES: i64 = 5;
const MAX_POISONED_ARTIFACT_CLEANUP_ROWS_PER_CLAIM: usize = 64;
const MAX_ARTIFACT_CLEANUP_PATHS_JSON_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACT_CLEANUP_PATHS: usize = 4096;
const MAX_ARTIFACT_CLEANUP_PATH_BYTES: usize = 32 * 1024;
const MAX_ARTIFACT_CLEANUP_METADATA_BYTES: usize = 32 * 1024;
const MAX_ARTIFACT_CLEANUP_TIMESTAMP_BYTES: usize = 64;

type RawArtifactCleanupJob = (
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

fn raw_artifact_cleanup_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawArtifactCleanupJob> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get::<_, i64>(7)? != 0,
    ))
}

fn artifact_cleanup_job_is_claimable(
    conn: &Connection,
    requested_id: Option<&str>,
    now: &str,
) -> rusqlite::Result<bool> {
    if let Some(id) = requested_id {
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM artifact_cleanup_jobs
                 WHERE id = ?1
                   AND (claim_until IS NULL
                        OR typeof(claim_until) != 'text'
                        OR length(CAST(claim_until AS BLOB)) > ?3
                        OR instr(claim_until, 'T') != 11
                        OR julianday(claim_until) IS NULL
                        OR julianday(claim_until) <= julianday(?2))
                   AND (next_attempt_at IS NULL
                        OR typeof(next_attempt_at) != 'text'
                        OR length(CAST(next_attempt_at AS BLOB)) > ?3
                        OR instr(next_attempt_at, 'T') != 11
                        OR julianday(next_attempt_at) IS NULL
                        OR julianday(next_attempt_at) <= julianday(?2))
             )",
            params![id, now, MAX_ARTIFACT_CLEANUP_TIMESTAMP_BYTES as i64],
            |row| row.get(0),
        )
    } else {
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM artifact_cleanup_jobs
                 WHERE (claim_until IS NULL
                        OR typeof(claim_until) != 'text'
                        OR length(CAST(claim_until AS BLOB)) > ?2
                        OR instr(claim_until, 'T') != 11
                        OR julianday(claim_until) IS NULL
                        OR julianday(claim_until) <= julianday(?1))
                   AND (next_attempt_at IS NULL
                        OR typeof(next_attempt_at) != 'text'
                        OR length(CAST(next_attempt_at AS BLOB)) > ?2
                        OR instr(next_attempt_at, 'T') != 11
                        OR julianday(next_attempt_at) IS NULL
                        OR julianday(next_attempt_at) <= julianday(?1))
             )",
            params![now, MAX_ARTIFACT_CLEANUP_TIMESTAMP_BYTES as i64],
            |row| row.get(0),
        )
    }
}

fn decode_artifact_cleanup_job(
    id: String,
    kind: String,
    session_id: Option<String>,
    worktree_path: Option<String>,
    repository_path: Option<String>,
    paths_json: String,
) -> Result<ArtifactCleanupJob> {
    anyhow::ensure!(
        paths_json.len() <= MAX_ARTIFACT_CLEANUP_PATHS_JSON_BYTES,
        "artifact cleanup job {id} attachment paths exceed the decode limit"
    );
    let attachment_paths: Vec<String> = serde_json::from_str(&paths_json)
        .with_context(|| format!("artifact cleanup job {id} has invalid attachment paths"))?;
    anyhow::ensure!(
        attachment_paths.len() <= MAX_ARTIFACT_CLEANUP_PATHS,
        "artifact cleanup job {id} has too many attachment paths"
    );
    anyhow::ensure!(
        attachment_paths
            .iter()
            .all(|path| !path.is_empty() && path.len() <= MAX_ARTIFACT_CLEANUP_PATH_BYTES),
        "artifact cleanup job {id} contains an invalid attachment path length"
    );
    match kind.as_str() {
        "attachments" => anyhow::ensure!(
            session_id.is_none() && worktree_path.is_none() && repository_path.is_none(),
            "attachment cleanup job {id} has unexpected session fields"
        ),
        "deleted_session" => anyhow::ensure!(
            session_id.is_some() && worktree_path.is_some() && repository_path.is_some(),
            "deleted-session cleanup job {id} is missing required fields"
        ),
        _ => anyhow::bail!("artifact cleanup job {id} has unknown kind {kind:?}"),
    }
    Ok(ArtifactCleanupJob {
        id,
        claim_token: None,
        session_id,
        worktree_path,
        repository_path,
        attachment_paths,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactCleanupClaim {
    pub id: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonaDeletionClaim {
    pub id: String,
    pub token: String,
    pub attempts: i64,
}

const PERSONA_DELETION_CLAIM_MINUTES: i64 = 5;

pub(crate) struct PromptAcceptance {
    pub prompt: trouve_protocol::QueuedPrompt,
    pub tools_enabled: bool,
    pub attachments: Vec<(trouve_protocol::Attachment, String)>,
    pub claim_prompt_id: Option<String>,
    pub expected_previous_turn: Option<u64>,
    pub staging_cleanup_claim: Option<ArtifactCleanupClaim>,
}

impl ArtifactCleanupJob {
    pub(crate) fn claim(&self) -> Option<ArtifactCleanupClaim> {
        self.claim_token.as_ref().map(|token| ArtifactCleanupClaim {
            id: self.id.clone(),
            token: token.clone(),
        })
    }
}

/// One serialized event, in flight to the writer thread.
struct PendingEvent {
    scope: Scope,
    ts: chrono::DateTime<chrono::Utc>,
    /// Serialized on the caller's task so an unserializable event fails
    /// there instead of poisoning a whole batch.
    payload: String,
    event: Event,
    /// Optional relational mutation that must commit immediately before this
    /// source event and its derived projection update.
    mutation: Option<StoreMutation>,
}

enum StoreMutation {
    Insert {
        session: Box<Session>,
        initial_checkpoint: Box<CheckpointRow>,
    },
    Update {
        id: String,
        title: Option<String>,
        archived: Option<bool>,
    },
    UpdateThread {
        id: String,
        mode: Option<String>,
        model: Option<String>,
        model_options: Option<serde_json::Map<String, serde_json::Value>>,
        permission_mode: Option<PermissionMode>,
    },
    InsertThread {
        thread: Box<Thread>,
        model_options: serde_json::Map<String, serde_json::Value>,
        spawn: Option<(String, String)>,
    },
    Delete {
        id: String,
        cleanup: Box<ArtifactCleanupJob>,
    },
    UpsertSessionPrVerificationIntents {
        intents: Vec<SessionPrVerificationIntent>,
    },
    CompleteSessionPrVerificationIntent {
        intent: Box<SessionPrVerificationIntent>,
    },
    AcceptPrompt {
        prompt: Box<trouve_protocol::QueuedPrompt>,
        tools_enabled: bool,
        attachments: Vec<(trouve_protocol::Attachment, String)>,
        claim_prompt_id: Option<String>,
        expected_previous_turn: Option<u64>,
        staging_cleanup_claim: Option<ArtifactCleanupClaim>,
    },
    AppendCheckpoint {
        checkpoint: Box<CheckpointRow>,
    },
    AppendMessage {
        thread_id: String,
        payload: String,
        attachments: Vec<(trouve_protocol::Attachment, String)>,
        staging_cleanup_claim: Option<ArtifactCleanupClaim>,
    },
}

/// One caller's event batch, in flight to the writer thread.
struct AppendRequest {
    events: Vec<PendingEvent>,
    code_review_outbox_ids: Vec<i64>,
    /// Conditional mutations must not roll unrelated callers back when their
    /// precondition is stale.
    isolated: bool,
    reply: AppendReply,
    queued_at: std::time::Instant,
}

/// The on-disk event writer owns a dedicated SQLite connection so read-side
/// queries cannot hold it behind the store's process-local connection mutex.
/// In-memory stores must share their sole connection because independent
/// `:memory:` connections are independent databases.
enum EventWriterConnection {
    Dedicated(Connection),
    Shared(Arc<Mutex<Connection>>),
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
    conn: EventWriterConnection,
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
                let queued_at = first.queued_at;
                let isolate_request = first.isolated || !first.code_review_outbox_ids.is_empty();
                let mut requests = vec![first];
                while !isolate_request && event_count < APPEND_BATCH_MAX {
                    let Ok(request) = rx.try_recv() else {
                        break;
                    };
                    if request.isolated
                        || !request.code_review_outbox_ids.is_empty()
                        || event_count.saturating_add(request.events.len()) > APPEND_BATCH_MAX
                    {
                        deferred = Some(request);
                        break;
                    }
                    event_count += request.events.len();
                    requests.push(request);
                }
                let queue_wait = queued_at.elapsed();
                let (inserted, connection_wait, commit_elapsed) = match &conn {
                    EventWriterConnection::Dedicated(conn) => {
                        let started = std::time::Instant::now();
                        let inserted = insert_event_batch(
                            conn,
                            requests.iter().flat_map(|request| request.events.iter()),
                            event_count,
                            requests
                                .iter()
                                .flat_map(|request| request.code_review_outbox_ids.iter().copied()),
                        );
                        (inserted, std::time::Duration::ZERO, started.elapsed())
                    }
                    EventWriterConnection::Shared(conn) => {
                        let wait_started = std::time::Instant::now();
                        let conn = conn.lock().unwrap();
                        let connection_wait = wait_started.elapsed();
                        let started = std::time::Instant::now();
                        let inserted = insert_event_batch(
                            &conn,
                            requests.iter().flat_map(|request| request.events.iter()),
                            event_count,
                            requests
                                .iter()
                                .flat_map(|request| request.code_review_outbox_ids.iter().copied()),
                        );
                        (inserted, connection_wait, started.elapsed())
                    }
                };
                let total_elapsed = queue_wait
                    .saturating_add(connection_wait)
                    .saturating_add(commit_elapsed);
                if total_elapsed >= std::time::Duration::from_millis(20) {
                    tracing::warn!(
                        event_count,
                        request_count = requests.len(),
                        queue_wait_ms = queue_wait.as_millis(),
                        connection_wait_ms = connection_wait.as_millis(),
                        commit_ms = commit_elapsed.as_millis(),
                        total_ms = total_elapsed.as_millis(),
                        "slow event-log batch commit"
                    );
                } else {
                    tracing::trace!(
                        event_count,
                        request_count = requests.len(),
                        queue_wait_us = queue_wait.as_micros(),
                        connection_wait_us = connection_wait.as_micros(),
                        commit_us = commit_elapsed.as_micros(),
                        "event-log batch committed"
                    );
                }
                match inserted {
                    Ok(inserted) if inserted.skipped => {
                        for request in requests {
                            request.reply.send(Ok(Vec::new()));
                        }
                    }
                    Ok(inserted) => {
                        // Publish every committed source and derived event in
                        // exact cursor order before resolving append callers.
                        for envelope in inserted.published {
                            let _ = events_tx.send(envelope.clone());
                            let (kind, id) = scope_cols(&envelope.scope);
                            let scoped_sender = scoped_events
                                .lock()
                                .unwrap()
                                .get(&(kind.to_owned(), id))
                                .cloned();
                            if let Some(sender) = scoped_sender {
                                let _ = sender.send(envelope);
                            }
                        }

                        let mut cursors = inserted.source_cursors.into_iter();
                        for request in requests {
                            let mut envelopes = Vec::with_capacity(request.events.len());
                            for event in request.events {
                                let envelope = EventEnvelope {
                                    cursor: cursors.next().expect("one cursor per inserted event"),
                                    scope: event.scope,
                                    ts: event.ts,
                                    event: event.event,
                                };
                                envelopes.push(envelope);
                            }
                            request.reply.send(Ok(envelopes));
                        }
                    }
                    Err(e) => {
                        // The transaction rolled back: every waiter's event
                        // was equally not persisted.
                        let message = format!("appending event batch: {e}");
                        let sqlite_code = e.chain().find_map(|cause| {
                            cause
                                .downcast_ref::<rusqlite::Error>()
                                .and_then(|error| match error {
                                    rusqlite::Error::SqliteFailure(details, _) => {
                                        Some(details.code)
                                    }
                                    _ => None,
                                })
                        });
                        for request in requests {
                            request.reply.send(Err(anyhow::Error::new(EventWriterError {
                                message: message.clone(),
                                sqlite_code,
                            })));
                        }
                    }
                }
            }
        })
        .expect("spawning event writer thread");
    tx
}

#[derive(Debug, Clone)]
struct EventWriterError {
    message: String,
    sqlite_code: Option<rusqlite::ErrorCode>,
}

impl std::fmt::Display for EventWriterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EventWriterError {}

/// Recover the SQLite classification retained when the event-writer thread
/// transported a transaction error back across its reply channel.
pub(crate) fn event_writer_sqlite_error_code(error: &anyhow::Error) -> Option<rusqlite::ErrorCode> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<EventWriterError>())
        .and_then(|error| error.sqlite_code)
}

/// Insert a batch in queue order under one transaction, returning the
/// assigned cursors. All-or-nothing: on error the transaction rolls back.
struct InsertedEventBatch {
    skipped: bool,
    source_cursors: Vec<u64>,
    published: Vec<EventEnvelope>,
}

fn insert_session_row(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions
           (id, workspace_id, title, branch, worktree_path, base_ref, archived, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            session.id,
            session.workspace_id,
            session.title,
            session.branch,
            session.worktree_path,
            session.base_ref,
            session.archived,
            session.created_at.to_rfc3339()
        ],
    )?;
    Ok(())
}

fn insert_initial_checkpoint_row(
    conn: &Connection,
    checkpoint: &CheckpointRow,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO checkpoints
           (id, session_id, thread_id, turn, seq, commit_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            checkpoint.id,
            checkpoint.session_id,
            checkpoint.thread_id,
            checkpoint.turn as i64,
            checkpoint.seq,
            checkpoint.commit_hash,
            created_at.to_rfc3339()
        ],
    )?;
    Ok(())
}

fn append_checkpoint_row(
    conn: &Connection,
    row: &CheckpointRow,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
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
            created_at.to_rfc3339()
        ],
    )?;
    Ok(())
}

fn update_session_row(
    conn: &Connection,
    id: &str,
    title: Option<&str>,
    archived: Option<bool>,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE sessions
         SET title = COALESCE(?2, title), archived = COALESCE(?3, archived)
         WHERE id = ?1",
        params![id, title, archived],
    )?;
    anyhow::ensure!(updated == 1, "session {id} no longer exists");
    Ok(())
}

fn update_thread_row(
    conn: &Connection,
    id: &str,
    mode: Option<&str>,
    model: Option<&str>,
    model_options: Option<&serde_json::Map<String, serde_json::Value>>,
    permission_mode: Option<PermissionMode>,
) -> Result<()> {
    let model_options = model_options.map(serde_json::to_string).transpose()?;
    let permission_mode = permission_mode.map(permission_mode_str);
    let updated = conn.execute(
        "UPDATE threads
         SET mode = COALESCE(?2, mode),
             model = COALESCE(?3, model),
             model_options = COALESCE(?4, model_options),
             permission_mode = COALESCE(?5, permission_mode)
         WHERE id = ?1",
        params![id, mode, model, model_options, permission_mode],
    )?;
    anyhow::ensure!(updated == 1, "thread {id} no longer exists");
    Ok(())
}

fn delete_session_rows(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM session_pr_verification_intents WHERE session_id = ?1",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM events WHERE (scope_kind = 'session' AND scope_id = ?1)
         OR (scope_kind = 'thread' AND scope_id IN
             (SELECT id FROM threads WHERE session_id = ?1))",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM messages WHERE thread_id IN
         (SELECT id FROM threads WHERE session_id = ?1)",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM backend_sessions WHERE thread_id IN
         (SELECT id FROM threads WHERE session_id = ?1)",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM queued_prompts WHERE thread_id IN
         (SELECT id FROM threads WHERE session_id = ?1)",
        params![id],
    )?;
    conn.execute("DELETE FROM usage WHERE session_id = ?1", params![id])?;
    conn.execute("DELETE FROM checkpoints WHERE session_id = ?1", params![id])?;
    conn.execute(
        "DELETE FROM attachments WHERE thread_id IN
         (SELECT id FROM threads WHERE session_id = ?1)",
        params![id],
    )?;
    conn.execute(
        "DELETE FROM spawned_threads
         WHERE child_thread_id IN (SELECT id FROM threads WHERE session_id = ?1)
            OR parent_thread_id IN (SELECT id FROM threads WHERE session_id = ?1)",
        params![id],
    )?;
    conn.execute("DELETE FROM threads WHERE session_id = ?1", params![id])?;
    let deleted = conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    anyhow::ensure!(deleted == 1, "session {id} no longer exists");
    Ok(())
}

fn insert_artifact_cleanup_job(
    conn: &Connection,
    job: &ArtifactCleanupJob,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO artifact_cleanup_jobs
           (id, kind, session_id, worktree_path, repository_path,
            attachment_paths, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            job.id,
            job.kind(),
            job.session_id,
            job.worktree_path,
            job.repository_path,
            serde_json::to_string(&job.attachment_paths)?,
            timestamp.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn apply_store_mutation(
    conn: &Connection,
    mutation: &StoreMutation,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    match mutation {
        StoreMutation::Insert {
            session,
            initial_checkpoint,
        } => {
            insert_session_row(conn, session)?;
            insert_initial_checkpoint_row(conn, initial_checkpoint, timestamp)?;
        }
        StoreMutation::Update {
            id,
            title,
            archived,
        } => update_session_row(conn, id, title.as_deref(), *archived)?,
        StoreMutation::UpdateThread {
            id,
            mode,
            model,
            model_options,
            permission_mode,
        } => update_thread_row(
            conn,
            id,
            mode.as_deref(),
            model.as_deref(),
            model_options.as_ref(),
            *permission_mode,
        )?,
        StoreMutation::InsertThread {
            thread,
            model_options,
            spawn,
        } => {
            // Validate the owner in this transaction so a stale engine
            // snapshot cannot surface an FK error after session deletion.
            let child_workspace = conn
                .query_row(
                    "SELECT workspace_id FROM sessions WHERE id = ?1",
                    params![thread.session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            anyhow::ensure!(
                child_workspace.is_some(),
                "session {} no longer exists",
                thread.session_id
            );
            insert_thread_row(conn, thread, model_options)?;
            if let Some((parent, kind)) = spawn {
                let parent_owner = conn
                    .query_row(
                        "SELECT threads.session_id, sessions.workspace_id
                         FROM threads
                         JOIN sessions ON sessions.id = threads.session_id
                         WHERE threads.id = ?1",
                        params![parent],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .optional()?;
                let Some((parent_session, parent_workspace)) = parent_owner else {
                    anyhow::bail!("spawn parent {parent} no longer exists");
                };
                if *kind == "session" {
                    anyhow::ensure!(
                        child_workspace.as_deref() == Some(parent_workspace.as_str()),
                        "spawn parent {parent} does not belong to workspace of session {}",
                        thread.session_id
                    );
                } else {
                    anyhow::ensure!(
                        parent_session == thread.session_id,
                        "spawn parent {parent} does not belong to session {}",
                        thread.session_id
                    );
                }
                conn.execute(
                    "INSERT INTO spawned_threads (child_thread_id, parent_thread_id, kind)
                     VALUES (?1, ?2, ?3)",
                    params![thread.id, parent, kind],
                )?;
            }
        }
        StoreMutation::Delete { id, cleanup } => {
            insert_artifact_cleanup_job(conn, cleanup, timestamp)?;
            delete_session_rows(conn, id)?;
        }
        StoreMutation::UpsertSessionPrVerificationIntents { intents } => {
            for intent in intents {
                anyhow::ensure!(
                    !intent.branch.is_empty() && !intent.head_sha.is_empty(),
                    "pull request verification intent requires immutable branch and head evidence"
                );
                conn.execute(
                    "INSERT INTO session_pr_verification_intents
                       (session_id, host, owner, repository, pull_number, branch,
                        head_sha, attempts, last_failure_class, consecutive_failures,
                        next_attempt_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, '', 0, NULL, ?8)
                     ON CONFLICT(session_id, host, owner, repository, pull_number)
                     DO UPDATE SET
                       attempts = CASE
                         WHEN branch = excluded.branch AND head_sha = excluded.head_sha
                           THEN attempts ELSE 0 END,
                       last_failure_class = CASE
                         WHEN branch = excluded.branch AND head_sha = excluded.head_sha
                           THEN last_failure_class ELSE '' END,
                       consecutive_failures = CASE
                         WHEN branch = excluded.branch AND head_sha = excluded.head_sha
                           THEN consecutive_failures ELSE 0 END,
                       next_attempt_at = CASE
                         WHEN branch = excluded.branch AND head_sha = excluded.head_sha
                           THEN next_attempt_at ELSE NULL END,
                       created_at = CASE
                         WHEN branch = excluded.branch AND head_sha = excluded.head_sha
                           THEN created_at ELSE excluded.created_at END,
                       branch = excluded.branch,
                       head_sha = excluded.head_sha",
                    params![
                        intent.session_id,
                        intent.host,
                        intent.owner,
                        intent.repository,
                        intent.number as i64,
                        intent.branch,
                        intent.head_sha,
                        intent.created_at,
                    ],
                )?;
            }
        }
        StoreMutation::CompleteSessionPrVerificationIntent { intent } => {
            let deleted = conn.execute(
                "DELETE FROM session_pr_verification_intents
                 WHERE session_id = ?1 AND host = ?2 AND owner = ?3
                   AND repository = ?4 AND pull_number = ?5
                   AND branch = ?6 AND head_sha = ?7",
                params![
                    intent.session_id,
                    intent.host,
                    intent.owner,
                    intent.repository,
                    intent.number as i64,
                    intent.branch,
                    intent.head_sha,
                ],
            )?;
            if deleted == 0 {
                return Ok(false);
            }
        }
        StoreMutation::AcceptPrompt {
            prompt,
            tools_enabled,
            attachments,
            claim_prompt_id,
            expected_previous_turn,
            staging_cleanup_claim,
        } => {
            for (attachment, path) in attachments {
                conn.execute(
                    "INSERT INTO attachments
                       (id, thread_id, name, mime, size_bytes, path, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        attachment.id,
                        prompt.thread_id,
                        attachment.name,
                        attachment.mime,
                        attachment.size_bytes as i64,
                        path,
                        timestamp.to_rfc3339(),
                    ],
                )?;
            }
            conn.execute(
                "INSERT INTO queued_prompts
                   (id, thread_id, position, content, attachments, tools_enabled, claimed, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
                params![
                    prompt.id,
                    prompt.thread_id,
                    prompt.position as i64,
                    prompt.content,
                    serde_json::to_string(&prompt.attachments)?,
                    tools_enabled,
                    prompt.created_at,
                ],
            )?;
            if let Some(claim_prompt_id) = claim_prompt_id {
                let claimed = conn.execute(
                    "UPDATE queued_prompts SET claimed = 1
                     WHERE id = ?1 AND thread_id = ?2 AND claimed = 0",
                    params![claim_prompt_id, prompt.thread_id],
                )?;
                anyhow::ensure!(
                    claimed == 1,
                    "queued prompt changed while accepting message"
                );
            }
            if let Some(expected_previous_turn) = expected_previous_turn {
                let updated = conn.execute(
                    "UPDATE threads SET last_turn = last_turn + 1
                     WHERE id = ?1 AND last_turn = ?2",
                    params![prompt.thread_id, *expected_previous_turn as i64],
                )?;
                anyhow::ensure!(updated == 1, "turn changed while accepting message");
            }
            if let Some(claim) = staging_cleanup_claim {
                let deleted = conn.execute(
                    "DELETE FROM artifact_cleanup_jobs WHERE id = ?1 AND claim_token = ?2",
                    params![claim.id, claim.token],
                )?;
                anyhow::ensure!(
                    deleted == 1,
                    "attachment staging claim {} is no longer owned",
                    claim.id
                );
            }
        }
        StoreMutation::AppendCheckpoint { checkpoint } => {
            append_checkpoint_row(conn, checkpoint, timestamp)?;
        }
        StoreMutation::AppendMessage {
            thread_id,
            payload,
            attachments,
            staging_cleanup_claim,
        } => {
            for (attachment, path) in attachments {
                conn.execute(
                    "INSERT INTO attachments
                       (id, thread_id, name, mime, size_bytes, path, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        attachment.id,
                        thread_id,
                        attachment.name,
                        attachment.mime,
                        attachment.size_bytes as i64,
                        path,
                        timestamp.to_rfc3339(),
                    ],
                )?;
            }
            conn.execute(
                "INSERT INTO messages (thread_id, seq, payload)
                 VALUES (
                     ?1,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE thread_id = ?1),
                     ?2
                 )",
                params![thread_id, payload],
            )?;
            if let Some(claim) = staging_cleanup_claim {
                let deleted = conn.execute(
                    "DELETE FROM artifact_cleanup_jobs WHERE id = ?1 AND claim_token = ?2",
                    params![claim.id, claim.token],
                )?;
                anyhow::ensure!(
                    deleted == 1,
                    "attachment staging claim {} is no longer owned",
                    claim.id
                );
            }
        }
    }
    Ok(true)
}

fn code_review_outbox_rows_exist(conn: &Connection, ids: &[i64]) -> rusqlite::Result<bool> {
    if ids.is_empty() {
        return Ok(true);
    }
    let mut stmt = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM code_review_pending_events WHERE id = ?1)")?;
    for id in ids {
        if !stmt.query_row([id], |row| row.get::<_, bool>(0))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn insert_event_batch<'a>(
    conn: &Connection,
    batch: impl IntoIterator<Item = &'a PendingEvent>,
    event_count: usize,
    code_review_outbox_ids: impl IntoIterator<Item = i64>,
) -> Result<InsertedEventBatch> {
    let code_review_outbox_ids = code_review_outbox_ids.into_iter().collect::<Vec<_>>();
    if !code_review_outbox_rows_exist(conn, &code_review_outbox_ids)? {
        return Ok(InsertedEventBatch {
            skipped: true,
            source_cursors: Vec::new(),
            published: Vec::new(),
        });
    }
    let tx = write_transaction(conn)?;
    if !code_review_outbox_rows_exist(&tx, &code_review_outbox_ids)? {
        return Ok(InsertedEventBatch {
            skipped: true,
            source_cursors: Vec::new(),
            published: Vec::new(),
        });
    }
    let mut source_cursors = Vec::with_capacity(event_count);
    let mut published = Vec::with_capacity(event_count.saturating_mul(2));
    let mut thread_events = Vec::new();
    for event in batch {
        if let Some(mutation) = event.mutation.as_ref()
            && !apply_store_mutation(&tx, mutation, event.ts)?
        {
            return Ok(InsertedEventBatch {
                skipped: true,
                source_cursors: Vec::new(),
                published: Vec::new(),
            });
        }
        let (kind, id) = scope_cols(&event.scope);
        tx.execute(
            "INSERT INTO events (scope_kind, scope_id, ts, payload) VALUES (?1, ?2, ?3, ?4)",
            params![kind, id, event.ts.to_rfc3339(), event.payload],
        )?;
        let source_cursor = tx.last_insert_rowid() as u64;
        source_cursors.push(source_cursor);
        let source = EventEnvelope {
            cursor: source_cursor,
            scope: event.scope.clone(),
            ts: event.ts,
            event: event.event.clone(),
        };
        if let Scope::Thread(thread_id) = &event.scope {
            thread_events.push((thread_id.clone(), source.clone()));
        }
        published.push(source);

        if let Some(change) =
            project_session_summary(&tx, &event.scope, &event.event, source_cursor, event.ts)?
        {
            let derived = Event::SessionSummaryUpdated {
                session_id: change.session_id,
                summary: change.summary,
            };
            let payload = serde_json::to_string(&derived)?;
            tx.execute(
                "INSERT INTO events (scope_kind, scope_id, ts, payload)
                 VALUES ('server', '', ?1, ?2)",
                params![event.ts.to_rfc3339(), payload],
            )?;
            published.push(EventEnvelope {
                cursor: tx.last_insert_rowid() as u64,
                scope: Scope::Server,
                ts: event.ts,
                event: derived,
            });
            if let Some(notification) = change.notification {
                let payload = serde_json::to_string(&notification)?;
                tx.execute(
                    "INSERT INTO events (scope_kind, scope_id, ts, payload)
                     VALUES ('server', '', ?1, ?2)",
                    params![event.ts.to_rfc3339(), payload],
                )?;
                published.push(EventEnvelope {
                    cursor: tx.last_insert_rowid() as u64,
                    scope: Scope::Server,
                    ts: event.ts,
                    event: notification,
                });
            }
        }
        if let Some(change) = project_thread_status(
            &tx,
            &event.scope,
            &event.event,
            source_cursor,
            &event.ts.to_rfc3339(),
        )? {
            for status in change.statuses {
                let derived = Event::ThreadStatusUpdated { status };
                let payload = serde_json::to_string(&derived)?;
                tx.execute(
                    "INSERT INTO events (scope_kind, scope_id, ts, payload)
                     VALUES ('server', '', ?1, ?2)",
                    params![event.ts.to_rfc3339(), payload],
                )?;
                published.push(EventEnvelope {
                    cursor: tx.last_insert_rowid() as u64,
                    scope: Scope::Server,
                    ts: event.ts,
                    event: derived,
                });
            }
        }
    }
    {
        let mut stmt = tx.prepare_cached("DELETE FROM code_review_pending_events WHERE id = ?1")?;
        for id in code_review_outbox_ids {
            let deleted = stmt.execute([id])?;
            anyhow::ensure!(
                deleted == 1,
                "code review pending event {id} was concurrently consumed"
            );
        }
    }
    update_thread_view_caches(&tx, &thread_events)?;
    tx.commit()?;
    Ok(InsertedEventBatch {
        skipped: false,
        source_cursors,
        published,
    })
}

fn compact_tool_argument(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    const SUMMARY_KEYS: &[&str] = &[
        "tool",
        "toolName",
        "name",
        "arguments",
        "command",
        "cmd",
        "cwd",
        "query",
        "pattern",
        "url",
        "path",
        "file_path",
        "title",
        "repo",
        "offset",
        "limit",
        "line",
        "start_line",
        "end_line",
    ];
    if depth > 3 {
        return serde_json::Value::Null;
    }
    match value {
        serde_json::Value::String(value) => {
            let mut chars = value.chars();
            let mut summary = chars.by_ref().take(320).collect::<String>();
            if chars.next().is_some() {
                summary.push('…');
            }
            serde_json::Value::String(summary)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .take(4)
                .map(|value| compact_tool_argument(value, depth + 1))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .filter(|(key, _)| SUMMARY_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), compact_tool_argument(value, depth + 1)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn persist_materialized_thread_items(
    conn: &Connection,
    thread_id: &str,
    start: u64,
    items: Vec<MaterializedThreadItem>,
) -> Result<()> {
    for (offset, materialized) in items.into_iter().enumerate() {
        let item_index = start
            .checked_add(offset as u64)
            .context("thread-view item index overflow")?;
        let persisted = match materialized.item {
            ThreadViewItem::ToolCall {
                call_id,
                tool,
                args,
                status,
                result,
                duration_ms,
                ..
            } => {
                let encoded_args = serde_json::to_string(&args)?;
                let encoded_result = result.as_ref().map(serde_json::to_string).transpose()?;
                conn.execute(
                    "INSERT INTO thread_tool_details (thread_id, call_id, args, result)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(thread_id, call_id) DO UPDATE SET
                       args = excluded.args,
                       result = excluded.result",
                    params![thread_id, &call_id, encoded_args, encoded_result],
                )?;
                ThreadViewItem::ToolCall {
                    call_id,
                    tool,
                    args: compact_tool_argument(&args, 0),
                    details_deferred: true,
                    status,
                    result: None,
                    duration_ms,
                }
            }
            item => item,
        };
        conn.execute(
            "INSERT INTO thread_view_items (thread_id, item_index, item, turn_start)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(thread_id, item_index) DO UPDATE SET
               item = excluded.item,
               turn_start = excluded.turn_start",
            params![
                thread_id,
                i64::try_from(item_index).context("thread-view item index exceeds SQLite")?,
                serde_json::to_string(&persisted)?,
                i64::from(materialized.turn_start),
            ],
        )?;
    }
    Ok(())
}

fn load_materialized_thread_items(
    conn: &Connection,
    thread_id: &str,
    start: u64,
    end: u64,
) -> Result<Vec<ThreadViewItem>> {
    if start >= end {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT item FROM thread_view_items
         WHERE thread_id = ?1 AND item_index >= ?2 AND item_index < ?3
         ORDER BY item_index",
    )?;
    let rows = stmt.query_map(
        params![
            thread_id,
            i64::try_from(start).context("thread-view start exceeds SQLite")?,
            i64::try_from(end).context("thread-view end exceeds SQLite")?
        ],
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

fn materialized_thread_turn_boundary(
    conn: &Connection,
    thread_id: &str,
    at_or_before: u64,
) -> Result<Option<u64>> {
    conn.query_row(
        "SELECT item_index FROM thread_view_items
         WHERE thread_id = ?1 AND turn_start = 1 AND item_index <= ?2
         ORDER BY item_index DESC LIMIT 1",
        params![
            thread_id,
            i64::try_from(at_or_before).context("thread-view boundary exceeds SQLite")?
        ],
        |row| Ok(row.get::<_, i64>(0)? as u64),
    )
    .optional()
    .map_err(Into::into)
}

/// Expand a requested backward page to the beginning of its oldest turn.
/// The web renderer virtualizes complete turns; splitting one across pages
/// would mutate the visible row when the preceding page is prepended.
fn thread_view_page_start(
    conn: &Connection,
    thread_id: &str,
    requested_start: u64,
    materialized: u64,
    live_turn_starts: &[bool],
) -> Result<u64> {
    if requested_start == 0 {
        return Ok(0);
    }
    if requested_start >= materialized {
        let relative = usize::try_from(requested_start - materialized)
            .context("live thread-view boundary exceeds memory")?;
        if let Some(boundary) = live_turn_starts
            .get(..=relative.min(live_turn_starts.len().saturating_sub(1)))
            .and_then(|items| items.iter().rposition(|turn_start| *turn_start))
        {
            return Ok(materialized + boundary as u64);
        }
        if materialized == 0 {
            return Ok(0);
        }
        return Ok(
            materialized_thread_turn_boundary(conn, thread_id, materialized - 1)?.unwrap_or(0),
        );
    }
    Ok(materialized_thread_turn_boundary(conn, thread_id, requested_start)?.unwrap_or(0))
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
        let (item_start, completed_items) = projection.take_materializable_prefix();
        persist_materialized_thread_items(tx, &thread_id, item_start, completed_items)?;
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
                mutation: None,
            })
        })
        .collect()
}

fn serialize_lifecycle_events(
    events: Vec<(Scope, Event)>,
    mutation: StoreMutation,
) -> Result<Vec<PendingEvent>> {
    let now = chrono::Utc::now();
    let mut pending = events
        .into_iter()
        .map(|(scope, event)| {
            Ok(PendingEvent {
                scope,
                ts: now,
                payload: serde_json::to_string(&event)?,
                event,
                mutation: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let first = pending
        .first_mut()
        .context("a lifecycle mutation requires at least one source event")?;
    first.mutation = Some(mutation);
    Ok(pending)
}

/// Reserve SQLite's writer slot before a write transaction reads state.
///
/// The event log uses a dedicated connection. A deferred transaction could
/// therefore read a snapshot, lose the writer race to an event-log commit,
/// and fail its later write with `SQLITE_BUSY_SNAPSHOT` (reported as error
/// code 5 / "database is locked"). `IMMEDIATE` makes SQLite's busy handler
/// wait before the read, so every read-modify-write transaction sees the
/// snapshot it can commit.
fn write_transaction(conn: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&mut conn)?;
        // Claims belong to dispatcher tasks in this process. After a crash
        // there is no worker to own them, so make the prompts visible and
        // explicitly dispatchable again instead of losing them.
        conn.execute(
            "UPDATE queued_prompts SET claimed = 0 WHERE claimed != 0",
            [],
        )?;
        let writer_conn = Connection::open(path)
            .with_context(|| format!("opening event-writer database {}", path.display()))?;
        writer_conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        writer_conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self::from_connections(conn, Some(writer_conn)))
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        // Match on-disk behavior so tests exercise the same constraints.
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        apply_migrations(&mut conn)?;
        conn.execute(
            "UPDATE queued_prompts SET claimed = 0 WHERE claimed != 0",
            [],
        )?;
        Ok(Self::from_connections(conn, None))
    }

    fn from_connections(conn: Connection, writer_conn: Option<Connection>) -> Self {
        let conn = Arc::new(Mutex::new(conn));
        let (events_tx, _) = broadcast::channel(4096);
        let scoped_events = Arc::new(Mutex::new(HashMap::new()));
        let append_tx = spawn_event_writer(
            writer_conn.map_or_else(
                || EventWriterConnection::Shared(Arc::clone(&conn)),
                EventWriterConnection::Dedicated,
            ),
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
        let mut envelopes = self.append_pending_events(serialize_events(scope, vec![event])?)?;
        Ok(envelopes.pop().expect("single append returns one event"))
    }

    /// Persist a same-scope batch synchronously. Use this for cancellation
    /// guards whose lifecycle must not be split after enqueue by dropping an
    /// async future.
    pub fn append_events(&self, scope: Scope, events: Vec<Event>) -> Result<Vec<EventEnvelope>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        self.append_pending_events(serialize_events(scope, events)?)
    }

    fn append_pending_events(&self, events: Vec<PendingEvent>) -> Result<Vec<EventEnvelope>> {
        let (reply, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.append_tx
            .send(AppendRequest {
                events,
                code_review_outbox_ids: Vec::new(),
                isolated: false,
                reply: AppendReply::Sync(reply),
                queued_at: std::time::Instant::now(),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
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
                code_review_outbox_ids: Vec::new(),
                isolated: false,
                reply: AppendReply::Async(reply),
                queued_at: std::time::Instant::now(),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
    }

    /// Persist a tool completion and the PR verification intents derived from
    /// it in one writer transaction. A crash can therefore leave neither an
    /// untracked successful creation nor an intent without its source event.
    pub(crate) async fn append_events_with_session_pr_verification_intents(
        &self,
        scope: Scope,
        events: Vec<Event>,
        intents: Vec<SessionPrVerificationIntent>,
    ) -> Result<Vec<EventEnvelope>> {
        if intents.is_empty() {
            return self.append_events_async(scope, events).await;
        }
        let pending = serialize_lifecycle_events(
            events
                .into_iter()
                .map(|event| (scope.clone(), event))
                .collect(),
            StoreMutation::UpsertSessionPrVerificationIntents { intents },
        )?;
        self.append_pending_events_async(pending).await
    }

    async fn append_pending_events_async(
        &self,
        events: Vec<PendingEvent>,
    ) -> Result<Vec<EventEnvelope>> {
        let isolated = events.iter().any(|event| {
            matches!(
                event.mutation.as_ref(),
                Some(StoreMutation::CompleteSessionPrVerificationIntent { .. })
            )
        });
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.append_tx
            .send(AppendRequest {
                events,
                code_review_outbox_ids: Vec::new(),
                isolated,
                reply: AppendReply::Async(reply),
                queued_at: std::time::Instant::now(),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
    }

    /// Sessions with verification work whose bounded backoff has elapsed.
    pub(crate) fn due_session_pr_verification_sessions(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id
             FROM session_pr_verification_intents
             WHERE next_attempt_at IS NULL OR next_attempt_at <= ?1
             GROUP BY session_id
             ORDER BY MIN(created_at)
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![chrono::Utc::now().to_rfc3339(), limit as i64],
            |row| row.get(0),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn due_session_pr_verification_intents(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionPrVerificationIntent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, host, owner, repository, pull_number, branch,
                    head_sha, attempts, last_failure_class, consecutive_failures, created_at
             FROM session_pr_verification_intents
             WHERE session_id = ?1
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?2)
             ORDER BY created_at, pull_number
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![session_id, chrono::Utc::now().to_rfc3339(), limit as i64],
            |row| {
                Ok(SessionPrVerificationIntent {
                    session_id: row.get(0)?,
                    host: row.get(1)?,
                    owner: row.get(2)?,
                    repository: row.get(3)?,
                    number: row.get::<_, i64>(4)? as u64,
                    branch: row.get(5)?,
                    head_sha: row.get(6)?,
                    attempts: row.get::<_, i64>(7)? as u32,
                    last_failure_class: row.get(8)?,
                    consecutive_failures: row.get::<_, i64>(9)? as u32,
                    created_at: row.get(10)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn session_pr_verification_retry_delay(attempts: u32) -> i64 {
        (1_i64 << attempts.min(15)).min(6 * 60 * 60)
    }

    /// Upgrade an intent written by the short-lived pending-evidence format.
    /// Matching the original empty tuple makes concurrent cleanup idempotent.
    pub(crate) fn set_session_pr_verification_evidence(
        &self,
        intent: &SessionPrVerificationIntent,
        branch: &str,
        head_sha: &str,
    ) -> Result<bool> {
        let updated = self.conn.lock().unwrap().execute(
            "UPDATE session_pr_verification_intents
             SET branch = ?8, head_sha = ?9, attempts = 0,
                 last_failure_class = '', consecutive_failures = 0,
                 next_attempt_at = NULL
             WHERE session_id = ?1 AND host = ?2 AND owner = ?3
               AND repository = ?4 AND pull_number = ?5
               AND branch = ?6 AND head_sha = ?7",
            params![
                intent.session_id,
                intent.host,
                intent.owner,
                intent.repository,
                intent.number as i64,
                intent.branch,
                intent.head_sha,
                branch,
                head_sha,
            ],
        )?;
        Ok(updated == 1)
    }

    /// Keep a transient failure durable while bounding GitHub request rate.
    /// The delay grows exponentially and caps at six hours so long outages do
    /// not turn durable nominations into sustained GitHub traffic.
    pub(crate) fn defer_session_pr_verification(
        &self,
        intent: &SessionPrVerificationIntent,
        failure_class: &str,
        count_request: bool,
        delay_seconds: i64,
    ) -> Result<()> {
        anyhow::ensure!(
            !failure_class.is_empty(),
            "verification failure class is empty"
        );
        anyhow::ensure!(
            delay_seconds > 0,
            "verification retry delay is not positive"
        );
        let next_attempt_at =
            (chrono::Utc::now() + chrono::Duration::seconds(delay_seconds)).to_rfc3339();
        self.conn.lock().unwrap().execute(
            "UPDATE session_pr_verification_intents
             SET attempts = attempts + ?8,
                 consecutive_failures = CASE
                   WHEN last_failure_class = ?9 THEN consecutive_failures + 1 ELSE 1 END,
                 last_failure_class = ?9,
                 next_attempt_at = ?10
             WHERE session_id = ?1 AND host = ?2 AND owner = ?3
               AND repository = ?4 AND pull_number = ?5
               AND branch = ?6 AND head_sha = ?7",
            params![
                intent.session_id,
                intent.host,
                intent.owner,
                intent.repository,
                intent.number as i64,
                intent.branch,
                intent.head_sha,
                i64::from(count_request),
                failure_class,
                next_attempt_at,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn discard_session_pr_verification(
        &self,
        intent: &SessionPrVerificationIntent,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM session_pr_verification_intents
             WHERE session_id = ?1 AND host = ?2 AND owner = ?3
               AND repository = ?4 AND pull_number = ?5
               AND branch = ?6 AND head_sha = ?7",
            params![
                intent.session_id,
                intent.host,
                intent.owner,
                intent.repository,
                intent.number as i64,
                intent.branch,
                intent.head_sha,
            ],
        )?;
        Ok(())
    }

    /// Remove an intent and append its association event atomically. The
    /// evidence predicates make a concurrent replacement or completion an
    /// idempotent no-op instead of publishing a duplicate association.
    pub(crate) async fn complete_session_pr_verification(
        &self,
        intent: SessionPrVerificationIntent,
        event: Event,
    ) -> Result<Option<EventEnvelope>> {
        let pending = serialize_lifecycle_events(
            vec![(Scope::Session(intent.session_id.clone()), event)],
            StoreMutation::CompleteSessionPrVerificationIntent {
                intent: Box::new(intent),
            },
        )?;
        Ok(self.append_pending_events_async(pending).await?.pop())
    }

    /// Persist one event without blocking a Tokio worker thread, while
    /// retaining the same commit-before-return guarantee as `append_event`.
    pub async fn append_event_async(&self, scope: Scope, event: Event) -> Result<EventEnvelope> {
        let mut envelopes = self.append_events_async(scope, vec![event]).await?;
        Ok(envelopes.pop().expect("single append returns one event"))
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
        turn_aligned: bool,
    ) -> Result<(u64, ThreadViewSnapshot)> {
        self.thread_view_snapshot_with_retries(
            thread_id,
            before,
            limit,
            turn_aligned,
            THREAD_VIEW_CACHE_RACE_RETRIES,
        )
    }

    fn thread_view_snapshot_with_retries(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
        turn_aligned: bool,
        retries_remaining: usize,
    ) -> Result<(u64, ThreadViewSnapshot)> {
        let (mut projection, cache_valid, observed_cache, rows) = {
            let conn = self.conn.lock().unwrap();
            let cached = conn
                .query_row(
                    "SELECT cursor, schema_version, state FROM thread_view_cache
                     WHERE thread_id = ?1",
                    params![thread_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)? as u64,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            let observed_cache = cached
                .as_ref()
                .map(|(cursor, version, _)| (*cursor, *version));
            let mut cache_valid = false;
            let projection: ThreadProjection = cached
                .and_then(|(_, version, state)| {
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
                observed_cache,
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
        let (item_start, completed_items) = projection.take_materializable_prefix();
        if needs_write {
            projection.snapshot.item_offset = 0;
            projection.snapshot.total_items = 0;
            projection.snapshot.has_older = false;
            let state = serde_json::to_string(&projection)?;
            let conn = self.conn.lock().unwrap();
            let tx = write_transaction(&conn)?;
            let current_cache = tx
                .query_row(
                    "SELECT cursor, schema_version FROM thread_view_cache WHERE thread_id = ?1",
                    params![thread_id],
                    |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let cache_advanced = current_cache != observed_cache
                && current_cache.is_some_and(|(cursor, version)| {
                    cursor > projection.cursor
                        || (cursor == projection.cursor && version == THREAD_VIEW_SCHEMA_VERSION)
                });
            if cache_advanced {
                tx.commit()?;
                drop(conn);
                anyhow::ensure!(
                    retries_remaining > 0,
                    "thread-view cache kept advancing while building snapshot"
                );
                return self.thread_view_snapshot_with_retries(
                    thread_id,
                    before,
                    limit,
                    turn_aligned,
                    retries_remaining - 1,
                );
            }
            if !cache_valid {
                tx.execute(
                    "DELETE FROM thread_view_items WHERE thread_id = ?1",
                    params![thread_id],
                )?;
                tx.execute(
                    "DELETE FROM thread_tool_details WHERE thread_id = ?1",
                    params![thread_id],
                )?;
            }
            persist_materialized_thread_items(&tx, thread_id, item_start, completed_items)?;
            tx.execute(
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
            tx.commit()?;
        }
        let cursor = projection.cursor;
        let materialized = projection.materialized_items();
        let total = projection.total_items();
        let end = before.unwrap_or(total).min(total);
        let requested_start = end.saturating_sub(u64::try_from(limit.max(1)).unwrap_or(u64::MAX));
        let (start, mut items) = {
            let conn = self.conn.lock().unwrap();
            let start = if turn_aligned {
                thread_view_page_start(
                    &conn,
                    thread_id,
                    requested_start,
                    materialized,
                    projection.live_turn_starts(),
                )?
            } else {
                requested_start
            };
            let items = load_materialized_thread_items(
                &conn,
                thread_id,
                start.min(materialized),
                end.min(materialized),
            )?;
            (start, items)
        };
        if end > materialized {
            let tail_start = start.saturating_sub(materialized) as usize;
            let tail_end = end.saturating_sub(materialized) as usize;
            items.extend_from_slice(&projection.snapshot.items[tail_start..tail_end]);
        }
        anyhow::ensure!(
            items.len() as u64 == end.saturating_sub(start),
            "materialized thread-view page is not contiguous"
        );
        let mut snapshot = projection.snapshot;
        snapshot.items = items;
        snapshot.item_offset = start;
        snapshot.total_items = total;
        snapshot.has_older = start > 0;
        Ok((cursor, snapshot))
    }

    /// Load the full payload for one completed historical tool call. The
    /// thread event log remains authoritative; this row is a rebuildable
    /// projection used only to avoid embedding large payloads in every page.
    pub fn thread_tool_details(
        &self,
        thread_id: &str,
        call_id: &str,
    ) -> Result<Option<ThreadToolDetails>> {
        let conn = self.conn.lock().unwrap();
        let encoded = conn
            .query_row(
                "SELECT args, result FROM thread_tool_details
                 WHERE thread_id = ?1 AND call_id = ?2",
                params![thread_id, call_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        encoded
            .map(|(args, result)| {
                Ok(ThreadToolDetails {
                    call_id: call_id.to_string(),
                    args: serde_json::from_str(&args)?,
                    result: result
                        .map(|result| serde_json::from_str(&result))
                        .transpose()?,
                })
            })
            .transpose()
    }

    /// Atomic session-summary snapshot. The cursor and rows share one SQLite
    /// read transaction, so an update is either present in both or replayed
    /// from `/v1/events` after this cursor—never missed between two reads.
    pub fn session_summaries_snapshot(&self) -> Result<SessionSummariesSnapshot> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        let cursor = tx
            .query_row(
                "SELECT MAX(cursor) FROM events
                 WHERE scope_kind = 'server' AND scope_id = ''",
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .unwrap_or(0) as u64;
        let session_ids = {
            let mut stmt = tx.prepare(
                "SELECT session_id FROM session_summaries
                 ORDER BY archived, updated_at DESC, session_id",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut summaries = Vec::with_capacity(session_ids.len());
        for session_id in session_ids {
            if let Some(summary) = session_summary(&tx, &session_id)? {
                summaries.push(summary);
            }
        }
        tx.commit()?;
        Ok(SessionSummariesSnapshot { summaries, cursor })
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

    /// Most recently persisted account PR snapshot event for `host`.
    ///
    /// The scan runs newest-first in bounded pages and stops at the first
    /// matching host. Payloads are decoded after releasing the SQLite
    /// connection mutex so a cold or missing host cannot block unrelated
    /// store operations for the full server history.
    pub fn latest_github_pr_snapshot_event(&self, host: &str) -> Result<Option<EventEnvelope>> {
        const PAGE_SIZE: usize = 64;

        let mut before = i64::MAX;
        loop {
            let page = {
                let conn = self.conn.lock().unwrap();
                let mut stmt = conn.prepare(
                    "SELECT cursor, ts, payload FROM events
                     WHERE scope_kind = 'server' AND scope_id = '' AND cursor < ?1
                     ORDER BY cursor DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![before, PAGE_SIZE as i64], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                let mut page = Vec::new();
                for row in rows {
                    page.push(row?);
                }
                page
            };
            let Some((oldest, _, _)) = page.last() else {
                return Ok(None);
            };
            before = *oldest;
            let exhausted = page.len() < PAGE_SIZE;
            for (cursor, ts, payload) in page {
                let Ok(event) = serde_json::from_str::<Event>(&payload) else {
                    continue;
                };
                let Event::GithubPullRequestsUpdated { pull_requests } = &event else {
                    continue;
                };
                if pull_requests.host.eq_ignore_ascii_case(host) {
                    return Ok(Some(EventEnvelope {
                        cursor: cursor as u64,
                        scope: Scope::Server,
                        ts: ts.parse().unwrap_or_else(|_| chrono::Utc::now()),
                        event,
                    }));
                }
            }
            if exhausted {
                return Ok(None);
            }
        }
    }

    /// Most recently persisted account PR replacement payload for `host`.
    pub fn latest_github_pr_snapshot(&self, host: &str) -> Result<Option<GithubPrList>> {
        Ok(self
            .latest_github_pr_snapshot_event(host)?
            .and_then(|envelope| match envelope.event {
                Event::GithubPullRequestsUpdated { pull_requests } => Some(pull_requests),
                _ => None,
            }))
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
        insert_session_row(&self.conn.lock().unwrap(), s)
    }

    /// Persist a newly created session, its pristine checkpoint, and every
    /// initial lifecycle event in the event writer's one transaction.
    pub(crate) fn insert_session_with_lifecycle(
        &self,
        session: &Session,
        initial_checkpoint: &CheckpointRow,
        events: Vec<(Scope, Event)>,
    ) -> Result<Vec<EventEnvelope>> {
        self.append_pending_events(serialize_lifecycle_events(
            events,
            StoreMutation::Insert {
                session: Box::new(session.clone()),
                initial_checkpoint: Box::new(initial_checkpoint.clone()),
            },
        )?)
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
        update_session_row(&self.conn.lock().unwrap(), id, title, archived)
    }

    /// Rename/archive and append the lifecycle source event atomically.
    pub(crate) fn update_session_with_event(
        &self,
        id: &str,
        title: Option<&str>,
        archived: Option<bool>,
        event: Event,
    ) -> Result<EventEnvelope> {
        let pending = serialize_lifecycle_events(
            vec![(Scope::Server, event)],
            StoreMutation::Update {
                id: id.to_string(),
                title: title.map(str::to_owned),
                archived,
            },
        )?;
        Ok(self
            .append_pending_events(pending)?
            .pop()
            .expect("one lifecycle event returns one envelope"))
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        // One transaction so a failure can't leave a half-deleted session.
        let tx = write_transaction(&conn)?;
        delete_session_rows(&tx, id)?;
        tx.commit()?;
        Ok(())
    }

    /// Delete relational/session-scoped state and append the server tombstone
    /// source event atomically before any filesystem cleanup begins.
    pub(crate) fn delete_session_with_event(
        &self,
        id: &str,
        cleanup: ArtifactCleanupJob,
        event: Event,
    ) -> Result<EventEnvelope> {
        let pending = serialize_lifecycle_events(
            vec![(Scope::Server, event)],
            StoreMutation::Delete {
                id: id.to_string(),
                cleanup: Box::new(cleanup),
            },
        )?;
        Ok(self
            .append_pending_events(pending)?
            .pop()
            .expect("one lifecycle event returns one envelope"))
    }

    pub(crate) fn stage_attachment_cleanup(
        &self,
        attachment_paths: Vec<String>,
    ) -> Result<Option<ArtifactCleanupJob>> {
        if attachment_paths.is_empty() {
            return Ok(None);
        }
        let mut job = ArtifactCleanupJob::attachments(attachment_paths);
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        insert_artifact_cleanup_job(&tx, &job, chrono::Utc::now())?;
        // The preparing request owns this job. A crashed request releases it
        // automatically when the bounded lease expires.
        let claim_until = (chrono::Utc::now()
            + chrono::Duration::minutes(ARTIFACT_CLEANUP_CLAIM_MINUTES))
        .to_rfc3339();
        let claim_token = uuid::Uuid::new_v4().simple().to_string();
        tx.execute(
            "UPDATE artifact_cleanup_jobs
             SET claim_until = ?2, claim_token = ?3 WHERE id = ?1",
            params![job.id, claim_until, claim_token],
        )?;
        tx.commit()?;
        job.claim_token = Some(claim_token);
        Ok(Some(job))
    }

    pub(crate) fn claim_artifact_cleanup_job(
        &self,
        id: &str,
    ) -> Result<Option<ArtifactCleanupJob>> {
        self.claim_artifact_cleanup_job_where(Some(id))
    }

    pub(crate) fn claim_next_artifact_cleanup_job(&self) -> Result<Option<ArtifactCleanupJob>> {
        self.claim_artifact_cleanup_job_where(None)
    }

    fn claim_artifact_cleanup_job_where(
        &self,
        requested_id: Option<&str>,
    ) -> Result<Option<ArtifactCleanupJob>> {
        let conn = self.conn.lock().unwrap();
        let probe_now = chrono::Utc::now().to_rfc3339();
        if !artifact_cleanup_job_is_claimable(&conn, requested_id, &probe_now)? {
            return Ok(None);
        }
        let tx = write_transaction(&conn)?;
        let now_at = chrono::Utc::now();
        let now = now_at.to_rfc3339();
        for _ in 0..MAX_POISONED_ARTIFACT_CLEANUP_ROWS_PER_CLAIM {
            let raw: Option<RawArtifactCleanupJob> = if let Some(id) = requested_id {
                tx.query_row(
                    "SELECT rowid,
                            CASE WHEN typeof(id) = 'text' AND length(CAST(id AS BLOB)) <= 128 THEN id END,
                            CASE WHEN typeof(kind) = 'text' AND length(CAST(kind AS BLOB)) <= ?3 THEN kind END,
                            CASE WHEN session_id IS NULL OR (typeof(session_id) = 'text' AND length(CAST(session_id AS BLOB)) <= ?3) THEN session_id END,
                            CASE WHEN worktree_path IS NULL OR (typeof(worktree_path) = 'text' AND length(CAST(worktree_path AS BLOB)) <= ?3) THEN worktree_path END,
                            CASE WHEN repository_path IS NULL OR (typeof(repository_path) = 'text' AND length(CAST(repository_path AS BLOB)) <= ?3) THEN repository_path END,
                            CASE WHEN typeof(attachment_paths) = 'text' AND length(CAST(attachment_paths AS BLOB)) <= ?4 THEN attachment_paths END,
                            CASE WHEN typeof(id) = 'text' AND length(CAST(id AS BLOB)) <= 128
                                   AND typeof(kind) = 'text' AND length(CAST(kind AS BLOB)) <= ?3
                                   AND (session_id IS NULL OR (typeof(session_id) = 'text' AND length(CAST(session_id AS BLOB)) <= ?3))
                                   AND (worktree_path IS NULL OR (typeof(worktree_path) = 'text' AND length(CAST(worktree_path AS BLOB)) <= ?3))
                                   AND (repository_path IS NULL OR (typeof(repository_path) = 'text' AND length(CAST(repository_path AS BLOB)) <= ?3))
                                   AND typeof(attachment_paths) = 'text' AND length(CAST(attachment_paths AS BLOB)) <= ?4
                                   AND typeof(attempts) = 'integer' AND attempts >= 0
                                   AND (claim_until IS NULL OR (typeof(claim_until) = 'text'
                                        AND length(CAST(claim_until AS BLOB)) <= ?5
                                        AND instr(claim_until, 'T') = 11
                                        AND julianday(claim_until) IS NOT NULL))
                                   AND (next_attempt_at IS NULL OR (typeof(next_attempt_at) = 'text'
                                        AND length(CAST(next_attempt_at AS BLOB)) <= ?5
                                        AND instr(next_attempt_at, 'T') = 11
                                        AND julianday(next_attempt_at) IS NOT NULL))
                                 THEN 1 ELSE 0 END
                     FROM artifact_cleanup_jobs
                     WHERE id = ?1
                       AND (claim_until IS NULL
                            OR typeof(claim_until) != 'text'
                            OR length(CAST(claim_until AS BLOB)) > ?5
                            OR instr(claim_until, 'T') != 11
                            OR julianday(claim_until) IS NULL
                            OR julianday(claim_until) <= julianday(?2))
                       AND (next_attempt_at IS NULL
                            OR typeof(next_attempt_at) != 'text'
                            OR length(CAST(next_attempt_at AS BLOB)) > ?5
                            OR instr(next_attempt_at, 'T') != 11
                            OR julianday(next_attempt_at) IS NULL
                            OR julianday(next_attempt_at) <= julianday(?2))",
                    params![id, now, MAX_ARTIFACT_CLEANUP_METADATA_BYTES as i64, MAX_ARTIFACT_CLEANUP_PATHS_JSON_BYTES as i64, MAX_ARTIFACT_CLEANUP_TIMESTAMP_BYTES as i64],
                    raw_artifact_cleanup_job,
                )
                .optional()?
            } else {
                tx.query_row(
                    "SELECT rowid,
                            CASE WHEN typeof(id) = 'text' AND length(CAST(id AS BLOB)) <= 128 THEN id END,
                            CASE WHEN typeof(kind) = 'text' AND length(CAST(kind AS BLOB)) <= ?2 THEN kind END,
                            CASE WHEN session_id IS NULL OR (typeof(session_id) = 'text' AND length(CAST(session_id AS BLOB)) <= ?2) THEN session_id END,
                            CASE WHEN worktree_path IS NULL OR (typeof(worktree_path) = 'text' AND length(CAST(worktree_path AS BLOB)) <= ?2) THEN worktree_path END,
                            CASE WHEN repository_path IS NULL OR (typeof(repository_path) = 'text' AND length(CAST(repository_path AS BLOB)) <= ?2) THEN repository_path END,
                            CASE WHEN typeof(attachment_paths) = 'text' AND length(CAST(attachment_paths AS BLOB)) <= ?3 THEN attachment_paths END,
                            CASE WHEN typeof(id) = 'text' AND length(CAST(id AS BLOB)) <= 128
                                   AND typeof(kind) = 'text' AND length(CAST(kind AS BLOB)) <= ?2
                                   AND (session_id IS NULL OR (typeof(session_id) = 'text' AND length(CAST(session_id AS BLOB)) <= ?2))
                                   AND (worktree_path IS NULL OR (typeof(worktree_path) = 'text' AND length(CAST(worktree_path AS BLOB)) <= ?2))
                                   AND (repository_path IS NULL OR (typeof(repository_path) = 'text' AND length(CAST(repository_path AS BLOB)) <= ?2))
                                   AND typeof(attachment_paths) = 'text' AND length(CAST(attachment_paths AS BLOB)) <= ?3
                                   AND typeof(attempts) = 'integer' AND attempts >= 0
                                   AND (claim_until IS NULL OR (typeof(claim_until) = 'text'
                                        AND length(CAST(claim_until AS BLOB)) <= ?4
                                        AND instr(claim_until, 'T') = 11
                                        AND julianday(claim_until) IS NOT NULL))
                                   AND (next_attempt_at IS NULL OR (typeof(next_attempt_at) = 'text'
                                        AND length(CAST(next_attempt_at AS BLOB)) <= ?4
                                        AND instr(next_attempt_at, 'T') = 11
                                        AND julianday(next_attempt_at) IS NOT NULL))
                                 THEN 1 ELSE 0 END
                     FROM artifact_cleanup_jobs
                     WHERE (claim_until IS NULL
                            OR typeof(claim_until) != 'text'
                            OR length(CAST(claim_until AS BLOB)) > ?4
                            OR instr(claim_until, 'T') != 11
                            OR julianday(claim_until) IS NULL
                            OR julianday(claim_until) <= julianday(?1))
                       AND (next_attempt_at IS NULL
                            OR typeof(next_attempt_at) != 'text'
                            OR length(CAST(next_attempt_at AS BLOB)) > ?4
                            OR instr(next_attempt_at, 'T') != 11
                            OR julianday(next_attempt_at) IS NULL
                            OR julianday(next_attempt_at) <= julianday(?1))
                     ORDER BY created_at, id LIMIT 1",
                    params![now, MAX_ARTIFACT_CLEANUP_METADATA_BYTES as i64, MAX_ARTIFACT_CLEANUP_PATHS_JSON_BYTES as i64, MAX_ARTIFACT_CLEANUP_TIMESTAMP_BYTES as i64],
                    raw_artifact_cleanup_job,
                )
                .optional()?
            };
            let Some((
                rowid,
                id,
                kind,
                session_id,
                worktree_path,
                repository_path,
                paths_json,
                metadata_is_valid,
            )) = raw
            else {
                tx.commit()?;
                return Ok(None);
            };
            let decoded = if metadata_is_valid {
                id.zip(kind)
                    .zip(paths_json)
                    .ok_or_else(|| anyhow::anyhow!("cleanup metadata exceeds its decode limit"))
                    .and_then(|((id, kind), paths_json)| {
                        decode_artifact_cleanup_job(
                            id,
                            kind,
                            session_id,
                            worktree_path,
                            repository_path,
                            paths_json,
                        )
                    })
            } else {
                Err(anyhow::anyhow!(
                    "cleanup metadata has an invalid SQLite type or exceeds its decode limit"
                ))
            };
            let mut job = match decoded {
                Ok(job) => job,
                Err(error) => {
                    let error = format!("malformed durable cleanup intent: {error:#}");
                    let next_attempt_at = (now_at + chrono::Duration::minutes(10)).to_rfc3339();
                    tx.execute(
                        "UPDATE artifact_cleanup_jobs
                         SET attempts = CASE
                               WHEN typeof(attempts) = 'integer'
                                    AND attempts >= 0
                                    AND attempts < 9223372036854775807
                                 THEN attempts + 1 ELSE 1 END,
                             last_error = ?2,
                             next_attempt_at = ?3, claim_until = NULL, claim_token = NULL
                         WHERE rowid = ?1",
                        params![rowid, error, next_attempt_at],
                    )?;
                    tracing::warn!(cleanup_rowid = rowid, %error, "quarantined malformed artifact cleanup job");
                    if requested_id.is_some() {
                        tx.commit()?;
                        return Ok(None);
                    }
                    continue;
                }
            };
            let claim_until =
                (now_at + chrono::Duration::minutes(ARTIFACT_CLEANUP_CLAIM_MINUTES)).to_rfc3339();
            let claim_token = uuid::Uuid::new_v4().simple().to_string();
            let claimed = tx.execute(
                "UPDATE artifact_cleanup_jobs
                 SET claim_until = ?2, claim_token = ?3
                 WHERE id = ?1
                   AND (claim_until IS NULL OR julianday(claim_until) <= julianday(?4))
                   AND (next_attempt_at IS NULL OR julianday(next_attempt_at) <= julianday(?4))",
                params![&job.id, claim_until, claim_token, now],
            )?;
            if claimed != 1 {
                tx.commit()?;
                return Ok(None);
            }
            job.claim_token = Some(claim_token);
            tx.commit()?;
            return Ok(Some(job));
        }
        tx.commit()?;
        Ok(None)
    }

    pub(crate) fn renew_artifact_cleanup_claim(
        &self,
        claim: &ArtifactCleanupClaim,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let claim_until = (chrono::Utc::now()
            + chrono::Duration::minutes(ARTIFACT_CLEANUP_CLAIM_MINUTES))
        .to_rfc3339();
        Ok(conn.execute(
            "UPDATE artifact_cleanup_jobs SET claim_until = ?3
             WHERE id = ?1 AND claim_token = ?2",
            params![claim.id, claim.token, claim_until],
        )? == 1)
    }

    pub(crate) fn complete_claimed_artifact_cleanup_job(
        &self,
        claim: &ArtifactCleanupClaim,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM artifact_cleanup_jobs WHERE id = ?1 AND claim_token = ?2",
            params![claim.id, claim.token],
        )?;
        anyhow::ensure!(
            deleted == 1,
            "artifact cleanup claim {} is no longer owned",
            claim.id
        );
        Ok(())
    }

    pub(crate) fn fail_claimed_artifact_cleanup_job(
        &self,
        claim: &ArtifactCleanupClaim,
        error: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let attempts = conn
            .query_row(
                "SELECT CASE
                          WHEN typeof(attempts) = 'integer' AND attempts >= 0
                            THEN attempts END
                   FROM artifact_cleanup_jobs
                 WHERE id = ?1 AND claim_token = ?2",
                params![claim.id, claim.token],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .context("artifact cleanup claim is no longer owned")?
            .unwrap_or(0);
        let delay_seconds = match attempts {
            0 => 1,
            1 => 5,
            2 => 30,
            3 => 120,
            _ => 600,
        };
        let next_attempt_at =
            (chrono::Utc::now() + chrono::Duration::seconds(delay_seconds)).to_rfc3339();
        let updated = conn.execute(
            "UPDATE artifact_cleanup_jobs
             SET attempts = CASE
                   WHEN typeof(attempts) = 'integer'
                        AND attempts >= 0
                        AND attempts < 9223372036854775807
                     THEN attempts + 1 ELSE 1 END,
                 last_error = ?3,
                 next_attempt_at = ?4, claim_until = NULL, claim_token = NULL
             WHERE id = ?1 AND claim_token = ?2",
            params![claim.id, claim.token, error, next_attempt_at],
        )?;
        anyhow::ensure!(
            updated == 1,
            "artifact cleanup claim {} is no longer owned",
            claim.id
        );
        Ok(())
    }

    // --- threads ------------------------------------------------------------

    pub fn insert_thread(
        &self,
        t: &Thread,
        model_options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        insert_thread_row(&self.conn.lock().unwrap(), t, model_options)
    }

    /// Insert a thread (and optional spawn edge) together with its durable
    /// creation edge. Parent ownership is validated in the same transaction.
    pub(crate) fn insert_thread_with_event(
        &self,
        thread: &Thread,
        model_options: &serde_json::Map<String, serde_json::Value>,
        spawn: Option<(&str, &str)>,
        event: Event,
    ) -> Result<EventEnvelope> {
        let pending = serialize_lifecycle_events(
            vec![(Scope::Server, event)],
            StoreMutation::InsertThread {
                thread: Box::new(thread.clone()),
                model_options: model_options.clone(),
                spawn: spawn.map(|(parent, kind)| (parent.to_string(), kind.to_string())),
            },
        )?;
        Ok(self
            .append_pending_events(pending)?
            .pop()
            .expect("one lifecycle event returns one envelope"))
    }

    /// Insert a spawned thread and its parent edge in one transaction. A
    /// concurrent reader can therefore never cache the child as an ordinary
    /// root thread between the two writes.
    pub fn insert_spawned_thread(
        &self,
        thread: &Thread,
        model_options: &serde_json::Map<String, serde_json::Value>,
        parent: &str,
        kind: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        insert_thread_row(&tx, thread, model_options)?;
        tx.execute(
            "INSERT INTO spawned_threads (child_thread_id, parent_thread_id, kind)
             VALUES (?1, ?2, ?3)",
            params![thread.id, parent, kind],
        )?;
        tx.commit()?;
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

    pub fn list_thread_statuses(&self, session_id: &str) -> Result<Vec<ThreadStatus>> {
        let conn = self.conn.lock().unwrap();
        thread_statuses(&conn, session_id)
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
        update_thread_row(
            &self.conn.lock().unwrap(),
            id,
            mode,
            model,
            model_options,
            permission_mode,
        )
    }

    pub(crate) fn update_thread_with_event(
        &self,
        id: &str,
        mode: Option<&str>,
        model: Option<&str>,
        model_options: Option<&serde_json::Map<String, serde_json::Value>>,
        permission_mode: Option<PermissionMode>,
        event: Event,
    ) -> Result<EventEnvelope> {
        let pending = serialize_lifecycle_events(
            vec![(Scope::Server, event)],
            StoreMutation::UpdateThread {
                id: id.to_string(),
                mode: mode.map(str::to_owned),
                model: model.map(str::to_owned),
                model_options: model_options.cloned(),
                permission_mode,
            },
        )?;
        Ok(self
            .append_pending_events(pending)?
            .pop()
            .expect("one lifecycle event returns one envelope"))
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
    // tools): drives bounded recursive delegation, hierarchy projections,
    // and per-parent/per-tree concurrency caps.

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

    /// Every descendant below `parent`, loaded in one recursive query. `UNION`
    /// bounds corrupt cycles and deduplicates children with malformed legacy
    /// parentage instead of turning hierarchy reads into unbounded N+1 walks.
    pub fn spawned_descendants(&self, parent: &str) -> Result<Vec<Thread>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "WITH RECURSIVE descendant_ids(id) AS (
                 SELECT child_thread_id FROM spawned_threads WHERE parent_thread_id = ?1
                 UNION
                 SELECT spawned.child_thread_id
                 FROM spawned_threads spawned
                 JOIN descendant_ids parent ON spawned.parent_thread_id = parent.id
             )
             SELECT {THREAD_COLUMNS} FROM threads
             WHERE id IN (SELECT id FROM descendant_ids)
             ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![parent], row_to_thread)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Aggregate usage for a spawned subtree (the root plus all descendants)
    /// without one usage query per historical child.
    pub fn spawned_subtree_usage(&self, root: &str) -> Result<trouve_protocol::UsageSummary> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "WITH RECURSIVE subtree(id) AS (
                 SELECT ?1
                 UNION
                 SELECT spawned.child_thread_id
                 FROM spawned_threads spawned
                 JOIN subtree parent ON spawned.parent_thread_id = parent.id
             )
             SELECT COUNT(*),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cached_input_tokens), 0),
                    COALESCE(SUM(cost_usd), 0.0)
             FROM usage WHERE thread_id IN (SELECT id FROM subtree)",
            params![root],
            |row| {
                Ok(trouve_protocol::UsageSummary {
                    turns: row.get::<_, i64>(0)? as u64,
                    input_tokens: row.get::<_, i64>(1)? as u64,
                    output_tokens: row.get::<_, i64>(2)? as u64,
                    cached_input_tokens: row.get::<_, i64>(3)? as u64,
                    cost_usd: row.get(4)?,
                })
            },
        )
        .map_err(Into::into)
    }

    /// Failed descendants in one subtree. The root's own detailed error is
    /// folded from its event log by the caller; descendant status is enough to
    /// prevent a failed nested worker from being reported as overall success.
    pub fn failed_spawned_descendants(&self, root: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "WITH RECURSIVE descendant_ids(id) AS (
                 SELECT child_thread_id FROM spawned_threads WHERE parent_thread_id = ?1
                 UNION
                 SELECT spawned.child_thread_id
                 FROM spawned_threads spawned
                 JOIN descendant_ids parent ON spawned.parent_thread_id = parent.id
             )
             SELECT statuses.thread_id
             FROM thread_statuses statuses
             WHERE statuses.thread_id IN (SELECT id FROM descendant_ids)
               AND statuses.last_outcome = 'failed'
             ORDER BY statuses.thread_id",
        )?;
        let rows = stmt.query_map(params![root], |row| row.get(0))?;
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

    /// Persist prompt acceptance, its attachment index rows, the optional
    /// dispatcher claim/turn allocation, and every resulting durable event in
    /// the event writer's single transaction. Callers construct the visible
    /// queue and turn shell while holding the engine's queue/activity locks.
    pub(crate) fn accept_prompt_with_events(
        &self,
        acceptance: PromptAcceptance,
        events: Vec<(Scope, Event)>,
    ) -> Result<Vec<EventEnvelope>> {
        let PromptAcceptance {
            prompt,
            tools_enabled,
            attachments,
            claim_prompt_id,
            expected_previous_turn,
            staging_cleanup_claim,
        } = acceptance;
        let pending = serialize_lifecycle_events(
            events,
            StoreMutation::AcceptPrompt {
                prompt: Box::new(prompt),
                tools_enabled,
                attachments,
                claim_prompt_id,
                expected_previous_turn,
                staging_cleanup_claim,
            },
        )?;
        self.append_pending_events(pending)
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

    pub(crate) fn next_queued_prompt_position(&self, thread_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let position: i64 = conn.query_row(
            "SELECT COALESCE(MAX(position), 0) + 1 FROM queued_prompts WHERE thread_id = ?1",
            params![thread_id],
            |row| row.get(0),
        )?;
        Ok(position as u64)
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
    pub fn update_queued_prompt(
        &self,
        id: &str,
        content: &str,
        attachments: &[trouve_protocol::Attachment],
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let attachments_json = serde_json::to_string(attachments)?;
        let n = conn.execute(
            "UPDATE queued_prompts
             SET content = ?2, attachments = ?3
             WHERE id = ?1 AND claimed = 0",
            params![id, content, attachments_json],
        )?;
        Ok(n > 0)
    }

    /// Update a visible queued prompt while atomically indexing newly staged
    /// attachments and removing rows for attachments no longer retained.
    /// The returned paths are safe for filesystem cleanup only after commit.
    pub(crate) fn update_queued_prompt_attachments(
        &self,
        id: &str,
        content: &str,
        attachments: &[trouve_protocol::Attachment],
        added: &[(trouve_protocol::Attachment, String)],
        removed_ids: &[String],
        staging_cleanup_claim: Option<&ArtifactCleanupClaim>,
    ) -> Result<Option<Option<ArtifactCleanupJob>>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let Some(thread_id) = tx
            .query_row(
                "SELECT thread_id FROM queued_prompts WHERE id = ?1 AND claimed = 0",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };

        for (attachment, path) in added {
            tx.execute(
                "INSERT INTO attachments
                   (id, thread_id, name, mime, size_bytes, path, created_at)
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
        }
        tx.execute(
            "UPDATE queued_prompts SET content = ?2, attachments = ?3 WHERE id = ?1",
            params![id, content, serde_json::to_string(attachments)?],
        )?;

        let mut removed_paths = Vec::with_capacity(removed_ids.len());
        for attachment_id in removed_ids {
            let path = tx
                .query_row(
                    "SELECT path FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment_id, thread_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(path) = path {
                tx.execute(
                    "DELETE FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment_id, thread_id],
                )?;
                removed_paths.push(path);
            }
        }
        if let Some(claim) = staging_cleanup_claim {
            let deleted = tx.execute(
                "DELETE FROM artifact_cleanup_jobs WHERE id = ?1 AND claim_token = ?2",
                params![claim.id, claim.token],
            )?;
            anyhow::ensure!(
                deleted == 1,
                "attachment staging claim {} is no longer owned",
                claim.id
            );
        }
        let cleanup = if removed_paths.is_empty() {
            None
        } else {
            let job = ArtifactCleanupJob::attachments(removed_paths);
            insert_artifact_cleanup_job(&tx, &job, chrono::Utc::now())?;
            Some(job)
        };
        tx.commit()?;
        Ok(Some(cleanup))
    }

    pub fn delete_queued_prompt(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM queued_prompts WHERE id = ?1 AND claimed = 0",
            params![id],
        )?;
        Ok(n > 0)
    }

    /// Delete a visible queued prompt and every attachment-index row it owns
    /// in one transaction. Returns the thread id and committed file paths for
    /// post-transaction filesystem cleanup.
    pub(crate) fn delete_queued_prompt_attachments(
        &self,
        id: &str,
    ) -> Result<Option<(String, Option<ArtifactCleanupJob>)>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let Some((thread_id, attachments_json)) = tx
            .query_row(
                "SELECT thread_id, attachments FROM queued_prompts
                 WHERE id = ?1 AND claimed = 0",
                params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let attachments = parse_attachments(&attachments_json);
        let mut paths = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let path = tx
                .query_row(
                    "SELECT path FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment.id, thread_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(path) = path {
                tx.execute(
                    "DELETE FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment.id, thread_id],
                )?;
                paths.push(path);
            }
        }
        tx.execute(
            "DELETE FROM queued_prompts WHERE id = ?1 AND claimed = 0",
            params![id],
        )?;
        let cleanup = if paths.is_empty() {
            None
        } else {
            let job = ArtifactCleanupJob::attachments(paths);
            insert_artifact_cleanup_job(&tx, &job, chrono::Utc::now())?;
            Some(job)
        };
        tx.commit()?;
        Ok(Some((thread_id, cleanup)))
    }

    /// Apply a full new order. `ids` must be exactly the thread's current
    /// queue; returns false (changing nothing) when it isn't, so a reorder
    /// racing a dispatch fails cleanly instead of corrupting positions.
    pub fn reorder_queued_prompts(&self, thread_id: &str, ids: &[String]) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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

    /// Move one visible prompt to the front in a single transaction and,
    /// when requested, claim it for an idle dispatcher. Returns `None` when
    /// the prompt no longer exists or another dispatcher already claimed it.
    pub fn prioritize_queued_prompt(
        &self,
        id: &str,
        claim: bool,
    ) -> Result<Option<trouve_protocol::QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let Some(mut prompt) = tx
            .query_row(
                "SELECT thread_id, content, attachments, created_at
                 FROM queued_prompts WHERE id = ?1 AND claimed = 0",
                params![id],
                |row| {
                    Ok(trouve_protocol::QueuedPrompt {
                        id: id.to_string(),
                        thread_id: row.get(0)?,
                        position: 0,
                        content: row.get(1)?,
                        attachments: parse_attachments(&row.get::<_, String>(2)?),
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?
        else {
            return Ok(None);
        };

        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM queued_prompts
                 WHERE thread_id = ?1 AND claimed = 0 ORDER BY position",
            )?;
            let rows = stmt.query_map(params![prompt.thread_id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let ordered = std::iter::once(id.to_string())
            .chain(ids.into_iter().filter(|candidate| candidate != id));
        for (index, candidate) in ordered.enumerate() {
            tx.execute(
                "UPDATE queued_prompts SET position = ?2 WHERE id = ?1 AND claimed = 0",
                params![candidate, index as i64],
            )?;
        }
        if claim {
            tx.execute(
                "UPDATE queued_prompts SET claimed = 1 WHERE id = ?1 AND claimed = 0",
                params![id],
            )?;
        }
        tx.commit()?;
        prompt.position = 0;
        Ok(Some(prompt))
    }

    /// Hide and return the front prompt while a dispatcher prepares its
    /// durable turn start. The row is deleted only after the user message is
    /// persisted; setup failures release it back to the visible queue.
    pub fn claim_queued_prompt(
        &self,
        thread_id: &str,
    ) -> Result<Option<trouve_protocol::QueuedPrompt>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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

    /// Remove steering attachments that were indexed before the backend
    /// accepted the steering command. Restrict every id to the owning thread
    /// so a malformed rollback cannot detach another prompt's upload.
    pub fn remove_attachments(
        &self,
        thread_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let mut paths = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            let path = tx
                .query_row(
                    "SELECT path FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment_id, thread_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(path) = path {
                tx.execute(
                    "DELETE FROM attachments WHERE id = ?1 AND thread_id = ?2",
                    params![attachment_id, thread_id],
                )?;
                paths.push(path);
            }
        }
        tx.commit()?;
        Ok(paths)
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
                                      thinking_level, permission_mode, schedule, enabled,
                                      next_run_at, last_run_at, last_session_id, last_error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
                a.thinking_level,
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
                    model = ?6, thinking_level = ?7, permission_mode = ?8, schedule = ?9,
                    enabled = ?10, next_run_at = ?11
             WHERE id = ?1",
            params![
                a.id,
                a.name,
                a.prompt,
                a.workspace_id,
                a.mode,
                a.model,
                a.thinking_level,
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
            "SELECT id, name, prompt, workspace_id, mode, model, thinking_level, permission_mode,
                    schedule, enabled, next_run_at, last_run_at, last_session_id, last_error, created_at
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
                "SELECT id, name, prompt, workspace_id, mode, model, thinking_level, permission_mode,
                        schedule, enabled, next_run_at, last_run_at, last_session_id, last_error, created_at
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

    pub fn replace_claimed_reviewer_profile(
        &self,
        reviewer: &trouve_protocol::ReviewerProfile,
        claim_token: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO code_review_identities
                    (id, name, prompt, model, thinking_level, built_in, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, prompt = excluded.prompt, model = excluded.model,
               thinking_level = excluded.thinking_level, built_in = excluded.built_in,
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
        let deleted = tx.execute(
            "DELETE FROM persona_cleanup_intents
             WHERE persona_id = ?1 AND claim_token = ?2",
            params![reviewer.id, claim_token],
        )?;
        anyhow::ensure!(
            deleted == 1,
            "persona deletion claim for {} was lost",
            reviewer.id
        );
        tx.commit()?;
        Ok(())
    }

    pub fn delete_custom_reviewer_profile(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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

    /// Record a durable custom-persona deletion before filesystem work begins.
    /// Repository references deliberately remain unchanged until the executor
    /// confirms that the persona file was removed.
    pub fn begin_persona_deletion(&self, id: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT OR IGNORE INTO persona_cleanup_intents (persona_id, created_at)
             VALUES (?1, ?2)",
            params![id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn persona_deletion_pending(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM persona_cleanup_intents WHERE persona_id = ?1",
                [id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn claim_persona_deletion(&self, id: &str) -> Result<Option<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now();
        let token = uuid::Uuid::new_v4().to_string();
        let claimed = tx.execute(
            "UPDATE persona_cleanup_intents
             SET claim_until = ?2, claim_token = ?3
             WHERE persona_id = ?1
               AND (claim_token IS NULL OR claim_until IS NULL OR claim_until <= ?4)",
            params![
                id,
                (now + chrono::Duration::minutes(5)).to_rfc3339(),
                token,
                now.to_rfc3339(),
            ],
        )?;
        tx.commit()?;
        Ok((claimed == 1).then_some(token))
    }

    pub fn release_persona_deletion_claim(&self, id: &str, token: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE persona_cleanup_intents
             SET claim_until = NULL, claim_token = NULL
             WHERE persona_id = ?1 AND claim_token = ?2",
            params![id, token],
        )?;
        Ok(())
    }

    pub fn renew_persona_deletion_claim(&self, id: &str, token: &str) -> Result<()> {
        let claim_until = (chrono::Utc::now()
            + chrono::Duration::minutes(PERSONA_DELETION_CLAIM_MINUTES))
        .to_rfc3339();
        let updated = self.conn.lock().unwrap().execute(
            "UPDATE persona_cleanup_intents SET claim_until = ?3
             WHERE persona_id = ?1 AND claim_token = ?2",
            params![id, token, claim_until],
        )?;
        anyhow::ensure!(updated == 1, "persona deletion claim for {id} was lost");
        Ok(())
    }

    /// Consume a pending deletion because the persona was recreated. Unlike
    /// deletion completion, repository selections and overrides are retained.
    pub fn cancel_persona_deletion(&self, id: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM persona_cleanup_intents WHERE persona_id = ?1",
            [id],
        )?;
        Ok(())
    }

    pub fn cancel_claimed_persona_deletion(&self, id: &str, token: &str) -> Result<()> {
        let deleted = self.conn.lock().unwrap().execute(
            "DELETE FROM persona_cleanup_intents
             WHERE persona_id = ?1 AND claim_token = ?2",
            params![id, token],
        )?;
        anyhow::ensure!(deleted == 1, "persona deletion claim for {id} was lost");
        Ok(())
    }

    pub fn pending_persona_deletions(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut statement = conn.prepare(
            "SELECT persona_id FROM persona_cleanup_intents ORDER BY created_at, persona_id",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub(crate) fn claim_next_persona_deletion(&self) -> Result<Option<PersonaDeletionClaim>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now_at = chrono::Utc::now();
        let now = now_at.to_rfc3339();
        let candidate = tx
            .query_row(
                "SELECT persona_id, attempts
                 FROM persona_cleanup_intents
                 WHERE (claim_until IS NULL OR julianday(claim_until) <= julianday(?1))
                   AND (next_attempt_at IS NULL OR julianday(next_attempt_at) <= julianday(?1))
                 ORDER BY created_at, persona_id
                 LIMIT 1",
                [&now],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((id, attempts)) = candidate else {
            tx.commit()?;
            return Ok(None);
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let claim_until =
            (now_at + chrono::Duration::minutes(PERSONA_DELETION_CLAIM_MINUTES)).to_rfc3339();
        let claimed = tx.execute(
            "UPDATE persona_cleanup_intents
             SET claim_until = ?2, claim_token = ?3
             WHERE persona_id = ?1
               AND (claim_until IS NULL OR julianday(claim_until) <= julianday(?4))
               AND (next_attempt_at IS NULL OR julianday(next_attempt_at) <= julianday(?4))",
            params![id, claim_until, token, now],
        )?;
        tx.commit()?;
        if claimed != 1 {
            return Ok(None);
        }
        Ok(Some(PersonaDeletionClaim {
            id,
            token,
            attempts,
        }))
    }

    pub(crate) fn fail_claimed_persona_deletion(&self, claim: &PersonaDeletionClaim) -> Result<()> {
        let delay_seconds = match claim.attempts {
            0 => 1,
            1 => 5,
            2 => 30,
            3 => 120,
            _ => 600,
        };
        let next_attempt_at =
            (chrono::Utc::now() + chrono::Duration::seconds(delay_seconds)).to_rfc3339();
        let updated = self.conn.lock().unwrap().execute(
            "UPDATE persona_cleanup_intents
             SET attempts = attempts + 1, next_attempt_at = ?3,
                 claim_until = NULL, claim_token = NULL
             WHERE persona_id = ?1 AND claim_token = ?2",
            params![claim.id, claim.token, next_attempt_at],
        )?;
        anyhow::ensure!(
            updated == 1,
            "persona deletion claim {} is no longer owned",
            claim.id
        );
        Ok(())
    }

    /// Remove a deleted custom persona from every repository and consume its
    /// durable intent atomically. Filesystem work has already completed and no
    /// executor call occurs while the SQLite connection is locked.
    pub fn complete_persona_deletion(&self, id: &str) -> Result<()> {
        self.complete_persona_deletion_with_claim(id, None)
    }

    pub(crate) fn complete_claimed_persona_deletion(
        &self,
        claim: &PersonaDeletionClaim,
    ) -> Result<()> {
        self.complete_persona_deletion_with_claim(&claim.id, Some(&claim.token))
    }

    pub(crate) fn complete_claimed_persona_deletion_token(
        &self,
        id: &str,
        token: &str,
    ) -> Result<()> {
        self.complete_persona_deletion_with_claim(id, Some(token))
    }

    fn complete_persona_deletion_with_claim(
        &self,
        id: &str,
        claim_token: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent_exists = match claim_token {
            Some(token) => tx
                .query_row(
                    "SELECT 1 FROM persona_cleanup_intents
                     WHERE persona_id = ?1 AND claim_token = ?2",
                    params![id, token],
                    |_| Ok(()),
                )
                .optional()?
                .is_some(),
            None => tx
                .query_row(
                    "SELECT 1 FROM persona_cleanup_intents
                     WHERE persona_id = ?1 AND claim_token IS NULL",
                    [id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some(),
        };
        anyhow::ensure!(intent_exists, "persona deletion intent for {id} is missing");
        let defaults = serde_json::to_string(&crate::reviewers::default_reviewer_ids())?;
        tx.execute(
            "UPDATE code_review_repositories SET
               identity_ids = CASE
                 WHEN EXISTS (SELECT 1 FROM json_each(identity_ids) WHERE value = ?1)
                 THEN CASE
                   WHEN EXISTS (SELECT 1 FROM json_each(identity_ids) WHERE value != ?1)
                   THEN (SELECT json_group_array(value) FROM json_each(identity_ids) WHERE value != ?1)
                   ELSE ?2
                 END
                 ELSE identity_ids
               END,
               included_reviewer_ids = (SELECT json_group_array(value) FROM json_each(included_reviewer_ids) WHERE value != ?1),
               excluded_reviewer_ids = (SELECT json_group_array(value) FROM json_each(excluded_reviewer_ids) WHERE value != ?1),
               reviewer_overrides = (SELECT json_group_array(value) FROM json_each(reviewer_overrides) WHERE json_extract(value, '$.reviewer_id') != ?1),
               updated_at = ?3
             WHERE EXISTS (SELECT 1 FROM json_each(identity_ids) WHERE value = ?1)
                OR EXISTS (SELECT 1 FROM json_each(included_reviewer_ids) WHERE value = ?1)
                OR EXISTS (SELECT 1 FROM json_each(excluded_reviewer_ids) WHERE value = ?1)
                OR EXISTS (SELECT 1 FROM json_each(reviewer_overrides) WHERE json_extract(value, '$.reviewer_id') = ?1)",
            params![id, defaults, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.execute("DELETE FROM code_review_identities WHERE id = ?1", [id])?;
        let deleted = match claim_token {
            Some(token) => tx.execute(
                "DELETE FROM persona_cleanup_intents
                 WHERE persona_id = ?1 AND claim_token = ?2",
                params![id, token],
            )?,
            None => tx.execute(
                "DELETE FROM persona_cleanup_intents
                 WHERE persona_id = ?1 AND claim_token IS NULL",
                [id],
            )?,
        };
        anyhow::ensure!(deleted == 1, "persona deletion intent for {id} is missing");
        tx.commit()?;
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
                     router_thinking_level, coordinator_thinking_level,
                     review_watermark_sha)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'queued',
                     ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                     ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?17)",
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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

    pub fn supersede_automatic_code_review_jobs_for_draft(
        &self,
        repository: &str,
        pull_number: u64,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM code_review_jobs
                 WHERE repository = ?1 AND pull_number = ?2
                   AND trigger = 'automatic'
                   AND status IN ('queued', 'running')
                   AND publication_claimed = 0
                 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(params![repository, pull_number as i64], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if !ids.is_empty() {
            tx.execute(
                "UPDATE code_review_jobs
                 SET status = 'stale', review_url = '',
                     error = 'pull request is a draft; automatic review stopped',
                     completed_at = ?3,
                     dedupe_key = 'draft-stale:' || id || ':' || dedupe_key
                 WHERE repository = ?1 AND pull_number = ?2
                   AND trigger = 'automatic'
                   AND status IN ('queued', 'running')
                   AND publication_claimed = 0",
                params![
                    repository,
                    pull_number as i64,
                    chrono::Utc::now().to_rfc3339(),
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let now = chrono::Utc::now().to_rfc3339();
        let interrupted_reviewers = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CODE_REVIEW_TASK_COLUMNS} FROM code_review_tasks
                 WHERE status IN ('queued', 'running') AND role = 'reviewer'
                   AND job_id IN (
                     SELECT id FROM code_review_jobs
                     WHERE status = 'running' AND publication_claimed = 0
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
                 model_started_at = NULL
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
             SET status = CASE
                     WHEN publication_accepted != 0 THEN 'succeeded'
                     ELSE 'failed'
                 END,
                 completed_at = ?1,
                 error = CASE
                     WHEN publication_accepted != 0 THEN ''
                     ELSE 'server restarted before review publication was accepted; retry required'
                 END,
                 publication_claimed = CASE
                     WHEN publication_accepted != 0 THEN publication_claimed
                     ELSE 0
                 END,
                 check_sync_error = CASE
                     WHEN publication_accepted != 0
                         THEN 'review publication requires reconciliation'
                     ELSE ''
                 END,
                 projection_retry_count = 0,
                 projection_retry_at = NULL,
                 projection_retryable = 1
             WHERE status = 'running' AND publication_claimed != 0",
            params![now],
        )?;
        tx.execute(
            "UPDATE code_review_jobs
             SET status = 'queued', started_at = NULL, cancel_requested = 0,
                 publication_claimed = 0,
                 error = 'server restarted while review was running'
             WHERE status = 'running' AND publication_claimed = 0",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn claim_code_review_job(&self) -> Result<Option<CodeReviewJobRecord>> {
        let conn = self.conn.lock().unwrap();
        let queued = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_review_jobs WHERE status = 'queued')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if !queued {
            return Ok(None);
        }
        let tx = write_transaction(&conn)?;
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
                 cancel_requested = 0, publication_claimed = 0,
                 publication_accepted = 0, error = ''
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

    /// Bind routing and reviewer attempts to the exact effective diff batches.
    /// A changed digest atomically clears routing and retires every task from
    /// the obsolete snapshot so crash recovery cannot mix generations.
    pub fn prepare_code_review_batch_snapshot(
        &self,
        job_id: &str,
        digest: &str,
    ) -> Result<CodeReviewBatchSnapshotUpdate> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let (status, current_digest): (String, String) = tx.query_row(
            "SELECT status, review_batch_digest FROM code_review_jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if status != "running" {
            anyhow::bail!("stale: review job {job_id} is no longer running");
        }
        if current_digest == digest {
            tx.commit()?;
            return Ok(CodeReviewBatchSnapshotUpdate { changed: false });
        }

        let artifact_count: i64 = tx.query_row(
            "SELECT
               (SELECT COUNT(*) FROM code_review_tasks WHERE job_id = ?1) +
               (SELECT COUNT(*) FROM code_review_routing_decisions WHERE job_id = ?1)",
            [job_id],
            |row| row.get(0),
        )?;
        let changed = !current_digest.is_empty() || artifact_count > 0;
        tx.execute(
            "UPDATE code_review_jobs
             SET review_batch_digest = ?2,
                 completed_reviewers = CASE WHEN ?3 THEN 0 ELSE completed_reviewers END
             WHERE id = ?1 AND status = 'running'",
            params![job_id, digest, changed],
        )?;
        if changed {
            let completed_at = chrono::Utc::now().to_rfc3339();
            let task_ids = {
                let mut stmt = tx.prepare(
                    "SELECT id FROM code_review_tasks
                     WHERE job_id = ?1 AND status != 'superseded'
                     ORDER BY created_at, rowid",
                )?;
                stmt.query_map([job_id], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            tx.execute(
                "DELETE FROM code_review_routing_decisions WHERE job_id = ?1",
                [job_id],
            )?;
            for task_id in task_ids {
                supersede_code_review_task(
                    &tx,
                    &task_id,
                    job_id,
                    &completed_at,
                    "effective review diff changed during recovery",
                )?;
            }
            enqueue_code_review_pending_event(
                &tx,
                job_id,
                &Event::CodeReviewRoutingUpdated {
                    job_id: job_id.to_owned(),
                    routing_decisions: Vec::new(),
                },
            )?;
            let total_reviewers = tx.query_row(
                "SELECT total_reviewers FROM code_review_jobs WHERE id = ?1",
                [job_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
            enqueue_code_review_pending_event(
                &tx,
                job_id,
                &Event::CodeReviewProgressUpdated {
                    job_id: job_id.to_owned(),
                    progress: trouve_protocol::CodeReviewProgress {
                        completed_reviewers: 0,
                        total_reviewers,
                        percent: 0,
                    },
                },
            )?;
        }
        tx.commit()?;
        Ok(CodeReviewBatchSnapshotUpdate { changed })
    }

    pub fn pending_code_review_events(&self, job_id: &str) -> Result<Vec<PendingCodeReviewEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, payload FROM code_review_pending_events
             WHERE job_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([job_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut pending = Vec::new();
        let mut invalid_ids = Vec::new();
        for row in rows {
            let (id, payload) = row?;
            match serde_json::from_str(&payload) {
                Ok(event) => pending.push(PendingCodeReviewEvent { id, event }),
                Err(error) => {
                    tracing::warn!(
                        job_id,
                        pending_event_id = id,
                        %error,
                        "discarding undeserializable pending code-review event"
                    );
                    invalid_ids.push(id);
                }
            }
        }
        drop(stmt);
        for id in invalid_ids {
            conn.execute("DELETE FROM code_review_pending_events WHERE id = ?1", [id])?;
        }
        Ok(pending)
    }

    /// Atomically project every valid pending transition into the durable
    /// event log and consume its outbox row. The dedicated writer publishes
    /// the committed envelopes only after both halves of the transaction
    /// succeed, so recovery cannot duplicate a transition with a new cursor.
    pub async fn flush_pending_code_review_events(
        &self,
        job_id: &str,
    ) -> Result<Vec<EventEnvelope>> {
        let pending = self.pending_code_review_events(job_id)?;
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::with_capacity(pending.len());
        let mut events = Vec::with_capacity(pending.len());
        for pending in pending {
            ids.push(pending.id);
            events.push(pending.event);
        }
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        self.append_tx
            .send(AppendRequest {
                events: serialize_events(Scope::CodeReviewJob(job_id.to_owned()), events)?,
                code_review_outbox_ids: ids,
                isolated: false,
                reply: AppendReply::Async(reply),
                queued_at: std::time::Instant::now(),
            })
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("event writer thread has exited"))?
    }

    pub fn code_review_jobs_with_pending_events(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT pending.job_id
             FROM code_review_pending_events pending
             JOIN code_review_jobs jobs ON jobs.id = pending.job_id
             WHERE jobs.status != 'running'
             GROUP BY pending.job_id ORDER BY MIN(pending.id) LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        Ok(completed_code_review_persona_count(&attempts))
    }

    /// Retire reviewer attempts whose fully rendered prompt no longer
    /// matches, and update progress in the same transaction so clients never
    /// observe the old completion count with replacement tasks pending.
    pub fn supersede_code_review_tasks_for_prompt_change(
        &self,
        job_id: &str,
        task_ids: &[String],
        total_reviewers: u64,
    ) -> Result<u64> {
        if task_ids.is_empty() {
            return self.completed_code_review_personas(job_id);
        }
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let running: bool = tx.query_row(
            "SELECT status = 'running' FROM code_review_jobs WHERE id = ?1",
            [job_id],
            |row| row.get(0),
        )?;
        if !running {
            anyhow::bail!("stale: review job {job_id} is no longer running");
        }
        let completed_at = chrono::Utc::now().to_rfc3339();
        for task_id in task_ids {
            supersede_code_review_task(
                &tx,
                task_id,
                job_id,
                &completed_at,
                "reviewer prompt changed during recovery",
            )?;
        }
        let attempts = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {CODE_REVIEW_TASK_COLUMNS},
                        rowid AS {CODE_REVIEW_TASK_INSERTION_ORDER_COLUMN}
                 FROM code_review_tasks WHERE job_id = ?1 ORDER BY rowid"
            ))?;
            stmt.query_map([job_id], row_to_code_review_task_attempt)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let completed_reviewers = completed_code_review_persona_count(&attempts);
        tx.execute(
            "UPDATE code_review_jobs
             SET completed_reviewers = ?2, total_reviewers = ?3
             WHERE id = ?1 AND status = 'running'",
            params![job_id, completed_reviewers as i64, total_reviewers as i64],
        )?;
        let percent = completed_reviewers
            .saturating_mul(100)
            .checked_div(total_reviewers)
            .map(|value| value.min(100) as u8)
            .unwrap_or(0);
        enqueue_code_review_pending_event(
            &tx,
            job_id,
            &Event::CodeReviewProgressUpdated {
                job_id: job_id.to_owned(),
                progress: trouve_protocol::CodeReviewProgress {
                    completed_reviewers,
                    total_reviewers,
                    percent,
                },
            },
        )?;
        tx.commit()?;
        Ok(completed_reviewers)
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        if old.publication_claimed {
            anyhow::bail!(
                "review publication may already exist; reconcile it instead of retrying reviewers"
            );
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
                 publication_accepted = 0,
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
                        (id, job_id, path, line, side, severity, confidence, title, body,
                         prompt_for_agents, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'open', ?11)",
                params![
                    finding_id,
                    job_id,
                    finding.path,
                    finding.line as i64,
                    finding.side,
                    finding.severity,
                    finding.confidence,
                    finding.title,
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
                         path, line, side, severity, confidence, title, body, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                    rejection.confidence,
                    rejection.title,
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
                    path, line, side, severity, confidence, title, body, reason
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
                    confidence: row.get(8)?,
                    title: row.get(9)?,
                    body: row.get(10)?,
                    reason: row.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Records a finding's published comment coordinates. A comment landing
    /// on an already-closed row re-arms its collapse: a concurrent round may
    /// have closed the finding from a snapshot taken before publication, and
    /// without re-arming that thread would never be collapsed. The arming
    /// transition (0 → 1) also resets retry metadata, so a freshly armed
    /// collapse never inherits backoff from an earlier, unrelated deferral.
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
                 github_publication_status = 'published',
                 github_thread_id = CASE
                     WHEN ?4 IS NOT NULL THEN ?4
                     WHEN github_comment_id IS ?2 THEN github_thread_id
                     ELSE NULL
                 END,
                 collapse_attempts = CASE
                     WHEN status != 'open' AND ?2 IS NOT NULL AND collapse_pending = 0 THEN 0
                     ELSE collapse_attempts
                 END,
                 collapse_next_attempt_at = CASE
                     WHEN status != 'open' AND ?2 IS NOT NULL AND collapse_pending = 0 THEN NULL
                     ELSE collapse_next_attempt_at
                 END,
                 collapse_pending = CASE
                     WHEN status != 'open' AND ?2 IS NOT NULL THEN 1
                     ELSE collapse_pending
                 END
             WHERE id = ?1",
            params![
                id,
                comment_id.map(|value| value as i64),
                comment_url,
                thread_id
            ],
        )? > 0)
    }

    pub fn set_code_review_finding_publication_status(
        &self,
        id: &str,
        status: trouve_protocol::CodeReviewFindingPublicationStatus,
    ) -> Result<bool> {
        Ok(self.set_code_review_findings_publication_status(&[id], status)? > 0)
    }

    pub fn set_code_review_findings_publication_status(
        &self,
        ids: &[&str],
        status: trouve_protocol::CodeReviewFindingPublicationStatus,
    ) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let status = match status {
            trouve_protocol::CodeReviewFindingPublicationStatus::Pending => "pending",
            trouve_protocol::CodeReviewFindingPublicationStatus::Published => "published",
            trouve_protocol::CodeReviewFindingPublicationStatus::NotEligible => "not_eligible",
            trouve_protocol::CodeReviewFindingPublicationStatus::SuppressedByPolicy => {
                "suppressed_by_policy"
            }
            trouve_protocol::CodeReviewFindingPublicationStatus::Failed => "failed",
        };
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let mut updated = 0;
        for id in ids {
            updated += tx.execute(
                "UPDATE code_review_findings SET github_publication_status = ?2 WHERE id = ?1",
                params![id, status],
            )?;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Closes an open finding and, in the same committed write, arms its
    /// thread collapse when the row currently has a published comment. The
    /// row's own github_comment_id decides — not a caller snapshot, which
    /// may predate the comment's publication by a concurrent round. Arming
    /// resets retry metadata: an open row cannot carry meaningful backoff.
    pub fn resolve_code_review_finding(&self, id: &str, status: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET status = ?2, resolved_at = ?3,
                 collapse_attempts = CASE
                     WHEN github_comment_id IS NOT NULL THEN 0
                     ELSE collapse_attempts
                 END,
                 collapse_next_attempt_at = CASE
                     WHEN github_comment_id IS NOT NULL THEN NULL
                     ELSE collapse_next_attempt_at
                 END,
                 collapse_pending = CASE
                     WHEN github_comment_id IS NOT NULL THEN 1
                     ELSE 0
                 END
             WHERE id = ?1 AND status = 'open'",
            params![id, status, chrono::Utc::now().to_rfc3339()],
        )? > 0)
    }

    /// Marks a finding's thread collapse as done; retry state resets with
    /// it. The clear only applies while the row still carries
    /// `expected_comment_id`: if concurrent publication re-armed the row
    /// with a different comment after the caller's snapshot, the newly armed
    /// work survives instead of being wiped by a stale pass.
    pub fn clear_code_review_thread_collapse(
        &self,
        id: &str,
        expected_comment_id: Option<u64>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET collapse_pending = 0, collapse_attempts = 0, collapse_next_attempt_at = NULL
             WHERE id = ?1 AND github_comment_id IS ?2",
            params![id, expected_comment_id.map(|value| value as i64)],
        )?;
        Ok(())
    }

    /// Caches (or resets, with `None`) a finding's GitHub thread id without
    /// touching its comment coordinates or collapse state, guarded on the
    /// comment the caller matched the thread against — so a stale pass can
    /// never overwrite a newer comment id published concurrently.
    pub fn cache_code_review_thread_id(
        &self,
        id: &str,
        expected_comment_id: u64,
        thread_id: Option<&str>,
    ) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET github_thread_id = ?3
             WHERE id = ?1 AND github_comment_id = ?2",
            params![id, expected_comment_id as i64, thread_id],
        )?;
        Ok(())
    }

    /// Reschedules a pending collapse for the next retry tick without
    /// counting a failure: used when the group budget ran out before the
    /// finding was attempted, so healthy tail findings do not accumulate
    /// exponential backoff they never earned.
    pub fn requeue_code_review_thread_collapse(&self, id: &str) -> Result<()> {
        let next_attempt = chrono::Utc::now() + chrono::Duration::seconds(60);
        self.conn.lock().unwrap().execute(
            "UPDATE code_review_findings
             SET collapse_next_attempt_at = ?2
             WHERE id = ?1",
            params![id, next_attempt.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Pushes a pending collapse's next attempt out with bounded exponential
    /// backoff (one minute doubling up to one hour), so a persistently
    /// failing finding cannot consume API quota on every retry pass.
    pub fn defer_code_review_thread_collapse(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        let attempts: i64 = tx
            .query_row(
                "SELECT collapse_attempts FROM code_review_findings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let delay_seconds = (60_i64 << attempts.clamp(0, 6)).min(3600);
        let next_attempt = chrono::Utc::now() + chrono::Duration::seconds(delay_seconds);
        tx.execute(
            "UPDATE code_review_findings
             SET collapse_attempts = collapse_attempts + 1,
                 collapse_next_attempt_at = ?2
             WHERE id = ?1",
            params![id, next_attempt.to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fixed findings whose GitHub review thread has not been confirmed
    /// collapsed and whose backoff window has elapsed, with the owning job's
    /// coordinates so a caller can build an installation client and retry
    /// the collapse. `limit` bounds each retry pass, and `excluded_ids`
    /// (typically the in-flight claims) are filtered out before the limit is
    /// applied so a batch never fills up with unactionable rows.
    pub fn pending_code_review_thread_collapses(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        limit: u64,
        excluded_ids: &[String],
    ) -> Result<Vec<(u64, String, u64, trouve_protocol::CodeReviewFinding)>> {
        let conn = self.conn.lock().unwrap();
        let exclusion = if excluded_ids.is_empty() {
            String::new()
        } else {
            format!(
                "AND f.id NOT IN ({})",
                vec!["?"; excluded_ids.len()].join(", ")
            )
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT j.installation_id, j.repository, j.pull_number,
                    f.id, f.job_id, f.path, f.line, f.side, f.severity, f.confidence, f.title, f.body,
                    f.prompt_for_agents, f.status, f.github_comment_id,
                    f.github_comment_url, f.github_publication_status,
                    f.github_thread_id, f.resolved_at
             FROM code_review_findings f
             JOIN code_review_jobs j ON j.id = f.job_id
             WHERE f.collapse_pending = 1
               AND (f.collapse_next_attempt_at IS NULL OR f.collapse_next_attempt_at <= ?)
               {exclusion}
             ORDER BY f.collapse_next_attempt_at IS NOT NULL,
                      f.collapse_next_attempt_at,
                      j.repository, j.pull_number, f.id
             LIMIT ?"
        ))?;
        let mut query_params: Vec<rusqlite::types::Value> = vec![now.to_rfc3339().into()];
        query_params.extend(
            excluded_ids
                .iter()
                .map(|id| rusqlite::types::Value::from(id.clone())),
        );
        query_params.push((limit as i64).into());
        let rows = stmt
            .query_map(rusqlite::params_from_iter(query_params), |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u64,
                    trouve_protocol::CodeReviewFinding {
                        id: row.get(3)?,
                        job_id: row.get(4)?,
                        path: row.get(5)?,
                        line: row.get::<_, i64>(6)? as u64,
                        side: row.get(7)?,
                        severity: row.get(8)?,
                        confidence: row.get(9)?,
                        title: row.get(10)?,
                        body: row.get(11)?,
                        prompt_for_agents: row.get(12)?,
                        status: row.get(13)?,
                        sources: Vec::new(),
                        github_comment_id: row.get::<_, Option<i64>>(14)?.map(|value| value as u64),
                        github_comment_url: row.get(15)?,
                        github_publication_status: code_review_publication_status(
                            &row.get::<_, String>(16)?,
                        ),
                        github_thread_id: row.get(17)?,
                        resolved_at: parse_optional_datetime(row.get(18)?),
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn code_review_findings(
        &self,
        job_id: &str,
    ) -> Result<Vec<trouve_protocol::CodeReviewFinding>> {
        let conn = self.conn.lock().unwrap();
        let base_rows: Vec<trouve_protocol::CodeReviewFinding> = {
            let mut stmt = conn.prepare(
                "SELECT id, job_id, path, line, side, severity, confidence, title, body,
                        prompt_for_agents, status, github_comment_id,
                        github_comment_url, github_publication_status,
                        github_thread_id, resolved_at
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
                    confidence: row.get(6)?,
                    title: row.get(7)?,
                    body: row.get(8)?,
                    prompt_for_agents: row.get(9)?,
                    status: row.get(10)?,
                    sources: Vec::new(),
                    github_comment_id: row.get::<_, Option<i64>>(11)?.map(|value| value as u64),
                    github_comment_url: row.get(12)?,
                    github_publication_status: code_review_publication_status(
                        &row.get::<_, String>(13)?,
                    ),
                    github_thread_id: row.get(14)?,
                    resolved_at: parse_optional_datetime(row.get(15)?),
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
                   AND t.status != 'superseded'
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
            "UPDATE code_review_jobs
             SET status = ?2,
                 review_url = CASE
                     WHEN ?3 != '' THEN ?3
                     WHEN lifecycle_comment_url != '' THEN lifecycle_comment_url
                     ELSE review_url
                 END,
                 error = ?4,
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        if old.publication_claimed {
            anyhow::bail!(
                "review publication may already exist; reconcile it instead of publishing again"
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
                     coordinator_thinking_level, review_watermark_sha)
             SELECT ?2, ?3, installation_id, repository, pull_number,
                    pull_title, pull_url, head_sha, base_ref, head_ref, 'retry',
                    'queued', model, prompt, identities, config_hash, ?4,
                    review_base_sha, review_scope, id, total_reviewers,
                    routing_mode, semantic_routing, included_reviewer_ids,
                    excluded_reviewer_ids, router_model, router_thinking_level,
                    coordinator_thinking_level, review_watermark_sha
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

    pub fn mark_code_review_publication_accepted(&self, id: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs SET publication_accepted = 1
             WHERE id = ?1 AND publication_claimed != 0",
            params![id],
        )? > 0)
    }

    pub fn release_code_review_publication_claim(&self, id: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE code_review_jobs
             SET publication_claimed = 0, publication_accepted = 0
             WHERE id = ?1 AND publication_accepted = 0",
            params![id],
        )? > 0)
    }

    pub fn reconcile_code_review_publication(
        &self,
        id: &str,
        review_url: &str,
        finding_ids: &[&str],
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
        for finding_id in finding_ids {
            tx.execute(
                "UPDATE code_review_findings
                 SET github_publication_status = 'published'
                 WHERE id = ?1",
                params![finding_id],
            )?;
        }
        let updated = tx.execute(
            "UPDATE code_review_jobs
             SET status = CASE
                     WHEN status = 'failed' AND publication_accepted = 0 THEN 'succeeded'
                     ELSE status
                 END,
                 error = CASE
                     WHEN status = 'failed' AND publication_accepted = 0 THEN ''
                     ELSE error
                 END,
                 review_url = ?2,
                 publication_accepted = 1
             WHERE id = ?1 AND publication_claimed != 0",
            params![id, review_url],
        )?;
        tx.commit()?;
        Ok(updated > 0)
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
            "UPDATE code_review_jobs
             SET lifecycle_comment_url = ?2,
                 review_url = CASE
                     WHEN status IN ('succeeded', 'failed', 'cancelled', 'stale')
                          AND (review_url = '' OR review_url = lifecycle_comment_url) THEN ?2
                     ELSE review_url
                 END
             WHERE id = ?1
               AND (lifecycle_comment_url != ?2
                    OR (status IN ('succeeded', 'failed', 'cancelled', 'stale')
                        AND (review_url = ''
                             OR (review_url = lifecycle_comment_url AND review_url != ?2))))",
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
        let conn = self.conn.lock().unwrap();
        let tx = write_transaction(&conn)?;
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
    /// requests (correct for billing); `context_input_tokens` is the
    /// provider-authoritative context size for the turn's *last* request.
    /// Summing per-iteration inputs over a multi-tool turn inflates the figure
    /// many-fold and spuriously trips compaction.
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
    /// request's provider-authoritative measurement, used by the compaction
    /// trigger and the UI usage indicator. Older rows recorded before this
    /// column existed report 0 (the caller falls back to a character
    /// estimate).
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
        let tx = write_transaction(&conn)?;
        append_checkpoint_row(&tx, row, chrono::Utc::now())?;
        tx.commit()?;
        Ok(())
    }

    /// Persist a checkpoint and its lifecycle event in the event writer's one
    /// transaction. This keeps the relational undo stack and durable event
    /// log in sync while concurrent threads are streaming events.
    pub(crate) fn append_checkpoint_with_event(
        &self,
        row: &CheckpointRow,
        scope: Scope,
        event: Event,
    ) -> Result<EventEnvelope> {
        let pending = serialize_lifecycle_events(
            vec![(scope, event)],
            StoreMutation::AppendCheckpoint {
                checkpoint: Box::new(row.clone()),
            },
        )?;
        Ok(self
            .append_pending_events(pending)?
            .pop()
            .expect("one checkpoint event returns one envelope"))
    }

    pub(crate) fn append_event_with_message(
        &self,
        scope: Scope,
        event: Event,
        thread_id: &str,
        payload: &serde_json::Value,
        attachments: Vec<(trouve_protocol::Attachment, String)>,
        staging_cleanup_claim: Option<ArtifactCleanupClaim>,
    ) -> Result<()> {
        let pending = serialize_lifecycle_events(
            vec![(scope, event)],
            StoreMutation::AppendMessage {
                thread_id: thread_id.to_string(),
                payload: payload.to_string(),
                attachments,
                staging_cleanup_claim,
            },
        )?;
        self.append_pending_events(pending)?;
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

    pub fn checkpoint(&self, checkpoint_id: &str) -> Result<Option<CheckpointRow>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, session_id, thread_id, turn, seq, commit_hash FROM checkpoints
             WHERE id = ?1",
            params![checkpoint_id],
            row_to_checkpoint,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn checkpoint_ids(&self, session_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut statement =
            conn.prepare("SELECT id FROM checkpoints WHERE session_id = ?1 ORDER BY seq")?;
        let rows = statement.query_map(params![session_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
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

    /// Sequence the next checkpoint at the tip of the currently selected
    /// undo branch. After an undo this deliberately reuses the first redo
    /// sequence; the append transaction truncates that stale redo tail.
    pub fn next_checkpoint_seq(&self, session_id: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let (latest, undo_pos) = conn.query_row(
            "SELECT (SELECT MAX(seq) FROM checkpoints WHERE session_id = ?1), undo_pos
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )?;
        Ok(undo_pos.unwrap_or(latest.unwrap_or(-1)) + 1)
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

/// Columns matching `row_to_thread`, including durable spawn parentage.
fn insert_thread_row(
    conn: &Connection,
    thread: &Thread,
    model_options: &serde_json::Map<String, serde_json::Value>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO threads
            (id, session_id, title, mode, model, permission_mode, model_options, todos, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            thread.id,
            thread.session_id,
            thread.title,
            thread.mode,
            thread.model,
            permission_mode_str(thread.permission_mode),
            serde_json::to_string(model_options)?,
            serde_json::to_string(&thread.todos)?,
            thread.created_at.to_rfc3339()
        ],
    )?;
    Ok(())
}

const THREAD_COLUMNS: &str = "id, session_id, mode, model, permission_mode, model_options, \
     created_at, EXISTS(SELECT 1 FROM spawned_threads st WHERE st.child_thread_id = threads.id), \
     todos, title, (SELECT st.parent_thread_id FROM spawned_threads st \
       WHERE st.child_thread_id = threads.id LIMIT 1)";

fn row_to_thread(r: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
    Ok(Thread {
        id: r.get(0)?,
        session_id: r.get(1)?,
        parent_thread_id: r.get(10)?,
        title: r.get(9)?,
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
    use trouve_protocol::{Event, ToolStatus};

    #[test]
    fn event_writer_errors_preserve_sqlite_classification() {
        let error = anyhow::Error::new(EventWriterError {
            message: "database is locked".into(),
            sqlite_code: Some(rusqlite::ErrorCode::DatabaseLocked),
        });
        assert_eq!(
            event_writer_sqlite_error_code(&error),
            Some(rusqlite::ErrorCode::DatabaseLocked)
        );
    }

    #[test]
    fn code_review_claim_waits_for_a_concurrent_writer_before_reading() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static HIT_BUSY_HANDLER: AtomicBool = AtomicBool::new(false);

        fn keep_waiting_for_writer(_: i32) -> bool {
            HIT_BUSY_HANDLER.store(true, Ordering::SeqCst);
            true
        }

        HIT_BUSY_HANDLER.store(false, Ordering::SeqCst);
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("review-write-contention.sqlite3");
        let store = Store::open(&database).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO code_review_jobs
                        (id, dedupe_key, installation_id, repository, pull_number,
                         pull_title, pull_url, head_sha, base_ref, head_ref,
                         trigger, status, created_at)
                 VALUES ('rv_busy', 'busy', 1, 'acme/widgets', 42,
                         'Original title', 'https://github.com/acme/widgets/pull/42',
                         'head', 'base', 'feature', 'automatic', 'queued', ?1)",
                [chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .busy_handler(Some(keep_waiting_for_writer))
            .unwrap();

        let mut blocker = Connection::open(&database).unwrap();
        let blocker_tx = blocker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        blocker_tx
            .execute(
                "UPDATE code_review_jobs SET pull_title = 'Committed title'
                 WHERE id = 'rv_busy'",
                [],
            )
            .unwrap();

        let claiming_store = store.clone();
        let claim = std::thread::spawn(move || claiming_store.claim_code_review_job());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !HIT_BUSY_HANDLER.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "review claim never waited for the concurrent writer"
            );
            std::thread::yield_now();
        }
        blocker_tx.commit().unwrap();

        let claimed = claim.join().unwrap().unwrap().unwrap();
        assert_eq!(claimed.job.status, "running");
        assert_eq!(claimed.job.pull_title, "Committed title");
    }

    #[test]
    fn empty_code_review_claim_does_not_wait_for_a_concurrent_writer() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static HIT_BUSY_HANDLER: AtomicBool = AtomicBool::new(false);

        fn stop_waiting_for_writer(_: i32) -> bool {
            HIT_BUSY_HANDLER.store(true, Ordering::SeqCst);
            false
        }

        HIT_BUSY_HANDLER.store(false, Ordering::SeqCst);
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("empty-review-write-contention.sqlite3");
        let store = Store::open(&database).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .busy_handler(Some(stop_waiting_for_writer))
            .unwrap();

        let mut blocker = Connection::open(&database).unwrap();
        let blocker_tx = blocker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();

        assert!(store.claim_code_review_job().unwrap().is_none());
        assert!(
            !HIT_BUSY_HANDLER.load(Ordering::SeqCst),
            "an empty review poll tried to reserve SQLite's writer slot"
        );
        blocker_tx.rollback().unwrap();
    }

    #[test]
    fn no_work_event_and_cleanup_polls_do_not_wait_for_a_concurrent_writer() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static HIT_BUSY_HANDLER: AtomicBool = AtomicBool::new(false);

        fn stop_waiting_for_writer(_: i32) -> bool {
            HIT_BUSY_HANDLER.store(true, Ordering::SeqCst);
            false
        }

        HIT_BUSY_HANDLER.store(false, Ordering::SeqCst);
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("no-work-write-contention.sqlite3");
        let store = Store::open(&database).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .busy_handler(Some(stop_waiting_for_writer))
            .unwrap();

        let mut blocker = Connection::open(&database).unwrap();
        let blocker_tx = blocker
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();

        let stale = insert_event_batch(
            &store.conn.lock().unwrap(),
            std::iter::empty::<&PendingEvent>(),
            0,
            [i64::MAX],
        )
        .unwrap();
        assert!(stale.skipped);
        assert!(store.claim_next_artifact_cleanup_job().unwrap().is_none());
        assert!(
            !HIT_BUSY_HANDLER.load(Ordering::SeqCst),
            "a no-work event or cleanup poll tried to reserve SQLite's writer slot"
        );
        blocker_tx.rollback().unwrap();
    }

    #[test]
    fn artifact_cleanup_claim_tokens_fence_stale_workers() {
        let store = Store::open_in_memory().unwrap();
        let staged = store
            .stage_attachment_cleanup(vec!["/tmp/attachment".into()])
            .unwrap()
            .unwrap();
        let staging_claim = staged.claim().unwrap();
        assert!(store.renew_artifact_cleanup_claim(&staging_claim).unwrap());

        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE artifact_cleanup_jobs SET claim_until = ?2 WHERE id = ?1",
                params![staged.id, "1970-01-01T00:00:00Z"],
            )
            .unwrap();
        let reclaimed = store
            .claim_artifact_cleanup_job(&staged.id)
            .unwrap()
            .unwrap();
        let current_claim = reclaimed.claim().unwrap();
        assert_ne!(current_claim.token, staging_claim.token);
        assert!(!store.renew_artifact_cleanup_claim(&staging_claim).unwrap());
        assert!(
            store
                .complete_claimed_artifact_cleanup_job(&staging_claim)
                .is_err()
        );
        assert!(
            store
                .fail_claimed_artifact_cleanup_job(&staging_claim, "stale")
                .is_err()
        );
        store
            .complete_claimed_artifact_cleanup_job(&current_claim)
            .unwrap();
    }

    #[test]
    fn malformed_cleanup_row_does_not_starve_later_valid_job() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn.lock().unwrap();
        conn.execute_batch(
            "INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, created_at)
             VALUES ('acj_poison_json', 'attachments', '{not-json', '2000-01-01T00:00:00Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, created_at)
             VALUES ('acj_poison_blob', CAST(X'80' AS BLOB), '[]', '2000-01-01T00:00:01Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, created_at)
             VALUES ('acj_poison_integer', 42, 7, '2000-01-01T00:00:02Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, claim_until, created_at)
             VALUES ('acj_poison_claim', 'attachments', '[]', CAST(X'80' AS BLOB),
                     '2000-01-01T00:00:03Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, next_attempt_at, created_at)
             VALUES ('acj_poison_backoff', 'attachments', '[]', 'not-a-timestamp',
                     '2000-01-01T00:00:04Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, attempts, created_at)
             VALUES ('acj_poison_attempts', 'attachments', '[]', CAST(X'80' AS BLOB),
                     '2000-01-01T00:00:05Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, claim_until, created_at)
             VALUES ('acj_future_claim', 'attachments', '[]', '2999-01-01T00:00:00Z',
                     '2000-01-01T00:00:06Z');
             INSERT INTO artifact_cleanup_jobs
               (id, kind, attachment_paths, next_attempt_at, created_at)
             VALUES ('acj_future_backoff', 'attachments', '[]', '2999-01-01T00:00:00Z',
                     '2000-01-01T00:00:07Z');",
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT typeof(kind) FROM artifact_cleanup_jobs WHERE id = 'acj_poison_blob'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "blob"
        );
        drop(conn);
        let valid = ArtifactCleanupJob::attachments(vec!["/tmp/valid".into()]);
        insert_artifact_cleanup_job(&store.conn.lock().unwrap(), &valid, chrono::Utc::now())
            .unwrap();

        let claimed = store.claim_next_artifact_cleanup_job().unwrap().unwrap();
        assert_eq!(claimed.id, valid.id);
        assert!(claimed.claim_token.is_some());
        for id in [
            "acj_poison_json",
            "acj_poison_blob",
            "acj_poison_integer",
            "acj_poison_claim",
            "acj_poison_backoff",
            "acj_poison_attempts",
        ] {
            let (attempts, last_error, next_attempt_at): (i64, Option<String>, Option<String>) =
                store
                    .conn
                    .lock()
                    .unwrap()
                    .query_row(
                        "SELECT attempts, last_error, next_attempt_at
                     FROM artifact_cleanup_jobs WHERE id = ?1",
                        params![id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .unwrap();
            assert_eq!(attempts, 1, "poisoned cleanup row {id} was not quarantined");
            assert!(
                last_error
                    .unwrap()
                    .contains("malformed durable cleanup intent")
            );
            assert!(next_attempt_at.is_some());
        }
        for id in ["acj_future_claim", "acj_future_backoff"] {
            let (attempts, last_error): (i64, Option<String>) = store
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT attempts, last_error FROM artifact_cleanup_jobs WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(attempts, 0, "valid future cleanup job {id} was modified");
            assert!(last_error.is_none());
        }
    }

    #[test]
    fn cleanup_failure_repairs_malformed_attempt_count() {
        let store = Store::open_in_memory().unwrap();
        let job = ArtifactCleanupJob::attachments(vec!["/tmp/retry".into()]);
        insert_artifact_cleanup_job(&store.conn.lock().unwrap(), &job, chrono::Utc::now()).unwrap();
        let claimed = store.claim_artifact_cleanup_job(&job.id).unwrap().unwrap();
        let claim = claimed.claim().unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE artifact_cleanup_jobs SET attempts = CAST(X'80' AS BLOB) WHERE id = ?1",
                params![job.id],
            )
            .unwrap();

        store
            .fail_claimed_artifact_cleanup_job(&claim, "retry cleanup")
            .unwrap();

        let (attempt_type, attempts, claim_until, claim_token, next_attempt_at): (
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT typeof(attempts), attempts, claim_until, claim_token, next_attempt_at
                   FROM artifact_cleanup_jobs WHERE id = ?1",
                params![job.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(attempt_type, "integer");
        assert_eq!(attempts, 1);
        assert!(claim_until.is_none());
        assert!(claim_token.is_none());
        assert!(next_attempt_at.is_some());
    }

    #[test]
    fn spawned_subtree_queries_include_nested_usage_and_failures() {
        let store = Store::open_in_memory().unwrap();
        for thread_id in ["root", "child", "grandchild"] {
            seed_thread(&store, thread_id);
        }
        store.insert_spawned("child", "root", "thread").unwrap();
        store
            .insert_spawned("grandchild", "child", "thread")
            .unwrap();
        for (turn, thread_id, input_tokens) in
            [(1, "root", 2), (1, "child", 3), (1, "grandchild", 5)]
        {
            store
                .record_usage(
                    "se_q",
                    thread_id,
                    turn,
                    &trouve_protocol::Usage {
                        input_tokens,
                        output_tokens: 1,
                        cached_input_tokens: 1,
                        ..Default::default()
                    },
                    input_tokens + 1,
                )
                .unwrap();
        }
        store
            .append_event(
                Scope::Thread("grandchild".into()),
                Event::TurnFailed {
                    turn: 1,
                    error: "nested failure".into(),
                },
            )
            .unwrap();

        let descendant_ids = store
            .spawned_descendants("root")
            .unwrap()
            .into_iter()
            .map(|thread| thread.id)
            .collect::<HashSet<_>>();
        assert_eq!(
            descendant_ids,
            HashSet::from(["child".to_string(), "grandchild".to_string()])
        );
        let usage = store.spawned_subtree_usage("root").unwrap();
        assert_eq!(usage.turns, 3);
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cached_input_tokens, 3);
        assert_eq!(
            store.failed_spawned_descendants("root").unwrap(),
            vec!["grandchild".to_string()]
        );
    }

    #[test]
    fn compact_tool_argument_bounds_utf8_summary() {
        let compact = compact_tool_argument(&serde_json::json!("🦀".repeat(400)), 0);
        let text = compact.as_str().unwrap();
        assert_eq!(text.chars().count(), 321);
        assert!(text.ends_with('…'));
        assert_eq!(text.trim_end_matches('…'), "🦀".repeat(320));
    }

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
                thinking_level: None,
                supports_steering: false,
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

        let (first_cursor, first) = store
            .thread_view_snapshot("th_view", None, 256, false)
            .unwrap();
        assert_eq!(first.items.len(), 3);
        assert_eq!(first.item_offset, 0);
        assert_eq!(first.total_items, 3);
        assert!(!first.has_older);
        assert!(first.turn_running);

        let (_, legacy_tail) = store
            .thread_view_snapshot("th_view", None, 2, false)
            .unwrap();
        assert_eq!(legacy_tail.item_offset, 1);
        assert_eq!(legacy_tail.items.len(), 2);
        assert!(legacy_tail.has_older);

        let (_, tail) = store
            .thread_view_snapshot("th_view", None, 2, true)
            .unwrap();
        // Pages expand to the beginning of the oldest included turn. Even a
        // tiny target cannot split the live turn that the client renders as
        // one stable virtual row.
        assert_eq!(tail.item_offset, 0);
        assert_eq!(tail.total_items, 3);
        assert!(!tail.has_older);
        assert_eq!(tail.items, first.items);

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
        let (second_cursor, second) = store
            .thread_view_snapshot("th_view", None, 256, false)
            .unwrap();
        assert!(second_cursor > first_cursor);
        assert!(!second.turn_running);
        assert_eq!(second.items.len(), 3);
        let changes_before = store.conn.lock().unwrap().total_changes();
        let (unchanged_cursor, unchanged) = store
            .thread_view_snapshot("th_view", None, 256, false)
            .unwrap();
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
        let (rebuilt_cursor, rebuilt) = store
            .thread_view_snapshot("th_view", None, 256, false)
            .unwrap();
        assert_eq!(rebuilt_cursor, second_cursor);
        assert_eq!(rebuilt.items, second.items);

        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE thread_view_cache SET state = '{' WHERE thread_id = 'th_view'",
                [],
            )
            .unwrap();
        let (repaired_cursor, repaired) = store
            .thread_view_snapshot("th_view", None, 256, false)
            .unwrap();
        assert_eq!(repaired_cursor, second_cursor);
        assert_eq!(repaired.items, second.items);

        for turn in 2..=101 {
            for event in [
                Event::TurnStarted {
                    turn,
                    mode: "code".into(),
                    model: "test/model".into(),
                    thinking_level: None,
                    supports_steering: false,
                },
                Event::UserMessage {
                    turn,
                    content: format!("historical prompt {turn}"),
                    attachments: Vec::new(),
                },
                Event::AssistantMessage {
                    turn,
                    content: format!("historical response {turn}"),
                },
                Event::TurnCompleted {
                    turn,
                    usage: Default::default(),
                    checkpoint_id: None,
                },
            ] {
                store.append_event(scope.clone(), event).unwrap();
            }
        }
        let (_, bounded) = store
            .thread_view_snapshot("th_view", None, 256, true)
            .unwrap();
        assert_eq!(bounded.items.len(), 258);
        assert_eq!(bounded.total_items, 303);
        assert_eq!(bounded.item_offset, 45);
        assert!(bounded.has_older);
        assert!(matches!(
            bounded.items.first(),
            Some(ThreadViewItem::TurnStatus { .. })
        ));
        let (_, older) = store
            .thread_view_snapshot("th_view", Some(bounded.item_offset), 256, true)
            .unwrap();
        assert_eq!(older.item_offset, 0);
        assert_eq!(older.items.len(), 45);
        assert!(!older.has_older);
    }

    #[test]
    fn turn_aligned_pages_do_not_backfill_across_cancelled_turns() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_cancelled_pages");
        let scope = Scope::Thread("th_cancelled_pages".into());
        for turn in 1..=200 {
            for event in [
                Event::TurnStarted {
                    turn,
                    mode: "code".into(),
                    model: "test/model".into(),
                    thinking_level: None,
                    supports_steering: false,
                },
                Event::UserMessage {
                    turn,
                    content: format!("cancelled prompt {turn}"),
                    attachments: Vec::new(),
                },
                Event::AssistantMessage {
                    turn,
                    content: format!("partial response {turn}"),
                },
                Event::TurnCancelled { turn },
            ] {
                store.append_event(scope.clone(), event).unwrap();
            }
        }

        let (_, newest) = store
            .thread_view_snapshot("th_cancelled_pages", None, 1, true)
            .unwrap();
        assert_eq!(newest.total_items, 400);
        assert_eq!(newest.item_offset, 398);
        assert_eq!(newest.items.len(), 2);
        assert!(matches!(
            newest.items.as_slice(),
            [
                ThreadViewItem::User { turn: 200, .. },
                ThreadViewItem::Assistant { turn: 200, .. },
            ]
        ));

        let (_, previous) = store
            .thread_view_snapshot("th_cancelled_pages", Some(newest.item_offset), 1, true)
            .unwrap();
        assert_eq!(previous.item_offset, 396);
        assert_eq!(previous.items.len(), 2);
        assert!(matches!(
            previous.items.as_slice(),
            [
                ThreadViewItem::User { turn: 199, .. },
                ThreadViewItem::Assistant { turn: 199, .. },
            ]
        ));
    }

    #[test]
    fn completed_tool_payloads_are_materialized_separately_from_history_pages() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_tool_details");
        let scope = Scope::Thread("th_tool_details".into());
        let large_argument = format!("argument-marker-{}", "a".repeat(32_768));
        let large_result = format!("result-marker-{}", "r".repeat(32_768));
        for event in [
            Event::TurnStarted {
                turn: 1,
                mode: "code".into(),
                model: "test/model".into(),
                thinking_level: None,
                supports_steering: false,
            },
            Event::UserMessage {
                turn: 1,
                content: "inspect the repository".into(),
                attachments: Vec::new(),
            },
            Event::ToolRequested {
                turn: 1,
                call_id: "large-read".into(),
                tool: "read_file".into(),
                args: serde_json::json!({
                    "path": "src/lib.rs",
                    "command": large_argument,
                }),
                requires_approval: false,
            },
            Event::ToolCompleted {
                call_id: "large-read".into(),
                status: ToolStatus::Ok,
                result: serde_json::json!({"content": large_result}),
                execution_duration_ms: Some(12),
            },
            Event::AssistantMessage {
                turn: 1,
                content: "done".into(),
            },
            Event::TurnCompleted {
                turn: 1,
                usage: Default::default(),
                checkpoint_id: None,
            },
        ] {
            store.append_event(scope.clone(), event).unwrap();
        }

        let (_, snapshot) = store
            .thread_view_snapshot("th_tool_details", None, 256, false)
            .unwrap();
        let compact_tool = snapshot
            .items
            .iter()
            .find_map(|item| match item {
                ThreadViewItem::ToolCall {
                    call_id,
                    args,
                    details_deferred,
                    result,
                    ..
                } if call_id == "large-read" => Some((args, *details_deferred, result.as_ref())),
                _ => None,
            })
            .expect("materialized tool call");
        assert!(compact_tool.1);
        assert!(compact_tool.2.is_none());
        assert!(serde_json::to_string(compact_tool.0).unwrap().len() < 1_024);

        let details = store
            .thread_tool_details("th_tool_details", "large-read")
            .unwrap()
            .expect("lazy tool details");
        assert_eq!(details.args["command"], large_argument);
        assert_eq!(details.result.unwrap()["content"], large_result);

        let cached_state = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT state FROM thread_view_cache WHERE thread_id = 'th_tool_details'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(!cached_state.contains("argument-marker"));
        assert!(!cached_state.contains("result-marker"));
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
        let mut conn = store.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE INDEX code_review_routing_decisions_job
             ON code_review_routing_decisions (job_id, batch_index, reviewer_id)",
        )
        .unwrap();
        apply_migrations(&mut conn).unwrap();
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
    fn migration_backfills_terminal_code_review_task_lifecycle_once() {
        let mut conn = Connection::open_in_memory().unwrap();
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

        apply_migrations(&mut conn).unwrap();

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

        let marker: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM store_migrations
                 WHERE id = 'code-review-terminal-task-lifecycle-v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, 1);
        conn.execute(
            "UPDATE code_review_tasks
             SET lifecycle_stage = 'queued', last_progress_at = NULL
             WHERE id = 'succeeded'",
            [],
        )
        .unwrap();
        apply_migrations(&mut conn).unwrap();
        let gated: (String, Option<String>) = conn
            .query_row(
                "SELECT lifecycle_stage, last_progress_at
                 FROM code_review_tasks WHERE id = 'succeeded'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(gated, ("queued".into(), None));
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
    fn migration_backfills_legacy_finding_publication_outcomes() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE store_migrations (
                id TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL
             );
             CREATE TABLE code_review_findings (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                line INTEGER NOT NULL,
                github_comment_url TEXT NOT NULL,
                github_publication_status TEXT NOT NULL
             );
             INSERT INTO code_review_findings VALUES
                ('published', 'src/lib.rs', 10, 'https://github.com/comment', 'pending'),
                ('empty-path', '', 10, '', 'pending'),
                ('zero-line', 'src/lib.rs', 0, '', 'pending'),
                ('eligible', 'src/lib.rs', 10, '', 'pending');",
        )
        .unwrap();

        conn.execute_batch(
            "CREATE TRIGGER reject_publication_migration_marker
             BEFORE INSERT ON store_migrations
             WHEN NEW.id = 'code-review-finding-publication-status-v1'
             BEGIN
                SELECT RAISE(FAIL, 'migration marker write blocked');
             END;",
        )
        .unwrap();
        assert!(migrate_code_review_finding_publication_status(&mut conn).is_err());
        assert_eq!(publication_status(&conn, "published"), "pending");
        assert_eq!(publication_status(&conn, "empty-path"), "pending");
        conn.execute_batch("DROP TRIGGER reject_publication_migration_marker")
            .unwrap();

        migrate_code_review_finding_publication_status(&mut conn).unwrap();

        assert_eq!(publication_status(&conn, "published"), "published");
        assert_eq!(publication_status(&conn, "empty-path"), "not_eligible");
        assert_eq!(publication_status(&conn, "zero-line"), "not_eligible");
        assert_eq!(publication_status(&conn, "eligible"), "pending");

        conn.execute(
            "UPDATE code_review_findings SET github_publication_status = 'pending'
             WHERE id = 'empty-path'",
            [],
        )
        .unwrap();
        migrate_code_review_finding_publication_status(&mut conn).unwrap();
        assert_eq!(publication_status(&conn, "empty-path"), "pending");
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM store_migrations
                 WHERE id = 'code-review-finding-publication-status-v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn publication_status_migration_serializes_concurrent_openers() {
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("concurrent-migration.sqlite3");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch(
            "CREATE TABLE store_migrations (
                id TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL
             );
             CREATE TABLE code_review_findings (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                line INTEGER NOT NULL,
                github_comment_url TEXT NOT NULL,
                github_publication_status TEXT NOT NULL
             );
             INSERT INTO code_review_findings VALUES
                ('published', 'src/lib.rs', 10, 'https://github.com/comment', 'pending');",
        )
        .unwrap();
        drop(conn);

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let openers = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut conn = Connection::open(database).unwrap();
                    conn.busy_timeout(SQLITE_BUSY_TIMEOUT).unwrap();
                    barrier.wait();
                    migrate_code_review_finding_publication_status(&mut conn)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for opener in openers {
            opener.join().unwrap().unwrap();
        }

        let conn = Connection::open(database).unwrap();
        assert_eq!(publication_status(&conn, "published"), "published");
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM store_migrations
                 WHERE id = 'code-review-finding-publication-status-v1'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn applied_publication_status_migration_does_not_take_a_write_lock() {
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("applied-migration.sqlite3");
        let mut setup = Connection::open(&database).unwrap();
        setup
            .execute_batch(
                "CREATE TABLE store_migrations (
                    id TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                 );
                 CREATE TABLE code_review_findings (
                    id TEXT PRIMARY KEY,
                    path TEXT NOT NULL,
                    line INTEGER NOT NULL,
                    github_comment_url TEXT NOT NULL,
                    github_publication_status TEXT NOT NULL
                 );",
            )
            .unwrap();
        migrate_code_review_finding_publication_status(&mut setup).unwrap();

        let mut writer = Connection::open(&database).unwrap();
        let _write = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut opener = Connection::open(&database).unwrap();
        opener.busy_timeout(Duration::ZERO).unwrap();

        migrate_code_review_finding_publication_status(&mut opener).unwrap();
    }

    fn publication_status(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT github_publication_status FROM code_review_findings WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .unwrap()
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
    fn session_summary_projection_is_transactional_resumable_and_tombstoned() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_summary");

        let created = store
            .append_event(
                Scope::Server,
                Event::SessionCreated {
                    session_id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                },
            )
            .unwrap();
        let initial = store.session_summaries_snapshot().unwrap();
        assert_eq!(initial.summaries.len(), 1);
        assert_eq!(initial.summaries[0].latest_cursor, created.cursor);
        assert!(
            initial.cursor > created.cursor,
            "the derived update follows its source"
        );
        assert_eq!(initial.summaries[0].outcome, SessionOutcome::Idle);

        store
            .append_event(
                Scope::Server,
                Event::ThreadCreated {
                    thread_id: "th_summary".into(),
                    session_id: "se_q".into(),
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Thread("th_summary".into()),
                Event::TurnStarted {
                    turn: 1,
                    mode: "code".into(),
                    model: "p/m".into(),
                    thinking_level: None,
                    supports_steering: false,
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Server,
                Event::SessionActivity {
                    session_id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                    active: true,
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Thread("th_summary".into()),
                Event::ApprovalRequested {
                    turn: 1,
                    call_id: "call_1".into(),
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Thread("th_summary".into()),
                Event::QuestionRequested {
                    turn: 1,
                    request_id: "question_1".into(),
                    title: Some("Choose".into()),
                    questions: vec![trouve_protocol::Question {
                        id: "choice".into(),
                        prompt: "Continue?".into(),
                        options: Vec::new(),
                        allow_multiple: false,
                    }],
                },
            )
            .unwrap();

        let waiting = store.session_summaries_snapshot().unwrap();
        assert_eq!(waiting.summaries[0].attention, SessionAttention::Both);
        assert_eq!(waiting.summaries[0].outcome, SessionOutcome::Running);
        assert!(waiting.summaries[0].active);
        assert_eq!(
            waiting.summaries[0].latest_thread_id.as_deref(),
            Some("th_summary")
        );

        store
            .append_event(
                Scope::Thread("th_summary".into()),
                Event::ApprovalResolved {
                    call_id: "call_1".into(),
                    decision: trouve_protocol::ApprovalDecision::Approve,
                },
            )
            .unwrap();
        let question_only = store.session_summaries_snapshot().unwrap();
        assert_eq!(
            question_only.summaries[0].attention,
            SessionAttention::Question
        );

        store
            .append_event(
                Scope::Thread("th_summary".into()),
                Event::TurnFailed {
                    turn: 1,
                    error: "expected test failure".into(),
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Server,
                Event::SessionActivity {
                    session_id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                    active: false,
                },
            )
            .unwrap();
        let failed = store.session_summaries_snapshot().unwrap();
        assert_eq!(failed.summaries[0].attention, SessionAttention::None);
        assert_eq!(failed.summaries[0].outcome, SessionOutcome::Failed);
        assert!(!failed.summaries[0].active);

        // A client that starts replay after the snapshot cursor observes the
        // complete replacement summary and cannot lose this concurrent edit.
        store
            .update_session_with_event(
                "se_q",
                None,
                Some(true),
                Event::SessionUpdated {
                    session_id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                },
            )
            .unwrap();
        let resumed = store.events_after(&Scope::Server, failed.cursor).unwrap();
        assert!(resumed.iter().any(|envelope| matches!(
            &envelope.event,
            Event::SessionSummaryUpdated {
                summary: Some(summary),
                ..
            } if summary.archived
        )));

        store
            .delete_session_with_event(
                "se_q",
                ArtifactCleanupJob::deleted_session(
                    "se_q".into(),
                    "/tmp/se_q".into(),
                    "/tmp/repo".into(),
                    Vec::new(),
                ),
                Event::SessionDeleted {
                    session_id: "se_q".into(),
                    workspace_id: "ws_q".into(),
                },
            )
            .unwrap();
        let deleted = store.session_summaries_snapshot().unwrap();
        assert!(deleted.summaries.is_empty());
        assert!(
            store
                .events_after(&Scope::Server, failed.cursor)
                .unwrap()
                .iter()
                .any(|envelope| matches!(
                    &envelope.event,
                    Event::SessionSummaryUpdated {
                        session_id,
                        summary: None,
                    } if session_id == "se_q"
                ))
        );
    }

    #[test]
    fn thread_status_projection_tracks_concurrent_threads_independently() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_waiting");
        seed_thread(&store, "th_finished");

        for thread_id in ["th_waiting", "th_finished"] {
            store
                .append_event(
                    Scope::Server,
                    Event::ThreadCreated {
                        thread_id: thread_id.into(),
                        session_id: "se_q".into(),
                    },
                )
                .unwrap();
            store
                .append_event(
                    Scope::Thread(thread_id.into()),
                    Event::TurnStarted {
                        turn: 1,
                        mode: "code".into(),
                        model: "p/m".into(),
                        thinking_level: None,
                        supports_steering: false,
                    },
                )
                .unwrap();
        }
        let approval = store
            .append_event(
                Scope::Thread("th_waiting".into()),
                Event::ApprovalRequested {
                    turn: 1,
                    call_id: "call_waiting".into(),
                },
            )
            .unwrap();
        let completed = store
            .append_event(
                Scope::Thread("th_finished".into()),
                Event::TurnCompleted {
                    turn: 1,
                    usage: Default::default(),
                    checkpoint_id: None,
                },
            )
            .unwrap();

        let statuses = store.list_thread_statuses("se_q").unwrap();
        let waiting = statuses
            .iter()
            .find(|status| status.thread_id == "th_waiting")
            .unwrap();
        assert!(waiting.active);
        assert_eq!(waiting.attention, SessionAttention::Approval);
        assert_eq!(waiting.outcome, SessionOutcome::Running);
        assert_eq!(waiting.latest_cursor, approval.cursor);
        assert!(waiting.started_at.is_some());
        assert!(waiting.completed_at.is_none());
        let finished = statuses
            .iter()
            .find(|status| status.thread_id == "th_finished")
            .unwrap();
        assert!(!finished.active);
        assert_eq!(finished.attention, SessionAttention::None);
        assert_eq!(finished.outcome, SessionOutcome::Succeeded);
        assert_eq!(finished.latest_cursor, completed.cursor);
        assert!(finished.started_at.is_some());
        assert!(finished.completed_at.is_some());
        assert!(finished.completed_at >= finished.started_at);

        let replacements = store.events_after(&Scope::Server, 0).unwrap();
        assert!(replacements.iter().any(|envelope| matches!(
            &envelope.event,
            Event::ThreadStatusUpdated { status }
                if status.thread_id == "th_waiting"
                    && status.attention == SessionAttention::Approval
        )));
        assert!(replacements.iter().any(|envelope| matches!(
            &envelope.event,
            Event::ThreadStatusUpdated { status }
                if status.thread_id == "th_finished"
                    && status.outcome == SessionOutcome::Succeeded
        )));

        store
            .append_event(
                Scope::Thread("th_waiting".into()),
                Event::TurnFailed {
                    turn: 1,
                    error: "approval no longer actionable".into(),
                },
            )
            .unwrap();
        let waiting = store
            .list_thread_statuses("se_q")
            .unwrap()
            .into_iter()
            .find(|status| status.thread_id == "th_waiting")
            .unwrap();
        assert!(!waiting.active);
        assert_eq!(waiting.attention, SessionAttention::None);
        assert_eq!(waiting.outcome, SessionOutcome::Failed);
        assert!(waiting.completed_at.is_some());
    }

    #[test]
    fn session_notification_edges_preserve_native_category_detail_and_order() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_notice");

        store
            .append_event(
                Scope::Thread("th_notice".into()),
                Event::TurnFailed {
                    turn: 1,
                    error: format!("  {}tail  ", "界".repeat(120)),
                },
            )
            .unwrap();
        store
            .append_event(
                Scope::Thread("th_notice".into()),
                Event::QuestionRequested {
                    turn: 2,
                    request_id: "question_notice".into(),
                    title: Some("  Choose a deployment target  ".into()),
                    questions: Vec::new(),
                },
            )
            .unwrap();

        let events = store.events_after(&Scope::Server, 0).unwrap();
        let notifications = events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::SessionNotification {
                    session_id,
                    thread_id,
                    kind,
                    detail,
                } => Some((
                    envelope.cursor,
                    session_id.as_str(),
                    thread_id.as_str(),
                    *kind,
                    detail.as_deref(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0].1, "se_q");
        assert_eq!(notifications[0].2, "th_notice");
        assert_eq!(
            notifications[0].3,
            trouve_protocol::SessionNotificationKind::TurnFailed
        );
        assert_eq!(
            notifications[0].4,
            Some(format!("{}…", "界".repeat(120)).as_str())
        );
        assert_eq!(
            notifications[1].3,
            trouve_protocol::SessionNotificationKind::QuestionRequested
        );
        assert_eq!(notifications[1].4, Some("Choose a deployment target"));

        for (notification_cursor, ..) in notifications {
            let summary_cursor = events
                .iter()
                .rev()
                .find(|envelope| {
                    envelope.cursor < notification_cursor
                        && matches!(envelope.event, Event::SessionSummaryUpdated { .. })
                })
                .map(|envelope| envelope.cursor)
                .expect("notification follows a replacement summary");
            assert_eq!(notification_cursor, summary_cursor + 1);
        }
    }

    #[test]
    fn reopening_persists_recovery_and_clears_process_owned_attention() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("recovery.db");
        let before_restart = {
            let store = Store::open(&path).unwrap();
            seed_thread(&store, "th_recovery");
            store
                .append_event(
                    Scope::Server,
                    Event::SessionCreated {
                        session_id: "se_q".into(),
                        workspace_id: "ws_q".into(),
                    },
                )
                .unwrap();
            store
                .append_event(
                    Scope::Server,
                    Event::SessionActivity {
                        session_id: "se_q".into(),
                        workspace_id: "ws_q".into(),
                        active: true,
                    },
                )
                .unwrap();
            store
                .append_event(
                    Scope::Thread("th_recovery".into()),
                    Event::ApprovalRequested {
                        turn: 1,
                        call_id: "call_crashed".into(),
                    },
                )
                .unwrap();
            let snapshot = store.session_summaries_snapshot().unwrap();
            assert!(snapshot.summaries[0].active);
            assert_eq!(snapshot.summaries[0].attention, SessionAttention::Approval);
            snapshot.cursor
        };

        let reopened = Store::open(&path).unwrap();
        let recovered = reopened.session_summaries_snapshot().unwrap();
        assert!(recovered.cursor > before_restart);
        assert_eq!(recovered.summaries.len(), 1);
        assert!(!recovered.summaries[0].active);
        assert_eq!(recovered.summaries[0].attention, SessionAttention::None);
        assert_eq!(recovered.summaries[0].outcome, SessionOutcome::Failed);

        let replay = reopened
            .events_after(&Scope::Server, before_restart)
            .unwrap();
        assert!(matches!(
            replay.first().map(|envelope| &envelope.event),
            Some(Event::SessionRecovered { session_id, .. }) if session_id == "se_q"
        ));
        assert!(matches!(
            replay.get(1).map(|envelope| &envelope.event),
            Some(Event::SessionSummaryUpdated {
                summary: Some(summary),
                ..
            }) if !summary.active
                && summary.attention == SessionAttention::None
                && summary.outcome == SessionOutcome::Failed
        ));
        assert!(replay.iter().any(|envelope| matches!(
            &envelope.event,
            Event::ThreadStatusUpdated { status }
                if status.thread_id == "th_recovery"
                    && !status.active
                    && status.attention == SessionAttention::None
                    && status.outcome == SessionOutcome::Failed
        )));
    }

    #[test]
    fn reopening_marks_only_interrupted_thread_statuses_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sibling-recovery.db");
        let (before_restart, finished_cursor, finished_completed_at) = {
            let store = Store::open(&path).unwrap();
            seed_thread(&store, "th_interrupted");
            seed_thread(&store, "th_succeeded");
            for thread_id in ["th_interrupted", "th_succeeded"] {
                store
                    .append_event(
                        Scope::Thread(thread_id.into()),
                        Event::TurnStarted {
                            turn: 1,
                            mode: "code".into(),
                            model: "test/model".into(),
                            thinking_level: None,
                            supports_steering: false,
                        },
                    )
                    .unwrap();
            }
            store
                .append_event(
                    Scope::Thread("th_interrupted".into()),
                    Event::ApprovalRequested {
                        turn: 1,
                        call_id: "call_interrupted".into(),
                    },
                )
                .unwrap();
            store
                .append_event(
                    Scope::Thread("th_succeeded".into()),
                    Event::TurnCompleted {
                        turn: 1,
                        usage: Default::default(),
                        checkpoint_id: None,
                    },
                )
                .unwrap();
            store
                .append_event(
                    Scope::Server,
                    Event::SessionActivity {
                        session_id: "se_q".into(),
                        workspace_id: "ws_q".into(),
                        active: true,
                    },
                )
                .unwrap();
            let finished = store
                .list_thread_statuses("se_q")
                .unwrap()
                .into_iter()
                .find(|status| status.thread_id == "th_succeeded")
                .unwrap();
            (
                store.latest_event_cursor(&Scope::Server).unwrap(),
                finished.latest_cursor,
                finished.completed_at,
            )
        };

        let reopened = Store::open(&path).unwrap();
        let statuses = reopened.list_thread_statuses("se_q").unwrap();
        let interrupted = statuses
            .iter()
            .find(|status| status.thread_id == "th_interrupted")
            .unwrap();
        assert!(!interrupted.active);
        assert_eq!(interrupted.attention, SessionAttention::None);
        assert_eq!(interrupted.outcome, SessionOutcome::Failed);
        assert!(interrupted.completed_at.is_some());
        let succeeded = statuses
            .iter()
            .find(|status| status.thread_id == "th_succeeded")
            .unwrap();
        assert!(!succeeded.active);
        assert_eq!(succeeded.attention, SessionAttention::None);
        assert_eq!(succeeded.outcome, SessionOutcome::Succeeded);
        assert_eq!(succeeded.latest_cursor, finished_cursor);
        assert_eq!(succeeded.completed_at, finished_completed_at);

        let replay = reopened
            .events_after(&Scope::Server, before_restart)
            .unwrap();
        assert!(replay.iter().any(|envelope| matches!(
            &envelope.event,
            Event::ThreadStatusUpdated { status }
                if status.thread_id == "th_interrupted"
                    && status.outcome == SessionOutcome::Failed
        )));
        assert!(!replay.iter().any(|envelope| matches!(
            &envelope.event,
            Event::ThreadStatusUpdated { status } if status.thread_id == "th_succeeded"
        )));
    }

    #[test]
    fn lifecycle_mutation_rolls_back_when_its_event_transaction_fails() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_workspace(&Workspace {
                id: "ws_atomic".into(),
                name: "atomic".into(),
                path: "/tmp/atomic".into(),
            })
            .unwrap();
        let session = Session {
            id: "se_atomic".into(),
            workspace_id: "ws_atomic".into(),
            title: "Atomic".into(),
            branch: "trouve/atomic".into(),
            worktree_path: "/tmp/atomic-worktree".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        let invalid_checkpoint = CheckpointRow {
            id: "cp_atomic".into(),
            session_id: "se_missing".into(),
            thread_id: None,
            turn: 0,
            seq: 0,
            commit_hash: "deadbeef".into(),
        };

        assert!(
            store
                .insert_session_with_lifecycle(
                    &session,
                    &invalid_checkpoint,
                    vec![(
                        Scope::Server,
                        Event::SessionCreated {
                            session_id: session.id.clone(),
                            workspace_id: session.workspace_id.clone(),
                        },
                    )],
                )
                .is_err()
        );
        assert!(store.session(&session.id).unwrap().is_none());
        assert!(store.events_after(&Scope::Server, 0).unwrap().is_empty());
        assert!(
            store
                .session_summaries_snapshot()
                .unwrap()
                .summaries
                .is_empty()
        );
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
    fn on_disk_event_writer_does_not_wait_for_read_connection_mutex() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("trouve.db")).unwrap();
        let read_guard = store.conn.lock().unwrap();
        let writer = store.clone();
        let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
        let append = std::thread::spawn(move || {
            result_tx
                .send(writer.append_event(
                    Scope::Server,
                    Event::AssistantDelta {
                        turn: 1,
                        text: "persisted independently".into(),
                    },
                ))
                .unwrap();
        });

        let result = result_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("event writer waited for the read-side connection mutex");
        assert!(result.is_ok());
        drop(read_guard);
        append.join().unwrap();
        assert_eq!(store.events_after(&Scope::Server, 0).unwrap().len(), 1);
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

    #[tokio::test]
    async fn pr_verification_intent_is_durable_atomic_and_session_scoped() {
        let store = Store::open_in_memory().unwrap();
        let workspace = Workspace {
            id: "ws_pr_intent".into(),
            name: "x".into(),
            path: "/tmp/repo-pr-intent".into(),
        };
        store.insert_workspace(&workspace).unwrap();
        let session = Session {
            id: "se_pr_intent".into(),
            workspace_id: workspace.id,
            title: "PR intent".into(),
            branch: "agent/session".into(),
            worktree_path: "/tmp/wt-pr-intent".into(),
            base_ref: "main".into(),
            archived: false,
            active: false,
            created_at: chrono::Utc::now(),
        };
        store.insert_session(&session).unwrap();
        let thread = Thread {
            id: "th_pr_intent".into(),
            session_id: session.id.clone(),
            parent_thread_id: None,
            title: None,
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
        let mut intent = SessionPrVerificationIntent {
            session_id: session.id.clone(),
            host: "github.com".into(),
            owner: "o".into(),
            repository: "r".into(),
            number: 42,
            branch: "agent/clean-pr".into(),
            head_sha: "1".repeat(40),
            attempts: 0,
            last_failure_class: String::new(),
            consecutive_failures: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let invalid_evidence = SessionPrVerificationIntent {
            branch: String::new(),
            head_sha: String::new(),
            ..intent.clone()
        };
        let error = store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread(thread.id.clone()),
                vec![Event::ToolCompleted {
                    call_id: "call-pr-without-evidence".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![invalid_evidence],
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("immutable branch and head evidence")
        );

        store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread(thread.id),
                vec![Event::ToolCompleted {
                    call_id: "call-pr".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![intent.clone()],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .due_session_pr_verification_intents(&session.id, 10)
                .unwrap(),
            vec![intent.clone()]
        );

        store
            .defer_session_pr_verification(&intent, "transient", true, 1)
            .unwrap();
        let (attempts, failure_class, consecutive_failures, next_attempt_at): (
            i64,
            String,
            i64,
            String,
        ) = {
            let conn = store.conn.lock().unwrap();
            conn.query_row(
                "SELECT attempts, last_failure_class, consecutive_failures, next_attempt_at
                 FROM session_pr_verification_intents
                 WHERE session_id = ?1 AND host = ?2 AND owner = ?3
                   AND repository = ?4 AND pull_number = ?5
                   AND branch = ?6 AND head_sha = ?7",
                params![
                    intent.session_id,
                    intent.host,
                    intent.owner,
                    intent.repository,
                    intent.number as i64,
                    intent.branch,
                    intent.head_sha,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap()
        };
        assert_eq!(attempts, 1);
        assert_eq!(failure_class, "transient");
        assert_eq!(consecutive_failures, 1);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&next_attempt_at).unwrap() > chrono::Utc::now()
        );
        assert!(
            store
                .due_session_pr_verification_intents(&session.id, 10)
                .unwrap()
                .is_empty(),
            "deferred intent must not remain immediately due"
        );
        store
            .defer_session_pr_verification(&intent, "transient", true, 2)
            .unwrap();
        store
            .defer_session_pr_verification(&intent, "authentication", false, 30)
            .unwrap();
        let switched_failure: (i64, String, i64) = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT attempts, last_failure_class, consecutive_failures
                 FROM session_pr_verification_intents
                 WHERE session_id = ?1 AND host = ?2 AND owner = ?3
                   AND repository = ?4 AND pull_number = ?5",
                params![
                    intent.session_id,
                    intent.host,
                    intent.owner,
                    intent.repository,
                    intent.number as i64,
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(switched_failure, (2, "authentication".into(), 1));

        store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread("th_pr_intent".into()),
                vec![Event::ToolCompleted {
                    call_id: "call-pr-duplicate".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![intent.clone()],
            )
            .await
            .unwrap();
        let attempts_after_duplicate: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT attempts FROM session_pr_verification_intents
                 WHERE session_id = ?1 AND host = ?2 AND owner = ?3
                   AND repository = ?4 AND pull_number = ?5",
                params![
                    intent.session_id,
                    intent.host,
                    intent.owner,
                    intent.repository,
                    intent.number as i64,
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attempts_after_duplicate, 2);
        assert!(
            store
                .due_session_pr_verification_intents(&session.id, 10)
                .unwrap()
                .is_empty(),
            "identical evidence must preserve accumulated backoff"
        );

        let replacement = SessionPrVerificationIntent {
            head_sha: "2".repeat(40),
            ..intent.clone()
        };
        store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread("th_pr_intent".into()),
                vec![Event::ToolCompleted {
                    call_id: "call-pr-replaced".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![replacement.clone()],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .due_session_pr_verification_intents(&session.id, 10)
                .unwrap(),
            vec![replacement.clone()],
            "new evidence must reset retry state"
        );
        intent = replacement;

        store.discard_session_pr_verification(&intent).unwrap();
        let remaining: i64 = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM session_pr_verification_intents
                 WHERE session_id = ?1",
                params![intent.session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);

        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO session_pr_verification_intents
                   (session_id, host, owner, repository, pull_number, branch,
                    head_sha, attempts, next_attempt_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, 43, '', '', 2, NULL, ?5)",
                params![
                    intent.session_id,
                    intent.host,
                    intent.owner,
                    intent.repository,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )
            .unwrap();
        let legacy = store
            .due_session_pr_verification_intents(&session.id, 10)
            .unwrap()
            .pop()
            .unwrap();
        assert!(legacy.branch.is_empty());
        assert!(legacy.head_sha.is_empty());
        assert!(
            store
                .set_session_pr_verification_evidence(
                    &legacy,
                    "agent/legacy-captured",
                    &"3".repeat(40),
                )
                .unwrap()
        );
        let upgraded = store
            .due_session_pr_verification_intents(&session.id, 10)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(upgraded.branch, "agent/legacy-captured");
        assert_eq!(upgraded.head_sha, "3".repeat(40));
        assert_eq!(upgraded.attempts, 0);
        assert!(upgraded.last_failure_class.is_empty());
        assert_eq!(upgraded.consecutive_failures, 0);
        store.discard_session_pr_verification(&upgraded).unwrap();

        store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread("th_pr_intent".into()),
                vec![Event::ToolCompleted {
                    call_id: "call-pr-requeued".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![intent.clone()],
            )
            .await
            .unwrap();

        assert!(
            store
                .complete_session_pr_verification(
                    intent.clone(),
                    Event::SessionPrOpened {
                        number: 42,
                        url: "https://github.com/o/r/pull/42".into(),
                    },
                )
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .due_session_pr_verification_intents(&session.id, 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .complete_session_pr_verification(
                    intent.clone(),
                    Event::SessionPrOpened {
                        number: 42,
                        url: "https://github.com/o/r/pull/42".into(),
                    },
                )
                .await
                .unwrap()
                .is_none(),
            "a completed intent is an idempotent no-op"
        );
        store
            .append_event(
                Scope::Server,
                Event::AssistantDelta {
                    turn: 1,
                    text: "unrelated append survives stale completion".into(),
                },
            )
            .unwrap();
        let associations = store
            .events_after(&Scope::Session(session.id), 0)
            .unwrap()
            .into_iter()
            .filter(|envelope| matches!(envelope.event, Event::SessionPrOpened { .. }))
            .count();
        assert_eq!(associations, 1);

        store
            .append_events_with_session_pr_verification_intents(
                Scope::Thread("th_pr_intent".into()),
                vec![Event::ToolCompleted {
                    call_id: "call-pr-again".into(),
                    status: ToolStatus::Ok,
                    result: serde_json::json!({"number": 42}),
                    execution_duration_ms: Some(1),
                }],
                vec![intent],
            )
            .await
            .unwrap();
        store.delete_session("se_pr_intent").unwrap();
        assert!(
            store
                .due_session_pr_verification_sessions(10)
                .unwrap()
                .is_empty(),
            "session deletion must remove pending PR verification work"
        );
    }

    #[test]
    fn pr_verification_retry_delay_reaches_six_hour_cap() {
        assert_eq!(Store::session_pr_verification_retry_delay(8), 256);
        assert_eq!(Store::session_pr_verification_retry_delay(14), 16_384);
        assert_eq!(Store::session_pr_verification_retry_delay(15), 21_600);
        assert_eq!(Store::session_pr_verification_retry_delay(u32::MAX), 21_600);
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
            parent_thread_id: None,
            title: None,
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
            parent_thread_id: Some("th_1".into()),
            title: None,
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
        assert_eq!(
            store
                .thread("th_child")
                .unwrap()
                .and_then(|thread| thread.parent_thread_id),
            Some("th_1".into()),
        );

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
                    parent_thread_id: None,
                    title: None,
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
    fn thread_titles_round_trip() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_seed");
        let named = Thread {
            id: "th_named".into(),
            session_id: "se_q".into(),
            parent_thread_id: None,
            title: Some("Review the parser edge cases".into()),
            mode: "review".into(),
            model: "p/m".into(),
            model_options: serde_json::Map::new(),
            permission_mode: PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: false,
            todos: Vec::new(),
        };

        store
            .insert_thread(&named, &serde_json::Map::new())
            .unwrap();

        let loaded = store.thread("th_named").unwrap().unwrap();
        assert_eq!(loaded.id, named.id);
        assert_eq!(loaded.title, named.title);
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
        let attachment = trouve_protocol::Attachment {
            id: "at_queue_edit".into(),
            name: "layout.png".into(),
            mime: "image/png".into(),
            size_bytes: 4,
        };
        let a = store.enqueue_prompt("th_1", "first", &[]).unwrap();
        let b = store
            .enqueue_prompt("th_1", "second", std::slice::from_ref(&attachment))
            .unwrap();
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
        assert!(
            store
                .update_queued_prompt(&b.id, "second v2", std::slice::from_ref(&attachment))
                .unwrap()
        );
        let edited = store.queued_prompts("th_1").unwrap();
        assert_eq!(edited[1].content, "second v2");
        assert_eq!(edited[1].attachments, [attachment]);
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
    fn prompt_acceptance_commits_queue_turn_attachments_and_events_atomically() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_accept");
        let attachment = trouve_protocol::Attachment {
            id: "at_accept".into(),
            name: "layout.png".into(),
            mime: "image/png".into(),
            size_bytes: 4,
        };
        let prompt = trouve_protocol::QueuedPrompt {
            id: "qp_accept".into(),
            thread_id: "th_accept".into(),
            position: 1,
            content: "Ship the prompt quickly".into(),
            attachments: vec![attachment.clone()],
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let events = vec![
            (
                Scope::Thread("th_accept".into()),
                Event::QueueUpdated {
                    prompts: Vec::new(),
                },
            ),
            (
                Scope::Thread("th_accept".into()),
                Event::TurnStarted {
                    turn: 1,
                    mode: "code".into(),
                    model: "p/m".into(),
                    thinking_level: None,
                    supports_steering: false,
                },
            ),
            (
                Scope::Thread("th_accept".into()),
                Event::UserMessage {
                    turn: 1,
                    content: prompt.content.clone(),
                    attachments: prompt.attachments.clone(),
                },
            ),
        ];

        store
            .accept_prompt_with_events(
                PromptAcceptance {
                    prompt: prompt.clone(),
                    tools_enabled: false,
                    attachments: vec![(attachment.clone(), "/tmp/at_accept.png".into())],
                    claim_prompt_id: Some(prompt.id.clone()),
                    expected_previous_turn: Some(0),
                    staging_cleanup_claim: None,
                },
                events,
            )
            .unwrap();

        assert_eq!(store.last_turn("th_accept").unwrap(), 1);
        assert!(store.queued_prompts("th_accept").unwrap().is_empty());
        assert!(!store.queued_prompt_tools_enabled(&prompt.id).unwrap());
        assert_eq!(
            store.attachment(&attachment.id).unwrap().unwrap().0,
            attachment
        );
        let persisted = store
            .events_after(&Scope::Thread("th_accept".into()), 0)
            .unwrap();
        assert_eq!(persisted.len(), 3);
        assert!(matches!(persisted[0].event, Event::QueueUpdated { .. }));
        assert!(matches!(
            persisted[1].event,
            Event::TurnStarted { turn: 1, .. }
        ));
        assert!(matches!(
            persisted[2].event,
            Event::UserMessage { turn: 1, .. }
        ));
        assert!(store.finish_queued_prompt(&prompt.id).unwrap());
    }

    #[test]
    fn failed_prompt_acceptance_rolls_back_every_related_row_and_event() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_accept_rollback");
        let attachment = trouve_protocol::Attachment {
            id: "at_accept_rollback".into(),
            name: "notes.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 5,
        };
        let prompt = trouve_protocol::QueuedPrompt {
            id: "qp_accept_rollback".into(),
            thread_id: "th_accept_rollback".into(),
            position: 1,
            content: "This transaction must roll back".into(),
            attachments: vec![attachment.clone()],
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let result = store.accept_prompt_with_events(
            PromptAcceptance {
                prompt,
                tools_enabled: true,
                attachments: vec![(attachment.clone(), "/tmp/at_accept_rollback.txt".into())],
                claim_prompt_id: Some("qp_accept_rollback".into()),
                expected_previous_turn: Some(99),
                staging_cleanup_claim: None,
            },
            vec![(
                Scope::Thread("th_accept_rollback".into()),
                Event::QueueUpdated {
                    prompts: Vec::new(),
                },
            )],
        );

        assert!(result.is_err());
        assert_eq!(store.last_turn("th_accept_rollback").unwrap(), 0);
        assert!(
            store
                .queued_prompt_thread("qp_accept_rollback")
                .unwrap()
                .is_none()
        );
        assert!(store.attachment(&attachment.id).unwrap().is_none());
        assert!(
            store
                .events_after(&Scope::Thread("th_accept_rollback".into()), 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn queued_prompt_priority_can_remain_visible_or_be_claimed() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_priority");
        let first = store.enqueue_prompt("th_priority", "first", &[]).unwrap();
        let second = store.enqueue_prompt("th_priority", "second", &[]).unwrap();
        let third = store.enqueue_prompt("th_priority", "third", &[]).unwrap();

        let prioritized = store
            .prioritize_queued_prompt(&third.id, false)
            .unwrap()
            .unwrap();
        assert_eq!(prioritized.position, 0);
        assert_eq!(
            store
                .queued_prompts("th_priority")
                .unwrap()
                .iter()
                .map(|prompt| prompt.content.as_str())
                .collect::<Vec<_>>(),
            ["third", "first", "second"]
        );

        let claimed = store
            .prioritize_queued_prompt(&second.id, true)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.content, "second");
        assert!(
            store
                .prioritize_queued_prompt(&second.id, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .queued_prompts("th_priority")
                .unwrap()
                .iter()
                .map(|prompt| prompt.content.as_str())
                .collect::<Vec<_>>(),
            ["third", "first"]
        );

        assert!(store.release_queued_prompt(&second.id).unwrap());
        assert_eq!(
            store
                .queued_prompts("th_priority")
                .unwrap()
                .iter()
                .map(|prompt| prompt.id.as_str())
                .collect::<Vec<_>>(),
            [second.id.as_str(), third.id.as_str(), first.id.as_str()]
        );
    }

    #[test]
    fn queued_prompt_attachment_changes_commit_rows_atomically() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_attachment_cleanup");
        let old = trouve_protocol::Attachment {
            id: "at_old".into(),
            name: "old.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 3,
        };
        let new = trouve_protocol::Attachment {
            id: "at_new".into(),
            name: "new.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 3,
        };
        store
            .add_attachment("th_attachment_cleanup", &old, "/tmp/at_old")
            .unwrap();
        let prompt = store
            .enqueue_prompt("th_attachment_cleanup", "old", std::slice::from_ref(&old))
            .unwrap();

        let removed = store
            .update_queued_prompt_attachments(
                &prompt.id,
                "new",
                std::slice::from_ref(&new),
                &[(new.clone(), "/tmp/at_new".into())],
                std::slice::from_ref(&old.id),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(removed.unwrap().attachment_paths, ["/tmp/at_old"]);
        assert!(store.attachment(&old.id).unwrap().is_none());
        assert!(store.attachment(&new.id).unwrap().is_some());
        assert_eq!(
            store.queued_prompts("th_attachment_cleanup").unwrap()[0].attachments,
            std::slice::from_ref(&new)
        );

        let (thread_id, removed) = store
            .delete_queued_prompt_attachments(&prompt.id)
            .unwrap()
            .unwrap();
        assert_eq!(thread_id, "th_attachment_cleanup");
        assert_eq!(removed.unwrap().attachment_paths, ["/tmp/at_new"]);
        assert!(store.attachment(&new.id).unwrap().is_none());
        assert!(
            store
                .queued_prompts("th_attachment_cleanup")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn steering_attachment_rollback_is_thread_scoped() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_attachment_cleanup");
        seed_thread(&store, "th_attachment_other");
        let owned = trouve_protocol::Attachment {
            id: "at_owned".into(),
            name: "owned.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 3,
        };
        let other = trouve_protocol::Attachment {
            id: "at_other".into(),
            name: "other.txt".into(),
            mime: "text/plain".into(),
            size_bytes: 3,
        };
        store
            .add_attachment("th_attachment_cleanup", &owned, "/tmp/at_owned")
            .unwrap();
        store
            .add_attachment("th_attachment_other", &other, "/tmp/at_other")
            .unwrap();

        let removed = store
            .remove_attachments(
                "th_attachment_cleanup",
                &[owned.id.clone(), other.id.clone()],
            )
            .unwrap();
        assert_eq!(removed, ["/tmp/at_owned"]);
        assert!(store.attachment(&owned.id).unwrap().is_none());
        assert!(store.attachment(&other.id).unwrap().is_some());
    }

    #[test]
    fn spawned_thread_insert_never_exposes_an_unparented_child() {
        let store = Store::open_in_memory().unwrap();
        seed_thread(&store, "th_spawn_parent");
        let child = Thread {
            id: "th_spawn_child".into(),
            session_id: "se_q".into(),
            parent_thread_id: Some("th_spawn_parent".into()),
            title: None,
            mode: "code".into(),
            model: "p/m".into(),
            model_options: serde_json::Map::new(),
            permission_mode: PermissionMode::Ask,
            created_at: chrono::Utc::now(),
            spawned: true,
            todos: Vec::new(),
        };
        store
            .insert_spawned_thread(&child, &serde_json::Map::new(), "th_spawn_parent", "thread")
            .unwrap();
        let loaded = store.thread(&child.id).unwrap().unwrap();
        assert!(loaded.spawned);
        assert_eq!(loaded.parent_thread_id.as_deref(), Some("th_spawn_parent"));

        let orphan = Thread {
            id: "th_spawn_orphan".into(),
            ..child
        };
        assert!(
            store
                .insert_spawned_thread(
                    &orphan,
                    &serde_json::Map::new(),
                    "th_missing_parent",
                    "thread",
                )
                .is_err()
        );
        assert!(store.thread(&orphan.id).unwrap().is_none());
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
            thinking_level: Some("high".into()),
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
        assert_eq!(listed[0].thinking_level.as_deref(), Some("high"));
        assert_eq!(listed[0].permission_mode, PermissionMode::Yolo);

        // Edit: rename + disable clears the next fire time.
        let mut edited = auto.clone();
        edited.name = "Morning triage".into();
        edited.enabled = false;
        edited.next_run_at = None;
        edited.thinking_level = Some("max".into());
        edited.permission_mode = PermissionMode::AllowList;
        assert!(store.update_automation(&edited).unwrap());
        let got = store.automation("auto_1").unwrap().unwrap();
        assert_eq!(got.name, "Morning triage");
        assert!(!got.enabled);
        assert!(got.next_run_at.is_none());
        assert_eq!(got.thinking_level.as_deref(), Some("max"));
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
            parent_thread_id: None,
            title: None,
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
        assert_eq!(store.next_checkpoint_seq("se_1").unwrap(), 3);
        // Simulate undo to seq 0, then a new checkpoint: seq 1-2 replaced.
        store.set_undo_pos("se_1", Some(0)).unwrap();
        assert_eq!(store.next_checkpoint_seq("se_1").unwrap(), 1);
        let replacement = CheckpointRow {
            id: "cp_new".into(),
            session_id: "se_1".into(),
            thread_id: None,
            turn: 9,
            seq: 0,
            commit_hash: "c1b".into(),
        };
        let envelope = store
            .append_checkpoint_with_event(
                &replacement,
                Scope::Session("se_1".into()),
                Event::CheckpointCreated {
                    checkpoint_id: replacement.id.clone(),
                    thread_id: "th_1".into(),
                    turn: replacement.turn,
                    commit: replacement.commit_hash.clone(),
                },
            )
            .unwrap();
        assert!(matches!(
            envelope.event,
            Event::CheckpointCreated { checkpoint_id, .. } if checkpoint_id == "cp_new"
        ));
        assert_eq!(store.latest_checkpoint_seq("se_1").unwrap(), Some(1));
        assert_eq!(store.undo_pos("se_1").unwrap(), None);
        assert_eq!(
            store.checkpoint_at("se_1", 1).unwrap().unwrap().commit_hash,
            "c1b"
        );
        let checkpoint = store.checkpoint("cp_new").unwrap().unwrap();
        assert_eq!(checkpoint.session_id, "se_1");
        assert_eq!(checkpoint.turn, 9);
        assert_eq!(checkpoint.seq, 1);
        assert!(
            store
                .events_after(&Scope::Session("se_1".into()), 0)
                .unwrap()
                .iter()
                .any(|event| matches!(
                    &event.event,
                    Event::CheckpointCreated { checkpoint_id, .. } if checkpoint_id == "cp_new"
                ))
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
    fn code_review_title_backfills_are_applied_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE store_migrations (id TEXT PRIMARY KEY, applied_at TEXT NOT NULL);
             CREATE TABLE code_review_findings (title TEXT NOT NULL, body TEXT NOT NULL);
             CREATE TABLE code_review_candidate_rejections (title TEXT NOT NULL, body TEXT NOT NULL);
             INSERT INTO code_review_findings VALUES ('', 'Finding body');
             INSERT INTO code_review_candidate_rejections VALUES ('', 'Rejection body');",
        )
        .unwrap();

        backfill_code_review_titles(&mut conn).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM code_review_findings", [], |row| {
                row.get(0)
            })
            .unwrap();
        let marker_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM store_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Finding body");
        assert_eq!(marker_count, 2);

        conn.execute("UPDATE code_review_findings SET title = ''", [])
            .unwrap();
        backfill_code_review_titles(&mut conn).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM code_review_findings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "");
    }

    #[test]
    fn persona_deletion_claims_are_fenced_and_back_off_durably() {
        let store = Store::open_in_memory().unwrap();
        store.begin_persona_deletion("custom").unwrap();

        let first = store.claim_next_persona_deletion().unwrap().unwrap();
        assert_eq!(first.id, "custom");
        assert_eq!(first.attempts, 0);
        assert!(store.claim_next_persona_deletion().unwrap().is_none());
        assert!(store.complete_persona_deletion("custom").is_err());
        assert!(store.persona_deletion_pending("custom").unwrap());

        store.fail_claimed_persona_deletion(&first).unwrap();
        assert!(store.claim_next_persona_deletion().unwrap().is_none());
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE persona_cleanup_intents SET next_attempt_at = ?2
                 WHERE persona_id = ?1",
                params![
                    "custom",
                    (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()
                ],
            )
            .unwrap();

        let second = store.claim_next_persona_deletion().unwrap().unwrap();
        assert_eq!(second.attempts, 1);
        assert_ne!(second.token, first.token);
        assert!(store.complete_claimed_persona_deletion(&first).is_err());
        store.complete_claimed_persona_deletion(&second).unwrap();
        assert!(!store.persona_deletion_pending("custom").unwrap());
    }

    #[test]
    fn persona_reference_cleanup_is_durable_and_preserves_unrelated_selection() {
        let store = Store::open_in_memory().unwrap();
        let request = trouve_protocol::UpdateCodeReviewRepositoryRequest {
            installation_id: 7,
            repository: "acme/widgets".into(),
            mode: trouve_protocol::CodeReviewMode::Manual,
            model: Some("openai/reviewer".into()),
            coordinator_thinking_level: None,
            router_model: None,
            router_thinking_level: None,
            prompt: "keep this".into(),
            reviewer_ids: Some(vec!["custom".into()]),
            routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Manual),
            semantic_routing: Some(false),
            included_reviewer_ids: Some(vec!["custom".into(), "reliability".into()]),
            excluded_reviewer_ids: Some(vec!["custom".into()]),
            reviewer_overrides: Some(vec![trouve_protocol::ReviewerOverride {
                reviewer_id: "custom".into(),
                model: None,
                thinking_level: None,
                prompt_mode: trouve_protocol::ReviewerPromptMode::Append,
                prompt: "custom prompt".into(),
            }]),
        };
        store.update_code_review_repository(&request).unwrap();
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/empty".into(),
                mode: trouve_protocol::CodeReviewMode::Manual,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: "preserve empty selection".into(),
                reviewer_ids: Some(Vec::new()),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Additive),
                semantic_routing: Some(false),
                included_reviewer_ids: Some(vec!["custom".into()]),
                excluded_reviewer_ids: Some(vec!["security".into()]),
                reviewer_overrides: None,
            })
            .unwrap();
        store
            .update_code_review_repository(&trouve_protocol::UpdateCodeReviewRepositoryRequest {
                installation_id: 7,
                repository: "acme/unrelated".into(),
                mode: trouve_protocol::CodeReviewMode::Manual,
                model: None,
                coordinator_thinking_level: None,
                router_model: None,
                router_thinking_level: None,
                prompt: String::new(),
                reviewer_ids: Some(vec!["reliability".into()]),
                routing_mode: Some(trouve_protocol::CodeReviewRoutingMode::Additive),
                semantic_routing: Some(false),
                included_reviewer_ids: Some(vec!["custom".into()]),
                excluded_reviewer_ids: None,
                reviewer_overrides: None,
            })
            .unwrap();

        store.begin_persona_deletion("custom").unwrap();
        assert!(store.persona_deletion_pending("custom").unwrap());
        assert_eq!(store.pending_persona_deletions().unwrap(), ["custom"]);
        let unchanged = store
            .list_code_review_repositories()
            .unwrap()
            .into_iter()
            .find(|repository| repository.repository == "acme/widgets")
            .unwrap();
        assert_eq!(unchanged.reviewer_ids, vec!["custom"]);
        assert_eq!(
            unchanged.included_reviewer_ids,
            vec!["custom", "reliability"]
        );
        assert_eq!(unchanged.reviewer_overrides.len(), 1);

        store.complete_persona_deletion("custom").unwrap();
        assert!(!store.persona_deletion_pending("custom").unwrap());
        let repositories = store.list_code_review_repositories().unwrap();
        let cleaned = repositories
            .iter()
            .find(|repository| repository.repository == "acme/widgets")
            .unwrap();
        assert_eq!(
            cleaned.reviewer_ids,
            crate::reviewers::default_reviewer_ids()
        );
        assert_eq!(cleaned.included_reviewer_ids, vec!["reliability"]);
        assert!(cleaned.excluded_reviewer_ids.is_empty());
        assert!(cleaned.reviewer_overrides.is_empty());
        assert_eq!(cleaned.prompt, "keep this");
        assert_eq!(cleaned.model.as_deref(), Some("openai/reviewer"));

        let empty = repositories
            .iter()
            .find(|repository| repository.repository == "acme/empty")
            .unwrap();
        assert!(empty.reviewer_ids.is_empty());
        assert!(empty.included_reviewer_ids.is_empty());
        assert_eq!(empty.excluded_reviewer_ids, ["security"]);
        assert_eq!(empty.prompt, "preserve empty selection");
        let unrelated = repositories
            .iter()
            .find(|repository| repository.repository == "acme/unrelated")
            .unwrap();
        assert_eq!(unrelated.reviewer_ids, ["reliability"]);
        assert!(unrelated.included_reviewer_ids.is_empty());
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
        assert_eq!(running.job.review_watermark_sha, queued.review_base_sha);
        let effective_base = "3333333333333333333333333333333333333333";
        assert!(
            store
                .set_code_review_job_review_base(&queued.id, effective_base)
                .unwrap()
        );
        let rebased = store.code_review_job(&queued.id).unwrap().unwrap().job;
        assert_eq!(rebased.review_base_sha, effective_base);
        assert_eq!(rebased.review_watermark_sha, queued.review_base_sha);
        assert!(
            !store
                .prepare_code_review_batch_snapshot(&queued.id, "digest-a")
                .unwrap()
                .changed
        );
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
    fn migrations_add_indexed_turn_boundaries_to_legacy_thread_view_items() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE thread_view_items (
               thread_id TEXT NOT NULL,
               item_index INTEGER NOT NULL,
               item TEXT NOT NULL,
               PRIMARY KEY (thread_id, item_index)
             );",
        )
        .unwrap();
        assert!(!SCHEMA.contains("thread_view_items_turn_start"));

        conn.execute_batch(SCHEMA).unwrap();
        apply_migrations(&mut conn).unwrap();

        let turn_start_columns = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(thread_view_items)")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|column| column == "turn_start")
                .count()
        };
        assert_eq!(turn_start_columns, 1);
        let indexed = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'thread_view_items_turn_start'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);

        apply_migrations(&mut conn).unwrap();
    }

    #[test]
    fn migrations_upgrade_findings_tables_that_predate_collapse_columns() {
        // A database created before collapse_pending existed: SCHEMA's
        // CREATE TABLE IF NOT EXISTS will not touch it, so every collapse
        // column and the partial index must arrive via MIGRATIONS — and the
        // index must not appear in SCHEMA, where it would run before the
        // column exists and abort open.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE code_review_findings (
               id TEXT PRIMARY KEY,
               job_id TEXT NOT NULL,
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
             INSERT INTO code_review_findings
                    (id, job_id, path, line, side, severity, body, status,
                     github_comment_id, created_at)
             VALUES ('rvf-historic', 'job', 'src/lib.rs', 3, 'RIGHT', 'medium',
                     'closed with a published comment', 'fixed', 9001, '2026-01-01'),
                    ('rvf-commentless', 'job', 'src/lib.rs', 4, 'RIGHT', 'medium',
                     'closed without a comment', 'fixed', NULL, '2026-01-01'),
                    ('rvf-open', 'job', 'src/lib.rs', 5, 'RIGHT', 'medium',
                     'still open', 'open', 9002, '2026-01-01');",
        )
        .unwrap();
        assert!(!SCHEMA.contains("code_review_findings_collapse_pending"));

        conn.execute_batch(SCHEMA).unwrap();
        apply_migrations(&mut conn).unwrap();

        let indexed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'code_review_findings_collapse_pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 1);

        // The one-time backfill arms only historical closed findings that
        // have a published comment; open or comment-less rows stay unarmed.
        fn armed(conn: &Connection, id: &str) -> i64 {
            conn.query_row(
                "SELECT collapse_pending FROM code_review_findings WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
        }
        assert_eq!(armed(&conn, "rvf-historic"), 1);
        assert_eq!(armed(&conn, "rvf-commentless"), 0);
        assert_eq!(armed(&conn, "rvf-open"), 0);

        // Re-running migrations (a later boot) must not re-arm cleared rows:
        // the backfill commits atomically with its store_migrations marker
        // and is skipped once the marker exists.
        conn.execute(
            "UPDATE code_review_findings SET collapse_pending = 0 WHERE id = 'rvf-historic'",
            [],
        )
        .unwrap();
        apply_migrations(&mut conn).unwrap();
        assert_eq!(armed(&conn, "rvf-historic"), 0);
    }

    #[test]
    fn collapse_arming_follows_the_row_not_the_snapshot() {
        let store = Store::open_in_memory().unwrap();
        let job = enqueue_backoff_test_job(&store);
        let findings = store
            .save_code_review_result(
                &job.id,
                "summary",
                "prompt",
                1,
                &[NewCodeReviewFinding {
                    path: "src/lib.rs".into(),
                    line: 3,
                    side: "RIGHT".into(),
                    severity: "medium".into(),
                    confidence: "high".into(),
                    title: "Test finding".into(),
                    body: "finding".into(),
                    prompt_for_agents: "fix".into(),
                    sources: Vec::new(),
                }],
                &[],
            )
            .unwrap();
        let id = findings[0].id.clone();
        let due = chrono::Utc::now() + chrono::Duration::hours(2);

        // Closed before any comment was published: nothing to collapse yet.
        assert!(store.resolve_code_review_finding(&id, "fixed").unwrap());
        assert!(
            store
                .pending_code_review_thread_collapses(due, 16, &[])
                .unwrap()
                .is_empty()
        );

        // Stale backoff accrued while the row had nothing to collapse (e.g.
        // a cleanup pass deferring on a listing failure) must not delay a
        // freshly armed collapse.
        store.defer_code_review_thread_collapse(&id).unwrap();
        store.defer_code_review_thread_collapse(&id).unwrap();

        // A comment published by a concurrent round after the close re-arms
        // the collapse with reset retry metadata: due immediately, not after
        // the inherited delay.
        store
            .update_code_review_finding_publication(&id, Some(9001), "https://example", None)
            .unwrap();
        let pending = store
            .pending_code_review_thread_collapses(chrono::Utc::now(), 16, &[])
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].3.id, id);

        // A clear guarded on a stale comment id is a no-op: the re-armed
        // work survives a pass that snapshotted the row before publication.
        store.clear_code_review_thread_collapse(&id, None).unwrap();
        assert_eq!(
            store
                .pending_code_review_thread_collapses(due, 16, &[])
                .unwrap()
                .len(),
            1
        );
        store
            .clear_code_review_thread_collapse(&id, Some(9001))
            .unwrap();
        assert!(
            store
                .pending_code_review_thread_collapses(due, 16, &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn collapse_backoff_doubles_from_one_minute_and_caps_at_one_hour() {
        let store = Store::open_in_memory().unwrap();
        let job = enqueue_backoff_test_job(&store);
        let findings = store
            .save_code_review_result(
                &job.id,
                "summary",
                "prompt",
                1,
                &[NewCodeReviewFinding {
                    path: "src/lib.rs".into(),
                    line: 3,
                    side: "RIGHT".into(),
                    severity: "medium".into(),
                    confidence: "high".into(),
                    title: "Test finding".into(),
                    body: "finding".into(),
                    prompt_for_agents: "fix".into(),
                    sources: Vec::new(),
                }],
                &[],
            )
            .unwrap();
        let id = findings[0].id.clone();
        store
            .update_code_review_finding_publication(&id, Some(9001), "https://example", None)
            .unwrap();
        assert!(store.resolve_code_review_finding(&id, "fixed").unwrap());

        let next_attempt = |store: &Store| -> chrono::DateTime<chrono::Utc> {
            store
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT collapse_next_attempt_at FROM code_review_findings WHERE id = ?1",
                    params![&id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
                .parse()
                .unwrap()
        };

        // First failure: due in one minute — not before, not much after.
        store.defer_code_review_thread_collapse(&id).unwrap();
        let first = next_attempt(&store);
        let elapsed = first - chrono::Utc::now();
        assert!(elapsed > chrono::Duration::seconds(55), "{elapsed}");
        assert!(elapsed <= chrono::Duration::seconds(61), "{elapsed}");

        // The delay doubles per failure and stops growing at one hour.
        for _ in 0..6 {
            store.defer_code_review_thread_collapse(&id).unwrap();
        }
        let capped = next_attempt(&store);
        let elapsed = capped - chrono::Utc::now();
        assert!(elapsed > chrono::Duration::minutes(59), "{elapsed}");
        assert!(elapsed <= chrono::Duration::minutes(61), "{elapsed}");

        store.defer_code_review_thread_collapse(&id).unwrap();
        let still_capped = next_attempt(&store) - chrono::Utc::now();
        assert!(
            still_capped <= chrono::Duration::minutes(61),
            "{still_capped}"
        );

        // A requeue reschedules for the next tick without counting a
        // failure: the attempt count is untouched, so a later real failure
        // resumes the capped backoff instead of restarting from one minute.
        store.requeue_code_review_thread_collapse(&id).unwrap();
        let requeued = next_attempt(&store) - chrono::Utc::now();
        assert!(requeued > chrono::Duration::seconds(55), "{requeued}");
        assert!(requeued <= chrono::Duration::seconds(61), "{requeued}");
        store.defer_code_review_thread_collapse(&id).unwrap();
        let after_requeue = next_attempt(&store) - chrono::Utc::now();
        assert!(
            after_requeue > chrono::Duration::minutes(59),
            "{after_requeue}"
        );
    }

    fn enqueue_backoff_test_job(store: &Store) -> trouve_protocol::CodeReviewJob {
        store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:backoff".into(),
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
                reviewers: crate::reviewers::built_in_reviewers()
                    .into_iter()
                    .take(1)
                    .collect(),
                routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
                semantic_routing: false,
                included_reviewer_ids: Vec::new(),
                excluded_reviewer_ids: Vec::new(),
                config_hash: "config".into(),
            })
            .unwrap()
            .unwrap()
    }

    #[test]
    fn watermark_backfill_uses_last_published_head_not_effective_base() {
        let store = Store::open_in_memory().unwrap();
        let job = enqueue_backoff_test_job(&store);
        let published_head = "3333333333333333333333333333333333333333";
        let fallback_base = "4444444444444444444444444444444444444444";
        store
            .mark_code_review_published(
                "acme/widgets",
                42,
                "1111111111111111111111111111111111111111",
                published_head,
            )
            .unwrap();
        {
            let mut conn = store.conn.lock().unwrap();
            conn.execute(
                "UPDATE code_review_jobs
                 SET review_base_sha = ?2, review_watermark_sha = '' WHERE id = ?1",
                params![job.id, fallback_base],
            )
            .unwrap();
            conn.execute(
                "DELETE FROM store_migrations WHERE id = 'code-review-watermark-backfill-v1'",
                [],
            )
            .unwrap();
            backfill_code_review_watermarks(&mut conn).unwrap();
        }

        let migrated = store.code_review_job(&job.id).unwrap().unwrap().job;
        assert_eq!(migrated.review_base_sha, fallback_base);
        assert_eq!(migrated.review_watermark_sha, published_head);
    }

    #[test]
    fn draft_stale_dedupe_normalization_runs_on_reopen_and_is_idempotent() {
        let data = tempfile::tempdir().unwrap();
        let database = data.path().join("draft-stale.sqlite3");
        let legacy = {
            let store = Store::open(&database).unwrap();
            let legacy = enqueue_backoff_test_job(&store);
            assert_eq!(
                store.claim_code_review_job().unwrap().unwrap().job.id,
                legacy.id
            );
            assert!(
                store
                    .finish_code_review_job(
                        &legacy.id,
                        "stale",
                        "",
                        "stale: pull request is a draft; automatic review stopped",
                    )
                    .unwrap()
            );
            legacy
        };

        let normalized_key = {
            let store = Store::open(&database).unwrap();
            let normalized_key = store
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT dedupe_key FROM code_review_jobs WHERE id = ?1",
                    params![legacy.id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap();
            let replacement = enqueue_backoff_test_job(&store);
            assert_ne!(replacement.id, legacy.id);
            normalized_key
        };

        let store = Store::open(&database).unwrap();
        let reopened_key = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT dedupe_key FROM code_review_jobs WHERE id = ?1",
                params![legacy.id],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(reopened_key, normalized_key);
    }

    #[test]
    fn a_latest_superseded_attempt_does_not_resurrect_an_older_success() {
        let store = Store::open_in_memory().unwrap();
        let job = enqueue_backoff_test_job(&store);
        store.claim_code_review_job().unwrap().unwrap();
        let reviewer = crate::reviewers::built_in_reviewers().remove(0);
        let create = |prompt: &str| {
            store
                .create_code_review_task(&NewCodeReviewTask {
                    job_id: job.id.clone(),
                    role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                    reviewer_id: Some(reviewer.id.clone()),
                    reviewer_name: reviewer.name.clone(),
                    batch_index: 0,
                    batch_count: 1,
                    model: Some("provider/default".into()),
                    prompt: prompt.into(),
                })
                .unwrap()
        };
        let old = create("old prompt");
        store
            .start_code_review_task(&old.id, "session", "thread", "provider/default")
            .unwrap();
        store
            .finish_code_review_task(&old.id, "succeeded", "old output", 0, "")
            .unwrap();
        let replacement = create("new prompt");
        store
            .supersede_code_review_tasks_for_prompt_change(
                &job.id,
                std::slice::from_ref(&replacement.id),
                1,
            )
            .unwrap();

        assert!(
            store
                .latest_code_review_reviewer_tasks(&job.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.completed_code_review_personas(&job.id).unwrap(), 0);
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
        assert!(
            !store
                .prepare_code_review_batch_snapshot(&queued.id, "digest-a")
                .unwrap()
                .changed
        );
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
        assert_eq!(
            store.code_review_routing_decisions(&queued.id).unwrap(),
            routing_decisions
        );
        assert!(
            !store
                .prepare_code_review_batch_snapshot(&queued.id, "digest-a")
                .unwrap()
                .changed
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
                    confidence: "high".into(),
                    title: "Ignored error".into(),
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
                    confidence: "high".into(),
                    title: "Simplify branch".into(),
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
        assert_eq!(detail.candidate_rejections[0].confidence, "high");
        assert_eq!(
            detail.candidate_rejections[0].reason,
            "This is a non-actionable style preference."
        );
        assert_eq!(detail.personas[0].confirmed_issue_count, 1);
        assert_eq!(detail.personas[0].status, "succeeded");
        assert_eq!(detail.findings[0].sources[0].task_id, task.id);
        assert_eq!(detail.findings[0].confidence, "high");
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

    #[tokio::test]
    async fn changed_batch_digest_clears_routing_and_supersedes_every_old_task() {
        let store = Store::open_in_memory().unwrap();
        let reviewer = crate::reviewers::built_in_reviewers().remove(0);
        let queued = store
            .enqueue_code_review_job(&NewCodeReviewJob {
                dedupe_key: "acme/widgets#42:changed-batches".into(),
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
                prompt: String::new(),
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
        assert!(
            !store
                .prepare_code_review_batch_snapshot(&queued.id, "digest-a")
                .unwrap()
                .changed
        );
        let routing = vec![trouve_protocol::CodeReviewRoutingDecision {
            batch_index: 0,
            reviewer_id: reviewer.id.clone(),
            reviewer_name: reviewer.name.clone(),
            selected: true,
            reasons: Vec::new(),
        }];
        store
            .save_code_review_routing_decisions(&queued.id, &routing)
            .unwrap();

        let crash_recovery = store
            .prepare_code_review_batch_snapshot(&queued.id, "digest-b")
            .unwrap();
        assert!(crash_recovery.changed);
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO code_review_pending_events (job_id, payload, created_at)
                 VALUES (?1, 'not-json', ?2)",
                params![queued.id, chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        let pending = store.pending_code_review_events(&queued.id).unwrap();
        assert!(
            store
                .code_review_jobs_with_pending_events(10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .conn
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM code_review_pending_events
                     WHERE job_id = ?1 AND payload = 'not-json'",
                    [queued.id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert!(pending.iter().any(|pending| matches!(
            &pending.event,
            Event::CodeReviewRoutingUpdated { routing_decisions, .. } if routing_decisions.is_empty()
        )));
        assert!(
            !store
                .prepare_code_review_batch_snapshot(&queued.id, "digest-b")
                .unwrap()
                .changed
        );
        assert_eq!(
            store.pending_code_review_events(&queued.id).unwrap().len(),
            pending.len(),
            "matching-digest recovery must retain undelivered transition events"
        );
        assert_eq!(
            store
                .flush_pending_code_review_events(&queued.id)
                .await
                .unwrap()
                .len(),
            pending.len()
        );
        let stale_events = serialize_events(
            Scope::CodeReviewJob(queued.id.clone()),
            pending
                .iter()
                .map(|pending| pending.event.clone())
                .collect(),
        )
        .unwrap();
        let stale_ids = pending.iter().map(|pending| pending.id).collect::<Vec<_>>();
        let stale_insert = insert_event_batch(
            &store.conn.lock().unwrap(),
            &stale_events,
            stale_events.len(),
            stale_ids.iter().copied(),
        )
        .unwrap();
        assert!(stale_insert.skipped);
        assert!(stale_insert.published.is_empty());
        let (reply, reply_rx) = std::sync::mpsc::sync_channel(1);
        store
            .append_tx
            .send(AppendRequest {
                events: stale_events,
                code_review_outbox_ids: stale_ids,
                isolated: false,
                reply: AppendReply::Sync(reply),
                queued_at: std::time::Instant::now(),
            })
            .unwrap();
        assert!(reply_rx.recv().unwrap().unwrap().is_empty());
        assert!(
            store
                .flush_pending_code_review_events(&queued.id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .code_review_jobs_with_pending_events(10)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .events_after(&Scope::CodeReviewJob(queued.id.clone()), 0)
                .unwrap()
                .iter()
                .any(|envelope| matches!(
                    &envelope.event,
                    Event::CodeReviewRoutingUpdated { routing_decisions, .. }
                        if routing_decisions.is_empty()
                ))
        );
        assert!(
            store
                .code_review_routing_decisions(&queued.id)
                .unwrap()
                .is_empty()
        );

        store
            .save_code_review_routing_decisions(&queued.id, &routing)
            .unwrap();
        let tasks = [0, 3]
            .into_iter()
            .map(|batch_index| {
                store
                    .create_code_review_task(&NewCodeReviewTask {
                        job_id: queued.id.clone(),
                        role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                        reviewer_id: Some(reviewer.id.clone()),
                        reviewer_name: reviewer.name.clone(),
                        batch_index,
                        batch_count: 4,
                        model: Some("provider/default".into()),
                        prompt: format!("batch {batch_index}"),
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        store
            .start_code_review_task(&tasks[0].id, "session", "thread", "provider/default")
            .unwrap()
            .unwrap();
        let failed = store
            .finish_code_review_task(&tasks[0].id, "failed", "", 0, "original failure")
            .unwrap()
            .unwrap();
        let changed = store
            .prepare_code_review_batch_snapshot(&queued.id, "digest-c")
            .unwrap();
        assert!(changed.changed);
        let pending = store.pending_code_review_events(&queued.id).unwrap();
        let superseded_tasks = pending
            .iter()
            .filter_map(|pending| match &pending.event {
                Event::CodeReviewTaskUpdated { task, .. } => Some(task.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(superseded_tasks.len(), 2);
        assert!(
            superseded_tasks
                .iter()
                .all(|task| task.status == "superseded")
        );
        let retained_failure = superseded_tasks
            .iter()
            .find(|task| task.id == failed.id)
            .unwrap();
        assert_eq!(retained_failure.error, "original failure");
        assert_eq!(retained_failure.completed_at, failed.completed_at);
        assert!(
            store
                .latest_code_review_reviewer_tasks(&queued.id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.completed_code_review_personas(&queued.id).unwrap(), 0);
        assert!(
            store
                .code_review_job_detail(&queued.id)
                .unwrap()
                .unwrap()
                .personas
                .is_empty()
        );
        assert!(
            store
                .code_review_stats(trouve_protocol::CodeReviewStatsRange::All, None)
                .unwrap()
                .personas
                .is_empty()
        );

        let prompt_task = store
            .create_code_review_task(&NewCodeReviewTask {
                job_id: queued.id.clone(),
                role: trouve_protocol::CodeReviewTaskRole::Reviewer,
                reviewer_id: Some(reviewer.id.clone()),
                reviewer_name: reviewer.name.clone(),
                batch_index: 0,
                batch_count: 1,
                model: Some("provider/default".into()),
                prompt: "old prompt".into(),
            })
            .unwrap();
        store
            .start_code_review_task(
                &prompt_task.id,
                "session",
                "prompt-thread",
                "provider/default",
            )
            .unwrap()
            .unwrap();
        let prompt_task = store
            .finish_code_review_task(&prompt_task.id, "succeeded", "old output", 0, "")
            .unwrap()
            .unwrap();
        store
            .set_code_review_job_progress(&queued.id, 1, 1)
            .unwrap();

        assert_eq!(
            store
                .supersede_code_review_tasks_for_prompt_change(
                    &queued.id,
                    std::slice::from_ref(&prompt_task.id),
                    1,
                )
                .unwrap(),
            0
        );
        let retired = store
            .code_review_task(&queued.id, &prompt_task.id)
            .unwrap()
            .unwrap();
        assert_eq!(retired.status, "superseded");
        assert_eq!(retired.completed_at, prompt_task.completed_at);
        assert_eq!(
            store
                .code_review_job(&queued.id)
                .unwrap()
                .unwrap()
                .job
                .progress
                .completed_reviewers,
            0
        );
        let pending = store.pending_code_review_events(&queued.id).unwrap();
        assert!(pending.iter().any(|pending| matches!(
            &pending.event,
            Event::CodeReviewTaskUpdated { task, .. } if task.id == prompt_task.id
        )));
        assert!(pending.iter().any(|pending| matches!(
            &pending.event,
            Event::CodeReviewProgressUpdated { progress, .. }
                if progress.completed_reviewers == 0
        )));
        assert!(
            store
                .finish_code_review_job(&queued.id, "failed", "", "test complete")
                .unwrap()
        );
        assert_eq!(
            store.code_review_jobs_with_pending_events(10).unwrap(),
            vec![queued.id.clone()]
        );
        let stale = store
            .prepare_code_review_batch_snapshot(&queued.id, "digest-d")
            .unwrap_err();
        assert!(stale.to_string().starts_with("stale:"));
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
        assert_eq!(interrupted.model_elapsed_ms, 1_200);
        assert!(interrupted.model_started_at.is_none());
        let retry = detail
            .tasks
            .iter()
            .find(|task| task.status == "queued")
            .unwrap();
        assert_eq!(retry.batch_index, 1);
        assert_ne!(retry.id, interrupted.id);
    }

    #[test]
    fn review_publication_claim_blocks_cancellation_and_recovers_for_retry() {
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

        store.recover_code_review_jobs().unwrap();
        let recovered = store.code_review_job(&job.id).unwrap().unwrap();
        assert_eq!(recovered.job.status, "failed");
        assert!(!recovered.publication_claimed);
        assert!(!recovered.publication_accepted);
        let retry = store.retry_code_review_job(&job.id).unwrap().unwrap();
        assert_eq!(
            store.claim_code_review_job().unwrap().unwrap().job.id,
            retry.id
        );
        assert!(store.claim_code_review_publication(&retry.id).unwrap());
        assert!(
            store
                .mark_code_review_publication_accepted(&retry.id)
                .unwrap()
        );

        store.recover_code_review_jobs().unwrap();
        let accepted = store.code_review_job(&retry.id).unwrap().unwrap();
        assert_eq!(accepted.job.status, "succeeded");
        assert!(accepted.publication_claimed);
        assert!(accepted.publication_accepted);
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

    #[test]
    fn draft_pull_supersedes_only_automatic_active_jobs() {
        let store = Store::open_in_memory().unwrap();
        let enqueue = |suffix: &str, trigger: &str| {
            store
                .enqueue_code_review_job(&NewCodeReviewJob {
                    dedupe_key: format!("acme/widgets#42:{suffix}"),
                    installation_id: 7,
                    repository: "acme/widgets".into(),
                    pull_number: 42,
                    pull_title: "Ship widgets".into(),
                    pull_url: "https://github.com/acme/widgets/pull/42".into(),
                    head_sha: "head".into(),
                    review_base_sha: "base".into(),
                    base_ref: "base".into(),
                    head_ref: "ship".into(),
                    scope: trouve_protocol::CodeReviewJobScope::Incremental,
                    trigger: trigger.into(),
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
                .unwrap()
        };

        let running_automatic = enqueue("automatic-running", "automatic");
        assert_eq!(
            store.claim_code_review_job().unwrap().unwrap().job.id,
            running_automatic.id
        );
        let publishing_automatic = enqueue("automatic-publishing", "automatic");
        assert_eq!(
            store.claim_code_review_job().unwrap().unwrap().job.id,
            publishing_automatic.id
        );
        assert!(
            store
                .claim_code_review_publication(&publishing_automatic.id)
                .unwrap()
        );
        let queued_automatic = enqueue("automatic-queued", "automatic");
        let queued_manual = enqueue("manual-queued", "manual");

        let mut superseded = store
            .supersede_automatic_code_review_jobs_for_draft("acme/widgets", 42)
            .unwrap();
        superseded.sort();
        let mut expected = vec![running_automatic.id.clone(), queued_automatic.id.clone()];
        expected.sort();
        assert_eq!(superseded, expected);
        for id in [&running_automatic.id, &queued_automatic.id] {
            let job = store.code_review_job(id).unwrap().unwrap().job;
            assert_eq!(job.status, "stale");
            assert_eq!(
                job.error,
                "pull request is a draft; automatic review stopped"
            );
        }
        assert_eq!(
            store
                .code_review_job(&publishing_automatic.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "running"
        );
        assert_eq!(
            store
                .code_review_job(&queued_manual.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "queued"
        );

        let requeued_automatic = enqueue("automatic-queued", "automatic");
        assert_ne!(requeued_automatic.id, queued_automatic.id);
        let retry = store
            .retry_code_review_job(&running_automatic.id)
            .unwrap()
            .unwrap();
        assert_eq!(retry.trigger, "retry");
        let superseded = store
            .supersede_automatic_code_review_jobs_for_draft("acme/widgets", 42)
            .unwrap();
        assert_eq!(superseded, vec![requeued_automatic.id]);
        assert_eq!(
            store
                .code_review_job(&retry.id)
                .unwrap()
                .unwrap()
                .job
                .status,
            "queued"
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
