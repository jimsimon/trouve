//! Paid, opt-in qualification of Cursor's shipping SDK Bridge path.
//!
//! This test deliberately starts at the public HTTP API: it installs the
//! managed runtime, creates a real session worktree, drives the production
//! Cursor backend through the secured internal MCP bridge, observes the
//! durable event log, concurrently routes two SDK agents in separate session
//! worktrees through one warm Bridge process, resumes the first agent, and
//! removes the managed runtime again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use trouve_core::Engine;
use trouve_core::config::{Config, ProviderConfig};
use trouve_core::store::Store;
use trouve_protocol::Scope;

const LIVE_TIMEOUT: Duration = Duration::from_secs(300);
const FILE_NAME: &str = "cursor-sdk-e2e.txt";
const FILE_CONTENT: &str = "cursor-sdk-production-e2e";
const OVERLAP_FILE_NAME: &str = "cursor-sdk-overlap-e2e.txt";
const RESUME_CONTENT: &str = "cursor-sdk-resume-worktree";
const PARALLEL_CONTENT: &str = "cursor-sdk-parallel-worktree";
const WRITE_MARKER: &str = "CURSOR_SDK_WRITE_OK";
const RESUME_MARKER: &str = "CURSOR_SDK_RESUME_OK";
const PARALLEL_MARKER: &str = "CURSOR_SDK_PARALLEL_OK";

struct LiveServerGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl LiveServerGuard {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn shutdown(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for LiveServerGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

struct ManagedCursorRuntimeGuard {
    data_dir: PathBuf,
    armed: bool,
}

impl ManagedCursorRuntimeGuard {
    fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ManagedCursorRuntimeGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for attempt in 0..=40 {
            match trouve_agents::install::uninstall(
                &self.data_dir,
                trouve_agents::install::CliId::CursorSdkBridge,
            ) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && attempt < 40 => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    eprintln!(
                        "failed to remove managed Cursor SDK runtime during test teardown: {error}"
                    );
                    return;
                }
            }
        }
    }
}

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

fn persisted_thread_events(engine: &Engine, thread_id: &str) -> Vec<serde_json::Value> {
    engine
        .store()
        .events_after(&Scope::Thread(thread_id.to_string()), 0)
        .expect("read complete persisted thread history")
        .into_iter()
        .map(|envelope| serde_json::to_value(envelope).expect("encode persisted thread event"))
        .collect()
}

