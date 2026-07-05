//! MCP stdio server (port of `semble/mcp.py`).
//!
//! Implements the Model Context Protocol directly over newline-delimited
//! JSON-RPC 2.0 on stdin/stdout: `initialize`, `tools/list`, and `tools/call`
//! for the `search` and `find_related` tools. Because index assembly is
//! incremental (content-addressed store), repos are cheaply re-validated on
//! every call after a cooldown, instead of upstream's full-rebuild staleness
//! dance.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::index::TrouveIndex;
use crate::types::ContentType;
use crate::utils::{format_results, is_git_url, resolve_chunk};

const PROTOCOL_VERSION: &str = "2024-11-05";
const CACHE_MAX_SIZE: usize = 10;
/// Don't re-validate a repo sooner than this many times the last build's duration.
const MIN_REVALIDATE_FACTOR: u32 = 3;

const REPO_DESCRIPTION: &str = "A local directory path or https:// or http:// git URL (e.g. \
    https://github.com/org/repo) to index and search. The index is cached after the first call, \
    so repeat queries are fast.";

const INSTRUCTIONS: &str = "Instant code search for any local or remote git repository. Call \
    `search` once with a focused query, it returns the file path and exact line. Navigate \
    directly to that file at the given line; do not grep for the same content. Use \
    `find_related` to discover similar code elsewhere in the same repo. When working in a local \
    project, pass the project root as `repo`. For remote repos, pass an explicit https:// URL. \
    Never guess or infer URLs.";

struct CachedIndex {
    index: TrouveIndex,
    built_at: Instant,
    build_duration: Duration,
    last_used: Instant,
}

struct IndexCache {
    content: Vec<ContentType>,
    entries: HashMap<String, CachedIndex>,
}

impl IndexCache {
    fn new(content: Vec<ContentType>) -> IndexCache {
        IndexCache {
            content,
            entries: HashMap::new(),
        }
    }

    fn cache_key(&self, repo: &str) -> String {
        if is_git_url(repo) {
            repo.to_string()
        } else {
            PathBuf::from(repo)
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| repo.to_string())
        }
    }

    fn get(&mut self, repo: &str) -> Result<&TrouveIndex, String> {
        if is_git_url(repo) && !repo.starts_with("https://") && !repo.starts_with("http://") {
            return Err(format!(
                "Only https://, http://, or local directory paths are accepted as `repo`. Got: {repo:?}"
            ));
        }
        let key = self.cache_key(repo);
        let needs_build = match self.entries.get(&key) {
            None => true,
            Some(cached) => {
                // Both local paths and git URLs are re-validated once outside
                // the cooldown window: local rebuilds are cheap incremental
                // patches, and remote rebuilds reuse the persistent clone
                // cache (a TTL-gated fetch instead of a full re-clone).
                cached.built_at.elapsed() >= cached.build_duration * MIN_REVALIDATE_FACTOR
            }
        };
        if needs_build {
            let start = Instant::now();
            let built = if is_git_url(repo) {
                TrouveIndex::from_git(repo, None, &self.content, None)
            } else {
                TrouveIndex::from_path(&PathBuf::from(repo), &self.content, None)
            }
            .map_err(|e| format!("Failed to index {repo:?}: {e}"))?;
            if self.entries.len() >= CACHE_MAX_SIZE && !self.entries.contains_key(&key) {
                // Evict least-recently-used.
                if let Some(lru) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, v)| v.last_used)
                    .map(|(k, _)| k.clone())
                {
                    self.entries.remove(&lru);
                }
            }
            self.entries.insert(
                key.clone(),
                CachedIndex {
                    index: built,
                    built_at: Instant::now(),
                    build_duration: start.elapsed(),
                    last_used: Instant::now(),
                },
            );
        }
        let entry = self.entries.get_mut(&key).unwrap();
        entry.last_used = Instant::now();
        Ok(&entry.index)
    }
}

