//! Paid, opt-in qualification of Cursor's shipping SDK Bridge path.
//!
//! This test deliberately starts at the public HTTP API: it installs the
//! managed runtime, creates a real session worktree, drives the production
//! Cursor backend through the secured internal MCP bridge, observes the
//! durable event log, resumes the same SDK agent in the same warm Bridge
//! process, and removes the managed runtime again. Setting
//! `CURSOR_E2E_REVIEW_JOB_URL` additionally runs two synthetic review cases
//! and replays every selected reviewer task from that public job against its
//! recorded git revision.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use trouve_core::Engine;
use trouve_core::config::{Config, ProviderConfig};
use trouve_core::store::{NewCodeReviewJob, NewCodeReviewTask, Store};

const LIVE_TIMEOUT: Duration = Duration::from_secs(300);
const FILE_NAME: &str = "cursor-sdk-e2e.txt";
const FILE_CONTENT: &str = "cursor-sdk-production-e2e";
const WRITE_MARKER: &str = "CURSOR_SDK_WRITE_OK";
const RESUME_MARKER: &str = "CURSOR_SDK_RESUME_OK";
const REVIEW_TOOL_CALL_LIMIT: u64 = 24;
const REVIEW_MEDIAN_TOOL_CALL_TARGET: f64 = 4.0;
const REVIEW_P90_TOOL_CALL_TARGET: usize = 8;

fn init_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir).args(args);
        assert!(
            trouve_process::output(&mut command)
                .expect("spawn git")
                .status
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "cursor-sdk-e2e@trouve.test"]);
    run(&["config", "user.name", "Trouve Cursor SDK E2E"]);
    std::fs::write(dir.join("README.md"), "# Cursor SDK E2E\n").unwrap();
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/authorization.rs"),
        r#"pub struct Request {
    pub is_admin: bool,
    pub revoked: bool,
}

pub fn authorize(request: &Request) -> bool {
    request.is_admin && !request.revoked
}

pub fn authorize_cached(request: &Request) -> bool {
    // Cache entries retain role membership but not revocation state.
    request.is_admin
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("src/handler.rs"),
        r#"use crate::authorization::{authorize_cached, Request};

pub fn handle_admin_request(request: &Request) -> bool {
    authorize_cached(request)
}
"#,
    )
    .unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

async fn wait_for_event(
    client: &reqwest::Client,
    url: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    let response = client
        .get(url)
        .send()
        .await
        .expect("open thread event stream")
        .error_for_status()
        .expect("thread event stream status");
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut events = Vec::new();
    let receive = async {
        while let Some(chunk) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(
                &chunk.expect("read thread event stream"),
            ));
            while let Some(position) = buffer.find('\n') {
                let line = buffer[..position].trim().to_string();
                buffer.drain(..=position);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let event: serde_json::Value =
                    serde_json::from_str(data.trim()).expect("decode thread event");
                let done = predicate(&event);
                events.push(event);
                if done {
                    return events;
                }
            }
        }
        panic!("thread event stream ended before the expected event");
    };
    tokio::time::timeout(LIVE_TIMEOUT, receive)
        .await
        .expect("timed out waiting for Cursor SDK event")
}

fn terminal_event(event: &serde_json::Value, turn: u64) -> bool {
    event["turn"] == turn
        && matches!(
            event["type"].as_str(),
            Some("turn.completed" | "turn.failed" | "turn.cancelled")
        )
}

fn assert_turn_completed(events: &[serde_json::Value], turn: u64) {
    let terminal = events
        .iter()
        .find(|event| terminal_event(event, turn))
        .expect("turn emitted a terminal event");
    assert_eq!(
        terminal["type"], "turn.completed",
        "Cursor SDK turn {turn} did not complete: {terminal}"
    );
    assert!(
        terminal["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0,
        "Cursor SDK turn {turn} omitted input usage: {terminal}"
    );
    assert!(
        terminal["usage"]["output_tokens"].as_u64().unwrap_or(0) > 0,
        "Cursor SDK turn {turn} omitted output usage: {terminal}"
    );
}

fn assistant_text(events: &[serde_json::Value], turn: u64) -> String {
    events
        .iter()
        .filter(|event| event["type"] == "assistant.message" && event["turn"] == turn)
        .filter_map(|event| event["content"].as_str())
        .collect()
}

fn first_file_containing(root: &Path, needle: &[u8]) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
        } else if metadata.is_file()
            && std::fs::read(&path)
                .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
        {
            return Some(path);
        }
    }
    None
}

fn bridge_runtime_dirs(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("bridge-runtime-"))
        {
            found.push(path);
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&path) {
            pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
        }
    }
    found.sort();
    found
}

