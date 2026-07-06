//! MCP (Model Context Protocol) client: stdio JSON-RPC to external tool
//! servers.
//!
//! Server configs are discovered from `.agents/.mcp.json` in the worktree
//! and `mcp.json` in the config dir (standard `mcpServers` shape; `${VAR}`
//! in env values expands from the process environment so secrets stay out
//! of the file). Discovered tools surface as `mcp__<server>__<tool>` through
//! the normal `ToolExecutor` chokepoint; the permission layer requires
//! first-use approval per server per session, even in yolo mode (invariant
//! 3 + prompt-injection guidance in the plan).
//!
//! The transport is deliberately minimal (newline-delimited JSON-RPC,
//! serialized request/response): enough for `initialize`, `tools/list`, and
//! `tools/call`, which is the entire surface trouve needs today.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use trouve_providers::ToolSpec;

/// Prefix for MCP tool names: `mcp__<server>__<tool>`.
pub const TOOL_PREFIX: &str = "mcp__";

/// One entry under `mcpServers` in `.mcp.json`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Values may be `${VAR}` references resolved from the environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct McpFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

/// Expand `${VAR}` references from the process environment. Missing vars
/// expand to the empty string (the server will fail loudly if it matters).
fn expand_env(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find('}') {
            Some(end) => {
                let var = &rest[start + 2..start + 2 + end];
                out.push_str(&std::env::var(var).unwrap_or_default());
                rest = &rest[start + 2 + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Discover MCP server configs. Workspace config wins on name collision.
pub fn discover_configs(
    config_dir: Option<&Path>,
    worktree: &Path,
) -> BTreeMap<String, McpServerConfig> {
    let mut servers = BTreeMap::new();
    let mut load = |path: &Path| {
        if let Ok(text) = std::fs::read_to_string(path) {
            match serde_json::from_str::<McpFile>(&text) {
                Ok(file) => servers.extend(file.mcp_servers),
                Err(e) => tracing::warn!("ignoring malformed {}: {e}", path.display()),
            }
        }
    };
    if let Some(dir) = config_dir {
        load(&dir.join("mcp.json"));
    }
    load(&worktree.join(".agents").join(".mcp.json"));
    servers
}

/// Split `mcp__<server>__<tool>` into (server, tool).
pub fn split_tool_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix(TOOL_PREFIX)?.split_once("__")
}

// --- transport ---------------------------------------------------------

struct Pipes {
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
}

/// A live connection to one MCP server process.
pub struct McpConnection {
    _child: Child,
    pipes: Mutex<Pipes>,
    next_id: AtomicI64,
    tools: Vec<ToolSpec>,
}

impl McpConnection {
    /// Spawn the server, run the `initialize` handshake, and list tools.
    pub async fn connect(server: &str, config: &McpServerConfig) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in &config.env {
            command.env(key, expand_env(value));
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawning MCP server '{server}' ({})", config.command))?;
        let stdin = child.stdin.take().context("mcp stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("mcp stdout")?).lines();

        let mut connection = Self {
            _child: child,
            pipes: Mutex::new(Pipes { stdin, stdout }),
            next_id: AtomicI64::new(1),
            tools: Vec::new(),
        };

        connection
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "trouve", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await
            .with_context(|| format!("initializing MCP server '{server}'"))?;
        connection
            .notify("notifications/initialized", json!({}))
            .await?;

        let listed = connection.request("tools/list", json!({})).await?;
        let tools = listed
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        connection.tools = tools
            .iter()
            .filter_map(|tool| {
                Some(ToolSpec {
                    name: format!("{TOOL_PREFIX}{server}__{}", tool.get("name")?.as_str()?),
                    description: tool
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    parameters: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                })
            })
            .collect();
        Ok(connection)
    }

    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut pipes = self.pipes.lock().await;
        pipes.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        pipes.stdin.flush().await?;
        Ok(())
    }

    /// Send a request and wait for its response, skipping any interleaved
    /// notifications. Requests are fully serialized behind the pipe mutex.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut pipes = self.pipes.lock().await;
        pipes.stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        pipes.stdin.flush().await?;
        loop {
            let Some(line) = pipes.stdout.next_line().await? else {
                bail!("MCP server closed the stream during '{method}'");
            };
            let Ok(reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply.get("id").and_then(|v| v.as_i64()) != Some(id) {
                continue; // notification or unrelated message
            }
            if let Some(error) = reply.get("error") {
                bail!("MCP '{method}' failed: {error}");
            }
            return Ok(reply.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Invoke a tool; returns the MCP result content flattened to a JSON
    /// value (single text block → string).
    pub async fn call_tool(&self, tool: &str, args: &Value) -> Result<(bool, Value)> {
        let result = self
            .request("tools/call", json!({"name": tool, "arguments": args}))
            .await?;
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let content = result.get("content").cloned().unwrap_or(Value::Null);
        let flattened = match &content {
            Value::Array(blocks) => {
                let texts: Vec<&str> = blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect();
                if texts.len() == blocks.len() && !texts.is_empty() {
                    Value::String(texts.join("\n"))
                } else {
                    content.clone()
                }
            }
            other => other.clone(),
        };
        Ok((is_error, flattened))
    }
}

/// Lazily-connected MCP servers, keyed by (worktree, server name).
#[derive(Default)]
pub struct McpManager {
    connections: Mutex<HashMap<(String, String), std::sync::Arc<McpConnection>>>,
}

impl McpManager {
    async fn connection(
        &self,
        config_dir: Option<&Path>,
        worktree: &Path,
        server: &str,
    ) -> Result<std::sync::Arc<McpConnection>> {
        let key = (worktree.to_string_lossy().to_string(), server.to_string());
        let mut connections = self.connections.lock().await;
        if let Some(existing) = connections.get(&key) {
            return Ok(existing.clone());
        }
        let configs = discover_configs(config_dir, worktree);
        let config = configs
            .get(server)
            .with_context(|| format!("no MCP server '{server}' configured"))?;
        let connection = std::sync::Arc::new(McpConnection::connect(server, config).await?);
        connections.insert(key, connection.clone());
        Ok(connection)
    }

    /// All MCP tool specs visible from this worktree. Connection failures
    /// are logged and skipped so a broken server doesn't block turns.
    pub async fn specs(&self, config_dir: Option<&Path>, worktree: &Path) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        for server in discover_configs(config_dir, worktree).keys() {
            match self.connection(config_dir, worktree, server).await {
                Ok(connection) => specs.extend(connection.tools().iter().cloned()),
                Err(e) => tracing::warn!("MCP server '{server}' unavailable: {e:#}"),
            }
        }
        specs
    }

    /// Execute `mcp__<server>__<tool>`.
    pub async fn call(
        &self,
        config_dir: Option<&Path>,
        worktree: &Path,
        name: &str,
        args: &Value,
    ) -> Result<(bool, Value)> {
        let (server, tool) =
            split_tool_name(name).with_context(|| format!("malformed MCP tool name: {name}"))?;
        let connection = self.connection(config_dir, worktree, server).await?;
        connection.call_tool(tool, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_with_env_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join(".agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join(".mcp.json"),
            r#"{"mcpServers": {"jira": {"command": "jira-mcp", "args": ["--stdio"],
                "env": {"TOKEN": "${TROUVE_TEST_JIRA_TOKEN}"}}}}"#,
        )
        .unwrap();
        let configs = discover_configs(None, tmp.path());
        assert_eq!(configs.len(), 1);
        let jira = &configs["jira"];
        assert_eq!(jira.command, "jira-mcp");
        assert_eq!(jira.args, vec!["--stdio"]);

        std::env::set_var("TROUVE_TEST_JIRA_TOKEN", "sekrit");
        assert_eq!(expand_env("${TROUVE_TEST_JIRA_TOKEN}"), "sekrit");
        assert_eq!(
            expand_env("Bearer ${TROUVE_TEST_JIRA_TOKEN}!"),
            "Bearer sekrit!"
        );
        assert_eq!(expand_env("${MISSING_VAR_XYZ}"), "");
    }

    #[test]
    fn tool_names_round_trip() {
        assert_eq!(
            split_tool_name("mcp__jira__create_issue"),
            Some(("jira", "create_issue"))
        );
        assert_eq!(split_tool_name("shell"), None);
        assert_eq!(split_tool_name("mcp__broken"), None);
    }

    /// End-to-end against a tiny fake MCP server implemented in Python.
    #[tokio::test]
    async fn connects_lists_and_calls_a_stdio_server() {
        let script = r#"
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    mid = msg.get("id")
    method = msg.get("method")
    if method == "initialize":
        out = {"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "2024-11-05",
               "capabilities": {}, "serverInfo": {"name": "fake", "version": "0"}}}
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        out = {"jsonrpc": "2.0", "id": mid, "result": {"tools": [
            {"name": "echo", "description": "Echo the input",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}}
    elif method == "tools/call":
        text = msg["params"]["arguments"].get("text", "")
        out = {"jsonrpc": "2.0", "id": mid, "result": {"content": [
            {"type": "text", "text": "echo: " + text}]}}
    else:
        out = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#;
        let tmp = tempfile::tempdir().unwrap();
        let script_path = tmp.path().join("fake_mcp.py");
        std::fs::write(&script_path, script).unwrap();
        let agents = tmp.path().join(".agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join(".mcp.json"),
            serde_json::to_string(&json!({"mcpServers": {"fake": {
                "command": "python3",
                "args": [script_path.to_string_lossy()],
            }}}))
            .unwrap(),
        )
        .unwrap();

        let manager = McpManager::default();
        let specs = manager.specs(None, tmp.path()).await;
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "mcp__fake__echo");

        let (is_error, value) = manager
            .call(None, tmp.path(), "mcp__fake__echo", &json!({"text": "hi"}))
            .await
            .unwrap();
        assert!(!is_error);
        assert_eq!(value, Value::String("echo: hi".into()));
    }
}
