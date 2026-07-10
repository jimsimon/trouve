//! Native semantic code search, backed by the trouve-search library
//! (hybrid BM25 + embedding index with an incremental, content-addressed
//! chunk store shared across branches and worktrees).
//!
//! Fast codebase indexing is a core harness feature: these tools run
//! in-process — no MCP server, no external binary — and share one LRU
//! index cache across all threads and sessions.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use trouve_search::mcp::{call_tool, IndexCache};
use trouve_search::types::ContentType;

use super::{Tool, ToolCtx, ToolResult};

/// Build the search index for a session worktree on a detached background
/// thread — the in-process equivalent of the agent plugins' SessionStart
/// hook (which runs `trouve-search stats` in the background). The build
/// populates the on-disk chunk store and snapshot shared across worktrees,
/// so the session's first `search` call assembles from cache instantly.
pub fn warm_index_in_background(worktree: PathBuf) {
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        match trouve_search::index::TrouveIndex::from_path(&worktree, &[ContentType::Code], None) {
            Ok(index) => {
                let stats = index.stats();
                tracing::info!(
                    "warmed search index for {} ({} files, {} chunks) in {:.1?}",
                    worktree.display(),
                    stats.indexed_files,
                    stats.total_chunks,
                    started.elapsed(),
                );
            }
            // Non-fatal by design: no embedding model yet, not a real repo,
            // etc. The first search call surfaces any persistent problem.
            Err(e) => tracing::debug!(
                "search index warm skipped for {}: {e:#}",
                worktree.display()
            ),
        }
    });
}

/// Opportunistic index-store GC after a session is archived or deleted.
///
/// The chunk store is shared across every worktree and branch of a repo
/// (keyed by its git common dir), so there is no per-session data to delete.
/// Instead, snapshots auto-prune and a throttled mark-and-sweep reclaims
/// entries no kept snapshot references; session teardown is a natural moment
/// to trigger that sweep.
pub fn gc_index_store_in_background(repo: PathBuf) {
    std::thread::spawn(move || {
        let identity = trouve_search::manifest::detect_repo_identity(&repo);
        match trouve_search::store::ChunkStore::open(identity.as_str()) {
            Ok(store) => {
                if let Some(report) = store.maybe_gc() {
                    tracing::info!(
                        "search index GC for {}: removed {} entries ({} bytes)",
                        repo.display(),
                        report.entries_removed,
                        report.bytes_removed,
                    );
                }
            }
            Err(e) => tracing::debug!("search index GC skipped for {}: {e:#}", repo.display()),
        }
    });
}

/// System-prompt guidance for external vendor agents (Claude Code, Codex)
/// that reach trouve's search tools over the MCP bridge. Vendor agents
/// strongly prefer their built-in shell/grep/glob tools; this steers them to
/// the semantic index first (wording adapted from trouve-search's agent
/// plugin docs). Tool references stay vendor-neutral — Claude sees the tools
/// as `mcp__trouve__search`, Codex under its own MCP naming — so they are
/// described by server + tool name.
pub const VENDOR_SEARCH_GUIDANCE: &str = "\
## Code search (IMPORTANT)

This workspace has a pre-built semantic code index, exposed as the `search` \
tool on the `trouve` MCP server. It is your PRIMARY tool for exploring this \
codebase: finding code, files, or configuration by intent or by symbol name, \
locating implementations, or understanding how something works.

- Do NOT explore with shell scans (`find`, `grep`, `rg`, `ls -R`, `cat`) or \
  built-in grep/glob tools for discovery. Call `search` first; it returns \
  file paths with exact line numbers.
- Read the returned file at the given line directly. Never grep for content \
  a search result already located.
- Call `find_related` on the `trouve` server with a result's file_path and \
  line to find similar implementations, callers, or tests.
- Plain grep is appropriate only for exhaustive literal matches (e.g. every \
  occurrence of an exact string before a rename) after `search` has located \
  the primary definition.";

/// One cache for the whole executor: indexes are expensive to build and
/// cheap to re-validate, so every session shares them.
pub fn shared_cache() -> Arc<Mutex<IndexCache>> {
    Arc::new(Mutex::new(IndexCache::new(vec![ContentType::Code])))
}