fn assert_turn_completed(events: &[serde_json::Value], turn: u64) {
    let terminals = events
        .iter()
        .enumerate()
        .filter(|(_, event)| terminal_event(event, turn))
        .collect::<Vec<_>>();
    assert_eq!(
        terminals.len(),
        1,
        "Cursor SDK turn {turn} did not emit exactly one terminal event: {terminals:?}"
    );
    let (terminal_index, terminal) = terminals[0];
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
    assert!(
        events[terminal_index + 1..]
            .iter()
            .all(|event| event["turn"] != turn),
        "Cursor SDK turn {turn} emitted durable events after its terminal event: {:?}",
        &events[terminal_index + 1..]
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

#[tokio::test]
async fn live_server_guard_aborts_during_unwind_cleanup() {
    let handle = tokio::spawn(std::future::pending::<()>());
    let abort_handle = handle.abort_handle();
    drop(LiveServerGuard::new(handle));
    tokio::task::yield_now().await;
    assert!(abort_handle.is_finished());
}

#[test]
fn managed_cursor_runtime_guard_removes_the_managed_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let runtime_root = temporary.path().join("cli/cursor-sdk-bridge/v1");
    let managed_bin = temporary.path().join("cli/bin/cursor-sdk-bridge");
    std::fs::create_dir_all(&runtime_root).unwrap();
    std::fs::create_dir_all(managed_bin.parent().unwrap()).unwrap();
    std::fs::write(runtime_root.join("cursor-sdk-bridge"), "fixture").unwrap();
    std::fs::write(&managed_bin, "fixture").unwrap();

    drop(ManagedCursorRuntimeGuard::new(
        temporary.path().to_path_buf(),
    ));

    assert!(!temporary.path().join("cli/cursor-sdk-bridge").exists());
    assert!(!managed_bin.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads the Cursor runtime and makes paid API calls; run with TROUVE_E2E=1 cargo test -- --ignored"]
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
    let mut runtime_guard = ManagedCursorRuntimeGuard::new(data_dir.clone());
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
    let server = LiveServerGuard::new(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap()
    }));
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("build bounded live-qualification HTTP client");
    let base = format!("http://{address}/v1");
    let cursor_state_root = data_dir.join("cursor-sdk");

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

    wait_for_event(&client, &events_url, |event| terminal_event(event, 1)).await;
    let first_events = persisted_thread_events(&engine, thread_id);
    assert_turn_completed(&first_events, 1);
    assert!(
        assistant_text(&first_events, 1).contains(WRITE_MARKER),
        "{first_events:?}"
    );
    let requests = first_events
        .iter()
        .filter(|event| event["type"] == "tool.requested" && event["turn"] == 1)
        .collect::<Vec<_>>();
    assert_eq!(
        requests.len(),
        1,
        "write turn escaped its exact one-call tool policy: {requests:?}"
    );
    assert_eq!(requests[0]["tool"], "write_file", "{:?}", requests[0]);
    assert_eq!(requests[0]["args"]["path"], FILE_NAME, "{:?}", requests[0]);
    assert_eq!(
        requests[0]["args"]["content"], FILE_CONTENT,
        "{:?}",
        requests[0]
    );
    let write_call_id = requests[0]["call_id"].as_str().unwrap();
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
    let first_runtime_dirs = bridge_runtime_dirs(&cursor_state_root);
    assert_eq!(
        first_runtime_dirs.len(),
        1,
        "the first turn should retain exactly one warm Bridge process: {first_runtime_dirs:?}"
    );

    let parallel_session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({
            "workspace_id": workspace["id"],
            "title": "Cursor SDK shared-Bridge qualification",
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
    let parallel_worktree = PathBuf::from(parallel_session["worktree_path"].as_str().unwrap());
    let parallel_thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": parallel_session["id"],
            "title": "Cursor SDK parallel agent",
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
    let parallel_thread_id = parallel_thread["id"].as_str().unwrap().to_string();
    let parallel_events_url = format!("{base}/threads/{parallel_thread_id}/events");

    let second_body = serde_json::json!({
        "content": format!(
            "Call write_file exactly once with \
             {{\"path\":\"{OVERLAP_FILE_NAME}\",\"content\":\"{RESUME_CONTENT}\"}}. \
             After it succeeds, reply with exactly {RESUME_MARKER}. Do not call any other tool."
        )
    });
    let parallel_body = serde_json::json!({
        "content": format!(
            "Call write_file exactly once with \
             {{\"path\":\"{OVERLAP_FILE_NAME}\",\"content\":\"{PARALLEL_CONTENT}\"}}. \
             After it succeeds, reply with exactly {PARALLEL_MARKER}. Do not call any other tool."
        )
    });
    let (second_send, parallel_send) = tokio::join!(
        client
            .post(format!("{base}/threads/{thread_id}/messages"))
            .json(&second_body)
            .send(),
        client
            .post(format!("{base}/threads/{parallel_thread_id}/messages"))
            .json(&parallel_body)
            .send(),
    );
    let second_send = second_send.unwrap();
    assert!(second_send.status().is_success(), "{second_send:?}");
    let parallel_send = parallel_send.unwrap();
    assert!(parallel_send.status().is_success(), "{parallel_send:?}");

    // Neither approval is released until both requests are durably visible.
    // Reaching this barrier proves that two agents have callbacks concurrently
    // admitted through the one process-wide Bridge callback registration.
    let (second_approval_events, parallel_approval_events) = tokio::join!(
        wait_for_event(&client, &events_url, |event| {
            (event["type"] == "approval.requested" && event["turn"] == 2)
                || terminal_event(event, 2)
        }),
        wait_for_event(&client, &parallel_events_url, |event| {
            (event["type"] == "approval.requested" && event["turn"] == 1)
                || terminal_event(event, 1)
        }),
    );
    let second_approval = second_approval_events
        .iter()
        .find(|event| event["type"] == "approval.requested" && event["turn"] == 2)
        .unwrap_or_else(|| {
            panic!("resume turn terminated before the overlap barrier: {second_approval_events:?}")
        });
    let parallel_approval = parallel_approval_events
        .iter()
        .find(|event| event["type"] == "approval.requested" && event["turn"] == 1)
        .unwrap_or_else(|| {
            panic!(
                "parallel turn terminated before the overlap barrier: {parallel_approval_events:?}"
            )
        });
    let second_call_id = second_approval["call_id"].as_str().unwrap();
    let parallel_call_id = parallel_approval["call_id"].as_str().unwrap();
    let second_requested = second_approval_events
        .iter()
        .find(|event| event["type"] == "tool.requested" && event["call_id"] == second_call_id)
        .expect("resume approval has a durable tool request");
    let parallel_requested = parallel_approval_events
        .iter()
        .find(|event| event["type"] == "tool.requested" && event["call_id"] == parallel_call_id)
        .expect("parallel approval has a durable tool request");
    assert_eq!(second_requested["tool"], "write_file", "{second_requested}");
    assert_eq!(
        second_requested["args"]["path"], OVERLAP_FILE_NAME,
        "{second_requested}"
    );
    assert_eq!(
        second_requested["args"]["content"], RESUME_CONTENT,
        "{second_requested}"
    );
    assert_eq!(
        parallel_requested["tool"], "write_file",
        "{parallel_requested}"
    );
    assert_eq!(
        parallel_requested["args"]["path"], OVERLAP_FILE_NAME,
        "{parallel_requested}"
    );
    assert_eq!(
        parallel_requested["args"]["content"], PARALLEL_CONTENT,
        "{parallel_requested}"
    );

    let (second_approval_response, parallel_approval_response) = tokio::join!(
        client
            .post(format!("{base}/approvals"))
            .json(&serde_json::json!({
                "thread_id": thread_id,
                "call_id": second_call_id,
                "decision": "approve"
            }))
            .send(),
        client
            .post(format!("{base}/approvals"))
            .json(&serde_json::json!({
                "thread_id": parallel_thread_id,
                "call_id": parallel_call_id,
                "decision": "approve"
            }))
            .send(),
    );
    assert_eq!(
        second_approval_response.unwrap().status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        parallel_approval_response.unwrap().status(),
        reqwest::StatusCode::NO_CONTENT
    );

    tokio::join!(
        wait_for_event(&client, &events_url, |event| terminal_event(event, 2)),
        wait_for_event(&client, &parallel_events_url, |event| terminal_event(
            event, 1
        )),
    );
    let second_events = persisted_thread_events(&engine, thread_id);
    assert_turn_completed(&second_events, 2);
    assert!(
        assistant_text(&second_events, 2).contains(RESUME_MARKER),
        "Cursor did not return the resume marker: {}",
        assistant_text(&second_events, 2)
    );
    let requests = second_events
        .iter()
        .filter(|event| event["type"] == "tool.requested" && event["turn"] == 2)
        .collect::<Vec<_>>();
    assert_eq!(
        requests.len(),
        1,
        "resume turn escaped its exact one-call tool policy: {requests:?}"
    );
    assert_eq!(requests[0]["tool"], "write_file", "{:?}", requests[0]);
    assert_eq!(
        requests[0]["args"]["path"], OVERLAP_FILE_NAME,
        "{:?}",
        requests[0]
    );
    assert_eq!(
        requests[0]["args"]["content"], RESUME_CONTENT,
        "{:?}",
        requests[0]
    );
    let overlap_call_id = requests[0]["call_id"].as_str().unwrap();
    assert!(second_events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["call_id"] == overlap_call_id
            && event["status"] == "ok"
    }));
    let parallel_events = persisted_thread_events(&engine, &parallel_thread_id);
    assert_turn_completed(&parallel_events, 1);
    assert!(
        assistant_text(&parallel_events, 1).contains(PARALLEL_MARKER),
        "Cursor did not return the parallel marker: {}",
        assistant_text(&parallel_events, 1)
    );
    let parallel_requests = parallel_events
        .iter()
        .filter(|event| event["type"] == "tool.requested" && event["turn"] == 1)
        .collect::<Vec<_>>();
    assert_eq!(
        parallel_requests.len(),
        1,
        "parallel turn escaped its exact one-call tool policy: {parallel_requests:?}"
    );
    assert_eq!(parallel_requests[0]["tool"], "write_file");
    assert_eq!(
        parallel_requests[0]["args"]["path"], OVERLAP_FILE_NAME,
        "parallel callback was routed to the wrong thread: {:?}",
        parallel_requests[0]
    );
    assert_eq!(
        parallel_requests[0]["args"]["content"], PARALLEL_CONTENT,
        "parallel callback was routed to the wrong thread: {:?}",
        parallel_requests[0]
    );
    assert!(parallel_events.iter().any(|event| {
        event["type"] == "tool.completed"
            && event["call_id"] == parallel_call_id
            && event["status"] == "ok"
    }));
    assert_eq!(
        std::fs::read_to_string(worktree.join(OVERLAP_FILE_NAME)).unwrap(),
        RESUME_CONTENT
    );
    assert_eq!(
        std::fs::read_to_string(parallel_worktree.join(OVERLAP_FILE_NAME)).unwrap(),
        PARALLEL_CONTENT
    );
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
    let parallel_vendor_session = engine
        .store()
        .backend_session(&parallel_thread_id, "cursor")
        .unwrap()
        .expect("parallel turn stored its Cursor SDK agent id");
    assert_ne!(
        parallel_vendor_session.0, vendor_session.0,
        "two Trouve threads unexpectedly shared one Cursor agent id"
    );
    assert_eq!(
        bridge_runtime_dirs(&cursor_state_root),
        first_runtime_dirs,
        "parallel agents started another Bridge instead of sharing the warm process"
    );

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
    assert_eq!(
        items
            .iter()
            .filter(|item| {
                item["kind"] == "tool_call"
                    && item["tool"] == "write_file"
                    && item["status"] == "ok"
            })
            .count(),
        2,
        "the durable view omitted a completed Cursor write: {items:?}"
    );
    // Scan the complete data directory while every managed-runtime generation
    // still exists. Teardown must not be able to erase the only place where a
    // leaked credential would otherwise have been found.
    assert_eq!(
        first_file_containing(&data_dir, api_key.as_bytes()),
        None,
        "Cursor API key was persisted under Trouve's data directory"
    );
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
    runtime_guard.disarm();

    server.shutdown().await;
    // Retain a post-cleanup assertion as a defense against credentials written
    // outside the managed runtime tree during shutdown.
    assert_eq!(
        first_file_containing(&data_dir, api_key.as_bytes()),
        None,
        "Cursor API key was persisted under Trouve's data directory"
    );
}