async fn install_cursor_sdk(client: &reqwest::Client, base: &str) {
    let response = client
        .post(format!("{base}/clis/cursor-sdk-bridge/install"))
        .send()
        .await
        .expect("start Cursor SDK install");
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    let deadline = Instant::now() + LIVE_TIMEOUT;
    loop {
        let status: serde_json::Value = client
            .get(format!("{base}/clis/cursor-sdk-bridge/install"))
            .send()
            .await
            .expect("read Cursor SDK install status")
            .error_for_status()
            .expect("Cursor SDK install status")
            .json()
            .await
            .expect("decode Cursor SDK install status");
        match status["status"].as_str() {
            Some("success") => return,
            Some("failed") => panic!(
                "managed Cursor SDK installation failed: {}",
                status["error"].as_str().unwrap_or("unknown error")
            ),
            Some("pending" | "none") if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            other => panic!("unexpected Cursor SDK install status: {other:?}"),
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct ReviewTurnEvidence {
    task_id: String,
    completed: bool,
    valid_json: bool,
    tool_call_count: usize,
    tool_calls_by_name: BTreeMap<String, usize>,
    error: String,
    #[serde(skip)]
    output: String,
}

#[derive(Debug, serde::Serialize)]
struct ReviewReplaySummary {
    job_id: String,
    selected_tasks: usize,
    completed_tasks: usize,
    valid_json_tasks: usize,
    total_tool_calls: usize,
    median_tool_calls: f64,
    p90_tool_calls: usize,
    max_tool_calls: usize,
    tasks_at_limit: usize,
    hard_cap_failures: usize,
    tool_calls_by_name: BTreeMap<String, usize>,
    tasks: Vec<ReviewTurnEvidence>,
    failures: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct SyntheticReviewSummary {
    diff_contained: ReviewTurnEvidence,
    context_dependent: ReviewTurnEvidence,
}

struct ReviewJobEndpoints {
    detail: String,
    tasks: String,
}

fn review_output_value(output: &str) -> Option<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(output.trim())
        .ok()
        .filter(|value| {
            value
                .get("findings")
                .is_some_and(serde_json::Value::is_array)
        })
}

fn review_output_mentions(output: &str, terms: &[&str]) -> bool {
    let Some(value) = review_output_value(output) else {
        return false;
    };
    let findings = value["findings"].as_array().unwrap();
    !findings.is_empty()
        && terms
            .iter()
            .any(|term| value.to_string().to_ascii_lowercase().contains(term))
}

fn review_tool_counts(events: &[serde_json::Value], turn: u64) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for tool in events
        .iter()
        .filter(|event| event["type"] == "tool.requested" && event["turn"] == turn)
        .filter_map(|event| event["tool"].as_str())
    {
        *counts.entry(tool.to_string()).or_default() += 1;
    }
    counts
}

fn median(values: &[usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) as f64 / 2.0
    } else {
        sorted[middle] as f64
    }
}

fn p90(values: &[usize]) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * 9).div_ceil(10);
    sorted[rank.saturating_sub(1)]
}

