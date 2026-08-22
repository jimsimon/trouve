//! Search the public web through keyless hosted MCP providers.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::Stream;
use serde_json::{Value, json};

use super::{Tool, ToolCtx, ToolResult};

const PARALLEL_ENDPOINT: &str = "https://search.parallel.ai/mcp";
const EXA_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const CACHE_CAPACITY: usize = 256;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_RETURN_CHARS: usize = 48 * 1024;
const MAX_SEARCH_ERROR_CHARS: usize = 8 * 1024;
const MAX_PROVIDER_ERROR_CHARS: usize = 3 * 1024;
const MAX_QUERY_CHARS: usize = 2_000;
const MIN_PROVIDER_INTERVAL: Duration = Duration::from_millis(250);
const PROVIDER_HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
const SESSION_CLEANUP_CONCURRENCY: usize = 8;
const SESSION_CLEANUP_CAPACITY: usize = 64;
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

#[derive(Clone, Copy)]
enum ProviderKind {
    Parallel,
    Exa,
}

enum SearchError {
    Cancelled,
    CancelledAfterHandoff(ProviderResponseFuture),
    Failed(String),
}

type ProviderResponseFuture =
    Pin<Box<dyn Future<Output = Result<reqwest::Response, reqwest::Error>> + Send>>;

struct McpSession {
    id: Option<String>,
    protocol_version: String,
    cleanup_permit: Option<tokio::sync::mpsc::OwnedPermit<SessionCleanupJob>>,
}

struct DispatchBody {
    bytes: Option<Bytes>,
    acknowledged: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Stream for DispatchBody {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(bytes) = this.bytes.take() else {
            return Poll::Ready(None);
        };
        if let Some(acknowledged) = this.acknowledged.take() {
            let _ = acknowledged.send(());
        }
        Poll::Ready(Some(Ok(bytes)))
    }
}

struct ProviderDispatcher {
    gate: tokio::sync::Mutex<Option<Instant>>,
    min_interval: Duration,
    handoff_timeout: Duration,
}

impl ProviderDispatcher {
    fn new(min_interval: Duration, handoff_timeout: Duration) -> Self {
        Self {
            gate: tokio::sync::Mutex::new(None),
            min_interval,
            handoff_timeout,
        }
    }

    /// Dispatch one quota-consuming provider operation in provider-visible
    /// order. The request-body handoff is the acknowledgement boundary: it
    /// occurs only after the HTTP transport is ready to consume outbound
    /// bytes, but does not couple later admission to response-header latency.
    async fn dispatch(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
        request: reqwest::RequestBuilder,
        body: &Value,
    ) -> Result<reqwest::Response, SearchError> {
        let mut last_request = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(SearchError::Cancelled),
            guard = self.gate.lock() => guard,
        };
        if let Some(last_request_at) = *last_request {
            let wait = self.min_interval.saturating_sub(last_request_at.elapsed());
            if !wait.is_zero() {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(SearchError::Cancelled),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }

        let payload = serde_json::to_vec(body)
            .expect("serializing a JSON value for a provider request cannot fail");
        let content_length = payload.len();
        let (acknowledged_tx, mut acknowledged_rx) = tokio::sync::oneshot::channel();
        let dispatch_body = DispatchBody {
            bytes: Some(Bytes::from(payload)),
            acknowledged: Some(acknowledged_tx),
        };
        let mut send: ProviderResponseFuture = Box::pin(
            request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::CONTENT_LENGTH, content_length)
                .body(reqwest::Body::wrap_stream(dispatch_body))
                .send(),
        );
        let mut handoff_timeout = Box::pin(tokio::time::sleep(self.handoff_timeout));
        let early_response = tokio::select! {
            biased;
            acknowledged = &mut acknowledged_rx => {
                if acknowledged.is_err() {
                    return match send.await {
                        Ok(_) => Err(SearchError::Failed(
                            "provider request ended before outbound body handoff".into(),
                        )),
                        Err(error) => Err(SearchError::Failed(format!(
                            "request failed: {error}"
                        ))),
                    };
                }
                None
            },
            _ = cancel.cancelled() => {
                if acknowledged_rx.try_recv().is_ok() {
                    *last_request = Some(Instant::now());
                    drop(last_request);
                    return Err(SearchError::CancelledAfterHandoff(send));
                }
                return Err(SearchError::Cancelled);
            },
            response = &mut send => Some(response),
            _ = &mut handoff_timeout => {
                return Err(SearchError::Failed(format!(
                    "provider dispatch timed out after {:.1}s",
                    self.handoff_timeout.as_secs_f64()
                )));
            },
        };
        *last_request = Some(Instant::now());
        drop(last_request);

        if let Some(response) = early_response {
            return response
                .map_err(|error| SearchError::Failed(format!("request failed: {error}")));
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SearchError::CancelledAfterHandoff(send)),
            response = &mut send => response
                .map_err(|error| SearchError::Failed(format!("request failed: {error}"))),
        }
    }
}

struct PendingInitializationJob {
    endpoint: String,
    protocol_version: String,
    response: ProviderResponseFuture,
}

struct SessionCleanupJob {
    endpoint: String,
    session_id: String,
    protocol_version: String,
}

struct SessionCleanupWorker {
    sender: Option<tokio::sync::mpsc::Sender<SessionCleanupJob>>,
    recovery_sender: Option<tokio::sync::mpsc::UnboundedSender<PendingInitializationJob>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    pending: Arc<AtomicUsize>,
}