fn tool_definitions() -> Value {
    let snippet_desc = "Lines of source to include per result. Default (10): function/class \
        signature + first body lines, enough to confirm the location. 0: file path and line \
        range only. Larger values include the full chunk.";
    json!([
        {
            "name": "search",
            "description": "Search once with a focused query describing what the code does or its name. \
                Write queries using function/class names or behavior descriptions, not error messages. \
                Returns file paths and line numbers — navigate directly there, do not repeat the search. \
                Pass a git URL or local path as `repo`; indexes are cached for the session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Natural language or code query."},
                    "repo": {"type": "string", "description": REPO_DESCRIPTION},
                    "top_k": {"type": "integer", "description": "Number of results to return.", "minimum": 1, "default": 5},
                    "max_snippet_lines": {"type": "integer", "description": snippet_desc, "minimum": 0, "default": 10}
                },
                "required": ["query", "repo"]
            }
        },
        {
            "name": "find_related",
            "description": "Find code similar to a known location. Useful for discovering all \
                implementations of an interface, all callers of a function, or all tests for a class. \
                Use after `search` when you need related code beyond the primary result. Pass \
                `file_path` and `line` from a prior search result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Path to the file as stored in the index (use file_path from a search result)."},
                    "line": {"type": "integer", "description": "Line number (1-indexed)."},
                    "repo": {"type": "string", "description": REPO_DESCRIPTION},
                    "top_k": {"type": "integer", "description": "Number of similar chunks to return.", "minimum": 1, "default": 5},
                    "max_snippet_lines": {"type": "integer", "description": snippet_desc, "minimum": 0, "default": 10}
                },
                "required": ["file_path", "line", "repo"]
            }
        }
    ])
}

/// `top_k`, enforcing the `"minimum": 1` the tool schema advertises.
fn arg_top_k(args: &Value) -> Result<usize, String> {
    match args.get("top_k") {
        None | Some(Value::Null) => Ok(5),
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => Ok(n as usize),
            _ => Err("`top_k` must be an integer of at least 1.".to_string()),
        },
    }
}

/// `max_snippet_lines`: omitted or `null` means the advertised default of
/// 10; values at least as large as the chunk return the full chunk.
fn arg_snippet_lines(args: &Value) -> Option<usize> {
    match args.get("max_snippet_lines") {
        None | Some(Value::Null) => Some(10),
        Some(v) => v.as_u64().map(|n| n as usize).or(Some(10)),
    }
}

/// Run a tool; `Err` becomes an `isError: true` tool result.
fn call_tool(cache: &mut IndexCache, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "search" => {
            let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
                return Err("Missing required argument: query".to_string());
            };
            let Some(repo) = args.get("repo").and_then(|v| v.as_str()) else {
                return Err("Missing required argument: repo".to_string());
            };
            let top_k = arg_top_k(args)?;
            let max_snippet_lines = arg_snippet_lines(args);
            let index = cache.get(repo)?;
            let results = index.search(query, top_k, None, None, None, None, max_snippet_lines);
            if results.is_empty() {
                Ok("No results found.".to_string())
            } else {
                Ok(format_results(query, &results, max_snippet_lines).to_string())
            }
        }
        "find_related" => {
            let Some(file_path) = args.get("file_path").and_then(|v| v.as_str()) else {
                return Err("Missing required argument: file_path".to_string());
            };
            let Some(line) = args.get("line").and_then(|v| v.as_u64()) else {
                return Err("Missing required argument: line".to_string());
            };
            let Some(repo) = args.get("repo").and_then(|v| v.as_str()) else {
                return Err("Missing required argument: repo".to_string());
            };
            let top_k = arg_top_k(args)?;
            let max_snippet_lines = arg_snippet_lines(args);
            let index = cache.get(repo)?;
            let Some(chunk) = resolve_chunk(&index.chunks, file_path, line as u32).cloned() else {
                return Err(format!(
                    "No chunk found at {file_path}:{line}. Make sure the file is indexed and the \
                     line number is within a known chunk."
                ));
            };
            let results = index.find_related(&chunk, top_k, max_snippet_lines);
            if results.is_empty() {
                Ok(format!("No related chunks found for {file_path}:{line}."))
            } else {
                let label = format!("Chunks related to {file_path}:{line}");
                Ok(format_results(&label, &results, max_snippet_lines).to_string())
            }
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

fn handle_request(cache: &mut IndexCache, request: &Value) -> Option<Value> {
    let id = request.get("id");
    let has_id = !(id.is_none() || id == Some(&Value::Null));
    let Some(method) = request.get("method").and_then(|m| m.as_str()) else {
        // A request (has an id) without a method must still get a reply, or
        // the client hangs waiting for one.
        if has_id {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id.unwrap().clone(),
                "error": {"code": -32600, "message": "Invalid Request: missing method"},
            }));
        }
        return None;
    };
    // Notifications get no response.
    if !has_id {
        return None;
    }
    let id = id.unwrap().clone();

    let result = match method {
        "initialize" => {
            let requested = request
                .pointer("/params/protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": requested,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "trouve-search", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_definitions()}),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let default_args = json!({});
            let args = request
                .pointer("/params/arguments")
                .unwrap_or(&default_args);
            // Tool failures are still tool *results* per MCP, but must be
            // flagged so clients treat them as failed calls.
            let (text, is_error) = match call_tool(cache, name, args) {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("Method not found: {method}")},
            }));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Start an MCP stdio server (blocks until stdin closes).
