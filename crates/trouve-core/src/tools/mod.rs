//! Tools and the `ToolExecutor` chokepoint (invariant 3).
//!
//! The agent loop never performs side effects itself: it gates each call
//! through the permission layer and hands execution to a `ToolExecutor`.
//! Local mode uses [`LocalToolExecutor`]; cloud isolation later swaps in a
//! container-backed implementation without touching the loop.

mod fs;
mod grep;
mod search;
mod shell;

pub use search::{gc_index_store_in_background, warm_index_in_background, VENDOR_SEARCH_GUIDANCE};

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Result};
use serde_json::Value;
use trouve_protocol::ToolStatus;
use trouve_providers::ToolSpec;

/// Execution context: everything a tool may touch. All paths resolve inside
/// the session worktree.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    pub worktree: PathBuf,
    /// Config dir for global tool discovery (MCP servers); None in tests.
    pub config_dir: Option<PathBuf>,
    /// Registered workspace repo root: its `.agents/.mcp.json` applies even
    /// before it is committed to the session branch.
    pub workspace_root: Option<PathBuf>,
}

impl ToolCtx {
    /// Resolve a model-supplied path inside the worktree, rejecting absolute
    /// paths and traversal.
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() {
            bail!("absolute paths are not allowed: {path}");
        }
        for comp in p.components() {
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                _ => bail!("path escapes the worktree: {path}"),
            }
        }
        Ok(self.worktree.join(p))
    }
}

pub struct ToolResult {
    pub status: ToolStatus,
    pub result: Value,
}

impl ToolResult {
    pub fn ok(result: Value) -> Self {
        Self {
            status: ToolStatus::Ok,
            result,
        }
    }
    pub fn error(message: impl std::fmt::Display) -> Self {
        Self {
            status: ToolStatus::Error,
            result: serde_json::json!({"error": message.to_string()}),
        }
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema of the arguments object.
    fn parameters(&self) -> Value;
    /// Whether the tool can change worktree or system state (drives the
    /// permission gate).
    fn mutates(&self) -> bool;
    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult;
}

/// The single chokepoint every side effect flows through.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Tool specs visible from this context (built-ins + workspace MCP
    /// tools, hence async and context-dependent).
    async fn specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec>;
    /// `None` when the tool is unknown.
    fn tool_mutates(&self, name: &str) -> Option<bool>;
    async fn execute(&self, ctx: &ToolCtx, name: &str, args: &Value) -> ToolResult;
}

/// Runs tools in-process against the local filesystem/shell, plus any MCP
/// servers configured for the workspace.
pub struct LocalToolExecutor {
    tools: Vec<Arc<dyn Tool>>,
    mcp: crate::mcp::McpManager,
}

impl Default for LocalToolExecutor {
    fn default() -> Self {
        Self::with_mcp_logs(crate::mcp::McpLogStore::default())
    }
}

impl LocalToolExecutor {
    /// Build with an externally-owned MCP log store so the engine can serve
    /// "view logs" for runtime connections too.
    pub fn with_mcp_logs(logs: crate::mcp::McpLogStore) -> Self {
        // Both search tools share one index cache (indexes are expensive to
        // build, cheap to re-validate, and identical across tools).
        let search_cache = search::shared_cache();
        Self {
            tools: vec![
                Arc::new(fs::ReadFile),
                Arc::new(fs::WriteFile),
                Arc::new(fs::ListDir),
                Arc::new(shell::Shell),
                Arc::new(grep::Grep),
                Arc::new(search::Search {
                    cache: search_cache.clone(),
                }),
                Arc::new(search::FindRelated {
                    cache: search_cache,
                }),
            ],
            mcp: crate::mcp::McpManager::with_logs(logs),
        }
    }

    fn find(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name)
    }
}

#[async_trait::async_trait]
impl ToolExecutor for LocalToolExecutor {
    async fn specs(&self, ctx: &ToolCtx) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect();
        specs.extend(
            self.mcp
                .specs(
                    ctx.config_dir.as_deref(),
                    ctx.workspace_root.as_deref(),
                    &ctx.worktree,
                )
                .await,
        );
        specs
    }

    fn tool_mutates(&self, name: &str) -> Option<bool> {
        if name.starts_with(crate::mcp::TOOL_PREFIX) {
            // MCP tools are external code: always treated as mutating so
            // the permission layer gates them (first-use approval).
            return Some(true);
        }
        self.find(name).map(|t| t.mutates())
    }

    async fn execute(&self, ctx: &ToolCtx, name: &str, args: &Value) -> ToolResult {
        if name.starts_with(crate::mcp::TOOL_PREFIX) {
            return match self
                .mcp
                .call(
                    ctx.config_dir.as_deref(),
                    ctx.workspace_root.as_deref(),
                    &ctx.worktree,
                    name,
                    args,
                )
                .await
            {
                Ok((false, value)) => ToolResult::ok(value),
                Ok((true, value)) => ToolResult {
                    status: ToolStatus::Error,
                    result: value,
                },
                Err(e) => ToolResult::error(format!("{e:#}")),
            };
        }
        match self.find(name) {
            Some(tool) => tool.run(ctx, args).await,
            None => ToolResult::error(format!("unknown tool: {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_rejects_escapes() {
        let ctx = ToolCtx {
            worktree: PathBuf::from("/tmp/wt"),
            ..Default::default()
        };
        assert!(ctx.resolve("src/main.rs").is_ok());
        assert!(ctx.resolve("./a/b").is_ok());
        assert!(ctx.resolve("/etc/passwd").is_err());
        assert!(ctx.resolve("../outside").is_err());
        assert!(ctx.resolve("a/../../outside").is_err());
    }

    #[tokio::test]
    async fn executor_reports_unknown_tools() {
        let exec = LocalToolExecutor::default();
        let ctx = ToolCtx {
            worktree: std::env::temp_dir(),
            ..Default::default()
        };
        let res = exec.execute(&ctx, "nope", &serde_json::json!({})).await;
        assert_eq!(res.status, ToolStatus::Error);
    }
}