impl SessionCleanupWorker {
    fn new() -> Self {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<SessionCleanupJob>(SESSION_CLEANUP_CAPACITY);
        // Header recovery must never consume DELETE admission. Its ingress is
        // drained immediately into deadline-bounded futures; production input
        // is itself bounded by the dispatch interval (at most roughly
        // SEARCH_TIMEOUT / MIN_PROVIDER_INTERVAL live recoveries per tool).
        let (recovery_sender, mut recovery_receiver) =
            tokio::sync::mpsc::unbounded_channel::<PendingInitializationJob>();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending.clone();
        let cleanup_thread = std::thread::Builder::new()
            .name("trouve-web-search-cleanup".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("web search cleanup runtime must start");
                runtime.block_on(async move {
                    let client = reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .build()
                        .expect("static web search cleanup client configuration is valid");
                    let mut cleanups = tokio::task::JoinSet::new();
                    loop {
                        while cleanups.len() >= SESSION_CLEANUP_CONCURRENCY {
                            let _ = cleanups.join_next().await;
                        }
                        let Some(request) = receiver.recv().await else {
                            break;
                        };
                        let client = client.clone();
                        let pending = worker_pending.clone();
                        cleanups.spawn(async move {
                            let _ = client
                                .delete(request.endpoint)
                                .timeout(SESSION_CLEANUP_TIMEOUT)
                                .header(MCP_PROTOCOL_VERSION_HEADER, request.protocol_version)
                                .header(MCP_SESSION_ID_HEADER, request.session_id)
                                .send()
                                .await;
                            pending.fetch_sub(1, Ordering::Release);
                        });
                    }
                    while cleanups.join_next().await.is_some() {}
                });
            })
            .expect("web search cleanup worker must start");
        let recovery_cleanup_sender = sender.clone();
        let recovery_pending = pending.clone();
        let recovery_thread = std::thread::Builder::new()
            .name("trouve-web-search-session-recovery".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("web search session recovery runtime must start");
                runtime.block_on(async move {
                    let mut recoveries = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            biased;
                            completed = recoveries.join_next(), if !recoveries.is_empty() => {
                                let _ = completed;
                            }
                            job = recovery_receiver.recv() => {
                                let Some(job) = job else {
                                    break;
                                };
                                let cleanup_sender = recovery_cleanup_sender.clone();
                                let pending = recovery_pending.clone();
                                recoveries.spawn(async move {
                                    let Ok(Ok(response)) =
                                        tokio::time::timeout(SEARCH_TIMEOUT, job.response).await
                                    else {
                                        pending.fetch_sub(1, Ordering::Release);
                                        return;
                                    };
                                    let Some(session_id) = response
                                        .headers()
                                        .get(MCP_SESSION_ID_HEADER)
                                        .and_then(|value| value.to_str().ok())
                                        .map(str::to_owned)
                                    else {
                                        pending.fetch_sub(1, Ordering::Release);
                                        return;
                                    };
                                    let Ok(permit) = cleanup_sender.reserve_owned().await else {
                                        pending.fetch_sub(1, Ordering::Release);
                                        return;
                                    };
                                    permit.send(SessionCleanupJob {
                                        endpoint: job.endpoint,
                                        session_id,
                                        protocol_version: job.protocol_version,
                                    });
                                });
                            }
                        }
                    }
                    while recoveries.join_next().await.is_some() {}
                });
            })
            .expect("web search session recovery worker must start");
        Self {
            sender: Some(sender),
            recovery_sender: Some(recovery_sender),
            threads: vec![cleanup_thread, recovery_thread],
            pending,
        }
    }

    async fn reserve(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::mpsc::OwnedPermit<SessionCleanupJob>, SearchError> {
        let Some(sender) = &self.sender else {
            return Err(SearchError::Failed(
                "web search session cleanup is unavailable".into(),
            ));
        };
        let sender = sender.clone();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SearchError::Cancelled),
            permit = sender.reserve_owned() => permit.map_err(|_| {
                SearchError::Failed("web search session cleanup is unavailable".into())
            }),
        }
    }

    fn send(
        &self,
        permit: tokio::sync::mpsc::OwnedPermit<SessionCleanupJob>,
        job: SessionCleanupJob,
    ) {
        self.pending.fetch_add(1, Ordering::AcqRel);
        permit.send(job);
    }

    fn recover(&self, job: PendingInitializationJob) {
        let Some(sender) = &self.recovery_sender else {
            return;
        };
        self.pending.fetch_add(1, Ordering::AcqRel);
        if sender.send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::Release);
        }
    }
}

impl Drop for SessionCleanupWorker {
    fn drop(&mut self) {
        // Close recovery admission first. Its independently-owned thread keeps
        // the cleanup queue alive until every handed-off initialization reaches
        // its request deadline and any discovered session is queued for DELETE.
        drop(self.recovery_sender.take());
        drop(self.sender.take());
        self.threads.clear();
    }
}

