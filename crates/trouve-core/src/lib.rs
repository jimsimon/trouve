//! Core engine of the trouve harness.
//!
//! Everything UI-visible flows through the event log ([`store::Store`]);
//! every side effect flows through the [`tools::ToolExecutor`] chokepoint;
//! all agent file operations happen inside a session's git worktree
//! ([`git`]). See `AGENTS.md` for the invariants.

pub mod automations;
pub mod config;
pub mod connectivity;
pub mod context;
pub mod engine;
pub mod git;
pub mod github;
pub mod local;
pub mod mcp;
pub mod modes;
pub mod permissions;
pub mod skills;
pub mod store;
pub mod terminal;
pub mod title;
pub mod title_model;
pub mod tools;

pub use engine::Engine;

/// Generate a prefixed unique id, e.g. `ws_1f3a…`.
pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}
