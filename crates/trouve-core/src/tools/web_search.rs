//! Search the public web through keyless hosted MCP providers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const PARALLEL_ENDPOINT: &str = "https://search.parallel.ai/mcp";
const EXA_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const CACHE_CAPACITY: usize = 256;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_RETURN_CHARS: usize = 48 * 1024;
const MAX_QUERY_CHARS: usize = 2_000;
const MIN_PROVIDER_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
enum ProviderKind {
    Parallel,
    Exa,
}

enum SearchError {
    Cancelled,
    Failed(String),
}

#[derive(Clone)]
struct SearchProvider {
    name: &'static str,
    endpoint: String,
    kind: ProviderKind,
}

impl SearchProvider {
    fn parallel(endpoint: impl Into<String>) -> Self {
        Self {
            name: "parallel",
            endpoint: endpoint.into(),
            kind: ProviderKind::Parallel,
        }
    }

    fn exa(endpoint: impl Into<String>) -> Self {
        Self {
            name: "exa",
            endpoint: endpoint.into(),
            kind: ProviderKind::Exa,
        }
    }

    fn tool_name(&self) -> &'static str {
        match self.kind {
            ProviderKind::Parallel => "web_search",
            ProviderKind::Exa => "web_search_exa",
        }
    }

    fn arguments(&self, query: &str, max_results: usize) -> Value {
        match self.kind {
            ProviderKind::Parallel => json!({
                "objective": query,
                "search_queries": [query],
            }),
            ProviderKind::Exa => json!({
                "query": query,
                "type": "auto",
                "numResults": max_results,
                "livecrawl": "fallback",
                "contextMaxCharacters": 12_000,
            }),
        }
    }

    fn cache_key(&self, normalized_query: &str, max_results: usize) -> String {
        match self.kind {
            // Parallel does not accept a result-count argument, so every
            // requested count represents the same provider request.
            ProviderKind::Parallel => format!("parallel\n{normalized_query}"),
            ProviderKind::Exa => format!("exa\n{normalized_query}\n{max_results}"),
        }
    }
}

#[derive(Clone)]
struct CachedSearch {
    provider: &'static str,
    content: String,
    truncated: bool,
    stored_at: Instant,
    last_used: u64,
}

#[derive(Default)]
struct SearchCache {
    entries: HashMap<String, CachedSearch>,
    clock: u64,
}

impl SearchCache {
    fn get(&mut self, key: &str) -> Option<CachedSearch> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if self
            .entries
            .get(key)
            .is_some_and(|entry| entry.stored_at.elapsed() >= CACHE_TTL)
        {
            self.entries.remove(key);
            return None;
        }
        let entry = self.entries.get_mut(key)?;
        entry.last_used = clock;
        Some(entry.clone())
    }

    fn insert(&mut self, key: String, provider: &'static str, content: String, truncated: bool) {
        self.clock = self.clock.wrapping_add(1);
        if self.entries.len() >= CACHE_CAPACITY
            && !self.entries.contains_key(&key)
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            key,
            CachedSearch {
                provider,
                content,
                truncated,
                stored_at: Instant::now(),
                last_used: self.clock,
            },
        );
    }
}

/// A native, read-only search tool with transparent provider failover.
pub struct WebSearch {
    client: reqwest::Client,
    providers: Vec<SearchProvider>,
    cache: Mutex<SearchCache>,
    query_locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    provider_gate: tokio::sync::Mutex<Option<Instant>>,
    min_provider_interval: Duration,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new(
            vec![
                SearchProvider::parallel(PARALLEL_ENDPOINT),
                SearchProvider::exa(EXA_ENDPOINT),
            ],
            MIN_PROVIDER_INTERVAL,
        )
    }
}