/// Incrementally assembles complete SSE events from arbitrary byte chunks.
#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseDecoder {
    /// Consume a network chunk and return every complete event it finishes.
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8(line)
                .map_err(|_| "provider returned non-UTF-8 SSE content".to_owned())?;
            self.process_line(&line, &mut events);
        }
        Ok(events)
    }

    /// Flush the final unterminated line and event at end of stream.
    fn finish(&mut self) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            if self.buffer.last() == Some(&b'\r') {
                self.buffer.pop();
            }
            let line = String::from_utf8(std::mem::take(&mut self.buffer))
                .map_err(|_| "provider returned non-UTF-8 SSE content".to_owned())?;
            self.process_line(&line, &mut events);
        }
        self.dispatch(&mut events);
        Ok(events)
    }

    /// Apply one decoded SSE line to the current event.
    fn process_line(&mut self, line: &str, events: &mut Vec<String>) {
        if line.is_empty() {
            self.dispatch(events);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field == "data" {
            self.data_lines
                .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }

    /// Emit the current event when it contains at least one data field.
    fn dispatch(&mut self, events: &mut Vec<String>) {
        if !self.data_lines.is_empty() {
            events.push(self.data_lines.join("\n"));
            self.data_lines.clear();
        }
    }
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
                "numResults": max_results,
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
    dispatcher: ProviderDispatcher,
    cleanup: SessionCleanupWorker,
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
        Self::new_with_handoff_timeout(providers, min_provider_interval, PROVIDER_HANDOFF_TIMEOUT)
    }

    fn new_with_handoff_timeout(
        providers: Vec<SearchProvider>,
        min_provider_interval: Duration,
        handoff_timeout: Duration,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(SEARCH_TIMEOUT)
            .user_agent(concat!("trouve-agent/", env!("CARGO_PKG_VERSION")))
            // Provider endpoints are fixed. Never let a provider pivot the
            // request to an unvalidated host or private network.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("static web search client configuration is valid");
        Self {
            client,
            providers,
            cache: Mutex::new(SearchCache::default()),
            query_locks: Mutex::new(HashMap::new()),
            dispatcher: ProviderDispatcher::new(min_provider_interval, handoff_timeout),
            cleanup: SessionCleanupWorker::new(),
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

    fn scoped_key(scope: &str, key: &str) -> String {
        format!("{}:{scope}{key}", scope.len())
    }

    fn cached(
        &self,
        scope: &str,
        normalized_query: &str,
        max_results: usize,
    ) -> Option<CachedSearch> {
        let mut cache = self.cache.lock().unwrap();
        self.providers.iter().find_map(|provider| {
            cache.get(&Self::scoped_key(
                scope,
                &provider.cache_key(normalized_query, max_results),
            ))
        })
    }

    async fn send_provider_request(
        &self,
        cancel: &tokio_util::sync::CancellationToken,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, SearchError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SearchError::Cancelled),
            response = request.send() => response
                .map_err(|error| SearchError::Failed(format!("request failed: {error}"))),
        }
    }

    fn provider_request(
        &self,
        provider: &SearchProvider,
        session: Option<&McpSession>,
    ) -> reqwest::RequestBuilder {
        let mut request = self.client.post(&provider.endpoint).header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
        if let Some(session) = session {
            request = request.header(
                MCP_PROTOCOL_VERSION_HEADER,
                session.protocol_version.as_str(),
            );
            if let Some(id) = &session.id {
                request = request.header(MCP_SESSION_ID_HEADER, id);
            }
        }
        request
    }

    async fn initialize_provider(
        &self,
        ctx: &ToolCtx,
        provider: &SearchProvider,
    ) -> Result<McpSession, SearchError> {
        // Reserving cleanup capacity before initialization means every remote
        // session we may allocate already owns a bounded teardown path.
        let cleanup_permit = self.cleanup.reserve(&ctx.cancel).await?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "trouve",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            },
        });
        let response = match self
            .dispatcher
            .dispatch(&ctx.cancel, self.provider_request(provider, None), &body)
            .await
        {
            Ok(response) => response,
            Err(SearchError::CancelledAfterHandoff(response)) => {
                // The caller remains promptly cancellable while independently
                // bounded lifecycle work captures and closes any session that
                // the handed-off initialization created.
                drop(cleanup_permit);
                self.cleanup.recover(PendingInitializationJob {
                    endpoint: provider.endpoint.clone(),
                    protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
                    response,
                });
                return Err(SearchError::Cancelled);
            }
            Err(error) => return Err(error),
        };
        let session_id = response
            .headers()
            .get(MCP_SESSION_ID_HEADER)
            .map(|value| {
                value.to_str().map(str::to_owned).map_err(|_| {
                    SearchError::Failed("provider returned an invalid MCP session id".into())
                })
            })
            .transpose()?;
        // Stateless MCP providers do not need teardown capacity after their
        // initialization headers establish that no session was allocated.
        let cleanup_permit = if session_id.is_some() {
            Some(cleanup_permit)
        } else {
            None
        };
        let mut session = McpSession {
            id: session_id,
            protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
            cleanup_permit,
        };
        let value = match self.read_mcp_response(ctx, response, 0).await {
            Ok(value) => value,
            Err(error) => {
                self.close_provider_session(provider, session);
                return Err(error);
            }
        };
        if value.get("error").is_some() {
            let error = extract_mcp_value(&value)
                .expect_err("an MCP error response cannot contain a successful tool result");
            self.close_provider_session(provider, session);
            return Err(SearchError::Failed(bounded_error(
                &error,
                MAX_PROVIDER_ERROR_CHARS,
            )));
        }
        let Some(protocol_version) = value
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
        else {
            self.close_provider_session(provider, session);
            return Err(SearchError::Failed(
                "provider initialization omitted protocol version".into(),
            ));
        };
        session.protocol_version = protocol_version.to_owned();
        let initialized = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {},
        });
        let response = self
            .send_provider_request(
                &ctx.cancel,
                self.provider_request(provider, Some(&session))
                    .json(&initialized),
            )
            .await;
        match response {
            Ok(response) if response.status().is_success() => Ok(session),
            Ok(response) => {
                let error = SearchError::Failed(format!(
                    "provider rejected MCP initialization with HTTP {}",
                    response.status()
                ));
                self.close_provider_session(provider, session);
                Err(error)
            }
            Err(error) => {
                self.close_provider_session(provider, session);
                Err(error)
            }
        }
    }

    fn close_provider_session(&self, provider: &SearchProvider, mut session: McpSession) {
        let Some(permit) = session.cleanup_permit.take() else {
            return;
        };
        let Some(session_id) = session.id else {
            return;
        };
        self.cleanup.send(
            permit,
            SessionCleanupJob {
                endpoint: provider.endpoint.clone(),
                session_id,
                protocol_version: session.protocol_version,
            },
        );
    }

    async fn call_provider(
        &self,
        ctx: &ToolCtx,
        provider: &SearchProvider,
        query: &str,
        max_results: usize,
    ) -> Result<String, SearchError> {
        let session = self.initialize_provider(ctx, provider).await?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": provider.tool_name(),
                "arguments": provider.arguments(query, max_results),
            }
        });
        let result = match self
            .dispatcher
            .dispatch(
                &ctx.cancel,
                self.provider_request(provider, Some(&session)),
                &body,
            )
            .await
        {
            Ok(response) => self
                .read_mcp_response(ctx, response, 1)
                .await
                .and_then(|value| {
                    extract_mcp_value(&value).map_err(|error| {
                        SearchError::Failed(bounded_error(&error, MAX_PROVIDER_ERROR_CHARS))
                    })
                }),
            Err(SearchError::CancelledAfterHandoff(response)) => {
                drop(response);
                Err(SearchError::Cancelled)
            }
            Err(error) => Err(error),
        };
        self.close_provider_session(provider, session);
        result
    }

    async fn read_mcp_response(
        &self,
        ctx: &ToolCtx,
        response: reqwest::Response,
        expected_id: u64,
    ) -> Result<Value, SearchError> {
        if !response.status().is_success() {
            return Err(SearchError::Failed(format!(
                "provider returned HTTP {}",
                response.status()
            )));
        }

        let is_event_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        let mut response = response;
        let mut bytes = Vec::new();
        let mut response_bytes = 0usize;
        let mut sse_decoder = is_event_stream.then(SseDecoder::default);
        let mut last_sse_error = None;
        loop {
            let chunk = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(SearchError::Cancelled),
                chunk = response.chunk() => chunk.map_err(|error| SearchError::Failed(format!("response failed: {error}")))?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            response_bytes = response_bytes.saturating_add(chunk.len());
            if response_bytes > MAX_RESPONSE_BYTES {
                return Err(SearchError::Failed(
                    "provider response exceeded 1 MiB".into(),
                ));
            }
            if let Some(decoder) = &mut sse_decoder {
                for event in decoder.push(&chunk).map_err(SearchError::Failed)? {
                    match extract_mcp_sse_event(&event, expected_id) {
                        Ok(Some(result)) => return Ok(result),
                        Ok(None) => {}
                        Err(error) => last_sse_error = Some(error),
                    }
                }
            } else {
                bytes.extend_from_slice(&chunk);
            }
        }
        if let Some(mut decoder) = sse_decoder {
            for event in decoder.finish().map_err(SearchError::Failed)? {
                match extract_mcp_sse_event(&event, expected_id) {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => {}
                    Err(error) => last_sse_error = Some(error),
                }
            }
            return Err(SearchError::Failed(bounded_error(
                last_sse_error
                    .as_deref()
                    .unwrap_or("provider returned no MCP result"),
                MAX_PROVIDER_ERROR_CHARS,
            )));
        }
        let body = String::from_utf8(bytes)
            .map_err(|_| SearchError::Failed("provider returned non-UTF-8 content".to_string()))?;
        extract_mcp_response(&body, expected_id)
            .map_err(|error| SearchError::Failed(bounded_error(&error, MAX_PROVIDER_ERROR_CHARS)))
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
        let max_results = match args.get("max_results") {
            None => 8,
            Some(value) => match value.as_u64() {
                Some(value) => value,
                None => {
                    return ToolResult::error("max_results must be an integer between 1 and 10");
                }
            },
        };
        if !(1..=10).contains(&max_results) {
            return ToolResult::error("max_results must be between 1 and 10");
        }
        let max_results = max_results as usize;
        let normalized_query = query.to_lowercase();
        // WebSearch is executor-global, so every cache and coalescing key must
        // include the caller scope. Isolated tests intentionally share the
        // empty scope; production calls always carry a stable thread id.
        let scope = ctx.thread_id.as_str();

        if let Some(cached) = self.cached(scope, &normalized_query, max_results) {
            return ToolResult::ok(search_result(cached, true));
        }

        // Only one call for a normalized query reaches a provider. Followers
        // wait, then consume the newly cached result.
        let provider_key = self
            .providers
            .first()
            .map(|provider| provider.cache_key(&normalized_query, max_results))
            .unwrap_or_else(|| format!("none\n{normalized_query}\n{max_results}"));
        let lock_key = Self::scoped_key(scope, &provider_key);
        let query_lock = self.query_lock(&lock_key);
        let _query_guard = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return ToolResult::error("search cancelled"),
            guard = query_lock.lock() => guard,
        };
        if let Some(cached) = self.cached(scope, &normalized_query, max_results) {
            return ToolResult::ok(search_result(cached, true));
        }

        let mut failures = Vec::new();
        for provider in &self.providers {
            match self.call_provider(ctx, provider, &query, max_results).await {
                Ok(content) => {
                    let truncated = content.chars().count() > MAX_RETURN_CHARS;
                    let content: String = content.chars().take(MAX_RETURN_CHARS).collect();
                    self.cache.lock().unwrap().insert(
                        Self::scoped_key(
                            scope,
                            &provider.cache_key(&normalized_query, max_results),
                        ),
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
                Err(SearchError::CancelledAfterHandoff(response)) => {
                    drop(response);
                    return ToolResult::error("search cancelled");
                }
                Err(SearchError::Failed(error)) => {
                    failures.push(format!(
                        "{}: {}",
                        provider.name,
                        bounded_error(&error, MAX_PROVIDER_ERROR_CHARS)
                    ));
                }
            }
        }

        ToolResult::error(bounded_error(
            &format!("all web search providers failed ({})", failures.join("; ")),
            MAX_SEARCH_ERROR_CHARS,
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

#[cfg(test)]
fn extract_mcp_text(body: &str) -> Result<String, String> {
    extract_mcp_response(body, 1).and_then(|value| extract_mcp_value(&value))
}

fn extract_mcp_response(body: &str, expected_id: u64) -> Result<Value, String> {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        return if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
            Ok(value)
        } else {
            Err("provider returned no MCP result".into())
        };
    }

    let mut decoder = SseDecoder::default();
    let mut events = decoder.push(body.as_bytes())?;
    events.extend(decoder.finish()?);
    let mut last_error = None;
    for event in events {
        match extract_mcp_sse_event(&event, expected_id) {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "provider returned no MCP result".into()))
}

/// Parse one SSE data payload, ignoring notifications and unrelated call ids.
fn extract_mcp_sse_event(data: &str, expected_id: u64) -> Result<Option<Value>, String> {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let value = serde_json::from_str::<Value>(data)
        .map_err(|error| format!("invalid SSE JSON: {error}"))?;
    if value.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Ok(None);
    }
    Ok(Some(value))
}

/// Bound untrusted provider text while preserving UTF-8 character boundaries.
fn bounded_error(message: &str, max_chars: usize) -> String {
    const SUFFIX: &str = "… [truncated]";
    if message.chars().count() <= max_chars {
        return message.to_owned();
    }
    let keep = max_chars.saturating_sub(SUFFIX.chars().count());
    let mut bounded: String = message.chars().take(keep).collect();
    bounded.push_str(SUFFIX);
    bounded
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

    async fn wait_for(mut predicate: impl FnMut() -> bool) {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[derive(Debug)]
    struct CapturedRequest {
        at: Instant,
        method: String,
        body: Option<Value>,
        session_id: Option<String>,
        protocol_version: Option<String>,
    }

    async fn capturing_mock_server_with_delays(
        status: &'static str,
        body: impl Into<String>,
        response_delay: Duration,
        cleanup_response_delay: Duration,
        initialization_response_delay: Duration,
        stateful_initialization: bool,
        initialization_body: Option<String>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<CapturedRequest>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let body = body.into();
        tokio::spawn({
            let calls = calls.clone();
            let requests = requests.clone();
            let initialization_body = initialization_body.clone();
            async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let calls = calls.clone();
                    let requests = requests.clone();
                    let body = body.clone();
                    let initialization_body = initialization_body.clone();
                    let cleanup_response_delay = cleanup_response_delay;
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buffer = [0; 4096];
                        loop {
                            let read = socket.read(&mut buffer).await.unwrap();
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&buffer[..read]);
                            let Some(headers_end) = request
                                .windows(4)
                                .position(|window| window == b"\r\n\r\n")
                                .map(|position| position + 4)
                            else {
                                continue;
                            };
                            let headers = String::from_utf8_lossy(&request[..headers_end]);
                            let content_length = headers
                                .lines()
                                .find_map(|line| {
                                    line.split_once(':')
                                        .filter(|(name, _)| {
                                            name.eq_ignore_ascii_case("content-length")
                                        })
                                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if request.len() >= headers_end + content_length {
                                break;
                            }
                        }
                        let headers_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|position| position + 4)
                            .unwrap();
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let method = headers
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().next())
                            .unwrap_or_default()
                            .to_owned();
                        let request_body = if request.len() > headers_end {
                            serde_json::from_slice(&request[headers_end..]).ok()
                        } else {
                            None
                        };
                        let rpc_method = request_body
                            .as_ref()
                            .and_then(|value: &Value| value.get("method"))
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        let streaming = rpc_method.as_deref() == Some("tools/call")
                            && body.trim_start().starts_with("data:");
                        let header = |name: &str| {
                            headers.lines().find_map(|line| {
                                line.split_once(':')
                                    .filter(|(header, _)| header.eq_ignore_ascii_case(name))
                                    .map(|(_, value)| value.trim().to_owned())
                            })
                        };
                        requests.lock().unwrap().push(CapturedRequest {
                            at: Instant::now(),
                            method: method.clone(),
                            body: request_body,
                            session_id: header(MCP_SESSION_ID_HEADER),
                            protocol_version: header(MCP_PROTOCOL_VERSION_HEADER),
                        });

                        let response = if method == "DELETE" {
                            tokio::time::sleep(cleanup_response_delay).await;
                            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_owned()
                        } else if status != "200 OK" {
                            calls.fetch_add(1, Ordering::SeqCst);
                            format!(
                                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        } else if rpc_method.as_deref() == Some("initialize") {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(initialization_response_delay).await;
                            let initialized = initialization_body.unwrap_or_else(|| {
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": 0,
                                    "result": {
                                        "protocolVersion": MCP_PROTOCOL_VERSION,
                                        "capabilities": {"tools": {}},
                                        "serverInfo": {"name": "mock-search", "version": "1"},
                                    },
                                })
                                .to_string()
                            });
                            let session_header = if stateful_initialization {
                                "Mcp-Session-Id: test-session\r\n"
                            } else {
                                ""
                            };
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n{initialized}",
                                initialized.len()
                            )
                        } else if rpc_method.as_deref() == Some("notifications/initialized") {
                            "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                        } else {
                            tokio::time::sleep(response_delay).await;
                            if streaming {
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n{body}"
                                )
                            } else {
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                            }
                        };
                        let _ = socket.write_all(response.as_bytes()).await;
                        if streaming {
                            let _ = socket.flush().await;
                            tokio::time::sleep(Duration::from_secs(30)).await;
                        }
                    });
                }
            }
        });
        (format!("http://{address}/mcp"), calls, requests)
    }

    async fn capturing_mock_server_with_initialization(
        status: &'static str,
        body: impl Into<String>,
        response_delay: Duration,
        initialization_body: Option<String>,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<CapturedRequest>>>) {
        capturing_mock_server_with_delays(
            status,
            body,
            response_delay,
            Duration::ZERO,
            Duration::ZERO,
            true,
            initialization_body,
        )
        .await
    }

    async fn capturing_mock_server_with_delay(
        status: &'static str,
        body: impl Into<String>,
        response_delay: Duration,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<CapturedRequest>>>) {
        capturing_mock_server_with_initialization(status, body, response_delay, None).await
    }

    async fn mock_server_with_delay(
        status: &'static str,
        body: impl Into<String>,
        response_delay: Duration,
    ) -> (String, Arc<AtomicUsize>) {
        let (endpoint, calls, _) =
            capturing_mock_server_with_delay(status, body, response_delay).await;
        (endpoint, calls)
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
        let multiline_sse = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\n",
            "data: \"id\":1,\"result\":{\"content\":[{\"type\":\"text\",",
            "\"text\":\"multi-line\"}]}}\n\n",
        );
        assert_eq!(extract_mcp_text(multiline_sse).unwrap(), "multi-line");
    }

    #[tokio::test]
    async fn returns_an_sse_result_without_waiting_for_eof() {
        let event = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{",
            "\"content\":[{\"type\":\"text\",\"text\":\"streamed\"}]}}\n\n",
        );
        let (endpoint, _) = mock_server("200 OK", event).await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tool.run(&ToolCtx::default(), &json!({"query": "streaming result"})),
        )
        .await
        .expect("a complete SSE response should not wait for connection close");

        assert_eq!(result.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(result.result["content"], "streamed");
    }

    #[tokio::test]
    async fn provider_redirects_are_not_followed() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"private target"}]}}"#;
        let (target, target_calls) = mock_server("200 OK", body).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);

        let result = tool
            .run(&ToolCtx::default(), &json!({"query": "redirect attempt"}))
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert!(
            result.result["error"]
                .as_str()
                .unwrap()
                .contains("HTTP 302")
        );
        assert_eq!(target_calls.load(Ordering::SeqCst), 0);
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
    async fn exa_request_uses_the_hosted_tool_schema() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"exa result"}]}}"#;
        let (endpoint, _, requests) =
            capturing_mock_server_with_delay("200 OK", body, Duration::ZERO).await;
        let tool = WebSearch::new(vec![SearchProvider::exa(endpoint)], Duration::ZERO);

        let result = tool
            .run(
                &ToolCtx::default(),
                &json!({"query": "schema check", "max_results": 4}),
            )
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Ok);
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| requests.lock().unwrap().len() >= 4),
        )
        .await
        .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].body.as_ref().unwrap()["method"], "initialize");
        assert!(requests[0].session_id.is_none());
        assert_eq!(
            requests[1].body.as_ref().unwrap()["method"],
            "notifications/initialized"
        );
        assert_eq!(requests[1].session_id.as_deref(), Some("test-session"));
        assert_eq!(
            requests[1].protocol_version.as_deref(),
            Some(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(
            requests[2].body.as_ref().unwrap()["params"],
            json!({
                "name": "web_search_exa",
                "arguments": {
                    "query": "schema check",
                    "numResults": 4,
                },
            })
        );
        assert_eq!(requests[2].session_id.as_deref(), Some("test-session"));
        assert_eq!(requests[3].method, "DELETE");
        assert_eq!(requests[3].session_id.as_deref(), Some("test-session"));
    }

    #[tokio::test]
    async fn initialization_failures_tear_down_allocated_sessions() {
        let invalid_initialization = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "result": {},
        })
        .to_string();
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"unused"}]}}"#;
        let (endpoint, calls, requests) = capturing_mock_server_with_initialization(
            "200 OK",
            body,
            Duration::ZERO,
            Some(invalid_initialization),
        )
        .await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);

        let result = tool
            .run(&ToolCtx::default(), &json!({"query": "bad initialization"}))
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert!(
            result.result["error"]
                .as_str()
                .unwrap()
                .contains("initialization omitted protocol version")
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.method == "DELETE")
            }),
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let requests = requests.lock().unwrap();
        let cleanup = requests
            .iter()
            .find(|request| request.method == "DELETE")
            .unwrap();
        assert_eq!(cleanup.session_id.as_deref(), Some("test-session"));
        assert_eq!(
            cleanup.protocol_version.as_deref(),
            Some(MCP_PROTOCOL_VERSION)
        );
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
    async fn cache_and_coalescing_are_scoped_to_the_calling_thread() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"scoped result"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);
        let first_ctx = ToolCtx {
            thread_id: "thread-a".into(),
            ..Default::default()
        };
        let second_ctx = ToolCtx {
            thread_id: "thread-b".into(),
            ..Default::default()
        };
        let args = json!({"query": "same private query"});

        let first = tool.run(&first_ctx, &args).await;
        let second = tool.run(&second_ctx, &args).await;
        let repeated = tool.run(&first_ctx, &args).await;

        assert_eq!(first.result["cache"]["hit"], false);
        assert_eq!(second.result["cache"]["hit"], false);
        assert_eq!(repeated.result["cache"]["hit"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn rate_limiter_orders_actual_provider_dispatches() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"ordered"}]}}"#;
        let (endpoint, _, requests) =
            capturing_mock_server_with_delay("200 OK", body, Duration::ZERO).await;
        let interval = Duration::from_millis(40);
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            interval,
        ));
        let first_ctx = ToolCtx {
            thread_id: "thread-a".into(),
            ..Default::default()
        };
        let second_ctx = ToolCtx {
            thread_id: "thread-b".into(),
            ..Default::default()
        };
        let first_args = json!({"query": "first query"});
        let second_args = json!({"query": "second query"});

        let (first, second) = tokio::join!(
            tool.run(&first_ctx, &first_args),
            tool.run(&second_ctx, &second_args),
        );

        assert_eq!(first.status, trouve_protocol::ToolStatus::Ok);
        assert_eq!(second.status, trouve_protocol::ToolStatus::Ok);
        let requests = requests.lock().unwrap();
        let dispatch_times: Vec<Instant> = requests
            .iter()
            .filter(|request| {
                matches!(
                    request
                        .body
                        .as_ref()
                        .and_then(|body| body.get("method"))
                        .and_then(Value::as_str),
                    Some("initialize" | "tools/call")
                )
            })
            .map(|request| request.at)
            .collect();
        assert_eq!(dispatch_times.len(), 4);
        assert!(
            dispatch_times
                .windows(2)
                .all(|pair| { pair[1].duration_since(pair[0]) >= Duration::from_millis(30) })
        );
    }

    #[tokio::test]
    async fn handed_off_request_is_rate_accounted_when_cancelled() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"unused"}]}}"#;
        let (endpoint, _, requests) = capturing_mock_server_with_delays(
            "200 OK",
            body,
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(20),
            true,
            None,
        )
        .await;
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            Duration::from_millis(80),
        ));
        let first_ctx = ToolCtx {
            thread_id: "thread-a".into(),
            ..Default::default()
        };
        let first_cancel = first_ctx.cancel.clone();
        let first = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&first_ctx, &json!({"query": "first cancelled handoff"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| !requests.lock().unwrap().is_empty()),
        )
        .await
        .unwrap();
        first_cancel.cancel();
        assert_eq!(
            first.await.unwrap().status,
            trouve_protocol::ToolStatus::Error
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.method == "DELETE")
            }),
        )
        .await
        .expect("a cancelled handed-off initialization must be cleaned up");

        let second_ctx = ToolCtx {
            thread_id: "thread-b".into(),
            ..Default::default()
        };
        let second_cancel = second_ctx.cancel.clone();
        let second = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&second_ctx, &json!({"query": "second after cancellation"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| {
                        request
                            .body
                            .as_ref()
                            .and_then(|body| body.get("method"))
                            .and_then(Value::as_str)
                            == Some("initialize")
                    })
                    .count()
                    >= 2
            }),
        )
        .await
        .unwrap();
        let observed_spacing = {
            let observed = requests.lock().unwrap();
            let initialization_times: Vec<_> = observed
                .iter()
                .filter(|request| {
                    request
                        .body
                        .as_ref()
                        .and_then(|body| body.get("method"))
                        .and_then(Value::as_str)
                        == Some("initialize")
                })
                .map(|request| request.at)
                .collect();
            initialization_times[1].duration_since(initialization_times[0])
        };
        assert!(observed_spacing >= Duration::from_millis(70));
        second_cancel.cancel();
        assert_eq!(
            second.await.unwrap().status,
            trouve_protocol::ToolStatus::Error
        );
    }

    #[tokio::test]
    async fn cancelled_initialization_returns_before_delayed_session_cleanup() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"unused"}]}}"#;
        let (endpoint, _, requests) = capturing_mock_server_with_delays(
            "200 OK",
            body,
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(300),
            true,
            None,
        )
        .await;
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            Duration::ZERO,
        ));
        let ctx = ToolCtx::default();
        let cancel = ctx.cancel.clone();
        let running = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&ctx, &json!({"query": "cancel stalled initialization"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| !requests.lock().unwrap().is_empty()),
        )
        .await
        .unwrap();

        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_millis(200), running)
            .await
            .expect("cancellation must not wait for initialization headers")
            .unwrap();
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert_eq!(result.result["error"], "search cancelled");
        let capacity_cancel = tokio_util::sync::CancellationToken::new();
        let mut permits = Vec::new();
        for _ in 0..SESSION_CLEANUP_CAPACITY {
            match tokio::time::timeout(
                Duration::from_millis(100),
                tool.cleanup.reserve(&capacity_cancel),
            )
            .await
            {
                Ok(Ok(permit)) => permits.push(permit),
                _ => panic!("pending header recovery must not consume teardown admission"),
            }
        }
        drop(permits);
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.method == "DELETE")
            }),
        )
        .await
        .expect("delayed initialization must retain its session cleanup path");
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| tool.cleanup.pending.load(Ordering::Acquire) == 0),
        )
        .await
        .expect("delayed initialization cleanup must remain bounded");
    }

    #[tokio::test]
    async fn outbound_handoff_timeout_releases_the_dispatch_gate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("https://{}/mcp", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _socket = socket;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let tool = Arc::new(WebSearch::new_with_handoff_timeout(
            vec![SearchProvider::parallel(endpoint)],
            Duration::ZERO,
            Duration::from_millis(50),
        ));
        let first_ctx = ToolCtx {
            thread_id: "thread-a".into(),
            ..Default::default()
        };
        let second_ctx = ToolCtx {
            thread_id: "thread-b".into(),
            ..Default::default()
        };
        let first_args = json!({"query": "first TLS stall"});
        let second_args = json!({"query": "second TLS stall"});

        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                tool.run(&first_ctx, &first_args),
                tool.run(&second_ctx, &second_args),
            )
        })
        .await
        .expect("handoff timeouts must release the dispatcher for later requests");
        for result in [first, second] {
            assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
            assert!(
                result.result["error"]
                    .as_str()
                    .unwrap()
                    .contains("provider dispatch timed out")
            );
        }
    }

    #[tokio::test]
    async fn stalled_response_headers_do_not_block_later_dispatches() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"late"}]}}"#;
        let (endpoint, _, requests) =
            capturing_mock_server_with_delay("200 OK", body, Duration::from_secs(30)).await;
        let tool = Arc::new(WebSearch::new_with_handoff_timeout(
            vec![SearchProvider::parallel(endpoint)],
            Duration::from_millis(40),
            Duration::from_secs(5),
        ));
        let first_ctx = ToolCtx {
            thread_id: "thread-a".into(),
            ..Default::default()
        };
        let first_cancel = first_ctx.cancel.clone();
        let first = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&first_ctx, &json!({"query": "first stalled query"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| {
                        request
                            .body
                            .as_ref()
                            .and_then(|body| body.get("method"))
                            .and_then(Value::as_str)
                            == Some("tools/call")
                    })
                    .count()
                    >= 1
            }),
        )
        .await
        .unwrap();

        let second_ctx = ToolCtx {
            thread_id: "thread-b".into(),
            ..Default::default()
        };
        let second_cancel = second_ctx.cancel.clone();
        let second = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&second_ctx, &json!({"query": "second stalled query"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|request| {
                        request
                            .body
                            .as_ref()
                            .and_then(|body| body.get("method"))
                            .and_then(Value::as_str)
                            == Some("tools/call")
                    })
                    .count()
                    >= 2
            }),
        )
        .await
        .expect("a stalled first response must not hold the dispatch gate");

        first_cancel.cancel();
        second_cancel.cancel();
        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(first, second)
        })
        .await
        .expect("cancellation must not wait for session cleanup");
        assert_eq!(first.unwrap().status, trouve_protocol::ToolStatus::Error);
        assert_eq!(second.unwrap().status, trouve_protocol::ToolStatus::Error);
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
        tokio::time::timeout(
            Duration::from_secs(2),
            wait_for(|| calls.load(Ordering::SeqCst) != 0),
        )
        .await
        .unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert_eq!(result.result["error"], "search cancelled");
    }

    #[tokio::test]
    async fn cancellation_returns_while_lifecycle_cleanup_remains_drainable() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"late"}]}}"#;
        let (endpoint, _, requests) = capturing_mock_server_with_delays(
            "200 OK",
            body,
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::ZERO,
            true,
            None,
        )
        .await;
        let tool = Arc::new(WebSearch::new(
            vec![SearchProvider::parallel(endpoint)],
            Duration::ZERO,
        ));
        let ctx = ToolCtx::default();
        let cancel = ctx.cancel.clone();
        let running = tokio::spawn({
            let tool = tool.clone();
            async move {
                tool.run(&ctx, &json!({"query": "cancel with stalled cleanup"}))
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests.lock().unwrap().iter().any(|request| {
                    request
                        .body
                        .as_ref()
                        .and_then(|body| body.get("method"))
                        .and_then(Value::as_str)
                        == Some("tools/call")
                })
            }),
        )
        .await
        .unwrap();

        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_millis(200), running)
            .await
            .expect("cancellation must not await session cleanup")
            .unwrap();
        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        assert_eq!(result.result["error"], "search cancelled");
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request.method == "DELETE")
            }),
        )
        .await
        .expect("the lifecycle worker must dispatch cleanup");

        let pending = tool.cleanup.pending.clone();
        let drop_started = Instant::now();
        drop(tool);
        assert!(
            drop_started.elapsed() < Duration::from_millis(100),
            "dropping the owner must only signal its independent cleanup drain"
        );
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for(|| pending.load(Ordering::Acquire) == 0),
        )
        .await
        .expect("the detached lifecycle worker must finish its bounded drain");
    }

    #[tokio::test]
    async fn cleanup_capacity_is_reserved_before_initialization() {
        let worker = SessionCleanupWorker::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut permits = Vec::new();
        for _ in 0..SESSION_CLEANUP_CAPACITY {
            match worker.reserve(&cancel).await {
                Ok(permit) => permits.push(permit),
                Err(_) => panic!("configured cleanup capacity must be reservable"),
            }
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), worker.reserve(&cancel))
                .await
                .is_err(),
            "session creation must wait before exceeding cleanup capacity"
        );

        drop(permits.pop());
        let replacement = tokio::time::timeout(Duration::from_secs(1), worker.reserve(&cancel))
            .await
            .expect("released cleanup capacity must admit another session");
        assert!(replacement.is_ok());
        drop(replacement);
        drop(permits);
        drop(worker);
    }

    #[tokio::test]
    async fn stateless_initialization_releases_cleanup_capacity() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"unused"}]}}"#;
        let (endpoint, _, _) = capturing_mock_server_with_delays(
            "200 OK",
            body,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            false,
            None,
        )
        .await;
        let provider = SearchProvider::parallel(endpoint);
        let tool = WebSearch::new(vec![], Duration::ZERO);

        let session = match tool
            .initialize_provider(&ToolCtx::default(), &provider)
            .await
        {
            Ok(session) => session,
            Err(_) => panic!("stateless mock initialization must succeed"),
        };

        assert!(session.id.is_none());
        assert!(session.cleanup_permit.is_none());
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut permits = Vec::new();
        for _ in 0..SESSION_CLEANUP_CAPACITY {
            let permit =
                tokio::time::timeout(Duration::from_secs(1), tool.cleanup.reserve(&cancel))
                    .await
                    .expect("stateless initialization must release its reservation");
            match permit {
                Ok(permit) => permits.push(permit),
                Err(_) => panic!("cleanup worker must remain available"),
            }
        }
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
    async fn bounds_provider_and_aggregate_errors() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "isError": true,
                "content": [{"type": "text", "text": "x".repeat(64 * 1024)}],
            },
        })
        .to_string();
        let (parallel, _) = mock_server("200 OK", body.clone()).await;
        let (exa, _) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(
            vec![SearchProvider::parallel(parallel), SearchProvider::exa(exa)],
            Duration::ZERO,
        );

        let result = tool
            .run(&ToolCtx::default(), &json!({"query": "large errors"}))
            .await;

        assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        let error = result.result["error"].as_str().unwrap();
        assert!(error.chars().count() <= MAX_SEARCH_ERROR_CHARS);
        assert!(error.contains("parallel:"));
        assert!(error.contains("exa:"));
        assert!(error.contains("[truncated]"));
    }

    #[tokio::test]
    async fn rejects_invalid_queries_without_network_access() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"unused"}]}}"#;
        let (endpoint, calls) = mock_server("200 OK", body).await;
        let tool = WebSearch::new(vec![SearchProvider::parallel(endpoint)], Duration::ZERO);
        for args in [
            json!({}),
            json!({"query": "  "}),
            json!({"query": "ok", "max_results": 11}),
            json!({"query": "ok", "max_results": -1}),
            json!({"query": "ok", "max_results": 1.5}),
            json!({"query": "ok", "max_results": "8"}),
            json!({"query": "ok", "max_results": null}),
        ] {
            let result = tool.run(&ToolCtx::default(), &args).await;
            assert_eq!(result.status, trouve_protocol::ToolStatus::Error);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
