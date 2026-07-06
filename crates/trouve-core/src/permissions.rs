//! The permission layer (ADR 0004): ask / allow-list / yolo, plus pending
//! approval bookkeeping.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tokio::sync::oneshot;
use trouve_protocol::{ApprovalDecision, PermissionMode};

/// What the permission engine decided for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Run without asking.
    Allow,
    /// Emit an approval request and wait for the user.
    NeedsApproval,
    /// Never run (read-only mode attempting a mutation).
    Deny,
}

/// Derive the allow-list key for a call: file tools key on the tool name,
/// shell keys on the first token of the command so "always approve" for
/// `cargo test` covers future `cargo …` invocations but not `rm`. MCP tools
/// key on the server so one approval unlocks the server for the session.
pub fn allow_key(tool: &str, args: &serde_json::Value) -> String {
    if tool == "shell" {
        let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let first = cmd.split_whitespace().next().unwrap_or("");
        return format!("shell:{first}");
    }
    if let Some((server, _)) = crate::mcp::split_tool_name(tool) {
        return format!("mcp:{server}");
    }
    tool.to_string()
}

pub fn gate(
    mode: PermissionMode,
    mode_read_only: bool,
    tool_mutates: bool,
    allow_list: &HashSet<String>,
    key: &str,
) -> Gate {
    if !tool_mutates {
        return Gate::Allow;
    }
    if mode_read_only {
        return Gate::Deny;
    }
    // MCP servers require first-use approval per session even in yolo:
    // they are external code and a prompt-injection channel.
    if key.starts_with("mcp:") {
        return if allow_list.contains(key) {
            Gate::Allow
        } else {
            Gate::NeedsApproval
        };
    }
    match mode {
        PermissionMode::Yolo => Gate::Allow,
        PermissionMode::AllowList | PermissionMode::Ask if allow_list.contains(key) => Gate::Allow,
        _ => Gate::NeedsApproval,
    }
}

/// Pending approvals: one oneshot per outstanding call, plus the per-session
/// allow-list that `always_approve` decisions feed.
#[derive(Default)]
pub struct ApprovalHub {
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
    allow_lists: Mutex<HashMap<String, HashSet<String>>>,
}

impl ApprovalHub {
    pub fn request(&self, call_id: &str) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(call_id.to_string(), tx);
        rx
    }

    /// Returns false when the call id is unknown (already resolved or bogus).
    pub fn resolve(&self, call_id: &str, decision: ApprovalDecision) -> bool {
        match self.pending.lock().unwrap().remove(call_id) {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    pub fn allow_list(&self, session_id: &str) -> HashSet<String> {
        self.allow_lists
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn extend_allow_list(&self, session_id: &str, key: String) {
        self.allow_lists
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .insert(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates() {
        let empty = HashSet::new();
        let mut listed = HashSet::new();
        listed.insert("shell:cargo".to_string());

        // Reads always pass.
        assert_eq!(
            gate(PermissionMode::Ask, false, false, &empty, "read_file"),
            Gate::Allow
        );
        // Read-only mode denies mutations even in yolo.
        assert_eq!(
            gate(PermissionMode::Yolo, true, true, &empty, "shell:rm"),
            Gate::Deny
        );
        // Ask prompts for mutations.
        assert_eq!(
            gate(PermissionMode::Ask, false, true, &empty, "write_file"),
            Gate::NeedsApproval
        );
        // Allow-list passes listed keys, prompts for others.
        assert_eq!(
            gate(
                PermissionMode::AllowList,
                false,
                true,
                &listed,
                "shell:cargo"
            ),
            Gate::Allow
        );
        assert_eq!(
            gate(PermissionMode::AllowList, false, true, &listed, "shell:rm"),
            Gate::NeedsApproval
        );
        // Yolo runs everything (non-read-only).
        assert_eq!(
            gate(PermissionMode::Yolo, false, true, &empty, "shell:rm"),
            Gate::Allow
        );
        // MCP servers need first-use approval even in yolo …
        assert_eq!(
            gate(PermissionMode::Yolo, false, true, &empty, "mcp:jira"),
            Gate::NeedsApproval
        );
        // … and are unlocked once the server is on the session allow-list.
        let mut mcp_listed = HashSet::new();
        mcp_listed.insert("mcp:jira".to_string());
        assert_eq!(
            gate(PermissionMode::Yolo, false, true, &mcp_listed, "mcp:jira"),
            Gate::Allow
        );
    }

    #[test]
    fn allow_key_shapes() {
        assert_eq!(
            allow_key("shell", &serde_json::json!({"command": "cargo test --all"})),
            "shell:cargo"
        );
        assert_eq!(
            allow_key("write_file", &serde_json::json!({"path": "x"})),
            "write_file"
        );
        assert_eq!(
            allow_key("mcp__jira__create_issue", &serde_json::json!({})),
            "mcp:jira"
        );
    }

    #[tokio::test]
    async fn approval_roundtrip() {
        let hub = ApprovalHub::default();
        let rx = hub.request("call_1");
        assert!(hub.resolve("call_1", ApprovalDecision::Approve));
        assert_eq!(rx.await.unwrap(), ApprovalDecision::Approve);
        assert!(!hub.resolve("call_1", ApprovalDecision::Deny));
    }
}