fn review_job_endpoints(raw: &str) -> Result<ReviewJobEndpoints, String> {
    let mut url = reqwest::Url::parse(raw)
        .map_err(|error| format!("CURSOR_E2E_REVIEW_JOB_URL is invalid: {error}"))?;
    let job_id = if let Some(fragment) = url.fragment() {
        fragment
            .trim_start_matches('/')
            .strip_prefix("jobs/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(str::to_string)
    } else {
        url.path()
            .split_once("/v1/code-review/jobs/")
            .and_then(|(_, suffix)| suffix.split('/').next())
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    }
    .ok_or_else(|| {
        "CURSOR_E2E_REVIEW_JOB_URL must be a review UI job URL or /v1/code-review/jobs/{id} endpoint"
            .to_string()
    })?;
    url.set_fragment(None);
    url.set_path(&format!("/v1/code-review/jobs/{job_id}"));
    url.set_query(None);
    let tasks = url.to_string().trim_end_matches('/').to_string();
    url.set_query(Some("include_task_content=false"));
    Ok(ReviewJobEndpoints {
        detail: url.to_string(),
        tasks,
    })
}

fn create_qualification_review_job(engine: &Engine, model: &str) -> String {
    let job = engine
        .store()
        .enqueue_code_review_job(&NewCodeReviewJob {
            dedupe_key: "cursor-sdk:evidence-driven-review-qualification".into(),
            installation_id: 1,
            repository: "trouve/cursor-sdk-qualification".into(),
            pull_number: 1,
            pull_title: "Evidence-driven review qualification".into(),
            pull_url: "https://example.invalid/trouve/cursor-sdk-qualification/pull/1".into(),
            head_sha: "2222222222222222222222222222222222222222".into(),
            review_base_sha: "1111111111111111111111111111111111111111".into(),
            base_ref: "main".into(),
            head_ref: "qualification".into(),
            scope: trouve_protocol::CodeReviewJobScope::Full,
            trigger: "qualification".into(),
            retry_of: None,
            model: Some(model.to_string()),
            coordinator_thinking_level: None,
            router_model: None,
            router_thinking_level: None,
            prompt: String::new(),
            reviewers: Vec::new(),
            routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
            semantic_routing: false,
            included_reviewer_ids: Vec::new(),
            excluded_reviewer_ids: Vec::new(),
            config_hash: "cursor-sdk-review-qualification".into(),
        })
        .unwrap()
        .expect("qualification review job is unique");
    let claimed = engine
        .store()
        .claim_code_review_job()
        .unwrap()
        .expect("claim qualification review job");
    assert_eq!(claimed.job.id, job.id);
    job.id
}

#[allow(clippy::too_many_arguments)]
async fn run_qualified_review_turn(
    client: &reqwest::Client,
    base: &str,
    engine: &Arc<Engine>,
    session_id: &str,
    qualification_job_id: &str,
    task_id: &str,
    title: &str,
    model: &str,
    prompt: &str,
) -> Result<ReviewTurnEvidence, String> {
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session_id,
            "title": title,
            "mode": "review",
            "model": model,
            "permission_mode": "yolo"
        }))
        .send()
        .await
        .map_err(|error| format!("creating review thread for {task_id}: {error}"))?
        .error_for_status()
        .map_err(|error| format!("review thread {task_id} status: {error}"))?
        .json()
        .await
        .map_err(|error| format!("decoding review thread for {task_id}: {error}"))?;
    let thread_id = thread["id"]
        .as_str()
        .ok_or_else(|| format!("review thread for {task_id} omitted id"))?;
    let local_task = engine
        .store()
        .create_code_review_task(&NewCodeReviewTask {
            job_id: qualification_job_id.to_string(),
            role: trouve_protocol::CodeReviewTaskRole::Reviewer,
            reviewer_id: Some(task_id.to_string()),
            reviewer_name: title.to_string(),
            batch_index: 0,
            batch_count: 1,
            model: Some(model.to_string()),
            prompt: prompt.to_string(),
        })
        .map_err(|error| format!("creating local review task {task_id}: {error}"))?;
    engine
        .store()
        .start_code_review_task(&local_task.id, session_id, thread_id, model)
        .map_err(|error| format!("starting local review task {task_id}: {error}"))?
        .ok_or_else(|| format!("local review task {task_id} was superseded"))?;
    let budget = engine
        .begin_automated_review_tool_budget_for_qualification(thread_id, REVIEW_TOOL_CALL_LIMIT)
        .map_err(|error| format!("arming review budget for {task_id}: {error}"))?;
    let send = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({ "content": prompt }))
        .send()
        .await
        .map_err(|error| format!("sending review task {task_id}: {error}"))?;
    if !send.status().is_success() {
        drop(budget);
        let error = format!("sending review task {task_id} returned {}", send.status());
        let _ = engine
            .store()
            .finish_code_review_task(&local_task.id, "failed", "", 0, &error);
        return Err(error);
    }
    let events = wait_for_event(
        client,
        &format!("{base}/threads/{thread_id}/events"),
        |event| terminal_event(event, 1),
    )
    .await;
    drop(budget);
    let terminal = events
        .iter()
        .find(|event| terminal_event(event, 1))
        .ok_or_else(|| format!("review task {task_id} omitted terminal event"))?;
    let completed = terminal["type"] == "turn.completed";
    let output = assistant_text(&events, 1);
    let valid_json = review_output_value(&output).is_some();
    let tool_calls_by_name = review_tool_counts(&events, 1);
    let tool_call_count = tool_calls_by_name.values().sum();
    let error = terminal["error"].as_str().unwrap_or_default().to_string();
    let candidate_count = review_output_value(&output)
        .and_then(|value| value["findings"].as_array().map(Vec::len))
        .unwrap_or(0);
    engine
        .store()
        .finish_code_review_task(
            &local_task.id,
            if completed { "succeeded" } else { "failed" },
            &output,
            candidate_count as u64,
            &error,
        )
        .map_err(|finish_error| format!("finishing local review task {task_id}: {finish_error}"))?;
    Ok(ReviewTurnEvidence {
        task_id: task_id.to_string(),
        completed,
        valid_json,
        tool_call_count,
        tool_calls_by_name,
        error,
        output,
    })
}

