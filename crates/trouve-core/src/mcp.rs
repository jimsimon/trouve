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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use trouve_providers::ToolSpec;

/// Prefix for MCP tool names: `mcp__<server>__<tool>`.
pub const TOOL_PREFIX: &str = "mcp__";

/// One entry under `mcpServers` in `.mcp.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    /// May be empty on a pure tombstone entry (`{"disabled": true}`).
    #[serde(default)]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Values may be `${VAR}` references resolved from the environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Tombstone: a higher-priority scope can disable a server inherited
    /// from a lower one (e.g. a branch's `.agents/.mcp.json` shadowing a
    /// user- or workspace-level server) without redefining it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

#[derive(Debug, Deserialize)]
struct McpFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, McpServerConfig>,
}

// --- logs ---------------------------------------------------------------

const LOG_CAP: usize = 400;

/// Rolling per-server log buffers (stderr lines + lifecycle events), shared
/// between the runtime `McpManager` and settings health probes so the
/// settings "View logs" button sees both.
#[derive(Default, Clone)]
pub struct McpLogStore {
    buffers: Arc<std::sync::Mutex<HashMap<String, VecDeque<String>>>>,
}

impl McpLogStore {
    pub fn push(&self, server: &str, line: impl AsRef<str>) {
        let stamp = chrono::Local::now().format("%H:%M:%S");
        let mut buffers = self.buffers.lock().unwrap();
        let buffer = buffers.entry(server.to_string()).or_default();
        if buffer.len() >= LOG_CAP {
            buffer.pop_front();
        }
        buffer.push_back(format!("[{stamp}] {}", line.as_ref()));
    }

    pub fn lines(&self, server: &str) -> Vec<String> {
        self.buffers
            .lock()
            .unwrap()
            .get(server)
            .map(|b| b.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Expand `${VAR}` references from the process environment. Missing vars
/// expand to the empty string (the server will fail loudly if it matters).
pub fn expand_env(value: &str) -> String {
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

/// The user-scoped MCP config file inside trouve's config dir.
pub fn user_config_path(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("mcp.json")
}

/// The workspace-scoped MCP config file inside a repo (or worktree) root.
pub fn workspace_config_path(root: &Path) -> std::path::PathBuf {
    root.join(".agents").join(".mcp.json")
}

/// Servers from one config file; empty when missing or malformed.
pub fn read_servers(path: &Path) -> BTreeMap<String, McpServerConfig> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_str::<McpFile>(&text) {
        Ok(file) => file.mcp_servers,
        Err(e) => {
            tracing::warn!("ignoring malformed {}: {e}", path.display());
            BTreeMap::new()
        }
    }
}

/// Add or replace one server in a config file, preserving any unrelated
/// keys the file may carry. Creates the file (and parent dir) if missing.
pub fn upsert_server(path: &Path, name: &str, config: &McpServerConfig) -> Result<()> {
    edit_file(path, |servers| {
        servers.insert(
            name.to_string(),
            serde_json::to_value(config).expect("mcp config serializes"),
        );
    })
}

/// Remove one server from a config file. Missing file or name is a no-op.
pub fn remove_server(path: &Path, name: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    edit_file(path, |servers| {
        servers.remove(name);
    })
}

fn edit_file(path: &Path, mutate: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<()> {
    let mut doc: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        Err(_) => json!({}),
    };
    let root = doc
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("mcpServers in {} is not an object", path.display()))?;
    mutate(servers);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&doc)? + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Discover MCP server configs: user config, overlaid by the workspace
/// repo's `.agents/.mcp.json`, overlaid by the session worktree's (so
/// settings edits apply immediately and committed files still win).
/// Entries left `disabled` after the merge are dropped — that's how a
/// branch removes a server it would otherwise inherit.
pub fn discover_configs(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    worktree: &Path,
) -> BTreeMap<String, McpServerConfig> {
    let mut servers = BTreeMap::new();
    if let Some(dir) = config_dir {
        servers.extend(read_servers(&user_config_path(dir)));
    }
    if let Some(root) = workspace_root
        && root != worktree
    {
        servers.extend(read_servers(&workspace_config_path(root)));
    }
    servers.extend(read_servers(&workspace_config_path(worktree)));
    servers.retain(|_, config| !config.disabled);
    servers
}

/// Like [`discover_configs`], but keeps disabled entries and tags each
/// server with the layer whose definition won: "app-wide" (the user-level
/// config applies to every workspace), "workspace" (the repo's committed
/// file), or "branch" (the session worktree's checkout). Feeds the
/// per-session effective-config view.
pub fn discover_with_provenance(
    config_dir: Option<&Path>,
    workspace_root: Option<&Path>,
    worktree: &Path,
) -> Vec<(String, McpServerConfig, String)> {
    let mut servers: BTreeMap<String, (McpServerConfig, String)> = BTreeMap::new();
    let mut overlay = |path: &Path, source: &str| {
        for (name, config) in read_servers(path) {
            servers.insert(name, (config, source.to_string()));
        }
    };
    if let Some(dir) = config_dir {
        overlay(&user_config_path(dir), "app-wide");
    }
    if let Some(root) = workspace_root
        && root != worktree
    {
        overlay(&workspace_config_path(root), "workspace");
    }
    overlay(&workspace_config_path(worktree), "branch");
    servers
        .into_iter()
        .map(|(name, (config, source))| (name, config, source))
        .collect()
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
    /// The server's stderr streams into `logs` when given (settings "View
    /// logs"); otherwise it is discarded.
    pub async fn connect(
        server: &str,
        config: &McpServerConfig,
        logs: Option<&McpLogStore>,
    ) -> Result<Self> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if logs.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        for (key, value) in &config.env {
            command.env(key, expand_env(value));
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("spawning MCP server '{server}' ({})", config.command))?;
        let stdin = child.stdin.take().context("mcp stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("mcp stdout")?).lines();
        if let (Some(logs), Some(stderr)) = (logs, child.stderr.take()) {
            let logs = logs.clone();
            let server = server.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.push(&server, line);
                }
            });
        }

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

/// Connect with a deadline and report the number of tools served — the
/// settings health check. The connection (and its process) is dropped
/// afterwards; stderr and lifecycle lines land in `logs`.
pub async fn probe(server: &str, config: &McpServerConfig, logs: &McpLogStore) -> Result<usize> {
    logs.push(server, format!("health check: spawning {}", config.command));
    let connection = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        McpConnection::connect(server, config, Some(logs)),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out after 10s during the MCP handshake"))?;
    match &connection {
        Ok(c) => logs.push(
            server,
            format!("health check: ok ({} tools)", c.tools().len()),
        ),
        Err(e) => logs.push(server, format!("health check: failed: {e:#}")),
    }
    Ok(connection?.tools().len())
}

/// Lazily-connected MCP servers, keyed by (worktree, server name).
#[derive(Default)]
pub struct McpManager {
    connections: Mutex<HashMap<(String, String), std::sync::Arc<McpConnection>>>,
    logs: McpLogStore,
}

impl McpManager {
    /// A manager whose connections log into an externally-owned store (the
    /// engine shares it with settings health probes).
    pub fn with_logs(logs: McpLogStore) -> Self {
        Self {
            connections: Mutex::default(),
            logs,
        }
    }

