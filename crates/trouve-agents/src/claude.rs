//! Claude Code backend, driving the `claude` CLI in print mode.
//!
//! Each turn runs `claude -p <prompt> --output-format stream-json --verbose`
//! in the session worktree, resuming the vendor session with `--resume`.
//! Claude Code rotates its session id on every resume, so we re-persist the
//! id from each turn's `system/init` event.
//!
//! Permission mapping: `Yolo` → `--dangerously-skip-permissions`,
//! `ReadOnly` → `--permission-mode plan`, `Ask` → the trouve MCP bridge's
//! `approval_prompt` tool via `--permission-prompt-tool`, so headless print
//! mode routes permission requests to trouve's approval flow instead of
//! failing them.
//!
//! Login is an interactive TUI (`/login` inside `claude`); we detect
//! credentials but can't orchestrate the flow headlessly.

use std::process::Stdio;

use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    async_stream, binary_on_path, model, AgentBackend, BackendError, BackendEvent,
    BackendEventStream, BackendLogin, BackendPermission, BackendStatus, BackendTurn,
};

pub struct ClaudeBackend {
    id: String,
    command: String,
}

impl ClaudeBackend {
    pub fn new(id: impl Into<String>, command: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "claude".into()),
        }
    }
}

#[async_trait::async_trait]
impl AgentBackend for ClaudeBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Claude Code accepts model aliases; it maps them to the newest
        // matching model on the account's plan.
        vec![
            model(&self.id, "sonnet", "Claude Sonnet (Claude Code)", 200_000),
            model(&self.id, "opus", "Claude Opus (Claude Code)", 200_000),
            model(&self.id, "haiku", "Claude Haiku (Claude Code)", 200_000),
        ]
    }

    fn status(&self) -> BackendStatus {
        let home = dirs::home_dir();
        let has_credentials = home
            .map(|h| {
                h.join(".claude").join(".credentials.json").exists()
                    || h.join(".claude.json").exists()
            })
            .unwrap_or(false);
        BackendStatus {
            installed: binary_on_path(&self.command),
            has_credentials,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        Err(BackendError::Auth(
            "Claude Code login is interactive: run `claude` in a terminal and use /login".into(),
        ))
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let mut cmd = Command::new(&self.command);
        cmd.arg("-p")
            .arg(&turn.prompt)
            .args(["--output-format", "stream-json"])
            .arg("--verbose")
            .current_dir(&turn.worktree)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(session) = &turn.session {
            cmd.args(["--resume", session]);
        }
        if !turn.model.is_empty() {
            cmd.args(["--model", &turn.model]);
        }
        if let Some(instr) = &turn.instructions {
            cmd.args(["--append-system-prompt", instr]);
        }
        // MCP bridge back into trouve. Two roles, both optional:
        //  - approval gate: in Ask mode, Claude's permission requests go to
        //    the bridge's approval_prompt tool (trouve's approval flow)
        //    instead of failing in headless print mode;
        //  - tool bridge: Claude's built-ins stand down and trouve's
        //    ToolExecutor serves tools (approvals then gate inside trouve,
        //    so the bridged server is pre-allowed).
        if let Some(bridge) = &turn.mcp_bridge {
            let env: serde_json::Map<String, serde_json::Value> = bridge
                .env
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            let mcp_config = serde_json::json!({
                "mcpServers": {
                    "trouve": {
                        "command": bridge.command,
                        "args": bridge.args,
                        "env": env,
                    }
                }
            });
            let path = std::env::temp_dir().join(format!("trouve-mcp-{}.json", turn.thread_id));
            std::fs::write(&path, mcp_config.to_string())?;
            cmd.arg("--mcp-config").arg(&path);
            cmd.arg("--strict-mcp-config");
            if bridge.bridge_tools {
                if !bridge.disallowed_tools.is_empty() {
                    cmd.args(["--disallowedTools", &bridge.disallowed_tools.join(",")]);
                }
                cmd.args(["--allowedTools", "mcp__trouve"]);
            }
            if matches!(turn.permission, BackendPermission::Ask) {
                cmd.args(["--permission-prompt-tool", "mcp__trouve__approval_prompt"]);
            }
        }
        match turn.permission {
            BackendPermission::Yolo => {
                cmd.arg("--dangerously-skip-permissions");
            }
            BackendPermission::ReadOnly => {
                cmd.args(["--permission-mode", "plan"]);
            }
            BackendPermission::Ask => {}
        }

        let command_name = self.command.clone();
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(command_name.clone()),
            _ => BackendError::Io(e),
        })?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stream = async_stream(move |tx| async move {
            let mut completed = false;
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                for out in map_event(&ev) {
                    if matches!(out, BackendEvent::Completed { .. }) {
                        completed = true;
                    }
                    let _ = tx.send(Ok(out)).await;
                }
            }
            let status = child.wait().await;
            let ok = status.as_ref().map(|s| s.success()).unwrap_or(false);
            if !ok {
                let mut err = String::new();
                let mut elines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = elines.next_line().await {
                    err.push_str(&l);
                    err.push('\n');
                    if err.len() > 4000 {
                        break;
                    }
                }
                let _ = tx
                    .send(Err(BackendError::Protocol(format!(
                        "claude exited with {:?}: {}",
                        status.ok(),
                        err.trim()
                    ))))
                    .await;
            } else if !completed {
                let _ = tx
                    .send(Ok(BackendEvent::Completed {
                        usage: Usage::default(),
                    }))
                    .await;
            }
        });
        Ok(stream.boxed())
    }
}