async fn replay_review_job(
    client: &reqwest::Client,
    base: &str,
    engine: &Arc<Engine>,
    qualification_job_id: &str,
    job_url: &str,
) -> Result<ReviewReplaySummary, String> {
    let endpoints = review_job_endpoints(job_url)?;
    let remote: serde_json::Value = client
        .get(&endpoints.detail)
        .send()
        .await
        .map_err(|error| format!("fetching review job: {error}"))?
        .error_for_status()
        .map_err(|error| format!("review job status: {error}"))?
        .json()
        .await
        .map_err(|error| format!("decoding review job: {error}"))?;
    let job = remote
        .get("job")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "review job response omitted job".to_string())?;
    let job_id = job
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "review job omitted id".to_string())?
        .to_string();
    let head_sha = job
        .get("head_sha")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "review job omitted head_sha".to_string())?;
    let base_sha = job
        .get("review_base_sha")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "review job omitted review_base_sha".to_string())?;
    let mut tasks = remote
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "review job omitted tasks".to_string())?
        .iter()
        .filter(|task| {
            task["role"] == "reviewer" && task["status"].as_str() != Some("not_applicable")
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Ok(limit) = std::env::var("CURSOR_E2E_REVIEW_TASK_LIMIT") {
        let limit = limit
            .parse::<usize>()
            .map_err(|error| format!("CURSOR_E2E_REVIEW_TASK_LIMIT: {error}"))?;
        tasks.truncate(limit);
    }
    if tasks.is_empty() {
        return Err("review job had no selected reviewer tasks".into());
    }
    let selected_tasks = tasks.len();
    let repository =
        std::env::current_dir().map_err(|error| format!("resolving repository root: {error}"))?;
    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({ "path": repository }))
        .send()
        .await
        .map_err(|error| format!("registering replay workspace: {error}"))?
        .error_for_status()
        .map_err(|error| format!("replay workspace status: {error}"))?
        .json()
        .await
        .map_err(|error| format!("decoding replay workspace: {error}"))?;
    let replay_session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({
            "workspace_id": workspace["id"],
            "title": format!("Cursor SDK replay {job_id}"),
            "base_ref": base_sha,
            "checkout_ref": head_sha,
            "fetch_latest": false
        }))
        .send()
        .await
        .map_err(|error| format!("creating replay session: {error}"))?
        .error_for_status()
        .map_err(|error| format!("replay session status: {error}"))?
        .json()
        .await
        .map_err(|error| format!("decoding replay session: {error}"))?;
    let replay_session_id = replay_session["id"]
        .as_str()
        .ok_or_else(|| "replay session omitted id".to_string())?
        .to_string();
    let concurrency = std::env::var("CURSOR_E2E_REVIEW_CONCURRENCY")
        .ok()
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|error| format!("CURSOR_E2E_REVIEW_CONCURRENCY: {error}"))
        })
        .transpose()?
        .unwrap_or(8)
        .clamp(1, 16);

    let results = futures::stream::iter(tasks.into_iter().map(|task| {
        let client = client.clone();
        let base = base.to_string();
        let engine = engine.clone();
        let session_id = replay_session_id.clone();
        let qualification_job_id = qualification_job_id.to_string();
        let task_base = endpoints.tasks.clone();
        async move {
            let task_id = task["id"]
                .as_str()
                .ok_or_else(|| "review task omitted id".to_string())?
                .to_string();
            let model = task["model"]
                .as_str()
                .ok_or_else(|| format!("review task {task_id} omitted model"))?
                .to_string();
            let detail: serde_json::Value = client
                .get(format!("{task_base}/tasks/{task_id}"))
                .send()
                .await
                .map_err(|error| format!("fetching task {task_id}: {error}"))?
                .error_for_status()
                .map_err(|error| format!("task {task_id} status: {error}"))?
                .json()
                .await
                .map_err(|error| format!("decoding task {task_id}: {error}"))?;
            let prompt = detail["prompt"]
                .as_str()
                .filter(|prompt| !prompt.is_empty())
                .ok_or_else(|| format!("review task {task_id} omitted prompt"))?;
            run_qualified_review_turn(
                &client,
                &base,
                &engine,
                &session_id,
                &qualification_job_id,
                &task_id,
                &format!("Replay {task_id}"),
                &model,
                prompt,
            )
            .await
        }
    }))
    .buffer_unordered(concurrency)
    .collect::<Vec<_>>()
    .await;

    let mut tasks = Vec::new();
    let mut failures = Vec::new();
    let mut tool_calls_by_name = BTreeMap::new();
    let mut tool_call_counts = Vec::new();
    let mut hard_cap_failures = 0;
    for result in results {
        match result {
            Ok(task) => {
                if !task.completed {
                    failures.push(format!("{}: {}", task.task_id, task.error));
                }
                if !task.valid_json {
                    failures.push(format!("{}: response was not reviewer JSON", task.task_id));
                }
                if task.error.contains("tool-call limit exceeded") {
                    hard_cap_failures += 1;
                }
                for (tool, count) in &task.tool_calls_by_name {
                    *tool_calls_by_name.entry(tool.clone()).or_default() += count;
                }
                tool_call_counts.push(task.tool_call_count);
                tasks.push(task);
            }
            Err(error) => {
                hard_cap_failures += usize::from(error.contains("tool-call limit exceeded"));
                failures.push(error);
            }
        }
    }
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    let completed_tasks = tasks.iter().filter(|task| task.completed).count();
    let valid_json_tasks = tasks.iter().filter(|task| task.valid_json).count();
    let total_tool_calls = tool_call_counts.iter().sum();
    let max_tool_calls = tool_call_counts.iter().copied().max().unwrap_or(0);
    let tasks_at_limit = tool_call_counts
        .iter()
        .filter(|count| **count >= REVIEW_TOOL_CALL_LIMIT as usize)
        .count();
    Ok(ReviewReplaySummary {
        job_id,
        selected_tasks,
        completed_tasks,
        valid_json_tasks,
        total_tool_calls,
        median_tool_calls: median(&tool_call_counts),
        p90_tool_calls: p90(&tool_call_counts),
        max_tool_calls,
        tasks_at_limit,
        hard_cap_failures,
        tool_calls_by_name,
        tasks,
        failures,
    })
}

