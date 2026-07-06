//! Cursor backend, driving the `cursor-agent` CLI.
//!
//! Each trouve thread maps to a Cursor chat (`cursor-agent create-chat`),
//! and each turn runs `cursor-agent -p --resume <chat> --output-format
//! stream-json --stream-partial-output` inside the session worktree, parsing
//! the NDJSON event stream.
//!
//! Permission mapping (v1, documented limitation): Cursor's headless mode
//! has no interactive approval bridge yet, so `Yolo` maps to `--force`,
//! `ReadOnly` to `--sandbox enabled`, and `Ask` runs with Cursor's defaults
//! (its own sandbox handles risky commands). Hook-based approval gating is a
//! follow-up.
//!
//! Auth: `cursor-agent login` (subscription) or the `CURSOR_API_KEY` env var
//! / configured API key (bills the user's plan) — both handled by the CLI.

use std::process::Stdio;

use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use trouve_protocol::{ModelInfo, Usage};

use crate::{
    async_stream, binary_on_path, model, spawn_login, AgentBackend, BackendError, BackendEvent,
    BackendEventStream, BackendLogin, BackendPermission, BackendStatus, BackendTurn,
};

pub struct CursorBackend {
    id: String,
    command: String,
    api_key: Option<String>,
    /// `cursor-agent models` result, cached for [`MODELS_TTL`].
    models_cache: tokio::sync::Mutex<Option<(std::time::Instant, Vec<ModelInfo>)>>,
}

/// How long a fetched vendor model list stays fresh.
const MODELS_TTL: std::time::Duration = std::time::Duration::from_secs(300);

impl CursorBackend {
    pub fn new(id: impl Into<String>, command: Option<String>, api_key: Option<String>) -> Self {
        Self {
            id: id.into(),
            command: command.unwrap_or_else(|| "cursor-agent".into()),
            api_key,
            models_cache: tokio::sync::Mutex::new(None),
        }
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new(&self.command);
        if let Some(key) = &self.api_key {
            cmd.env("CURSOR_API_KEY", key);
        }
        cmd
    }