    /// The shared log store (also fed by settings health probes).
    pub fn logs(&self) -> &McpLogStore {
        &self.logs
    }

    async fn connection(
        &self,
        config_dir: Option<&Path>,
        workspace_root: Option<&Path>,
        worktree: &Path,
        server: &str,
    ) -> Result<std::sync::Arc<McpConnection>> {
        let key = (worktree.to_string_lossy().to_string(), server.to_string());
        let mut connections = self.connections.lock().await;
        if let Some(existing) = connections.get(&key) {
            return Ok(existing.clone());
        }
        let configs = discover_configs(config_dir, workspace_root, worktree);
        let config = configs
            .get(server)
            .with_context(|| format!("no MCP server '{server}' configured"))?;
        let connection =
            std::sync::Arc::new(McpConnection::connect(server, config, Some(&self.logs)).await?);
        self.logs.push(
            server,
            format!("connected ({} tools)", connection.tools().len()),
        );
        connections.insert(key, connection.clone());
        Ok(connection)
    }

    /// All MCP tool specs visible from this worktree. Connection failures
    /// are logged and skipped so a broken server doesn't block turns.
    pub async fn specs(
        &self,
        config_dir: Option<&Path>,
        workspace_root: Option<&Path>,
        worktree: &Path,
    ) -> Vec<ToolSpec> {
        let mut specs = Vec::new();
        for server in discover_configs(config_dir, workspace_root, worktree).keys() {
            match self
                .connection(config_dir, workspace_root, worktree, server)
                .await
            {
                Ok(connection) => specs.extend(connection.tools().iter().cloned()),
                Err(e) => {
                    self.logs.push(server, format!("unavailable: {e:#}"));
                    tracing::warn!("MCP server '{server}' unavailable: {e:#}");
                }
            }
        }
        specs
    }

    /// Execute `mcp__<server>__<tool>`.
    pub async fn call(
        &self,
        config_dir: Option<&Path>,
        workspace_root: Option<&Path>,
        worktree: &Path,
        name: &str,
        args: &Value,
    ) -> Result<(bool, Value)> {
        let (server, tool) =
            split_tool_name(name).with_context(|| format!("malformed MCP tool name: {name}"))?;
        let connection = self
            .connection(config_dir, workspace_root, worktree, server)
            .await?;
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
        let configs = discover_configs(None, None, tmp.path());
        assert_eq!(configs.len(), 1);
        let jira = &configs["jira"];
        assert_eq!(jira.command, "jira-mcp");
        assert_eq!(jira.args, vec!["--stdio"]);

        // Safety: unique variable name, so parallel tests can't race on it.
        unsafe { std::env::set_var("TROUVE_TEST_JIRA_TOKEN", "sekrit") };
        assert_eq!(expand_env("${TROUVE_TEST_JIRA_TOKEN}"), "sekrit");
        assert_eq!(
            expand_env("Bearer ${TROUVE_TEST_JIRA_TOKEN}!"),
            "Bearer sekrit!"
        );
        assert_eq!(expand_env("${MISSING_VAR_XYZ}"), "");
    }

