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
    /// Global preference order for provider-neutral model routing. Providers
    /// omitted from this list remain eligible after the explicitly ordered
    /// entries. An empty list leaves routing to live health and learned
    /// route history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_order: Vec<String>,
    /// Default provider-neutral model for new threads. A provider-qualified
    /// value remains supported and explicitly pins that route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Global thinking level for new threads. The selected model's options
    /// schema decides whether the token is supported and which wire key it
    /// maps to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<String>,
    /// Global default permission mode for new threads, used by modes that
    /// don't set one of their own. Unset means Ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_permission_mode: Option<trouve_protocol::PermissionMode>,
    /// Whether the built-in "local" provider (managed llama-server) is
    /// active. Unset means enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_enabled: Option<bool>,
    /// Client id of a GitHub OAuth app (with device flow enabled) for
    /// "Sign in with GitHub" on github.com. Unset uses the built-in shared
    /// Trouve app (`github::DEFAULT_CLIENT_ID`); set it to route sign-in
    /// through your own OAuth app instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_client_id: Option<String>,
    /// Self-hosted GitHub Enterprise instances the integration should also
    /// talk to (each with its own auth). Managed from Settings →
    /// Integrations, or by hand here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub github_enterprise: Vec<GithubEnterpriseConfig>,
    /// Set when the on-disk config failed to parse and we fell back to
    /// defaults. Never serialized; `save_to` refuses to persist in this
    /// state so a parse error can't pave over the user's real config.
    #[serde(skip)]
    pub load_failed: bool,
}

/// One GitHub Enterprise Server instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubEnterpriseConfig {
    /// Hostname only, e.g. "github.example.com".
    pub host: String,
    /// Client id of an OAuth app on that instance (device flow enabled)
    /// for sign-in. Required because OAuth is the only credential source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
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
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                // A malformed file must not silently become defaults: the
                // very next persisted setting change would then rewrite
                // config.toml from that default snapshot, destroying the
                // user's hand-written providers, hosts, and inline keys. Back
                // the broken file up and refuse to persist over it (see
                // save_to) so nothing is lost.
                tracing::error!(
                    "failed to parse {}: {e}; running with defaults but NOT overwriting the file",
                    path.display()
                );
                let backup = path.with_extension("toml.corrupt");
                if let Err(e) = std::fs::copy(path, &backup) {
                    tracing::warn!(
                        "could not back up broken config to {}: {e}",
                        backup.display()
                    );
                }
                Self {
                    load_failed: true,
                    ..Self::default()
                }
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
        if self.load_failed {
            anyhow::bail!(
                "refusing to overwrite {}: it failed to parse at startup (a backup is at \
                 <config>.toml.corrupt); fix or remove it, then restart",
                path.display()
            );
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_config_is_preserved_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "default_model = \"a/b\"\n[not valid toml").unwrap();

        let cfg = Config::load_from(&path);
        assert!(cfg.load_failed);
        // A backup was made, and saving over the original is refused.
        assert!(path.with_extension("toml.corrupt").exists());
        assert!(cfg.save_to(&path).is_err());
        // The broken file is untouched.
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("not valid toml")
        );

        // A well-formed config loads and persists normally.
        std::fs::write(
            &path,
            "default_model = \"openai/gpt\"\ndefault_thinking_level = \"high\"\n",
        )
        .unwrap();
        let cfg = Config::load_from(&path);
        assert!(!cfg.load_failed);
        assert_eq!(cfg.default_model.as_deref(), Some("openai/gpt"));
        assert_eq!(cfg.default_thinking_level.as_deref(), Some("high"));
        cfg.save_to(&path).unwrap();
    }
}