/// Map one Claude Code stream-json event to zero or more backend events.
fn map_event(ev: &Value) -> Vec<BackendEvent> {
    match ev["type"].as_str() {
        // Claude rotates session ids per run; always persist the latest.
        Some("system") if ev["subtype"].as_str() == Some("init") => ev["session_id"]
            .as_str()
            .map(|sid| {
                vec![BackendEvent::SessionStarted {
                    session_id: sid.to_string(),
                }]
            })
            .unwrap_or_default(),
        Some("assistant") => {
            let mut out = Vec::new();
            if let Some(blocks) = ev["message"]["content"].as_array() {
                for b in blocks {
                    match b["type"].as_str() {
                        Some("text") => {
                            if let Some(t) = b["text"].as_str() {
                                if !t.is_empty() {
                                    out.push(BackendEvent::TextDelta(t.to_string()));
                                }
                            }
                        }
                        Some("tool_use") => out.push(BackendEvent::ToolStarted {
                            call_id: b["id"].as_str().unwrap_or("claude-tool").into(),
                            tool: b["name"].as_str().unwrap_or("tool").into(),
                            args: b["input"].clone(),
                        }),
                        _ => {}
                    }
                }
            }
            out
        }
        // Tool results come back as user-role messages.
        Some("user") => {
            let mut out = Vec::new();
            if let Some(blocks) = ev["message"]["content"].as_array() {
                for b in blocks {
                    if b["type"].as_str() == Some("tool_result") {
                        let ok = b["is_error"].as_bool() != Some(true);
                        out.push(BackendEvent::ToolCompleted {
                            call_id: b["tool_use_id"].as_str().unwrap_or("claude-tool").into(),
                            ok,
                            result: b["content"].clone(),
                        });
                    }
                }
            }
            out
        }
        Some("result") => {
            let usage = &ev["usage"];
            let mut events = Vec::new();
            // Session id also appears on the result event; keep it fresh.
            if let Some(sid) = ev["session_id"].as_str() {
                events.push(BackendEvent::SessionStarted {
                    session_id: sid.to_string(),
                });
            }
            events.push(BackendEvent::Completed {
                usage: Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    cached_input_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                    cost_usd: ev["total_cost_usd"].as_f64(),
                },
            });
            events
        }
        _ => vec![],
    }
}
