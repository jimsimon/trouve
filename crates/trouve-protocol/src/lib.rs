//! Wire types for the trouve harness protocol.
//!
//! This crate is the single source of truth for everything that crosses the
//! client/server boundary: request/response bodies, the event envelope, and
//! the OpenAPI schema derived from them. It contains **no logic** — see
//! `AGENTS.md` invariant 5.

pub mod events;
pub mod requests;

pub use events::*;
pub use requests::*;

/// Protocol version, independent of crate versions. Bump the minor for
/// additive changes and the major for breaking ones; the OpenAPI snapshot
/// test in `trouve-server` pins the serialized schema to this value.
// 0.2: added modes/diff/files inspection endpoints, GitHub PR endpoints,
// and the session.pr_opened event (all additive).
// 0.3: added provider configuration endpoints, session rename/archive
// (PATCH + session.updated), thread mode/model updates (PATCH +
// thread.updated), workspace branch listing, and context compaction
// events (all additive).
// 0.5: added the interactive question flow — question.requested /
// question.resolved events and POST /v1/questions (additive).
pub const PROTOCOL_VERSION: &str = "0.5";

pub type WorkspaceId = String;
pub type SessionId = String;
pub type ThreadId = String;
pub type CallId = String;
pub type CheckpointId = String;