pub fn serve(content: &[ContentType]) -> ExitCode {
    let mut cache = IndexCache::new(content.to_vec());
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            let mut out = stdout.lock();
            let _ = writeln!(
                out,
                "{}",
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}})
            );
            let _ = out.flush();
            continue;
        };
        if let Some(response) = handle_request(&mut cache, &request) {
            let mut out = stdout.lock();
            let _ = writeln!(out, "{response}");
            let _ = out.flush();
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list_tools() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}});
        let response = handle_request(&mut cache, &init).unwrap();
        assert_eq!(response["result"]["serverInfo"]["name"], "trouve-search");

        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = handle_request(&mut cache, &list).unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(tools[1]["name"], "find_related");
    }

    #[test]
    fn notifications_get_no_response() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle_request(&mut cache, &note).is_none());
    }

    #[test]
    fn unknown_method_returns_error() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "bogus/method"});
        let response = handle_request(&mut cache, &req).unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn rejects_non_http_git_urls() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        let err = cache.get("git://host/repo").err().unwrap();
        assert!(err.contains("Only https://"));
        let err = cache.get("ssh://git@host/repo").err().unwrap();
        assert!(err.contains("Only https://"));
    }

    #[test]
    fn tool_failures_are_flagged_as_errors() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        for (params, expect) in [
            (json!({"name": "bogus", "arguments": {}}), "Unknown tool"),
            (
                json!({"name": "search", "arguments": {}}),
                "Missing required argument: query",
            ),
            (
                json!({"name": "search", "arguments": {"query": "x", "repo": "/n", "top_k": 0}}),
                "`top_k`",
            ),
        ] {
            let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params});
            let response = handle_request(&mut cache, &req).unwrap();
            assert_eq!(response["result"]["isError"], true, "params: {params}");
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains(expect), "got: {text}");
        }
    }

    #[test]
    fn request_without_method_gets_invalid_request_error() {
        let mut cache = IndexCache::new(vec![ContentType::Code]);
        let req = json!({"jsonrpc": "2.0", "id": 7});
        let response = handle_request(&mut cache, &req).unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], 7);
        // Without an id it is malformed but unanswerable: no response.
        let note = json!({"jsonrpc": "2.0"});
        assert!(handle_request(&mut cache, &note).is_none());
    }

    #[test]
    fn snippet_lines_null_means_default() {
        assert_eq!(arg_snippet_lines(&json!({})), Some(10));
        assert_eq!(
            arg_snippet_lines(&json!({"max_snippet_lines": null})),
            Some(10)
        );
        assert_eq!(arg_snippet_lines(&json!({"max_snippet_lines": 0})), Some(0));
        assert_eq!(
            arg_snippet_lines(&json!({"max_snippet_lines": 40})),
            Some(40)
        );
    }
}
