//! MCP stdio server (port of `semble/mcp.py`).
//!
//! Implements the Model Context Protocol directly over newline-delimited
//! JSON-RPC 2.0 on stdin/stdout: `initialize`, `tools/list`, and `tools/call`
//! for the `search` and `find_related` tools. Because index assembly is
//! incremental (content-addressed store), repos are cheaply re-validated on
//! every call after a cooldown.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::index::TrouveIndex;
use crate::types::ContentType;
use crate::utils::{format_results, is_git_url, resolve_chunk};

const PROTOCOL_VERSION: &str = "2024-11-05";
const CACHE_MAX_SIZE: usize = 10;
const CACHE_MAX_HEAP_BYTES: usize = 512 * 1024 * 1024;
const MAX_TOOL_TOP_K: u64 = 100;
const MAX_TOOL_SNIPPET_LINES: u64 = 1_000;
/// Don't re-validate a repo sooner than this many times the last build's duration.
const MIN_REVALIDATE_FACTOR: u32 = 3;
const MIN_REVALIDATE_INTERVAL: Duration = Duration::from_millis(250);

const REPO_DESCRIPTION: &str = "A local directory path to index and search. The index is \
    cached after the first call, so repeat queries are fast.";
const CONTENT_DESCRIPTION: &str = "What to search: code, docs (documentation and prose), \
    config (YAML/TOML/etc.), or all. Omit to use the server's configured content.";

const INSTRUCTIONS: &str = "Instant code search for any local git repository. Call \
    `search` once with a focused query, it returns the file path and exact line. Navigate \
    directly to that file at the given line; do not grep for the same content. Use \
    `find_related` to discover similar code elsewhere in the same repo. Pass the project \
    root as `repo`.";

struct BuiltIndex {
    index: Arc<TrouveIndex>,
    built_at: Instant,
    build_duration: Duration,
}

/// One repo's slot in the cache: its own lock, held across (re)build and
/// query, so concurrent calls for the *same* repo coordinate while calls
/// for unrelated repos proceed in parallel.
struct RepoEntry {
    last_used: Mutex<Instant>,
    built: RwLock<Option<BuiltIndex>>,
    #[cfg(test)]
    retained_heap_override: Mutex<Option<(usize, usize)>>,
}

/// Lock, ignoring poisoning: a panicked call must not wedge every later
/// request (the cached state is rebuilt from disk if it is suspect).
fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn index_is_fresh(index: &BuiltIndex) -> bool {
    let cooldown = (index.build_duration * MIN_REVALIDATE_FACTOR).max(MIN_REVALIDATE_INTERVAL);
    index.built_at.elapsed() < cooldown
}

/// LRU cache of built indexes, re-validated after a cooldown. Internally
/// synchronized: the map-level lock is held only for entry lookup, insert,
/// and eviction, and each repo/content pair has its own entry lock, so
/// sessions touching different index variants never serialize on each
/// other's builds or searches.
/// Public so embedders (e.g. the trouve harness's native tools) can share
/// one cache across in-process [`call_tool`] invocations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    repo: String,
    content: Vec<ContentType>,
}

pub struct IndexCache {
    default_content: Vec<ContentType>,
    entries: Mutex<HashMap<CacheKey, Arc<RepoEntry>>>,
    shared_indexes: Mutex<HashMap<crate::index::IndexIdentity, Weak<TrouveIndex>>>,
    max_heap_bytes: usize,
}

fn normalized_content(content: &[ContentType]) -> Vec<ContentType> {
    let mut content = content.to_vec();
    if content.is_empty() {
        content.push(ContentType::Code);
    }
    content.sort_unstable();
    content.dedup();
    content
}

impl IndexCache {
    pub fn new(content: Vec<ContentType>) -> IndexCache {
        IndexCache {
            default_content: normalized_content(&content),
            entries: Mutex::new(HashMap::new()),
            shared_indexes: Mutex::new(HashMap::new()),
            max_heap_bytes: CACHE_MAX_HEAP_BYTES,
        }
    }