impl WebSearch {
    fn new(providers: Vec<SearchProvider>, min_provider_interval: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(SEARCH_TIMEOUT)
            .user_agent(concat!("trouve-agent/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("static web search client configuration is valid");
        Self {
            client,
            providers,
            cache: Mutex::new(SearchCache::default()),
            query_locks: Mutex::new(HashMap::new()),
            provider_gate: tokio::sync::Mutex::new(None),
            min_provider_interval,
        }
    }

    fn query_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.query_locks.lock().unwrap();
        locks.retain(|_, lock| lock.strong_count() != 0);
        if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    fn cached(&self, normalized_query: &str, max_results: usize) -> Option<CachedSearch> {
        let mut cache = self.cache.lock().unwrap();
        self.providers
            .iter()
            .find_map(|provider| cache.get(&provider.cache_key(normalized_query, max_results)))
    }

    async fn wait_for_provider_slot(&self, ctx: &ToolCtx) -> Result<(), SearchError> {
        let mut last_request = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(SearchError::Cancelled),
            guard = self.provider_gate.lock() => guard,
        };
        if let Some(last_request_at) = *last_request {
            let wait = self
                .min_provider_interval
                .saturating_sub(last_request_at.elapsed());
            if !wait.is_zero() {
                tokio::select! {
                    biased;
                    _ = ctx.cancel.cancelled() => return Err(SearchError::Cancelled),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
        *last_request = Some(Instant::now());
        Ok(())
    }

    async fn call_provider(
        &self,
        ctx: &ToolCtx,
        provider: &SearchProvider,
        query: &str,
        max_results: usize,
    ) -> Result<String, SearchError> {
        self.wait_for_provider_slot(ctx).await?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": provider.tool_name(),
                "arguments": provider.arguments(query, max_results),
            }
        });
        let response = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(SearchError::Cancelled),
            response = self.client
                .post(&provider.endpoint)
                .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
                .json(&body)
                .send() => response.map_err(|error| SearchError::Failed(format!("request failed: {error}")))?,
        };
        if !response.status().is_success() {
            return Err(SearchError::Failed(format!(
                "provider returned HTTP {}",
                response.status()
            )));
        }

        let mut response = response;
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(SearchError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| SearchError::Failed(format!("response failed: {error}")))?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(SearchError::Failed(
                    "provider response exceeded 1 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(bytes)
            .map_err(|_| SearchError::Failed("provider returned non-UTF-8 content".to_string()))?;
        extract_mcp_text(&body).map_err(SearchError::Failed)
    }
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the public web and return current results with source URLs and excerpts. No API key \
         is required. Use web_fetch to read a result in full."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language web search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Preferred maximum number of results",
                    "minimum": 1,
                    "maximum": 10,
                    "default": 8
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn mutates(&self) -> bool {
        false
    }

    async fn run(&self, ctx: &ToolCtx, args: &Value) -> ToolResult {
        if ctx.cancel.is_cancelled() {
            return ToolResult::error("search cancelled");
        }
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::error("missing required argument: query");
        };
        let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if query.is_empty() {
            return ToolResult::error("query must not be empty");
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return ToolResult::error(format!(
                "query exceeds the {MAX_QUERY_CHARS}-character limit"
            ));
        }
        let max_results = args.get("max_results").and_then(Value::as_u64).unwrap_or(8);
        if !(1..=10).contains(&max_results) {
            return ToolResult::error("max_results must be between 1 and 10");
        }
        let max_results = max_results as usize;
        let normalized_query = query.to_lowercase();

        if let Some(cached) = self.cached(&normalized_query, max_results) {
            return ToolResult::ok(search_result(cached, true));
        }

        // Only one call for a normalized query reaches a provider. Followers
        // wait, then consume the newly cached result.
        let lock_key = self
            .providers
            .first()
            .map(|provider| provider.cache_key(&normalized_query, max_results))
            .unwrap_or_else(|| format!("none\n{normalized_query}\n{max_results}"));
        let query_lock = self.query_lock(&lock_key);
        let _query_guard = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return ToolResult::error("search cancelled"),
            guard = query_lock.lock() => guard,
        };
        if let Some(cached) = self.cached(&normalized_query, max_results) {
            return ToolResult::ok(search_result(cached, true));
        }

        let mut failures = Vec::new();
        for provider in &self.providers {
            match self.call_provider(ctx, provider, &query, max_results).await {
                Ok(content) => {
                    let truncated = content.chars().count() > MAX_RETURN_CHARS;
                    let content: String = content.chars().take(MAX_RETURN_CHARS).collect();
                    self.cache.lock().unwrap().insert(
                        provider.cache_key(&normalized_query, max_results),
                        provider.name,
                        content.clone(),
                        truncated,
                    );
                    return ToolResult::ok(search_result(
                        CachedSearch {
                            provider: provider.name,
                            content,
                            truncated,
                            stored_at: Instant::now(),
                            last_used: 0,
                        },
                        false,
                    ));
                }
                Err(SearchError::Cancelled) => {
                    return ToolResult::error("search cancelled");
                }
                Err(SearchError::Failed(error)) => {
                    failures.push(format!("{}: {error}", provider.name));
                }
            }
        }

        ToolResult::error(format!(
            "all web search providers failed ({})",
            failures.join("; ")
        ))
    }
}