    /// Ask the CLI for the account's model catalog.
    async fn fetch_models(&self) -> Result<Vec<ModelInfo>, BackendError> {
        let mut cmd = self.base_command();
        cmd.arg("models").stdin(Stdio::null());
        let out = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
            .await
            .map_err(|_| BackendError::Protocol("cursor-agent models timed out".into()))?
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
                _ => BackendError::Io(e),
            })?;
        if !out.status.success() {
            return Err(BackendError::Protocol(format!(
                "cursor-agent models failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(parse_models_output(
            &self.id,
            &String::from_utf8_lossy(&out.stdout),
        ))
    }

    async fn create_chat(&self, worktree: &std::path::Path) -> Result<String, BackendError> {
        let out = self
            .base_command()
            .arg("create-chat")
            .current_dir(worktree)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
                _ => BackendError::Io(e),
            })?;
        if !out.status.success() {
            return Err(BackendError::Protocol(format!(
                "cursor-agent create-chat failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if id.is_empty() {
            return Err(BackendError::Protocol(
                "cursor-agent create-chat printed no chat id".into(),
            ));
        }
        Ok(id)
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    fn id(&self) -> &str {
        &self.id
    }

    fn models(&self) -> Vec<ModelInfo> {
        // Minimal offline fallback; `list_models` asks the vendor for the
        // real catalog (per-account, includes thinking/effort variants).
        vec![model(&self.id, "auto", "Cursor Auto", 200_000)]
    }

    async fn list_models(&self) -> Vec<ModelInfo> {
        let mut cache = self.models_cache.lock().await;
        if let Some((at, models)) = cache.as_ref() {
            if at.elapsed() < MODELS_TTL {
                return models.clone();
            }
        }
        match self.fetch_models().await {
            Ok(models) if !models.is_empty() => {
                *cache = Some((std::time::Instant::now(), models.clone()));
                models
            }
            Ok(_) => self.models(),
            Err(e) => {
                tracing::debug!("cursor-agent models failed: {e}; using static list");
                self.models()
            }
        }
    }

    fn status(&self) -> BackendStatus {
        let installed = binary_on_path(&self.command);
        let has_credentials = self.api_key.is_some()
            || std::env::var("CURSOR_API_KEY").is_ok()
            || dirs::config_dir()
                .map(|d| d.join("cursor-agent").exists())
                .unwrap_or(false)
            || dirs::home_dir()
                .map(|h| h.join(".cursor").join("cli-config.json").exists())
                .unwrap_or(false);
        BackendStatus {
            installed,
            has_credentials,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        spawn_login(&self.command, &["login"]).await
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let (chat_id, fresh_session) = match &turn.session {
            Some(id) => (id.clone(), false),
            None => (self.create_chat(&turn.worktree).await?, true),
        };

        let prompt = match (&turn.instructions, fresh_session) {
            (Some(instr), true) => format!(
                "<mode-instructions>\n{instr}\n</mode-instructions>\n\n{}",
                turn.prompt
            ),
            _ => turn.prompt.clone(),
        };

        let mut cmd = self.base_command();
        cmd.arg("-p")
            .arg(&prompt)
            .args(["--resume", &chat_id])
            .args(["--output-format", "stream-json"])
            .arg("--stream-partial-output")
            .current_dir(&turn.worktree)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !turn.model.is_empty() && turn.model != "auto" {
            cmd.args(["--model", &turn.model]);
        }
        match turn.permission {
            BackendPermission::Yolo => {
                cmd.arg("--force");
            }
            BackendPermission::ReadOnly => {
                cmd.args(["--sandbox", "enabled"]);
            }
            BackendPermission::Ask => {}
        }

        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => BackendError::NotInstalled(self.command.clone()),
            _ => BackendError::Io(e),
        })?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let stream = async_stream(move |tx| async move {
            if fresh_session {
                let _ = tx
                    .send(Ok(BackendEvent::SessionStarted {
                        session_id: chat_id.clone(),
                    }))
                    .await;
            }
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
                        "cursor-agent exited with {:?}: {}",
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

/// Parse `cursor-agent models` output: one `<id> - <display name>` line per
/// model, between an "Available models" header and a "Tip:" footer.
fn parse_models_output(backend_id: &str, output: &str) -> Vec<ModelInfo> {
    output
        .lines()
        .filter_map(|line| {
            let (id, display) = line.trim().split_once(" - ")?;
            let id = id.trim();
            if id.is_empty() || id.contains(' ') {
                return None;
            }
            // The CLI doesn't report context windows; "1M" in the display
            // name is the only signal, everything else gets a conservative
            // default.
            let window = if display.contains("1M") {
                1_000_000
            } else {
                200_000
            };
            Some(model(backend_id, id, display.trim(), window))
        })
        .collect()
}

/// Map one cursor-agent stream-json event to zero or more backend events.
fn map_event(ev: &Value) -> Vec<BackendEvent> {
    match ev["type"].as_str() {
        Some("assistant") => {
            // With --stream-partial-output, assistant events carry text
            // chunks in message.content[].text; append them in order.
            let mut out = Vec::new();
            if let Some(parts) = ev["message"]["content"].as_array() {
                for p in parts {
                    if let Some(t) = p["text"].as_str() {
                        if !t.is_empty() {
                            out.push(BackendEvent::TextDelta(t.to_string()));
                        }
                    }
                }
            }
            out
        }
        Some("tool_call") => {
            let subtype = ev["subtype"].as_str().unwrap_or("");
            let call = &ev["tool_call"];
            // The payload nests the specific call under a single key like
            // "readToolCall" / "shellToolCall".
            let (tool, body) = call
                .as_object()
                .and_then(|o| o.iter().find(|(k, _)| k.ends_with("ToolCall")))
                .map(|(k, v)| (k.trim_end_matches("ToolCall").to_string(), v.clone()))
                .unwrap_or_else(|| ("tool".to_string(), call.clone()));
            let call_id = ev["call_id"]
                .as_str()
                .or_else(|| call["call_id"].as_str())
                .or_else(|| body["call_id"].as_str())
                .unwrap_or("cursor-tool")
                .to_string();
            match subtype {
                "started" => vec![BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    args: body["args"].clone(),
                }],
                "completed" => {
                    let result = body["result"].clone();
                    let ok = result.get("error").map(Value::is_null).unwrap_or(true);
                    vec![BackendEvent::ToolCompleted {
                        call_id,
                        ok,
                        result,
                    }]
                }
                _ => vec![],
            }
        }
        Some("result") => {
            let usage = &ev["usage"];
            vec![BackendEvent::Completed {
                usage: Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    cached_input_tokens: usage["cached_input_tokens"].as_u64().unwrap_or(0),
                    cost_usd: None,
                },
            }]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_models_listing() {
        let out = "Available models\n\nauto - Auto\ngpt-5.3-codex-high - Codex 5.3 High\n\
                   grok-4.3 - Grok 4.3 1M\n\nTip: use --model <id> to switch.\n";
        let models = parse_models_output("cursor", out);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "cursor/auto",
                "cursor/gpt-5.3-codex-high",
                "cursor/grok-4.3"
            ]
        );
        assert_eq!(models[2].context_window, 1_000_000);
        assert_eq!(models[1].context_window, 200_000);
        assert_eq!(models[1].display_name, "Codex 5.3 High");
    }
}