    #[test]
    fn disabled_tombstone_removes_inherited_server() {
        let user_dir = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            user_config_path(user_dir.path()),
            r#"{"mcpServers": {
                "jira": {"command": "jira-mcp"},
                "linear": {"command": "linear-mcp"}}}"#,
        )
        .unwrap();
        let agents = worktree.path().join(".agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join(".mcp.json"),
            r#"{"mcpServers": {
                "jira": {"disabled": true},
                "docs": {"command": "docs-mcp"}}}"#,
        )
        .unwrap();

        let configs = discover_configs(Some(user_dir.path()), None, worktree.path());
        // jira is tombstoned by the worktree; linear inherited; docs added.
        assert!(!configs.contains_key("jira"));
        assert!(configs.contains_key("linear"));
        assert!(configs.contains_key("docs"));
    }

    #[test]
    fn provenance_tags_the_winning_layer_and_keeps_tombstones() {
        let user_dir = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        std::fs::write(
            user_config_path(user_dir.path()),
            r#"{"mcpServers": {
                "jira": {"command": "jira-mcp"},
                "linear": {"command": "linear-mcp"}}}"#,
        )
        .unwrap();
        for (dir, body) in [
            (
                workspace.path(),
                r#"{"mcpServers": {"docs": {"command": "docs-mcp"}}}"#,
            ),
            (
                worktree.path(),
                r#"{"mcpServers": {
                    "jira": {"disabled": true},
                    "docs": {"command": "docs-mcp-branch"}}}"#,
            ),
        ] {
            let agents = dir.join(".agents");
            std::fs::create_dir_all(&agents).unwrap();
            std::fs::write(agents.join(".mcp.json"), body).unwrap();
        }

        let servers = discover_with_provenance(
            Some(user_dir.path()),
            Some(workspace.path()),
            worktree.path(),
        );
        let find = |name: &str| servers.iter().find(|(n, _, _)| n == name).unwrap();

        let (_, config, source) = find("linear");
        assert_eq!(source, "app-wide");
        assert!(!config.disabled);
        // The branch redefines docs, so it wins over the workspace entry.
        let (_, config, source) = find("docs");
        assert_eq!(source, "branch");
        assert_eq!(config.command, "docs-mcp-branch");
        // Tombstones stay visible, tagged with the layer that disabled them.
        let (_, config, source) = find("jira");
        assert_eq!(source, "branch");
        assert!(config.disabled);
    }

    #[test]
    fn upsert_and_remove_edit_files_preserving_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"other": {"keep": true}, "mcpServers": {"jira": {"command": "jira-mcp"}}}"#,
        )
        .unwrap();

        let config = McpServerConfig {
            command: "linear-mcp".into(),
            args: vec!["--stdio".into()],
            env: BTreeMap::from([("TOKEN".into(), "${LINEAR_TOKEN}".into())]),
            disabled: false,
        };
        upsert_server(&path, "linear", &config).unwrap();

        let servers = read_servers(&path);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers["linear"], config);
        // Unrelated top-level keys survive the edit.
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["other"]["keep"], Value::Bool(true));

        remove_server(&path, "jira").unwrap();
        let servers = read_servers(&path);
        assert_eq!(servers.len(), 1);
        assert!(servers.contains_key("linear"));

        // Creating a fresh file (and parent dir) from nothing also works.
        let fresh = tmp.path().join("sub").join("new.json");
        upsert_server(&fresh, "solo", &config).unwrap();
        assert_eq!(read_servers(&fresh).len(), 1);
        // Removing from a missing file is a no-op.
        remove_server(&tmp.path().join("missing.json"), "x").unwrap();
    }

    #[test]
    fn log_store_caps_and_returns_lines() {
        let logs = McpLogStore::default();
        assert!(logs.lines("nope").is_empty());
        for i in 0..450 {
            logs.push("s", format!("line {i}"));
        }
        let lines = logs.lines("s");
        assert_eq!(lines.len(), 400);
        assert!(lines[0].ends_with("line 50"));
        assert!(lines[399].ends_with("line 449"));
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
        let specs = manager.specs(None, None, tmp.path()).await;
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "mcp__fake__echo");

        let (is_error, value) = manager
            .call(
                None,
                None,
                tmp.path(),
                "mcp__fake__echo",
                &json!({"text": "hi"}),
            )
            .await
            .unwrap();
        assert!(!is_error);
        assert_eq!(value, Value::String("echo: hi".into()));
    }
}
