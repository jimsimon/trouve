//! Server configuration: data locations and provider credentials.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Where the server keeps its database and session worktrees.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TROUVE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("trouve")
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("TROUVE_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("trouve")
        .join("config.toml")
}

/// `config.toml` shape. Secrets set through the API/CLI live in the secret
/// store; a key in the file (or via env var) is honored but never written
/// back by the server.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ProviderConfig>,
    /// Default model for new threads, e.g. "openai/gpt-4.1-mini".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Whether the built-in "local" provider (managed llama-server) is
    /// active. Unset means enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_enabled: Option<bool>,
    /// Client id of a GitHub OAuth app (with device flow enabled) for
    /// "Sign in with GitHub". Unset disables the OAuth path; a pasted
    /// token, GITHUB_TOKEN, or the gh CLI still work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_client_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Wire protocol / integration kind:
    /// - "openai-compat" (chat completions; OpenAI, OpenRouter, Ollama, ...)
    /// - "anthropic" (Messages API)
    /// - "codex-app-server", "cursor-cli", "claude-cli" (vendor agent
    ///   backends driven through their CLIs; auth lives in the vendor CLI)
    /// - "codex-responses" (EXPERIMENTAL direct ChatGPT-backend client using
    ///   the `codex login` token)
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable to read the key from when `api_key` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// OAuth endpoints for subscription auth (`trouve auth login <id>`).
    /// When set and no API key is available, requests use the stored OAuth
    /// tokens (refreshed automatically).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<trouve_providers::auth::OAuthConfig>,
    /// Path/name of the vendor binary for CLI-backed kinds (defaults:
    /// "codex", "cursor-agent", "claude"). Also how tests point adapters at
    /// stub binaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Claude Code only: disable the vendor's built-in tools and bridge
    /// trouve's ToolExecutor in over MCP (full trouve tool/permission
    /// fidelity), served from the engine's embedded HTTP MCP endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_bridge: Option<bool>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: default_kind(),
            base_url: None,
            api_key: None,
            api_key_env: None,
            oauth: None,
            command: None,
            tool_bridge: None,
        }
    }
}

fn default_kind() -> String {
    "openai-compat".into()
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("failed to parse {}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist to `config.toml`. API keys set via the protocol are stored in
    /// the secret store, so this only ever writes non-secret settings (plus
    /// any keys the user put in the file themselves, which are preserved).
    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
