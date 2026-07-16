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
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::index::TrouveIndex;
use crate::types::ContentType;
use crate::utils::{format_results, is_git_url, resolve_chunk};

const PROTOCOL_VERSION: &str = "2024-11-05";
const CACHE_MAX_SIZE: usize = 10;
/// Don't re-validate a repo sooner than this many times the last build's duration.
const MIN_REVALIDATE_FACTOR: u32 = 3;

const REPO_DESCRIPTION: &str = "A local directory path to index and search. The index is \
    cached after the first call, so repeat queries are fast.";

const INSTRUCTIONS: &str = "Instant code search for any local git repository. Call \
    `search` once with a focused query, it returns the file path and exact line. Navigate \
    directly to that file at the given line; do not grep for the same content. Use \
    `find_related` to discover similar code elsewhere in the same repo. Pass the project \
    root as `repo`.";

struct BuiltIndex {
    index: TrouveIndex,
    built_at: Instant,
    build_duration: Duration,
}

/// One repo's slot in the cache: its own lock, held across (re)build and
/// query, so concurrent calls for the *same* repo coordinate while calls
/// for unrelated repos proceed in parallel.
struct RepoEntry {
    last_used: Mutex<Instant>,
    built: Mutex<Option<BuiltIndex>>,
}

/// Lock, ignoring poisoning: a panicked call must not wedge every later
/// request (the cached state is rebuilt from disk if it is suspect).
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// LRU cache of built indexes, re-validated after a cooldown. Internally
/// synchronized: the map-level lock is held only for entry lookup, insert,
/// and eviction, and each repo has its own entry lock, so sessions touching
/// different repos never serialize on each other's builds or searches.
/// Public so embedders (e.g. the trouve harness's native tools) can share
/// one cache across in-process [`call_tool`] invocations.
pub struct IndexCache {
    content: Vec<ContentType>,
    entries: Mutex<HashMap<String, Arc<RepoEntry>>>,
}