/// Resolve the `repo` argument: session worktree by default, or a
/// workspace-relative subdirectory.
fn resolve_repo(ctx: &ToolCtx, args: &Value) -> Result<String, String> {
    match args.get("repo").and_then(Value::as_str) {
        None | Some("") | Some(".") => Ok(ctx.worktree.to_string_lossy().into_owned()),
        Some(url) if trouve_search::utils::is_git_url(url) => Err(
            "Remote git URLs are not supported; pass a workspace-relative directory.".to_string(),
        ),
        Some(rel) => ctx
            .resolve(rel)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| e.to_string()),
    }
}

/// Run one trouve-search tool off the async threads (indexing is CPU-heavy
/// and the first call may download the embedding model).
async fn run_search_tool(
    cache: &Arc<Mutex<IndexCache>>,
    name: &'static str,
    ctx: &ToolCtx,
    args: &Value,
) -> ToolResult {
    let repo = match resolve_repo(ctx, args) {
        Ok(r) => r,
        Err(e) => return ToolResult::error(e),
    };
    let mut args = args.clone();
    args["repo"] = json!(repo);
    let cache = cache.clone();
    let out = tokio::task::spawn_blocking(move || {
        let mut cache = cache.lock().unwrap();
        call_tool(&mut cache, name, &args)
    })
    .await
    .unwrap_or_else(|e| Err(format!("{name} panicked: {e}")));
    match out {
        Ok(text) => ToolResult::ok(Value::String(text)),
        Err(e) => ToolResult::error(e),
    }
}

const REPO_PARAM: &str = "Workspace-relative directory to search. \
     Default: the session worktree.";
const SNIPPET_PARAM: &str = "Lines of source per result. Default (10): signature + first \
     body lines. 0: path and line range only.";

pub struct Search {
    pub(super) cache: Arc<Mutex<IndexCache>>,
}

#[async_trait::async_trait]
impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }
    fn description(&self) -> &'static str {
        "Semantic code search over the workspace (hybrid keyword + embedding index). \
         Query with function/class names or behavior descriptions, not error messages. \
         Returns file paths and line numbers — navigate directly there instead of grepping \
         for the same content. The first call indexes the repo (and may download the \
         embedding model once); repeat calls are fast."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Natural language or code query."},
                "repo": {"type": "string", "description": REPO_PARAM},
                "top_k": {"type": "integer", "description": "Number of results.", "minimum": 1, "default": 5},
                "max_snippet_lines": {"type": "integer", "description": SNIPPET_PARAM, "minimum": 0, "default": 10}
            },
            "required": ["query"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        run_search_tool(&self.cache, "search", ctx, args).await
    }
}

pub struct FindRelated {
    pub(super) cache: Arc<Mutex<IndexCache>>,
}

#[async_trait::async_trait]
impl Tool for FindRelated {
    fn name(&self) -> &'static str {
        "find_related"
    }
    fn description(&self) -> &'static str {
        "Find code similar to a known location — implementations of an interface, callers \
         of a function, tests for a class. Use after `search` with a result's file_path \
         and line."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "Path as reported by a search result."},
                "line": {"type": "integer", "description": "Line number (1-indexed)."},
                "repo": {"type": "string", "description": REPO_PARAM},
                "top_k": {"type": "integer", "description": "Number of results.", "minimum": 1, "default": 5},
                "max_snippet_lines": {"type": "integer", "description": SNIPPET_PARAM, "minimum": 0, "default": 10}
            },
            "required": ["file_path", "line"]
        })
    }
    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        run_search_tool(&self.cache, "find_related", ctx, args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_resolution_defaults_and_rejects_escapes() {
        let ctx = ToolCtx {
            worktree: std::path::PathBuf::from("/tmp/wt"),
            ..Default::default()
        };
        assert_eq!(resolve_repo(&ctx, &json!({})).unwrap(), "/tmp/wt");
        assert_eq!(
            resolve_repo(&ctx, &json!({"repo": "."})).unwrap(),
            "/tmp/wt"
        );
        assert_eq!(
            resolve_repo(&ctx, &json!({"repo": "sub/dir"})).unwrap(),
            "/tmp/wt/sub/dir"
        );
        assert!(
            resolve_repo(&ctx, &json!({"repo": "https://github.com/org/repo"}))
                .is_err_and(|e| e.contains("not supported"))
        );
        assert!(resolve_repo(&ctx, &json!({"repo": "/etc"})).is_err());
        assert!(resolve_repo(&ctx, &json!({"repo": "../up"})).is_err());
    }
}