    fn cache_key(&self, repo: &str, content: &[ContentType]) -> CacheKey {
        let repo = PathBuf::from(repo)
            .canonicalize()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| repo.to_string());
        CacheKey {
            repo,
            content: normalized_content(content),
        }
    }

    /// Look up or create the repo/content entry, holding the map lock only
    /// for that. Eviction removes the LRU entry from the map; in-flight calls
    /// keep their `Arc` alive until they finish.
    fn entry(
        &self,
        repo: &str,
        content: &[ContentType],
    ) -> Result<(CacheKey, Arc<RepoEntry>), String> {
        if is_git_url(repo) {
            return Err(format!(
                "Remote git URLs are not supported; only local directory paths are accepted as \
                 `repo`. Clone the repository and pass the local path. Got: {repo:?}"
            ));
        }
        let key = self.cache_key(repo, content);
        let mut entries = lock_unpoisoned(&self.entries);
        if let Some(entry) = entries.get(&key) {
            *lock_unpoisoned(&entry.last_used) = Instant::now();
            return Ok((key, Arc::clone(entry)));
        }
        if entries.len() >= CACHE_MAX_SIZE {
            // Evict the least-recently-used entry nobody is using. The map
            // owns one reference; more means an in-flight call, and evicting
            // under it would let the next call for that repo build a
            // duplicate index behind a second lock. Only the map hands out
            // new references, and we hold its lock, so the count is stable.
            if let Some(lru) = entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(entry) == 1)
                .min_by_key(|(_, v)| *lock_unpoisoned(&v.last_used))
                .map(|(k, _)| k.clone())
            {
                entries.remove(&lru);
            }
        }
        let entry = Arc::new(RepoEntry {
            last_used: Mutex::new(Instant::now()),
            built: RwLock::new(None),
            #[cfg(test)]
            retained_heap_override: Mutex::new(None),
        });
        entries.insert(key.clone(), Arc::clone(&entry));
        Ok((key, entry))
    }

    fn intern_index(&self, index: TrouveIndex) -> Arc<TrouveIndex> {
        let identity = index.cache_identity().clone();
        let mut shared = lock_unpoisoned(&self.shared_indexes);
        shared.retain(|_, index| index.strong_count() > 0);
        if let Some(existing) = shared.get(&identity).and_then(Weak::upgrade) {
            return existing;
        }
        let index = Arc::new(index);
        shared.insert(identity, Arc::downgrade(&index));
        index
    }

    /// Enforce both the entry-count guardrail and a process-heap budget.
    /// In-flight entries are never evicted.
    fn trim(&self) -> bool {
        let mut entries = lock_unpoisoned(&self.entries);
        let mut removed = false;
        loop {
            let heap_bytes = retained_heap_bytes(&entries);
            if entries.len() <= CACHE_MAX_SIZE && heap_bytes <= self.max_heap_bytes {
                break;
            }
            let Some(lru) = entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(entry) == 1)
                .min_by_key(|(_, entry)| *lock_unpoisoned(&entry.last_used))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            entries.remove(&lru);
            removed = true;
        }
        removed
    }

    /// Run `f` against the repo's up-to-date index, (re)building it first
    /// if needed. Fresh-index queries share a read lock; only incremental
    /// revalidation is exclusive, so parallel tool calls against one repo do
    /// not queue behind each other after the initial build.
    fn with_index<R>(
        &self,
        repo: &str,
        content: &[ContentType],
        f: impl FnOnce(&TrouveIndex) -> R,
    ) -> Result<R, String> {
        let (key, entry) = self.entry(repo, content)?;
        let mut f = Some(f);
        let mut rebuilt = false;
        let result = (|| {
            let cached = {
                let built = read_unpoisoned(&entry.built);
                built
                    .as_ref()
                    .filter(|cached| index_is_fresh(cached))
                    .map(|cached| Arc::clone(&cached.index))
            };
            let index = if let Some(cached) = cached {
                cached
            } else {
                let mut built = write_unpoisoned(&entry.built);
                // A different caller may have completed revalidation while this one
                // waited for the write lock.
                if !built.as_ref().is_some_and(index_is_fresh) {
                    let start = Instant::now();
                    let index = TrouveIndex::from_path(&PathBuf::from(repo), &key.content, None)
                        .map_err(|e| format!("Failed to index {repo:?}: {e}"))?;
                    let index = self.intern_index(index);
                    *built = Some(BuiltIndex {
                        index,
                        built_at: Instant::now(),
                        build_duration: start.elapsed(),
                    });
                    rebuilt = true;
                }
                Arc::clone(&built.as_ref().expect("index was built").index)
            };
            Ok(f.take().expect("query closure is available")(&index))
        })();

        // Every caller trims after releasing its entry reference. This lets
        // the last call in a concurrent burst restore both cache limits.
        drop(entry);
        let evicted = self.trim();
        if evicted {
            lock_unpoisoned(&self.shared_indexes).retain(|_, index| index.strong_count() > 0);
        }
        if rebuilt || evicted {
            // Index assembly has large parallel scratch buffers. Returning
            // pages here prevents a long-lived MCP server from retaining the
            // build's high-water mark after buffers and evicted indexes drop.
            crate::release_unused_memory_in_background();
        }
        result
    }
}

