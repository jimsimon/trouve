//! Interactive installer: configure semble across coding agents.
//!
//! Port of `semble/installer/*`. Three integrations per agent: an MCP server
//! entry, a marked instructions block in the agent's config markdown, and a
//! dedicated `semble-search` sub-agent file.
//!
//! Deviation from upstream: JSON config edits use strict JSON round-tripping
//! (key order preserved) instead of a tree-sitter JSON5 grammar. Files that
//! do not parse as strict JSON (e.g. JSONC with comments) are reported as
//! "skipped", matching upstream's behaviour when its JSON5 grammar is
//! unavailable.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dialoguer::{Confirm, MultiSelect};
use serde_json::{json, Map, Value};

pub const SEMBLE_START: &str = "<!-- SEMBLE_START -->";
pub const SEMBLE_END: &str = "<!-- SEMBLE_END -->";

const CODEX_MCP_HEADER: &str = "[mcp_servers.semble]";
const CODEX_MCP_BLOCK: &str = "[mcp_servers.semble]\ncommand = \"semble\"\nargs = []\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
    NotFound,
    Removed,
    Error,
    Skipped,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Action::Created => "created",
            Action::Updated => "updated",
            Action::Unchanged => "unchanged",
            Action::NotFound => "not-found",
            Action::Removed => "removed",
            Action::Error => "error",
            Action::Skipped => "skipped",
        };
        f.write_str(s)
    }
}

fn instructions_block() -> String {
    format!(
        r#"{SEMBLE_START}
## Semble Code Search

A `semble` MCP server is available with two tools:
- `mcp__semble__search` — search the codebase with a natural-language or code query.
- `mcp__semble__find_related` — find code similar to a specific file and line.

Use `mcp__semble__search` to find where something is implemented — instead of using Grep or Glob to discover files. After semble returns the file and line, navigate there directly and read that file. Do not grep for the same content again.

Pass `--content docs` to search documentation and prose, `--content config` for config files, or `--content all` to search code, docs, and config together.

For CLI fallback or sub-agents without MCP access, use:

```bash
semble search "authentication flow" ./my-project --max-snippet-lines 10
semble search "deployment guide" ./my-project --content docs
semble search "database host port" ./my-project --content config
semble find-related src/auth.py 42 ./my-project
semble search "save model to disk" ./my-project --top-k 10
```

The index is built on first run and cached automatically; updates are incremental and shared across branches and worktrees.

### Workflow

1. Call `mcp__semble__search` with a query describing what the code does or its name. The tool returns results with 10 lines of context each (function/class signature + first body lines, enough to confirm the location).
2. Navigate directly to the top result's file and line. Read only the function or class at that location.
3. Make the edit. Do not re-search or grep for the same content.
4. Use `--content docs` for documentation, `--content config` for config files, or `--content all` for everything.
5. Optionally use `mcp__semble__find_related` with `file_path` and `line` to discover similar code elsewhere.
6. Use Grep only when you need every occurrence of a literal string across the whole repo (e.g., all callers of a renamed function).
{SEMBLE_END}
"#
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpFormat {
    Json,
    Toml,
}

#[derive(Debug, Clone)]
struct McpConfig {
    path: PathBuf,
    key: &'static str,
    entry: Value,
    format: McpFormat,
}

#[derive(Debug, Clone)]
pub struct AgentTarget {
    /// Stable identifier, matching upstream agent ids.
    #[allow(dead_code)]
    id: &'static str,
    display_name: &'static str,
    binary: Option<&'static str>,
    config_dir: Option<PathBuf>,
    mcp: Option<McpConfig>,
    instructions_path: Option<PathBuf>,
    subagent_path: Option<PathBuf>,
    subagent_asset: Option<&'static str>,
}

fn stdio_config() -> Value {
    json!({"command": "semble", "args": [], "type": "stdio"})
}

fn bare_stdio_config() -> Value {
    json!({"command": "semble", "args": []})
}

fn opencode_config() -> Value {
    json!({"command": ["semble"], "type": "local", "enabled": true})
}

fn zed_config() -> Value {
    json!({"source": "custom", "command": "semble", "args": []})
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn opencode_mcp_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"))
        .join("opencode");
    let jsonc = base.join("opencode.jsonc");
    let json_ = base.join("opencode.json");
    if jsonc.exists() {
        jsonc
    } else if json_.exists() {
        json_
    } else {
        jsonc
    }
}

fn vscode_mcp_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        home()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("mcp.json")
    } else if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home())
            .join("Code")
            .join("User")
            .join("mcp.json")
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join(".config"))
            .join("Code")
            .join("User")
            .join("mcp.json")
    }
}

