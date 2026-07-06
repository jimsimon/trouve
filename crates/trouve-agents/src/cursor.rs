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

    /// Turn a base model + options into the concrete vendor id. Threads
    /// created before the variant split may still store a full variant id;
    /// those pass through unchanged.
    async fn resolve_model(&self, turn: &BackendTurn) -> String {
        let (_, level, fast) = split_variant(&turn.model);
        if level.is_some() || fast {
            return turn.model.clone();
        }
        // The group's default level lives in the cached schema (the bare id
        // doesn't exist for groups like claude-opus-4-8, whose unlabeled
        // variant is claude-opus-4-8-high).
        let default_level = {
            let cache = self.models_cache.lock().await;
            cache.as_ref().and_then(|(_, models)| {
                let qualified = format!("{}/{}", self.id, turn.model);
                models.iter().find(|m| m.id == qualified).and_then(|m| {
                    m.options_schema
                        .pointer("/properties/thinking_level/default")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
            })
        };
        compose_model_id(&turn.model, &turn.model_options, default_level.as_deref())
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
            // Trouve creates the worktree from a workspace the user opened
            // deliberately; without this, headless runs abort with a
            // "Workspace Trust Required" prompt.
            .arg("--trust")
            .current_dir(&turn.worktree)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !turn.model.is_empty() && turn.model != "auto" {
            cmd.args(["--model", &self.resolve_model(&turn).await]);
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

/// Thinking/effort level tokens the catalog uses as id suffixes. Note that
/// `-max` here is an *effort level* (mirroring Anthropic's effort API), not
/// the IDE's "Max Mode" context toggle — the CLI has no such toggle and its
/// models already run at their full context window.
const LEVELS: [&str; 6] = ["none", "low", "medium", "high", "xhigh", "max"];

/// Split a raw catalog id into `(base, thinking level, fast)`. The grammar
/// is `<base>[-<level>][-fast]`; a `-thinking` marker stays part of the
/// base because Cursor treats thinking/non-thinking as distinct models.
fn split_variant(id: &str) -> (&str, Option<&str>, bool) {
    let (rest, fast) = match id.strip_suffix("-fast") {
        Some(rest) => (rest, true),
        None => (id, false),
    };
    if let Some((head, tail)) = rest.rsplit_once('-') {
        if LEVELS.contains(&tail) {
            return (head, Some(tail), fast);
        }
    }
    (rest, None, fast)
}

/// Parse `cursor-agent models` output (one `<id> - <display name>` line per
/// model) and fold the variant explosion into one entry per base model with
/// a `thinking_level` / `fast` options schema. The default variant of a
/// group is the one with the shortest display name ("Opus 4.8 1M" is the
/// unlabeled default among "Opus 4.8 1M Low/Medium/Extra High/...").
fn parse_models_output(backend_id: &str, output: &str) -> Vec<ModelInfo> {
    struct Group {
        order: usize,
        levels: Vec<String>, // "default" = the bare (level-less) id
        has_fast: bool,
        display: String,
        default_level: String,
    }
    let mut groups: std::collections::BTreeMap<String, Group> = Default::default();

    for (order, line) in output.lines().enumerate() {
        let Some((id, display)) = line.trim().split_once(" - ") else {
            continue;
        };
        let (id, display) = (id.trim(), display.trim());
        if id.is_empty() || id.contains(' ') {
            continue;
        }
        let (base, level, fast) = split_variant(id);
        let level = level.unwrap_or("default").to_string();
        let group = groups.entry(base.to_string()).or_insert_with(|| Group {
            order,
            levels: Vec::new(),
            has_fast: false,
            display: display.to_string(),
            default_level: level.clone(),
        });
        group.has_fast |= fast;
        if !fast {
            if !group.levels.contains(&level) {
                group.levels.push(level.clone());
            }
            // Shortest display in the group names the base model itself.
            if display.len() < group.display.len() {
                group.display = display.to_string();
                group.default_level = level;
            }
        }
    }

    let mut models: Vec<(usize, ModelInfo)> = groups
        .into_iter()
        .map(|(base, mut group)| {
            group.levels.sort_by_key(|l| level_rank(l));
            let mut properties = serde_json::Map::new();
            if group.levels.len() > 1 {
                properties.insert(
                    "thinking_level".into(),
                    serde_json::json!({
                        "type": "string",
                        "enum": group.levels,
                        "default": group.default_level,
                        "description": "How much thinking the model does before answering"
                    }),
                );
            }
            if group.has_fast {
                properties.insert(
                    "fast".into(),
                    serde_json::json!({
                        "type": "boolean",
                        "description": "Priority serving (consumes usage faster)"
                    }),
                );
            }
            // The CLI doesn't report context windows; "1M" in the display
            // name is the only signal, everything else gets a conservative
            // default. Those extended-context models are Max Mode-only —
            // the CLI has no toggle — and Max Mode usage bills at a premium
            // (20% surcharge on some Cursor plans), so flag them.
            let max_mode = group.display.contains("1M");
            let window = if max_mode { 1_000_000 } else { 200_000 };
            let mut info = model(backend_id, &base, &group.display, window);
            info.max_mode = max_mode;
            info.options_schema = serde_json::json!({
                "type": "object",
                "properties": properties,
            });
            (group.order, info)
        })
        .collect();
    models.sort_by_key(|(order, _)| *order);
    models.into_iter().map(|(_, info)| info).collect()
}

/// Sort order for level tokens; the bare "default" sits at medium.
fn level_rank(level: &str) -> usize {
    match level {
        "none" => 0,
        "low" => 1,
        "default" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        "max" => 6,
        _ => 7,
    }
}

/// Rebuild the concrete vendor model id from a base model plus options.
/// `default_level` is the group's unlabeled variant (from the schema);
/// "default" means the bare base id.
fn compose_model_id(
    base: &str,
    options: &serde_json::Map<String, Value>,
    default_level: Option<&str>,
) -> String {
    let level = options
        .get("thinking_level")
        .and_then(Value::as_str)
        .or(default_level)
        .unwrap_or("default");
    let mut id = base.to_string();
    if level != "default" {
        id.push('-');
        id.push_str(level);
    }
    if options.get("fast").and_then(Value::as_bool) == Some(true) {
        id.push_str("-fast");
    }
    id
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
        // Reasoning stream (thinking models); deltas coalesce client-side.
        Some("thinking") => ev["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| vec![BackendEvent::ThinkingDelta(t.to_string())])
            .unwrap_or_default(),
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
    fn groups_variant_explosion_into_base_models() {
        let out = "Available models\n\n\
            auto - Auto\n\
            gpt-5.3-codex-low - Codex 5.3 Low\n\
            gpt-5.3-codex - Codex 5.3\n\
            gpt-5.3-codex-fast - Codex 5.3 Fast\n\
            gpt-5.3-codex-high - Codex 5.3 High\n\
            gpt-5.3-codex-high-fast - Codex 5.3 High Fast\n\
            claude-opus-4-8-low - Opus 4.8 1M Low\n\
            claude-opus-4-8-high - Opus 4.8 1M\n\
            claude-opus-4-8-max - Opus 4.8 1M Max\n\
            claude-opus-4-8-thinking-high - Opus 4.8 1M Thinking\n\
            claude-opus-4-8-thinking-low - Opus 4.8 1M Low Thinking\n\
            grok-4.3 - Grok 4.3 1M\n\n\
            Tip: use --model <id> to switch.\n";
        let models = parse_models_output("cursor", out);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "cursor/auto",
                "cursor/gpt-5.3-codex",
                "cursor/claude-opus-4-8",
                "cursor/claude-opus-4-8-thinking",
                "cursor/grok-4.3",
            ]
        );

        // Bare variant is the default; levels sorted; fast noted.
        let codex = &models[1];
        assert_eq!(codex.display_name, "Codex 5.3");
        assert_eq!(
            codex
                .options_schema
                .pointer("/properties/thinking_level/enum")
                .unwrap(),
            &serde_json::json!(["low", "default", "high"])
        );
        assert_eq!(
            codex
                .options_schema
                .pointer("/properties/thinking_level/default")
                .and_then(Value::as_str),
            Some("default")
        );
        assert!(codex.options_schema.pointer("/properties/fast").is_some());

        // No bare id: the unlabeled display ("Opus 4.8 1M") marks high as
        // default; `-thinking` stays a separate model; `-max` is a level.
        let opus = &models[2];
        assert_eq!(opus.display_name, "Opus 4.8 1M");
        assert_eq!(opus.context_window, 1_000_000);
        // Extended-context models are Max Mode-only (billed at a premium).
        assert!(opus.max_mode);
        assert!(!models[1].max_mode);
        assert_eq!(
            opus.options_schema
                .pointer("/properties/thinking_level/enum")
                .unwrap(),
            &serde_json::json!(["low", "high", "max"])
        );
        assert_eq!(
            opus.options_schema
                .pointer("/properties/thinking_level/default")
                .and_then(Value::as_str),
            Some("high")
        );
        assert!(opus.options_schema.pointer("/properties/fast").is_none());

        // Single-variant models get no thinking knob.
        assert!(models[4]
            .options_schema
            .pointer("/properties/thinking_level")
            .is_none());
    }

    #[test]
    fn composes_variant_ids_from_options() {
        let opts = |v: Value| v.as_object().unwrap().clone();

        // Explicit level and fast.
        assert_eq!(
            compose_model_id(
                "claude-opus-4-8",
                &opts(serde_json::json!({"thinking_level": "max", "fast": true})),
                Some("high"),
            ),
            "claude-opus-4-8-max-fast"
        );
        // No option set: fall back to the group default.
        assert_eq!(
            compose_model_id("claude-opus-4-8", &serde_json::Map::new(), Some("high")),
            "claude-opus-4-8-high"
        );
        // "default" maps to the bare id.
        assert_eq!(
            compose_model_id(
                "gpt-5.3-codex",
                &opts(serde_json::json!({"thinking_level": "default"})),
                Some("default"),
            ),
            "gpt-5.3-codex"
        );
        // Unknown group (cold cache): bare id passes through.
        assert_eq!(
            compose_model_id("grok-4.3", &serde_json::Map::new(), None),
            "grok-4.3"
        );
    }
}