fn retained_heap_bytes(entries: &HashMap<CacheKey, Arc<RepoEntry>>) -> usize {
    let mut unique = HashMap::<usize, usize>::new();
    for entry in entries.values() {
        if let Some(built) = read_unpoisoned(&entry.built).as_ref() {
            unique
                .entry(Arc::as_ptr(&built.index) as usize)
                .or_insert_with(|| built.index.estimated_heap_bytes());
        } else {
            #[cfg(test)]
            if let Some((identity, heap_bytes)) = *lock_unpoisoned(&entry.retained_heap_override) {
                unique.entry(identity).or_insert(heap_bytes);
            }
        }
    }
    unique.values().sum()
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
                    "top_k": {"type": "integer", "description": "Number of results to return.", "minimum": 1, "maximum": MAX_TOOL_TOP_K, "default": 5},
                    "max_snippet_lines": {"type": "integer", "description": snippet_desc, "minimum": 0, "maximum": MAX_TOOL_SNIPPET_LINES, "default": 10},
                    "content": {"type": "string", "enum": ["code", "docs", "config", "all"], "description": CONTENT_DESCRIPTION}
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
                    "top_k": {"type": "integer", "description": "Number of similar chunks to return.", "minimum": 1, "maximum": MAX_TOOL_TOP_K, "default": 5},
                    "max_snippet_lines": {"type": "integer", "description": snippet_desc, "minimum": 0, "maximum": MAX_TOOL_SNIPPET_LINES, "default": 10},
                    "content": {"type": "string", "enum": ["code", "docs", "config", "all"], "description": CONTENT_DESCRIPTION}
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
            Some(n) if (1..=MAX_TOOL_TOP_K).contains(&n) => Ok(n as usize),
            _ => Err(format!(
                "`top_k` must be an integer from 1 to {MAX_TOOL_TOP_K}."
            )),
        },
    }
}

/// `max_snippet_lines`: omitted or `null` means the advertised default of
/// 10; values at least as large as the chunk return the full chunk.
fn arg_snippet_lines(args: &Value) -> Option<usize> {
    match args.get("max_snippet_lines") {
        None | Some(Value::Null) => Some(10),
        Some(v) => v
            .as_u64()
            .map(|n| n.min(MAX_TOOL_SNIPPET_LINES) as usize)
            .or(Some(10)),
    }
}

fn arg_content(cache: &IndexCache, args: &Value) -> Result<Vec<ContentType>, String> {
    match args.get("content") {
        None | Some(Value::Null) => Ok(cache.default_content.clone()),
        Some(Value::String(value)) if value == "all" => Ok(ContentType::ALL.to_vec()),
        Some(Value::String(value)) => ContentType::parse(value)
            .map(|content| vec![content])
            .ok_or_else(|| "`content` must be one of: code, docs, config, all.".to_string()),
        Some(_) => Err("`content` must be one of: code, docs, config, all.".to_string()),
    }
}