pub fn agents() -> Vec<AgentTarget> {
    let home = home();
    vec![
        AgentTarget {
            id: "claude",
            display_name: "Claude Code",
            binary: Some("claude"),
            config_dir: Some(home.join(".claude")),
            mcp: Some(McpConfig {
                path: home.join(".claude.json"),
                key: "mcpServers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".claude").join("CLAUDE.md")),
            subagent_path: Some(home.join(".claude").join("agents").join("semble-search.md")),
            subagent_asset: Some(include_str!("agents/claude.md")),
        },
        AgentTarget {
            id: "cursor",
            display_name: "Cursor",
            binary: Some("cursor"),
            config_dir: Some(home.join(".cursor")),
            mcp: Some(McpConfig {
                path: home.join(".cursor").join("mcp.json"),
                key: "mcpServers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: Some(home.join(".cursor").join("agents").join("semble-search.md")),
            subagent_asset: Some(include_str!("agents/cursor.md")),
        },
        AgentTarget {
            id: "gemini",
            display_name: "Gemini CLI",
            binary: Some("gemini"),
            config_dir: Some(home.join(".gemini")),
            mcp: Some(McpConfig {
                path: home.join(".gemini").join("settings.json"),
                key: "mcpServers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".gemini").join("GEMINI.md")),
            subagent_path: Some(home.join(".gemini").join("agents").join("semble-search.md")),
            subagent_asset: Some(include_str!("agents/gemini.md")),
        },
        AgentTarget {
            id: "kiro",
            display_name: "Kiro",
            binary: Some("kiro"),
            config_dir: Some(home.join(".kiro")),
            mcp: Some(McpConfig {
                path: home.join(".kiro").join("settings").join("mcp.json"),
                key: "mcpServers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".kiro").join("steering").join("semble.md")),
            subagent_path: Some(home.join(".kiro").join("agents").join("semble-search.md")),
            subagent_asset: Some(include_str!("agents/kiro.md")),
        },
        AgentTarget {
            id: "opencode",
            display_name: "Opencode",
            binary: Some("opencode"),
            config_dir: Some(home.join(".config").join("opencode")),
            mcp: Some(McpConfig {
                path: opencode_mcp_path(),
                key: "mcp",
                entry: opencode_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".config").join("opencode").join("AGENTS.md")),
            subagent_path: Some(
                home.join(".config")
                    .join("opencode")
                    .join("agents")
                    .join("semble-search.md"),
            ),
            subagent_asset: Some(include_str!("agents/opencode.md")),
        },
        AgentTarget {
            id: "copilot",
            display_name: "GitHub Copilot",
            binary: None,
            config_dir: Some(home.join(".config").join("github-copilot")),
            mcp: Some(McpConfig {
                path: home.join(".copilot").join("mcp-config.json"),
                key: "mcpServers",
                entry: bare_stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: Some(
                home.join(".copilot")
                    .join("agents")
                    .join("semble-search.agent.md"),
            ),
            subagent_asset: Some(include_str!("agents/copilot.md")),
        },
        AgentTarget {
            id: "codex",
            display_name: "Codex",
            binary: Some("codex"),
            config_dir: Some(home.join(".codex")),
            mcp: Some(McpConfig {
                path: home.join(".codex").join("config.toml"),
                key: "mcp_servers",
                entry: stdio_config(),
                format: McpFormat::Toml,
            }),
            instructions_path: Some(home.join(".codex").join("AGENTS.md")),
            subagent_path: Some(
                home.join(".codex")
                    .join("agents")
                    .join("semble-search.toml"),
            ),
            subagent_asset: Some(include_str!("agents/codex.toml")),
        },
        AgentTarget {
            id: "vscode",
            display_name: "VS Code",
            binary: Some("code"),
            config_dir: None,
            mcp: Some(McpConfig {
                path: vscode_mcp_path(),
                key: "servers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: None,
            subagent_asset: None,
        },
        AgentTarget {
            id: "windsurf",
            display_name: "Windsurf",
            binary: Some("windsurf"),
            config_dir: Some(home.join(".codeium").join("windsurf")),
            mcp: Some(McpConfig {
                path: home
                    .join(".codeium")
                    .join("windsurf")
                    .join("mcp_config.json"),
                key: "mcpServers",
                entry: bare_stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: None,
            subagent_asset: None,
        },
        AgentTarget {
            id: "zed",
            display_name: "Zed",
            binary: Some("zed"),
            config_dir: Some(home.join(".config").join("zed")),
            mcp: Some(McpConfig {
                path: home.join(".config").join("zed").join("settings.json"),
                key: "context_servers",
                entry: zed_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: None,
            subagent_asset: None,
        },
        AgentTarget {
            id: "reasonix",
            display_name: "Reasonix",
            binary: Some("reasonix"),
            config_dir: Some(home.join(".config").join("reasonix")),
            mcp: Some(McpConfig {
                path: home.join(".reasonix").join("config.json"),
                key: "mcpServers",
                entry: bare_stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".config").join("reasonix").join("REASONIX.md")),
            subagent_path: Some(
                home.join(".reasonix")
                    .join("skills")
                    .join("semble-search.md"),
            ),
            subagent_asset: Some(include_str!("agents/reasonix.md")),
        },
        AgentTarget {
            id: "pi",
            display_name: "Pi",
            binary: Some("pi"),
            config_dir: Some(home.join(".pi")),
            mcp: Some(McpConfig {
                path: home.join(".pi").join("agent").join("mcp.json"),
                key: "mcpServers",
                entry: bare_stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: None,
            subagent_path: Some(home.join(".pi").join("agents").join("semble-search.md")),
            subagent_asset: Some(include_str!("agents/pi.md")),
        },
        AgentTarget {
            id: "commandcode",
            display_name: "Command Code",
            binary: None,
            config_dir: Some(home.join(".commandcode")),
            mcp: Some(McpConfig {
                path: home.join(".commandcode").join("mcp.json"),
                key: "mcpServers",
                entry: bare_stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".commandcode").join("AGENTS.md")),
            subagent_path: Some(
                home.join(".commandcode")
                    .join("agents")
                    .join("semble-search.md"),
            ),
            subagent_asset: Some(include_str!("agents/commandcode.md")),
        },
        AgentTarget {
            id: "antigravity",
            display_name: "Antigravity",
            binary: Some("agy"),
            config_dir: Some(home.join(".gemini").join("antigravity-cli")),
            mcp: Some(McpConfig {
                path: home.join(".gemini").join("config").join("mcp_config.json"),
                key: "mcpServers",
                entry: stdio_config(),
                format: McpFormat::Json,
            }),
            instructions_path: Some(home.join(".gemini").join("GEMINI.md")),
            subagent_path: Some(
                home.join(".gemini")
                    .join("config")
                    .join("skills")
                    .join("semble-search")
                    .join("SKILL.md"),
            ),
            subagent_asset: Some(include_str!("agents/antigravity.md")),
        },
    ]
}

fn which(binary: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file())
}

/// Return true if the agent appears to be installed.
pub fn is_detected(agent: &AgentTarget) -> bool {
    if let Some(binary) = agent.binary {
        if which(binary) {
            return true;
        }
    }
    agent
        .config_dir
        .as_ref()
        .map(|d| d.exists())
        .unwrap_or(false)
}

// --- JSON config editing -----------------------------------------------------

/// Add or update `section_key.member_key = value` in a JSON config file.
pub fn merge_json_member(
    path: &Path,
    section_key: &str,
    member_key: &str,
    value: &Value,
) -> Action {
    let existed = path.exists();
    let text = if existed {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    if text.trim().is_empty() {
        // Missing or empty: write a clean fresh file.
        if std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
            return Action::Error;
        }
        let fresh = json!({ section_key: { member_key: value } });
        let body = serde_json::to_string_pretty(&fresh).unwrap() + "\n";
        return match std::fs::write(path, body) {
            Ok(()) => {
                if existed {
                    Action::Updated
                } else {
                    Action::Created
                }
            }
            Err(_) => Action::Error,
        };
    }

    let Ok(mut root) = serde_json::from_str::<Value>(&text) else {
        // Comments / JSON5 syntax: report skipped, add manually.
        return Action::Skipped;
    };
    let Some(obj) = root.as_object_mut() else {
        return Action::Error;
    };
    let section = obj
        .entry(section_key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(section_obj) = section.as_object_mut() else {
        return Action::Error;
    };
    let previous = section_obj.insert(member_key.to_string(), value.clone());
    if previous.as_ref() == Some(value) {
        return Action::Unchanged;
    }
    let body = serde_json::to_string_pretty(&root).unwrap() + "\n";
    match std::fs::write(path, body) {
        Ok(()) => Action::Updated,
        Err(_) => Action::Error,
    }
}

/// Remove `section_key.member_key` from a JSON config file.
pub fn remove_json_member(path: &Path, section_key: &str, member_key: &str) -> Action {
    if !path.exists() {
        return Action::NotFound;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let Ok(mut root) = serde_json::from_str::<Value>(&text) else {
        return Action::Skipped;
    };
    let removed = root
        .get_mut(section_key)
        .and_then(|s| s.as_object_mut())
        .and_then(|s| s.remove(member_key));
    if removed.is_none() {
        return Action::NotFound;
    }
    let body = serde_json::to_string_pretty(&root).unwrap() + "\n";
    match std::fs::write(path, body) {
        Ok(()) => Action::Removed,
        Err(_) => Action::Error,
    }
}

// --- TOML config editing (Codex) ----------------------------------------------

/// Drop all TOML tables matching `header` or any of its sub-tables.
fn strip_toml_section(text: &str, header: &str) -> String {
    let prefix = header.trim().trim_start_matches('[').trim_end_matches(']');
    let mut result = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let stripped = line.trim();
        let table_key = stripped.split('#').next().unwrap_or("").trim();
        if table_key.starts_with('[') && table_key.ends_with(']') {
            let table_name = &table_key[1..table_key.len() - 1];
            if table_name == prefix || table_name.starts_with(&format!("{prefix}.")) {
                skipping = true;
                continue;
            }
            skipping = false;
        }
        if skipping {
            continue;
        }
        result.push_str(line);
    }
    result
}

/// Add (or refresh) the semble `[mcp_servers.semble]` table in a Codex config.toml.
pub fn merge_toml_block(path: &Path) -> Action {
    if std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
        return Action::Error;
    }
    let existed = path.exists();
    let existing = if existed {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };
    if existing.contains(CODEX_MCP_BLOCK) {
        return Action::Unchanged;
    }
    let base = strip_toml_section(&existing, CODEX_MCP_HEADER);
    let base = base.trim_end_matches('\n');
    let body = if base.is_empty() {
        CODEX_MCP_BLOCK.to_string()
    } else {
        format!("{base}\n\n{CODEX_MCP_BLOCK}")
    };
    match std::fs::write(path, body) {
        Ok(()) => {
            if existed {
                Action::Updated
            } else {
                Action::Created
            }
        }
        Err(_) => Action::Error,
    }
}

/// Remove the semble `[mcp_servers.semble]` table from a Codex config.toml.
pub fn remove_toml_block(path: &Path) -> Action {
    if !path.exists() {
        return Action::NotFound;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if !existing.contains(CODEX_MCP_HEADER) {
        return Action::NotFound;
    }
    let remaining = strip_toml_section(&existing, CODEX_MCP_HEADER);
    let remaining = remaining.trim_matches('\n');
    let result = if remaining.is_empty() {
        std::fs::remove_file(path)
    } else {
        std::fs::write(path, format!("{remaining}\n"))
    };
    match result {
        Ok(()) => Action::Removed,
        Err(_) => Action::Error,
    }
}

// --- Marked instructions blocks -------------------------------------------------

/// Replace the marked semble section in `path`, or append it if absent.
pub fn replace_or_append_marked(path: &Path, content: &str) -> Action {
    if std::fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).is_err() {
        return Action::Error;
    }
    let existed = path.exists();
    let existing = if existed {
        std::fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    if let (Some(start_idx), Some(end_idx)) =
        (existing.find(SEMBLE_START), existing.find(SEMBLE_END))
    {
        if end_idx > start_idx {
            let before = &existing[..start_idx];
            let after = &existing[end_idx + SEMBLE_END.len()..];
            let updated = format!(
                "{before}{}\n{}",
                content.trim_matches('\n'),
                after.trim_start_matches('\n')
            );
            if updated == existing {
                return Action::Unchanged;
            }
            return match std::fs::write(path, updated) {
                Ok(()) => Action::Updated,
                Err(_) => Action::Error,
            };
        }
    }

    let separator = if !existing.is_empty() && !existing.ends_with("\n\n") {
        "\n\n"
    } else if !existing.is_empty() {
        "\n"
    } else {
        ""
    };
    match std::fs::write(path, format!("{existing}{separator}{content}")) {
        Ok(()) => {
            if existed {
                Action::Updated
            } else {
                Action::Created
            }
        }
        Err(_) => Action::Error,
    }
}

/// Remove the marked semble section from `path`.
pub fn remove_marked(path: &Path) -> Action {
    if !path.exists() {
        return Action::NotFound;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let (Some(start_idx), Some(end_idx)) = (existing.find(SEMBLE_START), existing.find(SEMBLE_END))
    else {
        return Action::NotFound;
    };
    if end_idx <= start_idx {
        return Action::NotFound;
    }
    let before = existing[..start_idx].trim_end_matches('\n');
    let after = existing[end_idx + SEMBLE_END.len()..].trim_start_matches('\n');
    let mut updated = format!("{before}\n{after}");
    updated = updated.trim_matches('\n').to_string();
    if existing.ends_with('\n') && !updated.is_empty() {
        updated.push('\n');
    }
    let result = if updated.trim().is_empty() {
        std::fs::remove_file(path)
    } else {
        std::fs::write(path, updated)
    };
    match result {
        Ok(()) => Action::Removed,
        Err(_) => Action::Error,
    }
}

// --- Integration application --------------------------------------------------

struct WriteResult {
    path: PathBuf,
    action: Action,
}

fn apply_mcp(agent: &AgentTarget, mode: Mode) -> Option<WriteResult> {
    let mcp = agent.mcp.as_ref()?;
    let action = match (mcp.format, mode) {
        (McpFormat::Toml, Mode::Install) => merge_toml_block(&mcp.path),
        (McpFormat::Toml, Mode::Uninstall) => remove_toml_block(&mcp.path),
        (McpFormat::Json, Mode::Install) => {
            merge_json_member(&mcp.path, mcp.key, "semble", &mcp.entry)
        }
        (McpFormat::Json, Mode::Uninstall) => remove_json_member(&mcp.path, mcp.key, "semble"),
    };
    Some(WriteResult {
        path: mcp.path.clone(),
        action,
    })
}

fn apply_instructions(agent: &AgentTarget, mode: Mode) -> Option<WriteResult> {
    let path = agent.instructions_path.as_ref()?;
    let action = match mode {
        Mode::Install => replace_or_append_marked(path, &instructions_block()),
        Mode::Uninstall => remove_marked(path),
    };
    Some(WriteResult {
        path: path.clone(),
        action,
    })
}

fn apply_subagent(agent: &AgentTarget, mode: Mode) -> Option<WriteResult> {
    let dest = agent.subagent_path.as_ref()?;
    let asset = agent.subagent_asset?;
    if mode == Mode::Uninstall {
        if !dest.exists() {
            return Some(WriteResult {
                path: dest.clone(),
                action: Action::NotFound,
            });
        }
        let action = match std::fs::remove_file(dest) {
            Ok(()) => Action::Removed,
            Err(_) => Action::Error,
        };
        return Some(WriteResult {
            path: dest.clone(),
            action,
        });
    }
    let existed = dest.exists();
    if std::fs::create_dir_all(dest.parent().unwrap_or(Path::new("."))).is_err() {
        return Some(WriteResult {
            path: dest.clone(),
            action: Action::Error,
        });
    }
    let action = match std::fs::write(dest, asset) {
        Ok(()) => {
            if existed {
                Action::Updated
            } else {
                Action::Created
            }
        }
        Err(_) => Action::Error,
    };
    Some(WriteResult {
        path: dest.clone(),
        action,
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Integration {
    Mcp,
    Instructions,
    Subagent,
}

impl Integration {
    fn label(&self) -> &'static str {
        match self {
            Integration::Mcp => "MCP server",
            Integration::Instructions => "Instructions",
            Integration::Subagent => "Sub-agent",
        }
    }

    fn desc(&self) -> &'static str {
        match self {
            Integration::Mcp => "lets the agent call semble directly as a tool",
            Integration::Instructions => "adds CLI usage guidance to AGENTS.md / CLAUDE.md",
            Integration::Subagent => "installs a dedicated semble-search sub-agent",
        }
    }

    fn plan_path(&self, agent: &AgentTarget) -> Option<PathBuf> {
        match self {
            Integration::Mcp => agent.mcp.as_ref().map(|m| m.path.clone()),
            Integration::Instructions => agent.instructions_path.clone(),
            Integration::Subagent => agent.subagent_path.clone(),
        }
    }

    fn apply(&self, agent: &AgentTarget, mode: Mode) -> Option<WriteResult> {
        match self {
            Integration::Mcp => apply_mcp(agent, mode),
            Integration::Instructions => apply_instructions(agent, mode),
            Integration::Subagent => apply_subagent(agent, mode),
        }
    }
}

/// Interactively install or uninstall semble across coding agents.
pub fn run(mode: Mode) -> ExitCode {
    let install = mode == Mode::Install;
    println!(
        "\n  {}\n",
        if install {
            "Semble Installer"
        } else {
            "Semble Uninstaller"
        }
    );

    let mut all_agents = agents();
    all_agents.sort_by_key(|a| !is_detected(a));
    let labels: Vec<String> = all_agents
        .iter()
        .map(|a| {
            format!(
                "{}{}",
                a.display_name,
                if is_detected(a) { "  (detected)" } else { "" }
            )
        })
        .collect();
    let defaults: Vec<bool> = all_agents
        .iter()
        .map(|a| install && is_detected(a))
        .collect();

    let prompt = if install {
        "Select agents to configure:"
    } else {
        "Select agents to remove configuration from:"
    };
    let Ok(chosen) = MultiSelect::new()
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&defaults)
        .interact()
    else {
        println!("Nothing selected. Exiting.");
        return ExitCode::SUCCESS;
    };
    if chosen.is_empty() {
        println!("Nothing selected. Exiting.");
        return ExitCode::SUCCESS;
    }
    let chosen_agents: Vec<&AgentTarget> = chosen.iter().map(|i| &all_agents[*i]).collect();

    let integrations = [
        Integration::Mcp,
        Integration::Instructions,
        Integration::Subagent,
    ];
    let integ_labels: Vec<String> = integrations
        .iter()
        .map(|i| format!("{:<13}  —  {}", i.label(), i.desc()))
        .collect();
    let Ok(chosen_integ) = MultiSelect::new()
        .with_prompt(if install {
            "Select integrations to enable:"
        } else {
            "Select integrations to remove:"
        })
        .items(&integ_labels)
        .defaults(&[true, true, true])
        .interact()
    else {
        println!("Nothing selected. Exiting.");
        return ExitCode::SUCCESS;
    };
    if chosen_integ.is_empty() {
        println!("Nothing selected. Exiting.");
        return ExitCode::SUCCESS;
    }
    let chosen_integrations: Vec<Integration> =
        chosen_integ.iter().map(|i| integrations[*i]).collect();

    println!("\n  Plan:\n");
    for agent in &chosen_agents {
        println!("  {}", agent.display_name);
        for integ in &chosen_integrations {
            match integ.plan_path(agent) {
                Some(path) => println!("    {:<13} +  {}", integ.label(), path.display()),
                None => println!("    {:<13} -  (not supported)", integ.label()),
            }
        }
    }
    println!();

    let question = if install {
        "Proceed?"
    } else {
        "Remove semble configuration?"
    };
    let confirmed = Confirm::new()
        .with_prompt(question)
        .default(install)
        .interact()
        .unwrap_or(false);
    if !confirmed {
        println!("Cancelled.");
        return ExitCode::SUCCESS;
    }

    println!();
    for agent in &chosen_agents {
        println!("  {}", agent.display_name);
        for integ in &chosen_integrations {
            match integ.apply(agent, mode) {
                None => println!("    - {}: not supported", integ.label()),
                Some(result) => {
                    let detail = match result.action {
                        Action::Skipped => {
                            " — config has comments/JSON5 syntax; add manually (see README)"
                        }
                        Action::Error => " — could not parse or edit config",
                        _ => "",
                    };
                    println!(
                        "    * {} ({}){} -> {}",
                        integ.label(),
                        result.action,
                        detail,
                        result.path.display()
                    );
                }
            }
        }
        println!();
    }
    println!(
        "  Done!{}\n",
        if install {
            " Restart your agents to pick up the changes."
        } else {
            ""
        }
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_json_creates_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let action = merge_json_member(&path, "mcpServers", "semble", &stdio_config());
        assert_eq!(action, Action::Created);
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["mcpServers"]["semble"]["command"], "semble");
    }

    #[test]
    fn merge_json_preserves_other_members() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {"other": {"command": "x"}}, "theme": "dark"}"#,
        )
        .unwrap();
        let action = merge_json_member(&path, "mcpServers", "semble", &stdio_config());
        assert_eq!(action, Action::Updated);
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["mcpServers"]["other"]["command"], "x");
        assert_eq!(parsed["mcpServers"]["semble"]["command"], "semble");
        assert_eq!(parsed["theme"], "dark");
    }

    #[test]
    fn merge_json_unchanged_when_identical() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        merge_json_member(&path, "mcpServers", "semble", &stdio_config());
        let action = merge_json_member(&path, "mcpServers", "semble", &stdio_config());
        assert_eq!(action, Action::Unchanged);
    }

    #[test]
    fn merge_json_skips_commented_configs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.jsonc");
        std::fs::write(&path, "{\n  // comment\n  \"a\": 1\n}").unwrap();
        assert_eq!(
            merge_json_member(&path, "mcpServers", "semble", &stdio_config()),
            Action::Skipped
        );
    }

    #[test]
    fn remove_json_member_works() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        merge_json_member(&path, "mcpServers", "semble", &stdio_config());
        assert_eq!(
            remove_json_member(&path, "mcpServers", "semble"),
            Action::Removed
        );
        assert_eq!(
            remove_json_member(&path, "mcpServers", "semble"),
            Action::NotFound
        );
    }

    #[test]
    fn toml_block_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[model]\nname = \"gpt\"\n").unwrap();
        assert_eq!(merge_toml_block(&path), Action::Updated);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[model]"));
        assert!(text.contains(CODEX_MCP_HEADER));
        assert_eq!(merge_toml_block(&path), Action::Unchanged);
        assert_eq!(remove_toml_block(&path), Action::Removed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[model]"));
        assert!(!text.contains(CODEX_MCP_HEADER));
    }

    #[test]
    fn marked_block_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "# My instructions\n").unwrap();
        assert_eq!(
            replace_or_append_marked(&path, &instructions_block()),
            Action::Updated
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# My instructions"));
        assert!(text.contains(SEMBLE_START));
        // Re-applying replaces in place, not duplicates.
        assert_eq!(
            replace_or_append_marked(&path, &instructions_block()),
            Action::Unchanged
        );
        assert_eq!(text.matches(SEMBLE_START).count(), 1);
        assert_eq!(remove_marked(&path), Action::Removed);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(SEMBLE_START));
        assert!(text.contains("# My instructions"));
    }

    #[test]
    fn agents_list_is_complete() {
        let list = agents();
        assert_eq!(list.len(), 14);
        let ids: Vec<&str> = list.iter().map(|a| a.id).collect();
        for id in [
            "claude",
            "cursor",
            "gemini",
            "kiro",
            "opencode",
            "copilot",
            "codex",
            "vscode",
            "windsurf",
            "zed",
            "reasonix",
            "pi",
            "commandcode",
            "antigravity",
        ] {
            assert!(ids.contains(&id), "missing agent {id}");
        }
    }
}