fn search_result(cached: CachedSearch, cache_hit: bool) -> Value {
    json!({
        "provider": cached.provider,
        "content": cached.content,
        "truncated": cached.truncated,
        "cache": {
            "hit": cache_hit,
            "age_seconds": cached.stored_at.elapsed().as_secs(),
            "ttl_seconds": CACHE_TTL.as_secs(),
        }
    })
}

fn extract_mcp_text(body: &str) -> Result<String, String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return extract_mcp_value(&value);
    }

    let mut last_error = None;
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        match serde_json::from_str::<Value>(data) {
            Ok(value) => match extract_mcp_value(&value) {
                Ok(text) => return Ok(text),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(format!("invalid SSE JSON: {error}")),
        }
    }
    Err(last_error.unwrap_or_else(|| "provider returned no MCP result".into()))
}

fn extract_mcp_value(value: &Value) -> Result<String, String> {
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown MCP error");
        return Err(message.to_owned());
    }
    let Some(result) = value.get("result") else {
        return Err("provider response omitted result".into());
    };
    let texts: Vec<&str> = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(texts
            .first()
            .copied()
            .unwrap_or("provider tool returned an error")
            .to_owned());
    }
    if texts.is_empty() {
        return Err("provider returned no text results".into());
    }
    Ok(texts.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn mock_server_with_delay(
        status: &'static str,
        body: impl Into<String>,
        response_delay: Duration,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let body = body.into();
        tokio::spawn({
            let calls = calls.clone();
            async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let mut request = vec![0; 16 * 1024];
                    let _ = socket.read(&mut request).await;
                    tokio::time::sleep(response_delay).await;
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            }
        });
        (format!("http://{address}/mcp"), calls)
    }

    async fn mock_server(
        status: &'static str,
        body: impl Into<String>,
    ) -> (String, Arc<AtomicUsize>) {
        mock_server_with_delay(status, body, Duration::ZERO).await
    }

    #[test]
    fn parses_json_and_sse_mcp_responses() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"one"}]}}"#;
        assert_eq!(extract_mcp_text(json).unwrap(), "one");
        let sse = format!("event: message\ndata: {json}\n\n");
        assert_eq!(extract_mcp_text(&sse).unwrap(), "one");
    }

    #[test]
    fn parallel_arguments_do_not_expose_session_identifiers() {
        let arguments = SearchProvider::parallel(PARALLEL_ENDPOINT).arguments("query", 8);
        assert_eq!(arguments["objective"], "query");
        assert_eq!(arguments["search_queries"], json!(["query"]));
        assert!(arguments.get("session_id").is_none());
        assert!(arguments.get("max_results").is_none());
        assert!(arguments.get("numResults").is_none());
    }

    #[tokio::test]
    async fn caches_normalized_queries() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"result with https://example.com"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);
        let ctx = ToolCtx::default();

        let first = tool.run(&ctx, &json!({"query": "rust   language"})).await;
        let second = tool.run(&ctx, &json!({"query": "RUST LANGUAGE"})).await;

        assert_eq!(first.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(second.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(first.result["cache"]["hit"], false);
        assert_eq!(second.result["cache"]["hit"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn parallel_cache_ignores_result_count_that_the_provider_does_not_accept() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"shared parallel result"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);
        let ctx = ToolCtx::default();

        let first = tool
            .run(&ctx, &json!({"query": "same request", "max_results": 3}))
            .await;
        let second = tool
            .run(&ctx, &json!({"query": "same request", "max_results": 9}))
            .await;

        assert_eq!(first.result["cache"]["hit"], false);
        assert_eq!(second.result["cache"]["hit"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exa_cache_retains_result_count_in_its_request_identity() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"exa result"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(vec![SearchProvider::exa(endpoint)], Duration::ZERO);
        let ctx = ToolCtx::default();

        let first = tool
            .run(&ctx, &json!({"query": "same request", "max_results": 3}))
            .await;
        let second = tool
            .run(&ctx, &json!({"query": "same request", "max_results": 9}))
            .await;
        let repeated = tool
            .run(&ctx, &json!({"query": "same request", "max_results": 3}))
            .await;

        assert_eq!(first.result["cache"]["hit"], false);
        assert_eq!(second.result["cache"]["hit"], false);
        assert_eq!(repeated.result["cache"]["hit"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn coalesces_concurrent_identical_queries() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"one shared result"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            Duration::ZERO,
        ));
        let ctx = ToolCtx::default();
        let args = json!({"query": "same query"});

        let (first, second) = tokio::join!(tool.run(&ctx, &args), tool.run(&ctx, &args));

        assert_eq!(first.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(second.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(first.result["cache"]["hit"], second.result["cache"]["hit"]);
    }

    #[tokio::test]
    async fn falls_back_to_exa_after_parallel_failure() {
        let (parallel, parallel_calls) = mock_server("429 Too Many Requests", "limited").await;
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"fallback result"}]}}"#;
        let (exa, exa_calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(
            vec![SearchProvider::parallel(parallel), SearchProvider::exa(exa)],
            Duration::ZERO,
        );

        let result = tool
            .run(&ToolCtx::default(), &json!({"query": "current topic"}))
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(result.result["provider"], "exa");
        assert_eq!(parallel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(exa_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn provider_cancellation_text_does_not_suppress_failover() {
        let error = r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"search cancelled"}]}}"#;
        let (parallel, parallel_calls) = mock_server("200 OK", error).await;
        let success = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"fallback result"}]}}"#;
        let (exa, exa_calls) = mock_server("200 OK", success).await;
        let tool = WebSearch::new(
            vec![SearchProvider::parallel(parallel), SearchProvider::exa(exa)],
            Duration::ZERO,
        );

        let result = tool
            .run(&ToolCtx::default(), &json!({"query": "current topic"}))
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(result.result["provider"], "exa");
        assert_eq!(parallel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(exa_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_stops_searches_before_and_during_provider_io() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"late result"}]}}"#;
        let (endpoint, calls) =
            mock_server_with_delay("200 OK", body, Duration::from_secs(30)).await;
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            Duration::ZERO,
        ));

        let pre_cancelled = ToolCtx::default();
        pre_cancelled.cancel.cancel();
        let result = tool
            .run(&pre_cancelled, &json!({"query": "cancel before request"}))
            .await;
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert_eq!(result.result["error"], "search cancelled");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let ctx = ToolCtx::default();
        let cancel = ctx.cancel.clone();
        let running = tokio::spawn({
            let tool = tool.clone();
            async move { tool.run(&ctx, &json!({"query": "cancel in flight"})).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert_eq!(result.result["error"], "search cancelled");
    }

    #[tokio::test]
    async fn enforces_response_size_and_reports_result_truncation() {
        let (endpoint, _) = mock_server("200 OK", "x".repeat(MAX_RESPONSE_BYTES + 1)).await;
        let oversized = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO)
            .run(
                &ToolCtx::default(),
                &json!({"query": "oversized provider response"}),
            )
            .await;
        assert_eq!(oversized.status, trouve_protocol::ToolStatus::Error);
        assert!(
            oversized.result["error"]
                .as_str()
                .unwrap()
                .contains("response exceeded 1 MiB")
        );

        let content = "x".repeat(MAX_RETURN_CHARS + 1);
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"content": [{"type": "text", "text": content}]},
        })
        .to_string();
        let (endpoint, _) = mock_server("200 OK", body).await;
        let truncated = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO)
            .run(
                &ToolCtx::default(),
                &json!({"query": "accepted oversized result"}),
            )
            .await;
        assert_eq!(truncated.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(truncated.result["truncated"], true);
        assert_eq!(
            truncated.result["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_RETURN_CHARS
        );
    }

    #[tokio::test]
    async fn rejects_invalid_queries_without_network_access() {
        let tool = WebSearch::new(Vec::new(), Duration::ZERO);
        for args in [
            json!({}),
            json!({"query": "  "}),
            json!({"query": "ok", "max_results": 11}),
        ] {
            let result = tool.run(&ToolCtx::default(), &args).await;
            assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        }
    }
}