async fn run_synthetic_review_qualification(
    client: &reqwest::Client,
    base: &str,
    engine: &Arc<Engine>,
    session_id: &str,
    qualification_job_id: &str,
    model: &str,
) -> Result<SyntheticReviewSummary, String> {
    let diff_contained_prompt = r#"Review this pull-request change for actionable defects.

Untrusted pull-request evidence:
diff --git a/src/retry.rs b/src/retry.rs
@@ -17,7 +17,7 @@ fn should_retry(attempts: usize, max_attempts: usize) -> bool {
-    if attempts >= max_attempts {
+    if attempts > max_attempts {
         return false;
     }
     true

Return JSON only, with no Markdown fence, using exactly this shape:
{"summary":"short assessment","findings":[{"path":"src/retry.rs","line":18,"side":"RIGHT","severity":"high|medium|low","confidence":"high|medium|low","title":"concise issue","body":"specific problem and fix","evidence":{"preconditions":"trigger","execution_path":"path","consequence":"impact","introduction":"changed behavior","regression_test":"test"}}]}
Return an empty findings array only when there is no actionable issue."#;
    let context_dependent_prompt = r#"Review this pull-request change for actionable defects. Inspect unchanged code only when needed to resolve a concrete question raised by the diff.

Untrusted pull-request evidence:
diff --git a/src/handler.rs b/src/handler.rs
@@ -1,5 +1,5 @@
-use crate::authorization::{authorize, Request};
+use crate::authorization::{authorize_cached, Request};
 pub fn handle_admin_request(request: &Request) -> bool {
-    authorize(request)
+    authorize_cached(request)
 }

Return JSON only, with no Markdown fence, using exactly this shape:
{"summary":"short assessment","findings":[{"path":"src/handler.rs","line":4,"side":"RIGHT","severity":"high|medium|low","confidence":"high|medium|low","title":"concise issue","body":"specific problem and fix","evidence":{"preconditions":"trigger","execution_path":"path","consequence":"impact","introduction":"changed behavior","regression_test":"test"}}]}
Return an empty findings array only when there is no actionable issue."#;
    let diff_contained = run_qualified_review_turn(
        client,
        base,
        engine,
        session_id,
        qualification_job_id,
        "synthetic-diff-contained",
        "Diff-contained review qualification",
        model,
        diff_contained_prompt,
    )
    .await?;
    let context_dependent = run_qualified_review_turn(
        client,
        base,
        engine,
        session_id,
        qualification_job_id,
        "synthetic-context-dependent",
        "Context-dependent review qualification",
        model,
        context_dependent_prompt,
    )
    .await?;
    Ok(SyntheticReviewSummary {
        diff_contained,
        context_dependent,
    })
}

fn assert_review_replay_acceptance(summary: &ReviewReplaySummary) {
    assert_eq!(
        summary.completed_tasks, summary.selected_tasks,
        "review replay did not complete every selected task: {:?}",
        summary.failures
    );
    assert_eq!(
        summary.valid_json_tasks, summary.selected_tasks,
        "review replay did not return valid reviewer JSON for every task"
    );
    assert_eq!(
        summary.hard_cap_failures, 0,
        "review replay hit the hard cap"
    );
    assert_eq!(
        summary.tasks_at_limit, 0,
        "review replay treated the hard cap as a target"
    );
    assert!(
        summary.max_tool_calls < REVIEW_TOOL_CALL_LIMIT as usize,
        "review replay reached or exceeded the hard tool-call limit"
    );
    assert!(
        summary.median_tool_calls <= REVIEW_MEDIAN_TOOL_CALL_TARGET,
        "review replay median {} exceeded {}",
        summary.median_tool_calls,
        REVIEW_MEDIAN_TOOL_CALL_TARGET
    );
    assert!(
        summary.p90_tool_calls <= REVIEW_P90_TOOL_CALL_TARGET,
        "review replay p90 {} exceeded {}",
        summary.p90_tool_calls,
        REVIEW_P90_TOOL_CALL_TARGET
    );
    assert!(
        summary.failures.is_empty(),
        "review replay failures: {:?}",
        summary.failures
    );
}

fn assert_synthetic_review_acceptance(summary: &SyntheticReviewSummary) {
    let diff = &summary.diff_contained;
    assert!(
        diff.completed,
        "diff-contained review failed: {}",
        diff.error
    );
    assert!(diff.valid_json, "diff-contained review did not return JSON");
    assert!(
        review_output_mentions(&diff.output, &["retry", "attempt"]),
        "diff-contained review missed the retry defect: {}",
        diff.output
    );
    assert_eq!(
        diff.tool_call_count, 0,
        "diff-contained defect triggered unnecessary lookup: {:?}",
        diff.tool_calls_by_name
    );

    let context = &summary.context_dependent;
    assert!(
        context.completed,
        "context-dependent review failed: {}",
        context.error
    );
    assert!(
        context.valid_json,
        "context-dependent review did not return JSON"
    );
    assert!(
        review_output_mentions(&context.output, &["revok"]),
        "context-dependent review missed the revocation defect: {}",
        context.output
    );
    assert!(
        (1..=4).contains(&context.tool_call_count),
        "context-dependent review did not use a targeted lookup: {:?}",
        context.tool_calls_by_name
    );
    assert!(
        context.tool_calls_by_name.keys().all(|tool| matches!(
            tool.as_str(),
            "read_file" | "search" | "find_related" | "grep"
        )),
        "context-dependent review used inventory/diff tools: {:?}",
        context.tool_calls_by_name
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "paid live test; run with TROUVE_E2E=1 and CURSOR_API_KEY"]
async fn cursor_sdk_shipping_path_installs_tools_resumes_and_cleans_up() {
    if std::env::var("TROUVE_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping: set TROUVE_E2E=1 to run live Cursor SDK qualification");
        return;
    }
    let api_key = std::env::var("CURSOR_API_KEY")
        .expect("CURSOR_API_KEY is required for live Cursor SDK qualification");
    assert!(
        !api_key.trim().is_empty(),
        "CURSOR_API_KEY must not be blank"
    );
    let model = std::env::var("CURSOR_E2E_MODEL").unwrap_or_else(|_| "composer-2.5".into());
    let qualified_model = format!("cursor/{model}");

    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repo");
    let data_dir = temporary.path().join("data");
    std::fs::create_dir(&repository).unwrap();
    init_repo(&repository);

    let mut config = Config {
        local_enabled: Some(false),
        ..Default::default()
    };
    config.providers.insert(
        "cursor".into(),
        ProviderConfig {
            kind: "cursor-sdk".into(),
            api_key: Some(api_key.clone()),
            tool_bridge: Some(true),
            ..Default::default()
        },
    );
    let engine = Arc::new(
        Engine::new(
            Store::open(&temporary.path().join("db/trouve.db")).unwrap(),
            data_dir.clone(),
            &config,
        )
        .with_config_dir(None)
        .with_default_model(&qualified_model),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    engine.set_base_url(&format!("http://{address}"));
    let router = trouve_server::build_secured_router(
        engine.clone(),
        trouve_server::ServerSecurity::loopback(),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    let base = format!("http://{address}/v1");

    install_cursor_sdk(&client, &base).await;
    let runtimes: serde_json::Value = client
        .get(format!("{base}/clis"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let cursor_runtime = runtimes["clis"]
        .as_array()
        .unwrap()
        .iter()
        .find(|runtime| runtime["id"] == "cursor-sdk-bridge")
        .expect("Cursor SDK runtime is listed");
    assert_eq!(cursor_runtime["source"], "managed", "{cursor_runtime}");
    let runtime_path = cursor_runtime["path"]
        .as_str()
        .expect("managed runtime path");
    assert!(Path::new(runtime_path).is_file());

    let health: Vec<serde_json::Value> = client
        .get(format!("{base}/subscriptions"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let cursor_health = health
        .iter()
        .find(|item| item["provider_id"] == "cursor")
        .expect("Cursor subscription health is returned");
    assert_eq!(cursor_health["status"], "ok", "{cursor_health}");
    assert!(
        !cursor_health["windows"].as_array().unwrap().is_empty(),
        "{cursor_health}"
    );

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({ "path": repository }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({
            "workspace_id": workspace["id"],
            "title": "Cursor SDK shipping-path qualification",
            "fetch_latest": false
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session["id"],
            "title": "Cursor SDK production adapter",
            "mode": "code",
            "model": format!("cursor/{model}"),
            "permission_mode": "ask"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");

    let first_send = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({
            "content": format!(
                "This is a qualification test. Call write_file exactly once with \
                 {{\"path\":\"{FILE_NAME}\",\"content\":\"{FILE_CONTENT}\"}}. \
                 Do not call shell or any other mutating tool. After it succeeds, \
                 reply with exactly {WRITE_MARKER}."
            )
        }))
        .send()
        .await
        .unwrap();
    assert!(first_send.status().is_success(), "{first_send:?}");

    let approval_events = wait_for_event(&client, &events_url, |event| {
        (event["type"] == "approval.requested" && event["turn"] == 1) || terminal_event(event, 1)
    })
    .await;
    let approval = approval_events
        .iter()
        .find(|event| event["type"] == "approval.requested" && event["turn"] == 1)
        .unwrap_or_else(|| panic!("write turn terminated before approval: {approval_events:?}"));
    let call_id = approval["call_id"].as_str().unwrap();
    let requested = approval_events
        .iter()
        .find(|event| event["type"] == "tool.requested" && event["call_id"] == call_id)
        .expect("write approval has a durable tool request");
    assert_eq!(requested["tool"], "write_file", "{requested}");
    assert_eq!(requested["args"]["path"], FILE_NAME, "{requested}");
    assert_eq!(requested["args"]["content"], FILE_CONTENT, "{requested}");

    let approval_response = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "call_id": call_id,
            "decision": "approve"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(approval_response.status(), reqwest::StatusCode::NO_CONTENT);

    let first_events = wait_for_event(&client, &events_url, |event| terminal_event(event, 1)).await;
    assert_turn_completed(&first_events, 1);
    assert!(
        assistant_text(&first_events, 1).contains(WRITE_MARKER),
        "{first_events:?}"
    );
    let writes = first_events
        .iter()
        .filter(|event| {
            event["type"] == "tool.requested" && event["turn"] == 1 && event["tool"] == "write_file"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        writes.len(),
        1,
        "write callback was not exactly once: {writes:?}"
    );
    let write_call_id = writes[0]["call_id"].as_str().unwrap();
    assert!(first_events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["call_id"] == write_call_id
            && event["status"] == "ok"
    }));
    assert_eq!(
        std::fs::read_to_string(worktree.join(FILE_NAME)).unwrap(),
        FILE_CONTENT
    );
    assert!(
        !repository.join(FILE_NAME).exists(),
        "Cursor mutated the registered checkout instead of the session worktree"
    );
    let vendor_session = engine
        .store()
        .backend_session(thread_id, "cursor")
        .unwrap()
        .expect("first turn stored the Cursor SDK agent id");
    let cursor_state_root = data_dir.join("cursor-sdk");
    let first_runtime_dirs = bridge_runtime_dirs(&cursor_state_root);
    assert_eq!(
        first_runtime_dirs.len(),
        1,
        "the first turn should retain exactly one warm Bridge process: {first_runtime_dirs:?}"
    );

    let second_send = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({
            "content": format!(
                "Call read_file exactly once with {{\"path\":\"{FILE_NAME}\"}}. \
                 If its complete content after removing one optional trailing newline is exactly \
                 {FILE_CONTENT}, reply with exactly {RESUME_MARKER}. Do not call any other tool."
            )
        }))
        .send()
        .await
        .unwrap();
    assert!(second_send.status().is_success(), "{second_send:?}");

    let second_events =
        wait_for_event(&client, &events_url, |event| terminal_event(event, 2)).await;
    assert_turn_completed(&second_events, 2);
    assert!(
        assistant_text(&second_events, 2).contains(RESUME_MARKER),
        "Cursor did not return the resume marker: {}",
        assistant_text(&second_events, 2)
    );
    let reads = second_events
        .iter()
        .filter(|event| {
            event["type"] == "tool.requested" && event["turn"] == 2 && event["tool"] == "read_file"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reads.len(),
        1,
        "read callback was not exactly once: {reads:?}"
    );
    let read_call_id = reads[0]["call_id"].as_str().unwrap();
    assert!(second_events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["call_id"] == read_call_id
            && event["status"] == "ok"
    }));
    assert_eq!(
        engine
            .store()
            .backend_session(thread_id, "cursor")
            .unwrap()
            .expect("second turn retained the Cursor SDK agent id")
            .0,
        vendor_session.0,
        "the warm Bridge process did not resume the same SDK agent"
    );
    assert_eq!(
        bridge_runtime_dirs(&cursor_state_root),
        first_runtime_dirs,
        "the second turn started a new Bridge instead of reusing the warm process"
    );

    let review_qualification = match std::env::var("CURSOR_E2E_REVIEW_JOB_URL") {
        Ok(job_url) => {
            let qualification_job_id = create_qualification_review_job(&engine, &qualified_model);
            let qualification_session_id = session["id"].as_str().unwrap().to_string();
            Some(
                async {
                    let synthetic = run_synthetic_review_qualification(
                        &client,
                        &base,
                        &engine,
                        &qualification_session_id,
                        &qualification_job_id,
                        &qualified_model,
                    )
                    .await?;
                    let replay =
                        replay_review_job(&client, &base, &engine, &qualification_job_id, &job_url)
                            .await?;
                    Ok::<_, String>((synthetic, replay))
                }
                .await,
            )
        }
        Err(_) => None,
    };

    let view: serde_json::Value = client
        .get(format!(
            "{base}/threads/{thread_id}/view?limit=100&turn_aligned=true"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = view["items"].as_array().unwrap();
    assert!(items.iter().any(|item| {
        item["kind"] == "tool_call" && item["tool"] == "write_file" && item["status"] == "ok"
    }));
    assert!(items.iter().any(|item| {
        item["kind"] == "tool_call" && item["tool"] == "read_file" && item["status"] == "ok"
    }));

    let uninstall = client
        .delete(format!("{base}/clis/cursor-sdk-bridge"))
        .send()
        .await
        .unwrap();
    assert_eq!(uninstall.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        !data_dir.join("cli/bin/cursor-sdk-bridge").exists(),
        "managed Cursor SDK runtime remained after uninstall"
    );
    assert!(
        bridge_runtime_dirs(&cursor_state_root).is_empty(),
        "uninstall did not stop and remove the warm Bridge runtime"
    );

    server.abort();
    let _ = server.await;
    assert_eq!(
        first_file_containing(&data_dir, api_key.as_bytes()),
        None,
        "Cursor API key was persisted under Trouve's data directory"
    );
    if let Some(result) = review_qualification {
        let (synthetic, replay) = result.expect("run evidence-driven review qualification");
        println!(
            "CURSOR_SDK_SYNTHETIC_REVIEW {}",
            serde_json::to_string(&synthetic).unwrap()
        );
        println!(
            "CURSOR_SDK_REVIEW_REPLAY {}",
            serde_json::to_string(&replay).unwrap()
        );
        assert_synthetic_review_acceptance(&synthetic);
        assert_review_replay_acceptance(&replay);
    }
}

#[test]
fn review_qualification_accepts_ui_and_api_job_urls() {
    let ui = review_job_endpoints("https://review.example/#/jobs/rv_example").unwrap();
    assert_eq!(
        ui.detail,
        "https://review.example/v1/code-review/jobs/rv_example?include_task_content=false"
    );
    assert_eq!(
        ui.tasks,
        "https://review.example/v1/code-review/jobs/rv_example"
    );

    let api = review_job_endpoints(
        "https://review.example/v1/code-review/jobs/rv_example?include_task_content=true",
    )
    .unwrap();
    assert_eq!(api.detail, ui.detail);
    assert_eq!(api.tasks, ui.tasks);
    assert!(review_job_endpoints("https://review.example/#/settings").is_err());
}

#[test]
fn review_qualification_uses_documented_distribution_statistics() {
    assert_eq!(median(&[]), 0.0);
    assert_eq!(median(&[1, 3, 5]), 3.0);
    assert_eq!(median(&[1, 3, 5, 9]), 4.0);
    assert_eq!(p90(&[]), 0);
    assert_eq!(p90(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]), 8);
}