/// Run the `search` / `find_related` tool with MCP-shaped arguments;
/// `Err` becomes an `isError: true` tool result (or an embedder's tool
/// error). Public for in-process embedding alongside [`IndexCache`];
/// the cache synchronizes internally, so concurrent calls only serialize
/// when they touch the same repo/content variant.
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
            let content = arg_content(cache, args)?;
            cache.with_index(repo, &content, |index| {
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
            let content = arg_content(cache, args)?;
            cache.with_index(repo, &content, |index| {
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
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["properties"]["content"]["enum"],
                json!(["code", "docs", "config", "all"])
            );
        }
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
            let err = cache.entry(repo, &[ContentType::Code]).err().unwrap();
            assert!(err.contains("not supported"), "repo: {repo}, got: {err}");
        }
    }

    #[test]
    fn content_defaults_to_normalized_cache_configuration_and_validates_overrides() {
        let cache = IndexCache::new(vec![
            ContentType::Docs,
            ContentType::Code,
            ContentType::Docs,
        ]);
        let configured = vec![ContentType::Code, ContentType::Docs];
        assert_eq!(arg_content(&cache, &json!({})).unwrap(), configured);
        assert_eq!(
            arg_content(&cache, &json!({"content": null})).unwrap(),
            configured
        );
        assert_eq!(
            arg_content(&cache, &json!({"content": "config"})).unwrap(),
            vec![ContentType::Config]
        );
        assert_eq!(
            arg_content(&cache, &json!({"content": "all"})).unwrap(),
            ContentType::ALL
        );
        for invalid in [json!("source"), json!(42)] {
            let error = arg_content(&cache, &json!({"content": invalid})).unwrap_err();
            assert!(error.contains("code, docs, config, all"));
        }
    }

    #[test]
    fn entry_cache_keys_by_repo_and_normalized_content_set() {
        let cache = test_cache();
        let repo = "/definitely-missing/content-key-repo";

        let (code_key, code_entry) = cache.entry(repo, &[ContentType::Code]).unwrap();
        let (duplicate_key, duplicate_entry) = cache
            .entry(repo, &[ContentType::Code, ContentType::Code])
            .unwrap();
        assert_eq!(code_key, duplicate_key);
        assert!(Arc::ptr_eq(&code_entry, &duplicate_entry));

        let (docs_key, docs_entry) = cache.entry(repo, &[ContentType::Docs]).unwrap();
        assert_ne!(code_key, docs_key);
        assert!(!Arc::ptr_eq(&code_entry, &docs_entry));

        let (all_key, all_entry) = cache.entry(repo, &ContentType::ALL).unwrap();
        let (unordered_key, unordered_entry) = cache
            .entry(
                repo,
                &[
                    ContentType::Config,
                    ContentType::Code,
                    ContentType::Docs,
                    ContentType::Code,
                ],
            )
            .unwrap();
        assert_eq!(all_key, unordered_key);
        assert!(Arc::ptr_eq(&all_entry, &unordered_entry));
        assert_eq!(lock_unpoisoned(&cache.entries).len(), 3);
    }

    #[test]
    fn entry_cache_evicts_the_oldest_idle_worktree_at_capacity() {
        let cache = test_cache();
        for index in 0..=CACHE_MAX_SIZE {
            drop(
                cache
                    .entry(
                        &format!("/definitely-missing/repo-{index}"),
                        &[ContentType::Code],
                    )
                    .unwrap(),
            );
        }
        let oldest = cache.cache_key("/definitely-missing/repo-0", &[ContentType::Code]);
        let newest = cache.cache_key(
            &format!("/definitely-missing/repo-{CACHE_MAX_SIZE}"),
            &[ContentType::Code],
        );
        let entries = lock_unpoisoned(&cache.entries);
        assert_eq!(entries.len(), CACHE_MAX_SIZE);
        assert!(!entries.contains_key(&oldest));
        assert!(entries.contains_key(&newest));
    }

    #[test]
    fn concurrent_entries_trim_after_the_last_active_call() {
        const RETAINED_BYTES: usize = 128;
        let cache = Arc::new(IndexCache {
            default_content: vec![ContentType::Code],
            entries: Mutex::new(HashMap::new()),
            shared_indexes: Mutex::new(HashMap::new()),
            max_heap_bytes: RETAINED_BYTES,
        });
        let worker_count = CACHE_MAX_SIZE + 4;
        let barrier = Arc::new(std::sync::Barrier::new(worker_count + 1));
        let workers = (0..worker_count)
            .map(|index| {
                let cache = Arc::clone(&cache);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let (_, entry) = cache
                        .entry(
                            &format!("/definitely-missing/concurrent-repo-{index}"),
                            &[ContentType::Code],
                        )
                        .unwrap();
                    let identity = if index < 2 { 1 } else { index };
                    *lock_unpoisoned(&entry.retained_heap_override) =
                        Some((identity, RETAINED_BYTES));
                    barrier.wait();
                    barrier.wait();
                    drop(entry);
                    cache.trim();
                })
            })
            .collect::<Vec<_>>();

        barrier.wait();
        assert_eq!(
            retained_heap_bytes(&lock_unpoisoned(&cache.entries)),
            (worker_count - 1) * RETAINED_BYTES,
            "shared indexes must only count once toward the heap budget"
        );
        barrier.wait();

        for worker in workers {
            worker.join().unwrap();
        }
        let entries = lock_unpoisoned(&cache.entries);
        assert!(entries.len() <= CACHE_MAX_SIZE);
        assert!(entries.len() < worker_count);
        assert!(retained_heap_bytes(&entries) <= cache.max_heap_bytes);
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
            (
                json!({"name": "search", "arguments": {"query": "x", "repo": "/n", "top_k": 101}}),
                "`top_k`",
            ),
            (
                json!({"name": "search", "arguments": {"query": "x", "repo": "/n", "content": "source"}}),
                "`content`",
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
        assert_eq!(
            arg_snippet_lines(&json!({"max_snippet_lines": 10_000})),
            Some(MAX_TOOL_SNIPPET_LINES as usize)
        );
    }
}