impl IndexCache {
    pub fn new(content: Vec<ContentType>) -> IndexCache {
        IndexCache {
            content,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn cache_key(&self, repo: &str) -> String {
        PathBuf::from(repo)
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| repo.to_string())
    }

    /// Look up or create the repo's entry, holding the map lock only for
    /// that. Eviction removes the LRU entry from the map; in-flight calls
    /// keep their `Arc` alive until they finish.
    fn entry(&self, repo: &str) -> Result<Arc<RepoEntry>, String> {
        if is_git_url(repo) {
            return Err(format!(
                "Remote git URLs are not supported; only local directory paths are accepted as \
                 `repo`. Clone the repository and pass the local path. Got: {repo:?}"
            ));
        }
        let key = self.cache_key(repo);
        let mut entries = lock_unpoisoned(&self.entries);
        if let Some(entry) = entries.get(&key) {
            *lock_unpoisoned(&entry.last_used) = Instant::now();
            return Ok(Arc::clone(entry));
        }
        if entries.len() >= CACHE_MAX_SIZE {
            // Evict least-recently-used.
            if let Some(lru) = entries
                .iter()
                .min_by_key(|(_, v)| *lock_unpoisoned(&v.last_used))
                .map(|(k, _)| k.clone())
            {
                entries.remove(&lru);
            }
        }
        let entry = Arc::new(RepoEntry {
            last_used: Mutex::new(Instant::now()),
            built: Mutex::new(None),
        });
        entries.insert(key, Arc::clone(&entry));
        Ok(entry)
    }

    /// Run `f` against the repo's up-to-date index, (re)building it first
    /// if needed. Holds only this repo's entry lock for the duration.
    fn with_index<R>(&self, repo: &str, f: impl FnOnce(&TrouveIndex) -> R) -> Result<R, String> {
        let entry = self.entry(repo)?;
        let mut built = lock_unpoisoned(&entry.built);
        let needs_build = match built.as_ref() {
            None => true,
            Some(cached) => {
                // Re-validated once outside the cooldown window: rebuilds
                // are cheap incremental patches.
                cached.built_at.elapsed() >= cached.build_duration * MIN_REVALIDATE_FACTOR
            }
        };
        if needs_build {
            let start = Instant::now();
            let index = TrouveIndex::from_path(&PathBuf::from(repo), &self.content, None)
                .map_err(|e| format!("Failed to index {repo:?}: {e}"))?;
            *built = Some(BuiltIndex {
                index,
                built_at: Instant::now(),
                build_duration: start.elapsed(),
            });
        }
        Ok(f(&built.as_ref().unwrap().index))
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
                Pass a local path as `repo`; indexes are cached for the session.",
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

/// Run the `search` / `find_related` tool with MCP-shaped arguments;
/// `Err` becomes an `isError: true` tool result (or an embedder's tool
/// error). Public for in-process embedding alongside [`IndexCache`];
/// the cache synchronizes internally, so concurrent calls only serialize
/// when they touch the same repo.
pub fn call_tool(cache: &IndexCache, name: &str, args: &Value) -> Result<String, String> {
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
            cache.with_index(repo, |index| {
                let results = index.search(query, top_k, None, None, None, None, max_snippet_lines);
                if results.is_empty() {
                    "No results found.".to_string()
                } else {
                    format_results(query, &results, max_snippet_lines).to_string()
                }
            })
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
            cache.with_index(repo, |index| {
                let Some(chunk) = resolve_chunk(&index.chunks, file_path, line as u32).cloned()
                else {
                    return Err(format!(
                        "No chunk found at {file_path}:{line}. Make sure the file is indexed and \
                         the line number is within a known chunk."
                    ));
                };
                let results = index.find_related(&chunk, top_k, max_snippet_lines);
                if results.is_empty() {
                    Ok(format!("No related chunks found for {file_path}:{line}."))
                } else {
                    let label = format!("Chunks related to {file_path}:{line}");
                    Ok(format_results(&label, &results, max_snippet_lines).to_string())
                }
            })?
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

pub(crate) fn handle_request(cache: &IndexCache, request: &Value) -> Option<Value> {
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
            // The cache locks per repo internally, so a slow index build
            // only stalls calls for that repo — other sessions' searches,
            // ping, and initialize proceed concurrently.
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

/// Answer one raw request line, or `None` for notifications.
pub(crate) fn respond_line(cache: &IndexCache, line: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(line) {
        Ok(request) => handle_request(cache, &request),
        Err(_) => Some(
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}),
        ),
    }
}

/// Serve newline-delimited JSON-RPC requests from `reader`, writing
/// responses to `writer`, until `reader` is exhausted. Shared by the stdio
/// server and each connection of the unix-socket daemon; the cache
/// synchronizes internally per repo, so connections only contend when they
/// query the same repository.
pub(crate) fn serve_lines<R: BufRead, W: Write>(cache: &IndexCache, reader: R, mut writer: W) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = respond_line(cache, &line) {
            // A buffered writer can accept the write and only fail on
            // flush; either way the client is gone, so stop serving
            // instead of executing further calls nobody will hear about.
            if writeln!(writer, "{response}").is_err() || writer.flush().is_err() {
                break;
            }
        }
    }
}

/// Start an MCP stdio server (blocks until stdin closes).
pub fn serve(content: &[ContentType]) -> ExitCode {
    let cache = IndexCache::new(content.to_vec());
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_lines(&cache, stdin.lock(), stdout.lock());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache() -> IndexCache {
        IndexCache::new(vec![ContentType::Code])
    }

    #[test]
    fn initialize_and_list_tools() {
        let cache = test_cache();
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}});
        let response = handle_request(&cache, &init).unwrap();
        assert_eq!(response["result"]["serverInfo"]["name"], "trouve-search");

        let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = handle_request(&cache, &list).unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(tools[1]["name"], "find_related");
    }

    #[test]
    fn notifications_get_no_response() {
        let cache = test_cache();
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle_request(&cache, &note).is_none());
    }

    #[test]
    fn unknown_method_returns_error() {
        let cache = test_cache();
        let req = json!({"jsonrpc": "2.0", "id": 5, "method": "bogus/method"});
        let response = handle_request(&cache, &req).unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn rejects_git_urls() {
        let cache = IndexCache::new(vec![ContentType::Code]);
        for repo in [
            "https://github.com/org/repo",
            "git://host/repo",
            "ssh://git@host/repo",
            "git@github.com:org/repo.git",
        ] {
            let err = cache.entry(repo).err().unwrap();
            assert!(err.contains("not supported"), "repo: {repo}, got: {err}");
        }
    }

    #[test]
    fn tool_failures_are_flagged_as_errors() {
        let cache = test_cache();
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
            let response = handle_request(&cache, &req).unwrap();
            assert_eq!(response["result"]["isError"], true, "params: {params}");
            let text = response["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains(expect), "got: {text}");
        }
    }

    #[test]
    fn request_without_method_gets_invalid_request_error() {
        let cache = test_cache();
        let req = json!({"jsonrpc": "2.0", "id": 7});
        let response = handle_request(&cache, &req).unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(response["id"], 7);
        // Without an id it is malformed but unanswerable: no response.
        let note = json!({"jsonrpc": "2.0"});
        assert!(handle_request(&cache, &note).is_none());
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
