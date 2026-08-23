//! End-to-end protocol test: a scripted provider drives the real server,
//! event streams, approval flow, checkpointing, and undo — no network, no
//! real model.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use trouve_core::Engine;
use trouve_core::config::Config;
use trouve_core::store::{NewCodeReviewJob, NewCodeReviewTask, Store};
use trouve_protocol::{Event, Scope, Usage};
use trouve_providers::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

/// Turn 1: asks to write hello.txt, then finishes with a message.
struct ScriptedProvider {
    calls: AtomicUsize,
}

struct StaticThenLiveModelProvider {
    live_calls: Arc<AtomicUsize>,
}

fn catalog_model(id: &str, display_name: &str) -> trouve_protocol::ModelInfo {
    trouve_protocol::ModelInfo {
        id: id.into(),
        display_name: display_name.into(),
        context_window: 100_000,
        supports_tools: true,
        input_price_per_mtok: None,
        output_price_per_mtok: None,
        options_schema: serde_json::json!({}),
    }
}

#[async_trait::async_trait]
impl Provider for StaticThenLiveModelProvider {
    fn id(&self) -> &str {
        "catalog-test"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![catalog_model("catalog-test/static", "Static catalog model")]
    }

    async fn list_models(&self) -> Vec<trouve_protocol::ModelInfo> {
        self.live_calls.fetch_add(1, Ordering::SeqCst);
        vec![catalog_model("catalog-test/live", "Live discovered model")]
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Request("catalog-only provider".into()))
    }
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }
    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "scripted/test-model".into(),
            display_name: "Scripted test model".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: Some(1.0),
            output_price_per_mtok: Some(2.0),
            options_schema: serde_json::json!({}),
        }]
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events: Vec<Result<ProviderEvent, ProviderError>> = match call {
            0 => vec![
                Ok(ProviderEvent::TextDelta("Writing the file.".into())),
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "call_1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({"path": "hello.txt", "content": "hi\n"}),
                })),
                Ok(ProviderEvent::Completed {
                    usage: Usage {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                }),
            ],
            _ => vec![
                Ok(ProviderEvent::TextDelta("Done.".into())),
                Ok(ProviderEvent::Completed {
                    usage: Usage {
                        input_tokens: 20,
                        output_tokens: 2,
                        ..Default::default()
                    },
                }),
            ],
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn init_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir).args(args);
        assert!(
            trouve_process::output(&mut command)
                .unwrap()
                .status
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "t@example.com"]);
    run(&["config", "user.name", "T"]);
    std::fs::write(dir.join("README.md"), "# test\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

#[tokio::test]
async fn models_return_static_data_then_refresh_live_discovery() {
    let tmp = tempfile::tempdir().unwrap();
    let live_calls = Arc::new(AtomicUsize::new(0));
    let engine = Arc::new(
        Engine::new(
            Store::open_in_memory().unwrap(),
            tmp.path().join("data"),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_provider(
            "catalog-test",
            Arc::new(StaticThenLiveModelProvider {
                live_calls: live_calls.clone(),
            }),
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();

    let static_models: Vec<trouve_protocol::ModelInfo> = client
        .get(format!("http://{addr}/v1/models"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        static_models
            .iter()
            .any(|model| model.id == "catalog-test/static")
    );
    assert_eq!(live_calls.load(Ordering::SeqCst), 0);

    let live_models: Vec<trouve_protocol::ModelInfo> = client
        .get(format!("http://{addr}/v1/models/refresh"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        live_models
            .iter()
            .any(|model| model.id == "catalog-test/live")
    );
    assert_eq!(live_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mcp_server_enablement_is_persisted_without_removing_the_definition() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("config");
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(config_dir.clone())),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let created = client
        .put(format!("{base}/mcp-servers/docs"))
        .json(&serde_json::json!({
            "scope": "user",
            "command": "docs-mcp",
            "args": ["--stdio"],
            "env": {"TOKEN": "${DOCS_TOKEN}"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::NO_CONTENT);

    let disabled = client
        .put(format!("{base}/mcp-servers/docs/enabled"))
        .json(&serde_json::json!({"scope": "user", "enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), reqwest::StatusCode::NO_CONTENT);

    let servers: serde_json::Value = client
        .get(format!("{base}/mcp-servers?probe=false"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(servers[0]["name"], "docs");
    assert_eq!(servers[0]["enabled"], false);
    assert_eq!(servers[0]["health"], "disabled");
    assert_eq!(servers[0]["command"], "docs-mcp");
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_dir.join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(persisted["mcpServers"]["docs"]["disabled"], true);
    assert_eq!(
        persisted["mcpServers"]["docs"]["env"]["TOKEN"],
        "${DOCS_TOKEN}"
    );

    let enabled = client
        .put(format!("{base}/mcp-servers/docs/enabled"))
        .json(&serde_json::json!({"scope": "user", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), reqwest::StatusCode::NO_CONTENT);
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_dir.join("mcp.json")).unwrap())
            .unwrap();
    assert!(persisted["mcpServers"]["docs"].get("disabled").is_none());

    let missing = client
        .put(format!("{base}/mcp-servers/missing/enabled"))
        .json(&serde_json::json!({"scope": "user", "enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_diff_manifest_and_selected_file_patch_are_independent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    std::fs::create_dir(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/setup guide.md"), "old guide\n").unwrap();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&repo)
        .args(["add", "docs/setup guide.md"]);
    assert!(trouve_process::status(&mut command).unwrap().success());
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&repo)
        .args(["commit", "-m", "add guide"]);
    assert!(trouve_process::status(&mut command).unwrap().success());

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": workspace["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap();
    let worktree = Path::new(session["worktree_path"].as_str().unwrap());
    std::fs::write(
        worktree.join("docs/setup guide.md"),
        "new guide\nextra line\n",
    )
    .unwrap();

    let summary: serde_json::Value = client
        .get(format!("{base}/sessions/{session_id}/diff/summary"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summary["files"][0]["path"], "docs/setup guide.md");
    assert_eq!(summary["additions"], 2);
    assert_eq!(summary["deletions"], 1);

    let selected: serde_json::Value = client
        .get(format!("{base}/sessions/{session_id}/diff/file"))
        .query(&[("path", "docs/setup guide.md")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(selected["path"], "docs/setup guide.md");
    assert!(selected["diff"].as_str().unwrap().contains("+extra line"));

    let invalid = client
        .get(format!("{base}/sessions/{session_id}/diff/file"))
        .query(&[("path", "../README.md")])
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
}

async fn wait_for_event(
    client: &reqwest::Client,
    url: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> Vec<serde_json::Value> {
    let fut = async {
        let resp = client.get(url).send().await.unwrap();
        let mut events = Vec::new();
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                if let Some(data) = line.strip_prefix("data:") {
                    let v: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
                    let done = predicate(&v);
                    events.push(v);
                    if done {
                        return events;
                    }
                }
            }
        }
        panic!("event stream ended before the expected event");
    };
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .expect("timed out waiting for event")
}

#[tokio::test]
async fn workspace_close_hides_and_reregister_reopens() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();
    let request = serde_json::json!({"path": repo.to_str().unwrap()});

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let workspace_id = workspace["id"].as_str().unwrap();

    let response = client
        .delete(format!("{base}/workspaces/{workspace_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/workspaces"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed.is_empty());

    // Closing is non-destructive, but a hidden workspace cannot accept new
    // activity until its path is registered again.
    let response = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({
            "workspace_id": workspace_id,
            "title": "Must not start"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    let reopened: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reopened["id"], workspace_id);
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/workspaces"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], workspace_id);
}

#[tokio::test]
async fn full_turn_with_approval_checkpoint_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    // Protocol info.
    let info: serde_json::Value = client
        .get(format!("{base}/info"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["protocol_version"], trouve_protocol::PROTOCOL_VERSION);

    // Global defaults round-trip through one atomic protocol operation and
    // are inherited by newly created threads.
    let resp = client
        .put(format!("{base}/config/defaults"))
        .json(&serde_json::json!({
            "model": "scripted/test-model",
            "default_thinking_level": "high",
            "permission_mode": "ask"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers["default_model"], "scripted/test-model");
    assert_eq!(providers["default_thinking_level"], "high");
    assert_eq!(providers["default_permission_mode"], "ask");

    // Validation happens before any of the replacement defaults are applied.
    let resp = client
        .put(format!("{base}/config/defaults"))
        .json(&serde_json::json!({
            "model": "not-provider-qualified",
            "default_thinking_level": "low",
            "permission_mode": "yolo"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers["default_model"], "scripted/test-model");
    assert_eq!(providers["default_thinking_level"], "high");
    assert_eq!(providers["default_permission_mode"], "ask");

    // Workspace -> session -> thread.
    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Test session"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let worktree = session["worktree_path"].as_str().unwrap().to_string();
    let branch = session["branch"].as_str().unwrap();
    let short_id = branch.strip_prefix("trouve/").unwrap();
    assert_eq!(short_id.len(), 6);
    assert!(
        short_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert!(Path::new(&worktree).join("README.md").exists());

    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap().to_string();
    assert_eq!(thread["model_options"]["thinking_level"], "high");

    // Send a message; the scripted provider requests a write, which needs
    // approval in the default "ask" mode.
    let accepted: serde_json::Value = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "write hello"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["turn"], 1);

    let events_url = format!("{base}/threads/{thread_id}/events");
    let events = wait_for_event(&client, &events_url, |e| e["type"] == "approval.requested").await;
    let call_id = events
        .iter()
        .find(|e| e["type"] == "approval.requested")
        .unwrap()["call_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(call_id, "call_1");

    // Destructive session cleanup must not race the active turn that is
    // currently waiting for this approval.
    let resp = client
        .delete(format!(
            "{base}/sessions/{}",
            session["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    assert!(Path::new(&worktree).exists());

    // Approve; the turn then finishes with a checkpoint.
    let resp = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "call_id": call_id,
            "decision": "approve",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let events = wait_for_event(&client, &events_url, |e| e["type"] == "turn.completed").await;
    let completed = events
        .iter()
        .find(|e| e["type"] == "turn.completed")
        .unwrap();
    assert!(
        completed["checkpoint_id"].is_string(),
        "mutating turn must checkpoint"
    );
    let checkpoint_id = completed["checkpoint_id"].as_str().unwrap().to_string();
    assert_eq!(completed["usage"]["input_tokens"], 30);
    assert_eq!(completed["usage"]["context_input_tokens"], 20);
    assert!((completed["usage"]["cost_usd"].as_f64().unwrap() - 0.000_044).abs() < 1e-12);
    let live_usage = events
        .iter()
        .filter(|event| event["type"] == "turn.usage_updated")
        .collect::<Vec<_>>();
    assert_eq!(live_usage.len(), 2);
    assert_eq!(live_usage[0]["usage"]["context_input_tokens"], 10);
    assert_eq!(live_usage[1]["usage"]["context_input_tokens"], 20);
    assert!((live_usage[0]["usage"]["cost_usd"].as_f64().unwrap() - 0.000_020).abs() < 1e-12);
    assert!((live_usage[1]["usage"]["cost_usd"].as_f64().unwrap() - 0.000_024).abs() < 1e-12);
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "tool.completed" && e["status"] == "ok")
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("hello.txt")).unwrap(),
        "hi\n"
    );

    // A fresh client seeds the folded chat at a precise cursor instead of
    // replaying the thread stream from zero.
    let view_response = client
        .get(format!("{base}/threads/{thread_id}/view"))
        .send()
        .await
        .unwrap();
    assert_eq!(view_response.status(), reqwest::StatusCode::OK);
    let view_cursor = view_response
        .headers()
        .get(trouve_protocol::EVENT_CURSOR_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let view: serde_json::Value = view_response.json().await.unwrap();
    assert!(view_cursor >= completed["cursor"].as_u64().unwrap());
    assert!(
        view["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "assistant" && item["content"] == "Writing the file.")
    );
    let folded_tool = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "tool_call" && item["status"] == "ok")
        .expect("completed tool call in folded view");
    assert!(
        folded_tool["duration_ms"].is_u64(),
        "folded tool calls retain server-measured execution time"
    );
    assert_eq!(folded_tool["details_deferred"], true);
    assert!(folded_tool.get("result").is_none() || folded_tool["result"].is_null());
    let folded_call_id = folded_tool["call_id"].as_str().unwrap();
    let tool_details: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/tools/{folded_call_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tool_details["call_id"], folded_call_id);
    assert_eq!(tool_details["args"]["content"], "hi\n");
    assert_eq!(tool_details["result"]["bytes_written"], 3);
    assert_eq!(view["turn_running"], false);
    assert_eq!(view["last_usage"]["context_input_tokens"], 20);
    let folded_turn = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "turn_status" && item["turn"] == 1)
        .expect("completed turn in folded view");
    assert_eq!(folded_turn["state"]["checkpoint_id"], checkpoint_id);
    let total_items = view["total_items"].as_u64().unwrap();
    assert!(total_items > 1);
    let tail: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/view?limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tail["items"].as_array().unwrap().len(), 1);
    assert_eq!(tail["item_offset"], total_items - 1);
    assert_eq!(tail["total_items"], total_items);
    assert_eq!(tail["has_older"], true);
    let aligned: serde_json::Value = client
        .get(format!(
            "{base}/threads/{thread_id}/view?limit=1&turn_aligned=true"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(aligned["item_offset"], 0);
    assert_eq!(
        aligned["items"].as_array().unwrap().len() as u64,
        total_items
    );
    assert_eq!(aligned["has_older"], false);
    assert_eq!(aligned["items"][0]["kind"], "turn_status");
    let older: serde_json::Value = client
        .get(format!(
            "{base}/threads/{thread_id}/view?limit=1&before={}",
            tail["item_offset"].as_u64().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(older["items"].as_array().unwrap().len(), 1);
    assert_eq!(older["item_offset"], total_items - 2);
    let capped: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/view?limit=10000"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(capped["items"].as_array().unwrap().len() <= 512);

    // Usage accounting aggregates the turn.
    let usage: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/usage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(usage["turns"], 1);
    assert_eq!(usage["input_tokens"], 30);
    assert_eq!(usage["output_tokens"], 7);

    // Cursor resumption: replay from mid-stream only returns later events.
    let mid = events[events.len() / 2]["cursor"].as_u64().unwrap();
    let tail = wait_for_event(&client, &format!("{events_url}?after={mid}"), |e| {
        e["type"] == "turn.completed"
    })
    .await;
    assert!(tail.iter().all(|e| e["cursor"].as_u64().unwrap() > mid));

    // A checkpoint fork starts a distinct session/worktree at the exact
    // post-turn tree and carries the source thread's effective settings.
    let fork: serde_json::Value = client
        .post(format!("{base}/checkpoints/{checkpoint_id}/fork"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fork_session_id = fork["session"]["id"].as_str().unwrap();
    let fork_worktree = fork["session"]["worktree_path"].as_str().unwrap();
    assert_ne!(fork_session_id, session["id"]);
    assert_eq!(fork["thread"]["session_id"], fork_session_id);
    assert_eq!(fork["thread"]["model"], thread["model"]);
    assert_eq!(fork["thread"]["model_options"], thread["model_options"]);
    assert_eq!(
        std::fs::read_to_string(Path::new(fork_worktree).join("hello.txt")).unwrap(),
        "hi\n"
    );
    let resp = client
        .delete(format!("{base}/sessions/{fork_session_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // An exact restore also resets uncheckpointed terminal/editor drift when
    // the checkpoint is already the undo stack's current position.
    std::fs::write(Path::new(&worktree).join("hello.txt"), "drift\n").unwrap();
    let resp = client
        .post(format!("{base}/checkpoints/{checkpoint_id}/restore"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("hello.txt")).unwrap(),
        "hi\n"
    );
    let restored = wait_for_event(
        &client,
        &format!("{base}/sessions/{}/events", session["id"].as_str().unwrap()),
        |event| event["type"] == "checkpoint.restored" && event["checkpoint_id"] == checkpoint_id,
    )
    .await;
    assert_eq!(restored.last().unwrap()["direction"], "exact");

    // Undo restores the pre-turn state.
    let session_id = session["id"].as_str().unwrap();
    let resp = client
        .post(format!("{base}/sessions/{session_id}/undo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(!Path::new(&worktree).join("hello.txt").exists());

    // Redo brings it back.
    let resp = client
        .post(format!("{base}/sessions/{session_id}/redo"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("hello.txt")).unwrap(),
        "hi\n"
    );

    // Once idle, deletion removes both the relational state and worktree.
    let resp = client
        .delete(format!("{base}/sessions/{session_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while Path::new(&worktree).exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deleted session worktree was not cleaned up");
    let resp = client
        .get(format!("{base}/sessions/{session_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

struct IterationLimitProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for IterationLimitProvider {
    fn id(&self) -> &str {
        "iteration-limit"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if tools.is_empty() {
            vec![
                Ok(ProviderEvent::TextDelta(
                    "I stopped at the step limit; continue to finish.".into(),
                )),
                Ok(ProviderEvent::Completed {
                    usage: Usage::default(),
                }),
            ]
        } else {
            vec![
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: format!("limit-call-{call}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "README.md"}),
                })),
                Ok(ProviderEvent::Completed {
                    usage: Usage::default(),
                }),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn iteration_limit_gets_a_final_tool_free_model_response() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let provider = Arc::new(IterationLimitProvider {
        calls: AtomicUsize::new(0),
    });
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("iteration-limit", provider.clone())
            .with_default_model("iteration-limit/test"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": workspace["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "keep reading forever"}))
        .send()
        .await
        .unwrap();

    let events = wait_for_event(
        &client,
        &format!("{base}/threads/{thread_id}/events"),
        |event| event["type"] == "turn.completed",
    )
    .await;
    assert!(events.iter().any(|event| {
        event["type"] == "assistant.message"
            && event["content"]
                .as_str()
                .is_some_and(|text| text.contains("continue to finish"))
    }));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 33);
}

/// Reports a model with a tiny context window; large usage on turn 1 forces
/// compaction at the start of turn 2. Call sequence: turn 1, summarization,
/// turn 2.
struct CompactingProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for CompactingProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "scripted/tiny-model".into(),
            display_name: "Tiny".into(),
            context_window: 1000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({}),
        }]
    }

    async fn stream_chat(
        &self,
        _model: &str,
        messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events: Vec<Result<ProviderEvent, ProviderError>> = match call {
            // Turn 1: report usage near the 1000-token window.
            0 => vec![
                Ok(ProviderEvent::TextDelta("First answer.".into())),
                Ok(ProviderEvent::Completed {
                    usage: Usage {
                        input_tokens: 900,
                        output_tokens: 5,
                        ..Default::default()
                    },
                }),
            ],
            // Compaction summarization request.
            1 => vec![
                Ok(ProviderEvent::TextDelta(
                    "Summary of everything so far.".into(),
                )),
                Ok(ProviderEvent::Completed {
                    usage: Usage::default(),
                }),
            ],
            // Turn 2 proper: history must be the compacted summary + the new
            // user message.
            _ => {
                assert!(
                    messages.iter().any(|m| matches!(
                        m,
                        Message::User(text) if text.contains("Summary of everything so far.")
                    )),
                    "turn 2 should run against the compacted transcript"
                );
                vec![
                    Ok(ProviderEvent::TextDelta("Second answer.".into())),
                    Ok(ProviderEvent::Completed {
                        usage: Usage::default(),
                    }),
                ]
            }
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn compaction_summarizes_transcript_near_context_window() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_config_file(None)
            .with_provider(
                "scripted",
                Arc::new(CompactingProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/tiny-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Compact"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");

    // Turn 1 records 900 input tokens against a 1000-token window.
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "first"}))
        .send()
        .await
        .unwrap();
    wait_for_event(&client, &events_url, |e| e["type"] == "turn.completed").await;

    // Turn 2 must compact before running.
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "second"}))
        .send()
        .await
        .unwrap();
    let events = wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 2
    })
    .await;
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "thread.compaction_started")
    );
    let completed = events
        .iter()
        .find(|e| e["type"] == "thread.compaction_completed")
        .expect("compaction completes");
    assert!(completed["messages_compacted"].as_u64().unwrap() >= 2);

    // The folded transcript retains the boundary after completion instead
    // of reducing it to the snapshot's transient `compacting` flag.
    let view: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/view"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let marker = view["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "compaction")
        .expect("completed compaction remains in the folded transcript");
    assert_eq!(marker["turn"], 2);
    assert_eq!(marker["state"]["state"], "completed");
    assert_eq!(
        marker["state"]["messages_compacted"],
        completed["messages_compacted"]
    );
}

#[tokio::test]
async fn session_and_thread_updates_and_provider_config() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let config_file = tmp.path().join("config.toml");
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_config_file(Some(config_file.clone()))
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    // Branch listing knows the repo's branches and HEAD.
    let branches: serde_json::Value = client
        .get(format!("{base}/workspaces/{ws_id}/branches"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(branches["head"], "main");
    assert!(
        branches["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "main")
    );

    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws_id, "title": "Original"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap();
    assert_eq!(session["archived"], false);

    // The web/PWA bootstrap projection is an atomic snapshot paired with a
    // resume cursor after its transactionally emitted replacement event.
    let summaries: serde_json::Value = client
        .get(format!("{base}/session-summaries"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let summary = &summaries["summaries"][0];
    assert_eq!(summary["session_id"], session_id);
    assert_eq!(summary["workspace_id"], ws_id);
    assert_eq!(summary["archived"], false);
    assert!(summaries["cursor"].as_u64().unwrap() > summary["latest_cursor"].as_u64().unwrap());

    // Rename + archive via PATCH.
    let updated: serde_json::Value = client
        .patch(format!("{base}/sessions/{session_id}"))
        .json(&serde_json::json!({"title": "Renamed", "archived": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["title"], "Renamed");
    assert_eq!(updated["archived"], true);

    // Background title generation uses a persistence-boundary compare-and-set:
    // the matching provisional title can be replaced once, while a stale
    // result cannot overwrite the newer title.
    let generated: serde_json::Value = client
        .patch(format!("{base}/sessions/{session_id}"))
        .json(&serde_json::json!({
            "title": "Generated",
            "expected_title": "Renamed"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(generated["title"], "Generated");
    let stale = client
        .patch(format!("{base}/sessions/{session_id}"))
        .json(&serde_json::json!({
            "title": "Stale generated title",
            "expected_title": "Renamed"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let persisted: serde_json::Value = client
        .get(format!("{base}/sessions/{session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(persisted["title"], "Generated");
    let summaries: serde_json::Value = client
        .get(format!("{base}/session-summaries"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(summaries["summaries"][0]["archived"], true);

    // Thread creation succeeds even with an unconfigured model (validation
    // is deferred to send time), then PATCH switches mode/model.
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session_id,
            "title": "  Review   the parser\nedge cases  ",
            "model": "nonexistent/model"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    assert_eq!(thread["model"], "nonexistent/model");
    assert_eq!(thread["title"], "Review the parser edge cases");

    let thread_statuses: serde_json::Value = client
        .get(format!("{base}/thread-statuses?session_id={session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(thread_statuses[0]["thread_id"], thread_id);
    assert_eq!(thread_statuses[0]["session_id"], session_id);
    assert_eq!(thread_statuses[0]["active"], false);
    assert_eq!(thread_statuses[0]["attention"], "none");
    assert_eq!(thread_statuses[0]["outcome"], "idle");

    let patched: serde_json::Value = client
        .patch(format!("{base}/threads/{thread_id}"))
        .json(&serde_json::json!({"mode": "plan", "model": "scripted/test-model"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["mode"], "plan");
    assert_eq!(patched["model"], "scripted/test-model");

    // Known-provider presets: models.dev catalog data plus Trouve's local and
    // subscription-CLI integrations.
    let known: serde_json::Value = client
        .get(format!("{base}/providers/known"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let known = known.as_array().unwrap();
    assert!(known.len() >= 145);
    let openrouter = known
        .iter()
        .find(|k| k["id"] == "openrouter")
        .expect("openrouter preset");
    assert_eq!(openrouter["kind"], "openai-compat");
    assert_eq!(openrouter["base_url"], "https://openrouter.ai/api/v1");
    assert_eq!(openrouter["api_key_env"], "OPENROUTER_API_KEY");
    assert_eq!(openrouter["auth"], "api-key");
    assert_eq!(openrouter["category"], "api");
    assert!(known.iter().any(|k| k["id"] == "anthropic"));
    let ollama = known
        .iter()
        .find(|k| k["id"] == "ollama")
        .expect("ollama preset");
    assert_eq!(ollama["category"], "local");
    let kimi_code = known
        .iter()
        .find(|k| k["id"] == "kimi-code")
        .expect("kimi-code preset");
    assert_eq!(kimi_code["category"], "subscription");
    assert_eq!(kimi_code["auth"], "api-key");
    assert_eq!(kimi_code["base_url"], "https://api.kimi.com/coding/v1");
    // Policy invariant: we never ship OAuth presets that piggyback on
    // vendors' own CLI client registrations (account-ban risk). OAuth is
    // manual-config only; subscriptions go through vendor CLIs instead.
    assert!(
        known.iter().all(|k| k["auth"] != "oauth"),
        "no subscription presets in the shipped catalog"
    );
    // Subscription agent backends: auth lives in the vendor CLI.
    for (id, kind) in [
        ("codex", "codex-app-server"),
        ("cursor", "cursor-cli"),
        ("claude-code", "claude-cli"),
    ] {
        let preset = known
            .iter()
            .find(|k| k["id"] == id)
            .unwrap_or_else(|| panic!("{id} preset"));
        assert_eq!(preset["kind"], kind);
        assert_eq!(preset["auth"], "cli");
        assert_eq!(preset["category"], "subscription");
        assert!(!preset["experimental"].as_bool().unwrap_or(false));
    }
    // Cursor also ships a key-authenticated preset (usage-based billing)
    // alongside the subscription one; same cursor-cli backend.
    let cursor_api = known
        .iter()
        .find(|k| k["id"] == "cursor-api")
        .expect("cursor-api preset");
    assert_eq!(cursor_api["kind"], "cursor-cli");
    assert_eq!(cursor_api["auth"], "api-key");
    assert_eq!(cursor_api["category"], "api");
    assert_eq!(cursor_api["api_key_env"], "CURSOR_API_KEY");
    assert!(known.iter().all(|k| k["id"] != "codex-api"));

    // Login endpoints exist but reject providers without manual OAuth config.
    let resp = client
        .post(format!("{base}/providers/openrouter/login"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let status: serde_json::Value = client
        .get(format!("{base}/providers/openrouter/login"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["status"], "none");

    // Provider CRUD: upsert writes the config file, delete removes it.
    let provider: serde_json::Value = client
        .put(format!("{base}/providers/openrouter"))
        .json(&serde_json::json!({
            "kind": "openai-compat",
            "base_url": "https://openrouter.ai/api/v1"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(provider["id"], "openrouter");
    assert_eq!(provider["category"], "api");
    assert!(config_file.exists());
    let config_text = std::fs::read_to_string(&config_file).unwrap();
    assert!(config_text.contains("openrouter"));
    // Upserting a known preset auto-fills the conventional key env var.
    assert!(config_text.contains("OPENROUTER_API_KEY"));

    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        providers["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "openrouter")
    );

    // Global defaults persist together in one config-file update.
    let resp = client
        .put(format!("{base}/config/defaults"))
        .json(&serde_json::json!({
            "model": "scripted/test-model",
            "default_thinking_level": "medium",
            "permission_mode": "yolo"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers["default_model"], "scripted/test-model");
    assert_eq!(providers["default_thinking_level"], "medium");
    assert_eq!(providers["default_permission_mode"], "yolo");
    let saved = Config::load_from(&config_file);
    assert_eq!(saved.default_model.as_deref(), Some("scripted/test-model"));
    assert_eq!(saved.default_thinking_level.as_deref(), Some("medium"));
    assert_eq!(
        saved.default_permission_mode,
        Some(trouve_protocol::PermissionMode::Yolo)
    );
    let resp = client
        .put(format!("{base}/config/defaults"))
        .json(&serde_json::json!({
            "model": "scripted/test-model",
            "default_thinking_level": null,
            "permission_mode": "ask"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let saved = Config::load_from(&config_file);
    assert_eq!(saved.default_thinking_level, None);
    assert_eq!(
        saved.default_permission_mode,
        Some(trouve_protocol::PermissionMode::Ask)
    );

    let resp = client
        .delete(format!("{base}/providers/openrouter"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !providers["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "openrouter")
    );

    // Deletion commits its relational cleanup and durable summary tombstone
    // together, so a following bootstrap cannot resurrect the session.
    let response = client
        .delete(format!("{base}/sessions/{session_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let summaries: serde_json::Value = client
        .get(format!("{base}/session-summaries"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(summaries["summaries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn read_only_mode_denies_mutations_without_prompting() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Plan session"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"], "mode": "plan"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "write hello"}))
        .send()
        .await
        .unwrap();

    let events_url = format!("{base}/threads/{thread_id}/events");
    let client2 = client.clone();
    let events = wait_for_event(&client2, &events_url, |e| e["type"] == "turn.completed").await;
    // write_file isn't in plan mode's tool list: denied, no approval prompt.
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "tool.completed" && e["status"] == "denied")
    );
    assert!(!events.iter().any(|e| e["type"] == "approval.requested"));
    let worktree = session["worktree_path"].as_str().unwrap();
    assert!(!Path::new(worktree).join("hello.txt").exists());
}

/// Turn 1: asks the user two questions via the engine-served ask_question
/// tool; turn 2: records the tool result it was fed and finishes.
struct QuestionProvider {
    calls: AtomicUsize,
    fed_back: std::sync::Mutex<Vec<Message>>,
}

#[async_trait::async_trait]
impl Provider for QuestionProvider {
    fn id(&self) -> &str {
        "questions"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        messages: &[Message],
        tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events: Vec<Result<ProviderEvent, ProviderError>> = match call {
            0 => {
                // The engine always offers the ask_question tool.
                assert!(
                    tools.iter().any(|t| t.name == "ask_question"),
                    "ask_question must be in the tool specs"
                );
                vec![
                    Ok(ProviderEvent::ToolCall(ToolCallRequest {
                        id: "q_call_1".into(),
                        name: "ask_question".into(),
                        // Bare-string options exercise id synthesis.
                        arguments: serde_json::json!({
                            "title": "Preferences",
                            "questions": [
                                {"prompt": "Favorite color?", "options": ["Red", "Blue"]},
                                {"prompt": "Fruits?", "options": ["Apple", "Banana"],
                                 "allow_multiple": true},
                            ],
                        }),
                    })),
                    Ok(ProviderEvent::Completed {
                        usage: Usage::default(),
                    }),
                ]
            }
            _ => {
                *self.fed_back.lock().unwrap() = messages.to_vec();
                vec![
                    Ok(ProviderEvent::TextDelta("Noted.".into())),
                    Ok(ProviderEvent::Completed {
                        usage: Usage::default(),
                    }),
                ]
            }
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn ask_question_tool_round_trips_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let provider = Arc::new(QuestionProvider {
        calls: AtomicUsize::new(0),
        fed_back: std::sync::Mutex::new(Vec::new()),
    });
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("questions", provider.clone())
            .with_default_model("questions/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Question session"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "ask me things"}))
        .send()
        .await
        .unwrap();

    // The turn blocks on question.requested (ungated: no approval events).
    let events_url = format!("{base}/threads/{thread_id}/events");
    let events = wait_for_event(&client, &events_url, |e| e["type"] == "question.requested").await;
    let req = events
        .iter()
        .find(|e| e["type"] == "question.requested")
        .unwrap();
    assert_eq!(req["title"], "Preferences");
    let questions = req["questions"].as_array().unwrap();
    assert_eq!(questions.len(), 2);
    // Ids were synthesized for the bare-string options.
    assert_eq!(questions[0]["id"], "q1");
    assert_eq!(
        questions[0]["options"][0],
        serde_json::json!({"id": "opt1", "label": "Red"})
    );
    assert_eq!(questions[1]["allow_multiple"], true);
    assert!(!events.iter().any(|e| e["type"] == "approval.requested"));
    let request_id = req["request_id"].as_str().unwrap();

    // Unknown request ids are a 404.
    let resp = client
        .post(format!("{base}/questions"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "request_id": "bogus",
            "answers": [],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Answer: single choice + a multi-choice with an "Other" free-form.
    let resp = client
        .post(format!("{base}/questions"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "request_id": request_id,
            "answers": [
                {"question_id": "q1", "selected_option_ids": ["opt1"]},
                {"question_id": "q2", "selected_option_ids": ["opt2"],
                 "other_text": "mango"},
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let events = wait_for_event(&client, &events_url, |e| e["type"] == "turn.completed").await;
    let resolved = events
        .iter()
        .find(|e| e["type"] == "question.resolved")
        .unwrap();
    assert_eq!(resolved["answers"][0]["selected_option_ids"][0], "opt1");
    assert_eq!(resolved["answers"][1]["other_text"], "mango");

    // The model got the answers back as labels (ids were synthetic).
    let fed = provider.fed_back.lock().unwrap().clone();
    let result = fed
        .iter()
        .find_map(|m| match m {
            Message::ToolResult {
                call_id, content, ..
            } if call_id == "q_call_1" => Some(content),
            _ => None,
        })
        .expect("ask_question result fed back to the model");
    let result: serde_json::Value = serde_json::from_str(result).unwrap();
    assert_eq!(result["status"], "answered");
    assert_eq!(result["answers"][0]["selected"][0], "Red");
    assert_eq!(result["answers"][1]["selected"][0], "Banana");
    assert_eq!(result["answers"][1]["other"], "mango");
}

// --- external agent backends -------------------------------------------------

/// Minimal `AgentBackend` for handoff tests: records the (resume session,
/// prompt) each turn arrives with, replies with fixed text, and issues one
/// stable vendor session id per instance.
struct HandoffBackend {
    name: &'static str,
    turns: std::sync::Mutex<Vec<(Option<String>, String)>>,
}

impl HandoffBackend {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            turns: std::sync::Mutex::new(Vec::new()),
        }
    }
}

/// Backend whose turns remain open until the test releases them. Entering
/// `run_turn` proves that scheduler capacity and the session lifecycle lease
/// have both been acquired.
struct ConcurrentBackend {
    started: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}

impl ConcurrentBackend {
    fn new() -> Self {
        Self {
            started: tokio::sync::Semaphore::new(0),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[async_trait::async_trait]
impl trouve_agents::AgentBackend for ConcurrentBackend {
    fn id(&self) -> &str {
        "concurrent"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "concurrent/m".into(),
            display_name: "Concurrent".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn status(&self) -> trouve_agents::BackendStatus {
        trouve_agents::BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    async fn start_login(
        &self,
    ) -> Result<trouve_agents::BackendLogin, trouve_agents::BackendError> {
        Err(trouve_agents::BackendError::Auth("not needed".into()))
    }

    async fn run_turn(
        &self,
        _turn: trouve_agents::BackendTurn,
    ) -> Result<trouve_agents::BackendEventStream, trouve_agents::BackendError> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("test release semaphore remains open")
            .forget();
        let events = vec![Ok(trouve_agents::BackendEvent::Completed {
            usage: Usage::default(),
        })];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[async_trait::async_trait]
impl trouve_agents::AgentBackend for HandoffBackend {
    fn id(&self) -> &str {
        self.name
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: format!("{}/m", self.name),
            display_name: self.name.into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn status(&self) -> trouve_agents::BackendStatus {
        trouve_agents::BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    async fn start_login(
        &self,
    ) -> Result<trouve_agents::BackendLogin, trouve_agents::BackendError> {
        Err(trouve_agents::BackendError::Auth("not needed".into()))
    }

    async fn run_turn(
        &self,
        turn: trouve_agents::BackendTurn,
    ) -> Result<trouve_agents::BackendEventStream, trouve_agents::BackendError> {
        let fresh = turn.session.is_none();
        self.turns
            .lock()
            .unwrap()
            .push((turn.session.clone(), turn.prompt.clone()));
        let name = self.name;
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            use trouve_agents::BackendEvent as E;
            if fresh {
                let _ = tx
                    .send(Ok(E::SessionStarted {
                        session_id: format!("{name}-sess"),
                    }))
                    .await;
            }
            let _ = tx
                .send(Ok(E::TextDelta(format!("reply from {name}"))))
                .await;
            let _ = tx
                .send(Ok(E::Completed {
                    usage: Usage::default(),
                }))
                .await;
        });
        let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn code_turns_in_two_threads_of_one_session_enter_the_backend_concurrently() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let backend = Arc::new(ConcurrentBackend::new());
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_backend("concurrent", backend.clone())
            .with_default_model("concurrent/m"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": workspace["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let mut thread_ids = Vec::new();
    for _ in 0..2 {
        let thread: serde_json::Value = client
            .post(format!("{base}/threads"))
            .json(&serde_json::json!({"session_id": session["id"]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        thread_ids.push(thread["id"].as_str().unwrap().to_owned());
    }

    for thread_id in &thread_ids {
        let response = client
            .post(format!("{base}/threads/{thread_id}/messages"))
            .json(&serde_json::json!({"content": "work concurrently"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    }

    tokio::time::timeout(Duration::from_secs(5), backend.started.acquire_many(2))
        .await
        .expect("both same-session code turns should reach the backend")
        .expect("test started semaphore remains open")
        .forget();
    backend.release.add_permits(2);

    for thread_id in thread_ids {
        let events = wait_for_event(
            &client,
            &format!("{base}/threads/{thread_id}/events"),
            |event| event["type"] == "turn.completed",
        )
        .await;
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "turn.capacity_acquired")
        );
    }
}

/// Swapping models mid-thread: each vendor keeps its own resumable
/// session, a vendor joining a thread with history gets a handoff digest
/// of the prior conversation prepended to its first prompt, and switching
/// back to the first vendor resumes its session digest-free.
#[tokio::test]
async fn model_swap_hands_off_history_and_keeps_vendor_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let agent_a = Arc::new(HandoffBackend::new("agent-a"));
    let agent_b = Arc::new(HandoffBackend::new("agent-b"));
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_backend("agent-a", agent_a.clone())
            .with_backend("agent-b", agent_b.clone())
            .with_default_model("agent-a/m"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Swap"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");

    let send = |content: &str| {
        let client = client.clone();
        let url = format!("{base}/threads/{thread_id}/messages");
        let body = serde_json::json!({"content": content});
        async move {
            client.post(url).json(&body).send().await.unwrap();
        }
    };
    let set_model = |model: &str| {
        let client = client.clone();
        let url = format!("{base}/threads/{thread_id}");
        let body = serde_json::json!({"model": model});
        async move {
            let resp = client.patch(url).json(&body).send().await.unwrap();
            assert_eq!(resp.status(), 200);
        }
    };

    // Turn 1 on agent-a: a fresh thread — no session, no digest.
    send("first message").await;
    wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 1
    })
    .await;
    {
        let turns = agent_a.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].0, None);
        assert_eq!(turns[0].1, "first message");
    }

    // Turn 2 on agent-b: no vendor session here yet, so its first prompt
    // carries a digest of the conversation agent-a had.
    set_model("agent-b/m").await;
    send("second message").await;
    wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 2
    })
    .await;
    {
        let turns = agent_b.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].0, None);
        let prompt = &turns[0].1;
        assert!(prompt.starts_with("[Handoff:"), "digest missing: {prompt}");
        assert!(prompt.contains("first message"));
        assert!(prompt.contains("reply from agent-a"));
        assert!(prompt.ends_with("second message"));
    }

    // Turn 3 back on agent-a: its vendor session survived agent-b's turn
    // (per-backend keying), and it gets caught up on just the turn it
    // missed — not the history its own session already carries.
    set_model("agent-a/m").await;
    send("third message").await;
    wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 3
    })
    .await;
    {
        let turns = agent_a.turns.lock().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].0.as_deref(), Some("agent-a-sess"));
        let prompt = &turns[1].1;
        assert!(
            prompt.starts_with("[Handoff: since your last turn"),
            "catch-up digest missing: {prompt}"
        );
        assert!(prompt.contains("second message"));
        assert!(prompt.contains("reply from agent-b"));
        assert!(
            !prompt.contains("first message"),
            "already-seen history repeated"
        );
        assert!(prompt.ends_with("third message"));
    }

    // agent-b's session survived too, and its catch-up covers only
    // agent-a's interleaved turn.
    set_model("agent-b/m").await;
    send("fourth message").await;
    wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 4
    })
    .await;
    {
        let turns = agent_b.turns.lock().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].0.as_deref(), Some("agent-b-sess"));
        let prompt = &turns[1].1;
        assert!(prompt.starts_with("[Handoff: since your last turn"));
        assert!(prompt.contains("third message"));
        assert!(
            !prompt.contains("first message"),
            "already-seen history repeated"
        );
        assert!(prompt.ends_with("fourth message"));
    }
}

/// Holds a vendor turn open until its native steering method is called, then
/// completes. This exercises the real HTTP endpoint, engine turn registry,
/// backend capability, durable event ordering, and folded thread view.
struct SteerableBackend {
    steers: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    tool_release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl SteerableBackend {
    fn new() -> Self {
        Self {
            steers: std::sync::Mutex::new(Vec::new()),
            release: tokio::sync::Mutex::new(None),
            tool_release: tokio::sync::Mutex::new(None),
        }
    }

    async fn release_tool(&self) {
        let release = self
            .tool_release
            .lock()
            .await
            .take()
            .expect("deferred fake tool has a release signal");
        let _ = release.send(());
    }
}

#[async_trait::async_trait]
impl trouve_agents::AgentBackend for SteerableBackend {
    fn id(&self) -> &str {
        "steerable-agent"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "steerable-agent/model".into(),
            display_name: "Steerable Agent".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn status(&self) -> trouve_agents::BackendStatus {
        trouve_agents::BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    fn supports_steering(&self) -> bool {
        true
    }

    async fn steer_turn(
        &self,
        steer: trouve_agents::BackendSteer,
    ) -> Result<(), trouve_agents::BackendError> {
        self.steers.lock().unwrap().push((
            steer.session,
            steer.prompt,
            steer
                .attachments
                .into_iter()
                .map(|attachment| {
                    attachment
                        .local_path
                        .expect("steered attachment has a verified local path")
                        .display()
                        .to_string()
                })
                .collect(),
        ));
        let release =
            self.release.lock().await.take().ok_or_else(|| {
                trouve_agents::BackendError::Protocol("no active fake turn".into())
            })?;
        let _ = release.send(());
        Ok(())
    }

    async fn start_login(
        &self,
    ) -> Result<trouve_agents::BackendLogin, trouve_agents::BackendError> {
        Err(trouve_agents::BackendError::Auth("not needed".into()))
    }

    async fn run_turn(
        &self,
        turn: trouve_agents::BackendTurn,
    ) -> Result<trouve_agents::BackendEventStream, trouve_agents::BackendError> {
        let defer_attachment_lane = turn.prompt == "Defer attachment steering.";
        let cancel_deferred_steering = turn.prompt == "Cancel deferred steering.";
        let hold_mutation_lane = turn.prompt == "Hold the mutation lane."
            || defer_attachment_lane
            || cancel_deferred_steering;
        let held_call_id = if defer_attachment_lane {
            "deferred-mutation"
        } else if cancel_deferred_steering {
            "cancelled-mutation"
        } else {
            "held-mutation"
        };
        let cancel = turn.cancel.clone();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self.release.lock().await = Some(release_tx);
        let (tool_release_tx, tool_release_rx) = tokio::sync::oneshot::channel();
        if defer_attachment_lane {
            *self.tool_release.lock().await = Some(tool_release_tx);
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            use trouve_agents::BackendEvent as E;
            let _ = tx
                .send(Ok(E::SessionStarted {
                    session_id: "steerable-vendor-session".into(),
                }))
                .await;
            let _ = tx
                .send(Ok(E::ThinkingDelta("Initial direction.".into())))
                .await;
            if hold_mutation_lane {
                let (approved_tx, approved_rx) = tokio::sync::oneshot::channel();
                let _ = tx
                    .send(Ok(E::ApprovalNeeded {
                        call_id: held_call_id.into(),
                        tool: "commandExecution".into(),
                        args: serde_json::json!({"command": "hold mutation lane"}),
                        responder: approved_tx,
                    }))
                    .await;
                if !approved_rx.await.unwrap_or(false) {
                    return;
                }
                let _ = tx
                    .send(Ok(E::ToolStarted {
                        call_id: held_call_id.into(),
                        tool: "commandExecution".into(),
                        args: serde_json::json!({"command": "hold mutation lane"}),
                    }))
                    .await;
            }
            if defer_attachment_lane {
                if tool_release_rx.await.is_err() {
                    return;
                }
                let _ = tx
                    .send(Ok(E::ToolCompleted {
                        call_id: held_call_id.into(),
                        ok: true,
                        result: serde_json::json!({"exitCode": 0}),
                    }))
                    .await;
            }
            let released = if cancel_deferred_steering {
                tokio::select! {
                    _ = cancel.cancelled() => false,
                    released = release_rx => released.is_ok(),
                }
            } else {
                release_rx.await.is_ok()
            };
            if !released {
                return;
            }
            if hold_mutation_lane && !defer_attachment_lane {
                let _ = tx
                    .send(Ok(E::ToolCompleted {
                        call_id: held_call_id.into(),
                        ok: true,
                        result: serde_json::json!({"exitCode": 0}),
                    }))
                    .await;
            }
            let _ = tx.send(Ok(E::ThinkingCompleted)).await;
            let _ = tx.send(Ok(E::TextDelta("Steering applied.".into()))).await;
            let _ = tx
                .send(Ok(E::Completed {
                    usage: Usage::default(),
                }))
                .await;
        });
        Ok(Box::pin(futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
        })))
    }
}

#[tokio::test]
async fn active_backend_turn_can_be_steered_and_replays_on_its_timeline() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let backend = Arc::new(SteerableBackend::new());
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_backend("steerable-agent", backend.clone())
            .with_default_model("steerable-agent/model"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": workspace["id"], "title": "Steer"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session["id"],
            "permission_mode": "yolo",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");

    let started = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "Begin the implementation."}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let before = wait_for_event(&client, &events_url, |event| {
        event["type"] == "assistant.thinking"
    })
    .await;
    assert!(
        before
            .iter()
            .any(|event| { event["type"] == "turn.started" && event["supports_steering"] == true })
    );

    let steered = client
        .post(format!("{base}/threads/{thread_id}/steer"))
        .json(&serde_json::json!({
            "content": "Prioritize the layout regression.",
            "attachments": [{
                "name": "reference.png",
                "mime": "image/png",
                "data": "iVBORw0KGgo=",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(steered.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(
        steered.json::<serde_json::Value>().await.unwrap()["turn"],
        1
    );

    let events = wait_for_event(&client, &events_url, |event| {
        event["type"] == "turn.completed"
    })
    .await;
    let thinking_index = events
        .iter()
        .position(|event| event["type"] == "assistant.thinking")
        .unwrap();
    let steering_index = events
        .iter()
        .position(|event| event["type"] == "turn.steered")
        .unwrap();
    let response_index = events
        .iter()
        .position(|event| event["type"] == "assistant.delta")
        .unwrap();
    assert!(thinking_index < steering_index && steering_index < response_index);
    let steering = &events[steering_index];
    assert_eq!(steering["content"], "Prioritize the layout regression.");
    assert_eq!(steering["attachments"][0]["name"], "reference.png");

    let view: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/view"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(view["turn_steerable"]["1"], true);
    assert!(view["items"].as_array().unwrap().iter().any(|item| {
        item["kind"] == "steered" && item["content"] == "Prioritize the layout regression."
    }));

    {
        let received = backend.steers.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].0, "steerable-vendor-session");
        assert_eq!(received[0].1, "Prioritize the layout regression.");
        assert_eq!(received[0].2.len(), 1);
        assert!(Path::new(&received[0].2[0]).exists());
    }

    let started = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "Hold the mutation lane."}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "tool.started" && event["call_id"] == "held-mutation"
    })
    .await;

    let steered = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client
            .post(format!("{base}/threads/{thread_id}/steer"))
            .json(&serde_json::json!({"content": "Continue without waiting for the tool."}))
            .send(),
    )
    .await
    .expect("text-only steering waited for the session mutation lane")
    .unwrap();
    assert_eq!(steered.status(), reqwest::StatusCode::ACCEPTED);

    wait_for_event(&client, &events_url, |event| {
        event["type"] == "turn.completed" && event["turn"] == 2
    })
    .await;
    {
        let received = backend.steers.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[1].1, "Continue without waiting for the tool.");
        assert!(received[1].2.is_empty());
    }

    let started = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "Defer attachment steering."}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "tool.started" && event["call_id"] == "deferred-mutation"
    })
    .await;
    let steer_client = client.clone();
    let steer_url = format!("{base}/threads/{thread_id}/steer");
    let pending_steer = tokio::spawn(async move {
        steer_client
            .post(steer_url)
            .json(&serde_json::json!({
                "content": "Use the deferred reference.",
                "attachments": [{
                    "name": "deferred.png",
                    "mime": "image/png",
                    "data": "iVBORw0KGgo=",
                }],
            }))
            .send()
            .await
    });
    let parked = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            engine.wait_for_steer_mutation_lane(thread_id),
            engine.wait_for_steer_mutation_lane(thread_id),
        )
    })
    .await
    .expect("attachment steering never parked on the held mutation lane");
    assert_eq!(parked, (true, true));
    assert!(!pending_steer.is_finished());
    backend.release_tool().await;
    let steered = tokio::time::timeout(std::time::Duration::from_secs(10), pending_steer)
        .await
        .expect("attachment steering did not resume after the mutation lane released")
        .unwrap()
        .unwrap();
    assert_eq!(steered.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "turn.completed" && event["turn"] == 3
    })
    .await;
    {
        let received = backend.steers.lock().unwrap();
        assert_eq!(received.len(), 3);
        assert_eq!(received[2].1, "Use the deferred reference.");
        assert_eq!(received[2].2.len(), 1);
    }

    let started = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "Cancel deferred steering."}))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "tool.started" && event["call_id"] == "cancelled-mutation"
    })
    .await;
    let steer_client = client.clone();
    let steer_url = format!("{base}/threads/{thread_id}/steer");
    let pending_steer = tokio::spawn(async move {
        steer_client
            .post(steer_url)
            .json(&serde_json::json!({
                "content": "This steer should be cancelled.",
                "attachments": [{
                    "name": "cancelled.png",
                    "mime": "image/png",
                    "data": "iVBORw0KGgo=",
                }],
            }))
            .send()
            .await
    });
    let parked = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        engine.wait_for_steer_mutation_lane(thread_id),
    )
    .await
    .expect("cancelled attachment steering never parked on the mutation lane");
    assert!(parked);
    assert!(!pending_steer.is_finished());
    let cancelled = client
        .post(format!("{base}/threads/{thread_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert!(cancelled.status().is_success());
    let steer_response = tokio::time::timeout(std::time::Duration::from_secs(10), pending_steer)
        .await
        .expect("cancelled deferred steering request did not finish")
        .unwrap()
        .unwrap();
    assert_eq!(steer_response.status(), reqwest::StatusCode::CONFLICT);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "turn.cancelled" && event["turn"] == 4
    })
    .await;

    // A new attachment-bearing turn must acquire the same session lane,
    // proving cancellation dropped both the deferred waiter and any permit.
    let restarted = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({
            "content": "Start after cancellation.",
            "attachments": [{
                "name": "after-cancel.png",
                "mime": "image/png",
                "data": "iVBORw0KGgo=",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(restarted.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "assistant.thinking" && event["turn"] == 5
    })
    .await;
    let release = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client
            .post(format!("{base}/threads/{thread_id}/steer"))
            .json(&serde_json::json!({
                "content": "Finish the restarted turn.",
                "attachments": [{
                    "name": "after-cancel-steer.png",
                    "mime": "image/png",
                    "data": "iVBORw0KGgo=",
                }],
            }))
            .send(),
    )
    .await
    .expect("attachment steering did not finish after cancellation")
    .unwrap();
    assert_eq!(release.status(), reqwest::StatusCode::ACCEPTED);
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "turn.completed" && event["turn"] == 5
    })
    .await;
    {
        let received = backend.steers.lock().unwrap();
        assert_eq!(received.len(), 4);
        assert_eq!(received[3].1, "Finish the restarted turn.");
        assert_eq!(received[3].2.len(), 1);
        assert!(Path::new(&received[3].2[0]).exists());
    }
}

/// Scripted `AgentBackend`: every turn asks for approval of one "command",
/// writes a file to the worktree when approved, and completes with usage.
/// Records the vendor session id it was resumed with, per turn.
struct ScriptedBackend {
    sessions_seen: std::sync::Mutex<Vec<Option<String>>>,
    bridge_urls_seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl ScriptedBackend {
    fn new() -> Self {
        Self {
            sessions_seen: std::sync::Mutex::new(Vec::new()),
            bridge_urls_seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl trouve_agents::AgentBackend for ScriptedBackend {
    fn id(&self) -> &str {
        "fake-agent"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "fake-agent/agent-model".into(),
            display_name: "Fake Agent".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn status(&self) -> trouve_agents::BackendStatus {
        trouve_agents::BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    async fn start_login(
        &self,
    ) -> Result<trouve_agents::BackendLogin, trouve_agents::BackendError> {
        Err(trouve_agents::BackendError::Auth("not needed".into()))
    }

    async fn run_turn(
        &self,
        turn: trouve_agents::BackendTurn,
    ) -> Result<trouve_agents::BackendEventStream, trouve_agents::BackendError> {
        self.sessions_seen
            .lock()
            .unwrap()
            .push(turn.session.clone());
        self.bridge_urls_seen
            .lock()
            .unwrap()
            .push(turn.mcp_bridge.as_ref().map(|bridge| bridge.url.clone()));
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let fresh = turn.session.is_none();
        let worktree = turn.worktree.clone();
        tokio::spawn(async move {
            use trouve_agents::BackendEvent as E;
            if fresh {
                let _ = tx
                    .send(Ok(E::SessionStarted {
                        session_id: "vendor-sess-1".into(),
                    }))
                    .await;
            }
            let _ = tx.send(Ok(E::CompactionStarted)).await;
            let _ = tx.send(Ok(E::CompactionCompleted)).await;
            let _ = tx
                .send(Ok(E::UsageUpdated {
                    usage: Usage {
                        input_tokens: 12,
                        output_tokens: 3,
                        cached_input_tokens: 28,
                        context_input_tokens: Some(40),
                        context_window: Some(100_000),
                        ..Default::default()
                    },
                }))
                .await;
            let _ = tx.send(Ok(E::TextDelta("Working on it. ".into()))).await;
            let (ok_tx, ok_rx) = tokio::sync::oneshot::channel();
            let _ = tx
                .send(Ok(E::ApprovalNeeded {
                    call_id: "vendor-call-1".into(),
                    tool: "commandExecution".into(),
                    args: serde_json::json!({"command": "touch agent.txt"}),
                    responder: ok_tx,
                }))
                .await;
            let approved = ok_rx.await.unwrap_or(false);
            if approved {
                std::fs::write(worktree.join("agent.txt"), "from agent\n").unwrap();
                let _ = tx
                    .send(Ok(E::ToolCompleted {
                        call_id: "vendor-call-1".into(),
                        ok: true,
                        result: serde_json::json!({"exitCode": 0}),
                    }))
                    .await;
            } else {
                let _ = tx
                    .send(Ok(E::ToolCompleted {
                        call_id: "vendor-call-1".into(),
                        ok: false,
                        result: serde_json::json!({"error": "declined"}),
                    }))
                    .await;
            }
            let _ = tx.send(Ok(E::TextDelta("Done.".into()))).await;
            let _ = tx
                .send(Ok(E::Completed {
                    usage: Usage {
                        input_tokens: 40,
                        output_tokens: 9,
                        context_input_tokens: Some(40),
                        context_window: Some(100_000),
                        ..Default::default()
                    },
                }))
                .await;
        });
        let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn backend_turns_bridge_approvals_resume_sessions_and_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let backend = Arc::new(ScriptedBackend::new());
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let mut config = Config::default();
    config.providers.insert(
        "claude-code".into(),
        trouve_core::config::ProviderConfig {
            command: Some("/definitely/not/a/claude-test-binary".into()),
            ..Default::default()
        },
    );
    config.providers.insert(
        "fake-agent".into(),
        trouve_core::config::ProviderConfig {
            kind: "claude-cli".into(),
            command: Some("/definitely/not/a/fake-agent-test-binary".into()),
            ..Default::default()
        },
    );
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &config)
            .with_config_dir(None)
            .with_backend("fake-agent", backend.clone())
            .with_default_model("fake-agent/agent-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    engine.set_base_url(&format!("http://{addr}"));
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    // Backend models are listed alongside provider models.
    let models: serde_json::Value = client
        .get(format!("{base}/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        models
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "fake-agent/agent-model")
    );

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Agent session"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let worktree = session["worktree_path"].as_str().unwrap().to_string();

    // Ask mode (default): the vendor's approval request goes through our
    // approval flow.
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap().to_string();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "make a file"}))
        .send()
        .await
        .unwrap();

    let events_url = format!("{base}/threads/{thread_id}/events");
    let events = wait_for_event(&client, &events_url, |e| e["type"] == "approval.requested").await;
    let call_id = events
        .iter()
        .find(|e| e["type"] == "approval.requested")
        .unwrap()["call_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(call_id, "vendor-call-1");

    // The embedded MCP bridge is scoped to the active vendor turn. While
    // that turn is waiting for approval, it can advertise and execute trouve
    // tools through the engine's policy gate.
    let mcp_url = backend
        .bridge_urls_seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .flatten()
        .expect("active backend turn receives an engine-issued bridge capability");
    let mcp = |body: serde_json::Value| {
        let client = client.clone();
        let url = mcp_url.clone();
        async move {
            let request_id = body["id"].clone();
            let response = client.post(url).json(&body).send().await.unwrap();
            let status = response.status();
            let text = response.text().await.unwrap();
            assert!(
                status.is_success(),
                "MCP bridge request {request_id} returned {status}: {text}"
            );
            serde_json::from_str::<serde_json::Value>(&text)
                .unwrap_or_else(|error| panic!("MCP bridge returned invalid JSON: {error}: {text}"))
        }
    };
    let init = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26"}
    }))
    .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "trouve-bridge");

    let listed = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
    }))
    .await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| spec["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file") && names.contains(&"write_file"));
    assert!(names.contains(&"approval_prompt"));

    let called = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "list_dir", "arguments": {"path": "."}}
    }))
    .await;
    assert_eq!(called["result"]["isError"], false, "{called}");

    // The vendor-side permission shim is also bound to this exact active
    // ticket. An in-worktree write reaches the ordinary approval flow and
    // returns allow after the user approves it.
    let in_worktree_input = serde_json::json!({
        "file_path": Path::new(&worktree)
            .join("bridge-approved.txt")
            .to_string_lossy()
            .to_string(),
    });
    let in_worktree_call = tokio::spawn(mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {
            "name": "approval_prompt",
            "arguments": {"tool_name": "Write", "input": in_worktree_input.clone()}
        }
    })));
    let approval_events = wait_for_event(&client, &events_url, |event| {
        event["type"] == "approval.requested" && event["call_id"] != "vendor-call-1"
    })
    .await;
    let bridge_call_id = approval_events
        .iter()
        .find(|event| event["type"] == "approval.requested" && event["call_id"] != "vendor-call-1")
        .unwrap()["call_id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "call_id": bridge_call_id,
            "decision": "approve",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let allowed = in_worktree_call.await.unwrap();
    let allowed_verdict: serde_json::Value =
        serde_json::from_str(allowed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(allowed_verdict["behavior"], "allow", "{allowed}");
    assert_eq!(allowed_verdict["updatedInput"], in_worktree_input);

    // The same valid ticket cannot authorize a vendor write outside the
    // session worktree, even while another backend approval is still live.
    let outside_input = serde_json::json!({
        "file_path": tmp
            .path()
            .join("outside-agent-write.txt")
            .to_string_lossy()
            .to_string(),
    });
    let denied = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {
            "name": "approval_prompt",
            "arguments": {"tool_name": "Write", "input": outside_input}
        }
    }))
    .await;
    let denied_verdict: serde_json::Value =
        serde_json::from_str(denied["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(denied_verdict["behavior"], "deny", "{denied}");

    let resp = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({
            "thread_id": thread_id,
            "call_id": call_id,
            "decision": "approve",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    let events = wait_for_event(&client, &events_url, |e| e["type"] == "turn.completed").await;
    let completed = events
        .iter()
        .find(|e| e["type"] == "turn.completed")
        .unwrap();
    // The vendor mutated the worktree: same checkpoint flow as native turns.
    assert!(
        completed["checkpoint_id"].is_string(),
        "backend turn must checkpoint"
    );
    assert_eq!(completed["usage"]["input_tokens"], 40);
    assert_eq!(completed["usage"]["context_input_tokens"], 40);
    assert!(events.iter().any(|e| e["type"] == "turn.usage_updated"));
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "thread.compaction_started")
    );
    assert!(
        events.iter().any(|e| {
            e["type"] == "thread.compaction_completed" && e["messages_compacted"] == 0
        })
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "tool.completed" && e["status"] == "ok")
    );
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("agent.txt")).unwrap(),
        "from agent\n"
    );

    // Turn 2 on the same thread resumes the persisted vendor session; yolo
    // permission auto-approves without an approval.requested event.
    let patched = client
        .patch(format!("{base}/threads/{thread_id}"))
        .json(&serde_json::json!({"permission_mode": "yolo"}))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), 200);

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "again"}))
        .send()
        .await
        .unwrap();
    let events = wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 2
    })
    .await;
    assert!(
        !events
            .iter()
            .any(|e| e["type"] == "approval.requested" && e["turn"] == 2),
        "yolo must not prompt"
    );

    let sessions = backend.sessions_seen.lock().unwrap().clone();
    assert_eq!(
        sessions,
        vec![None, Some("vendor-sess-1".to_string())],
        "turn 2 must resume the vendor session persisted in turn 1"
    );
    let bridge_urls = backend.bridge_urls_seen.lock().unwrap().clone();
    assert_eq!(bridge_urls.len(), 2);
    assert_eq!(
        bridge_urls[0], bridge_urls[1],
        "a persistent vendor MCP client must keep one capability URL across resumed turns"
    );

    // Usage from both turns is accounted.
    let usage: serde_json::Value = client
        .get(format!("{base}/threads/{thread_id}/usage"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(usage["turns"], 2);
    assert_eq!(usage["input_tokens"], 80);

    // The persistent capability is dormant at terminal state. Even discovery
    // and the approval shim fail until this same thread starts another turn.
    let response = client
        .post(&mcp_url)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/list", "params": {}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("invalid or stale bridge capability ticket")
    );

    // CLI-kind provider CRUD: upsert reports auth "cli"; login relays the
    // vendor flow (the configured test binary is absent, so it fails with
    // 400).
    let provider: serde_json::Value = client
        .put(format!("{base}/providers/claude-code"))
        .json(&serde_json::json!({"kind": "claude-cli"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(provider["auth"], "cli");
    let resp = client
        .post(format!("{base}/providers/claude-code/login"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

/// Backend whose startup owns an explicit cancellation-cleanup boundary.
/// It does not return from `run_turn` until the test acknowledges cleanup,
/// mirroring a vendor request that must be interrupted before the next turn.
struct CancellationAckBackend {
    entered: Arc<tokio::sync::Semaphore>,
    cleanup_started: Arc<tokio::sync::Semaphore>,
    cleanup_release: Arc<tokio::sync::Semaphore>,
}

impl CancellationAckBackend {
    fn new() -> Self {
        Self {
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            cleanup_started: Arc::new(tokio::sync::Semaphore::new(0)),
            cleanup_release: Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }
}

#[async_trait::async_trait]
impl trouve_agents::AgentBackend for CancellationAckBackend {
    fn id(&self) -> &str {
        "cancellation-ack"
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: "cancellation-ack/model".into(),
            display_name: "Cancellation acknowledgement".into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    fn status(&self) -> trouve_agents::BackendStatus {
        trouve_agents::BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    async fn start_login(
        &self,
    ) -> Result<trouve_agents::BackendLogin, trouve_agents::BackendError> {
        Err(trouve_agents::BackendError::Auth("not needed".into()))
    }

    async fn run_turn(
        &self,
        turn: trouve_agents::BackendTurn,
    ) -> Result<trouve_agents::BackendEventStream, trouve_agents::BackendError> {
        self.entered.add_permits(1);
        turn.cancel.cancelled().await;
        self.cleanup_started.add_permits(1);
        self.cleanup_release
            .clone()
            .acquire_owned()
            .await
            .unwrap()
            .forget();
        Err(trouve_agents::BackendError::Cancelled)
    }
}

#[tokio::test]
async fn cancellation_terminal_event_waits_for_backend_cleanup_acknowledgement() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let backend = Arc::new(CancellationAckBackend::new());
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_backend("cancellation-ack", backend.clone())
            .with_default_model("cancellation-ack/model"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": workspace["id"], "title": "Cancel ack"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "begin"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        backend.entered.clone().acquire_owned(),
    )
    .await
    .expect("backend startup should begin")
    .unwrap()
    .forget();

    let cancelled = client
        .post(format!("{base}/threads/{thread_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancelled.status(), reqwest::StatusCode::NO_CONTENT);
    tokio::time::timeout(
        Duration::from_secs(2),
        backend.cleanup_started.clone().acquire_owned(),
    )
    .await
    .expect("backend should observe cancellation")
    .unwrap()
    .forget();

    let before_ack = store
        .events_after(&Scope::Thread(thread_id.to_string()), 0)
        .unwrap();
    assert!(
        !before_ack
            .iter()
            .any(|event| matches!(event.event, Event::TurnCancelled { .. })),
        "turn.cancelled must not overtake vendor cleanup"
    );

    backend.cleanup_release.add_permits(1);
    let events = wait_for_event(
        &client,
        &format!("{base}/threads/{thread_id}/events"),
        |event| event["type"] == "turn.cancelled",
    )
    .await;
    assert!(events.iter().any(|event| event["type"] == "turn.cancelled"));
    let after_ack = store
        .events_after(&Scope::Thread(thread_id.to_string()), 0)
        .unwrap();
    assert_eq!(
        after_ack
            .iter()
            .filter(|event| {
                matches!(
                    event.event,
                    Event::TurnCancelled { .. }
                        | Event::TurnCompleted { .. }
                        | Event::TurnFailed { .. }
                )
            })
            .count(),
        1,
        "a cancelled turn must have exactly one terminal event"
    );
}

/// Echoes the last user message, but holds each reply until the test grants
/// a semaphore permit — keeps a turn "running" while the queue endpoints
/// are exercised.
struct GatedProvider {
    gate: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "gated"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let gate = self.gate.clone();
        let last = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::User(t) => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let events = futures::stream::once(async move {
            gate.acquire_owned().await.unwrap().forget();
            Ok(ProviderEvent::TextDelta(format!("echo: {last}")))
        })
        .chain(futures::stream::iter(vec![Ok(ProviderEvent::Completed {
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Default::default()
            },
        })]));
        Ok(Box::pin(events))
    }
}

/// A follow-up submitted after the cancel request must start as the next
/// turn even when it reaches the engine before cancellation cleanup releases
/// the active-thread claim.
#[tokio::test]
async fn prompt_submitted_during_cancellation_starts_next_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("gated", Arc::new(GatedProvider { gate: gate.clone() }))
            .with_default_model("gated/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Cancel"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "cancel me"}))
        .send()
        .await
        .unwrap();

    // Wait for the dispatcher to install its token. Once cancellation is
    // accepted, do not yield before sending: this deterministically covers
    // the window where the old dispatcher still owns the thread claim.
    let cancel_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match engine.cancel_turn(thread_id) {
            Ok(()) => break,
            Err(_) if std::time::Instant::now() < cancel_deadline => {
                tokio::task::yield_now().await;
            }
            Err(error) => panic!("turn never became cancellable: {error}"),
        }
    }
    let accepted = engine
        .send_message(thread_id, "replacement".into(), Vec::new())
        .unwrap();
    assert!(accepted.queued, "the cancelling turn still owns the claim");

    // The cancelled stream never consumes a permit; the replacement does.
    gate.add_permits(1);
    let events_url = format!("{base}/threads/{thread_id}/events");
    let events = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_event(&client, &events_url, |event| {
            event["type"] == "turn.completed" && event["turn"] == 2
        }),
    )
    .await
    .expect("replacement prompt never started after cancellation");

    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "turn.cancelled" && event["turn"] == 1 })
    );
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "turn.started" && event["turn"] == 2 })
    );
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|event| event["type"] == "user.message")
        .map(|event| event["content"].as_str().unwrap())
        .collect();
    assert_eq!(user_messages, ["cancel me", "replacement"]);
}

#[tokio::test]
async fn selected_queued_prompt_interrupts_the_active_turn_and_runs_next() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("gated", Arc::new(GatedProvider { gate: gate.clone() }))
            .with_default_model("gated/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Priority queue"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    for content in ["interrupt me", "ordinary follow-up", "send this now"] {
        client
            .post(format!("{base}/threads/{thread_id}/messages"))
            .json(&serde_json::json!({"content": content}))
            .send()
            .await
            .unwrap();
    }
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue.len(), 2);
    let selected_id = queue[1]["id"].as_str().unwrap();

    let accepted: serde_json::Value = client
        .post(format!("{base}/queue/{selected_id}/dispatch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["thread_id"], thread_id);
    assert_eq!(accepted["turn"], 0);
    assert_eq!(accepted["queued"], true);

    // The interrupted stream consumes no permit. The selected prompt gets
    // the first one, then normal queue draining resumes with the older item.
    gate.add_permits(2);
    let events = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_event(
            &client,
            &format!("{base}/threads/{thread_id}/events"),
            |event| event["type"] == "turn.completed" && event["turn"] == 3,
        ),
    )
    .await
    .expect("selected queued prompt never ran after interrupting the active turn");
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "turn.cancelled" && event["turn"] == 1)
    );
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|event| event["type"] == "user.message")
        .map(|event| event["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        user_messages,
        ["interrupt me", "send this now", "ordinary follow-up"]
    );

    // The same prompt-specific endpoint also starts an explicitly paused
    // idle queue without relying on a separate reorder request.
    for content in [
        "pause again",
        "older paused prompt",
        "selected paused prompt",
    ] {
        client
            .post(format!("{base}/threads/{thread_id}/messages"))
            .json(&serde_json::json!({"content": content}))
            .send()
            .await
            .unwrap();
    }
    let response = client
        .post(format!("{base}/threads/{thread_id}/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_event(
            &client,
            &format!("{base}/threads/{thread_id}/events"),
            |event| event["type"] == "turn.cancelled" && event["turn"] == 4,
        ),
    )
    .await
    .expect("second turn never reached its cancelled state");
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let selected_id = queue[1]["id"].as_str().unwrap();
    let accepted: serde_json::Value = client
        .post(format!("{base}/queue/{selected_id}/dispatch"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(accepted["turn"], 5);
    assert_eq!(accepted["queued"], false);

    gate.add_permits(2);
    let events = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_event(
            &client,
            &format!("{base}/threads/{thread_id}/events"),
            |event| event["type"] == "turn.completed" && event["turn"] == 6,
        ),
    )
    .await
    .expect("selected prompt never started from the paused idle queue");
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|event| event["type"] == "user.message")
        .map(|event| event["content"].as_str().unwrap())
        .collect();
    assert_eq!(
        user_messages,
        [
            "interrupt me",
            "send this now",
            "ordinary follow-up",
            "pause again",
            "selected paused prompt",
            "older paused prompt",
        ]
    );
}

#[tokio::test]
async fn queued_prompts_crud_and_in_order_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("gated", Arc::new(GatedProvider { gate: gate.clone() }))
            .with_default_model("gated/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Queue"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");
    let send = |content: &str| {
        let client = client.clone();
        let url = format!("{base}/threads/{thread_id}/messages");
        let content = content.to_string();
        async move {
            client
                .post(url)
                .json(&serde_json::json!({"content": content}))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };

    // First message dispatches immediately (turn 1, held open by the gate);
    // everything sent while it runs queues up.
    let first = send("one").await;
    assert_eq!(first["turn"], 1);
    assert_eq!(first["queued"], false);
    let second: serde_json::Value = client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({
            "content": "two",
            "attachments": [{
                "name": "before.png",
                "mime": "image/png",
                "data": "YmVmb3Jl"
            }]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["queued"], true);
    assert_eq!(second["turn"], 0);
    assert_eq!(second["queued_prompt"]["content"], "two");
    assert_eq!(
        second["queued_prompt"]["attachments"][0]["name"],
        "before.png"
    );
    send("three").await;

    // While turn 1 is held open the session reports activity (drives the
    // sidebar indicator in clients).
    let sessions: Vec<serde_json::Value> = client
        .get(format!("{base}/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions[0]["active"], true);

    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0]["content"], "two");
    assert_eq!(queue[1]["content"], "three");
    assert_eq!(queue[0]["attachments"][0]["name"], "before.png");
    let id_two = queue[0]["id"].as_str().unwrap().to_string();
    let id_three = queue[1]["id"].as_str().unwrap().to_string();
    assert_eq!(second["queued_prompt"]["id"], id_two);

    // Old clients send only content; omitted attachment fields preserve the
    // prompt's stored files.
    let resp = client
        .patch(format!("{base}/queue/{id_two}"))
        .json(&serde_json::json!({"content": "two v1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue[0]["attachments"][0]["name"], "before.png");

    // New clients can remove retained files and append fresh uploads in the
    // same edit without re-uploading attachments that remain selected.
    let resp = client
        .patch(format!("{base}/queue/{id_two}"))
        .json(&serde_json::json!({
            "content": "two v2",
            "retained_attachment_ids": [],
            "attachments": [{
                "name": "after.png",
                "mime": "image/png",
                "data": "YWZ0ZXI="
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue[0]["attachments"].as_array().unwrap().len(), 1);
    assert_eq!(queue[0]["attachments"][0]["name"], "after.png");

    // Reorder: "three" now runs before "two v2". A stale id set conflicts.
    let resp = client
        .put(format!("{base}/threads/{thread_id}/queue"))
        .json(&serde_json::json!({"ids": [id_three, "bogus"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let reordered: Vec<serde_json::Value> = client
        .put(format!("{base}/threads/{thread_id}/queue"))
        .json(&serde_json::json!({"ids": [id_three, id_two]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reordered[0]["content"], "three");
    assert_eq!(reordered[1]["content"], "two v2");

    // Delete: queue a fourth prompt and remove it again.
    send("four").await;
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id_four = queue[2]["id"].as_str().unwrap().to_string();
    let resp = client
        .delete(format!("{base}/queue/{id_four}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Release the gate: turn 1 finishes, then the queue drains in order.
    gate.add_permits(3);
    let events = wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 3
    })
    .await;
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "user.message")
        .map(|e| e["content"].as_str().unwrap())
        .collect();
    assert_eq!(user_messages, ["one", "three", "two v2"]);

    // The queue announced every change on the event stream and ended empty.
    let last_queue = events
        .iter()
        .rfind(|e| e["type"] == "thread.queue_updated")
        .expect("queue events published");
    assert_eq!(last_queue["prompts"].as_array().unwrap().len(), 0);

    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(queue.is_empty());

    // Activity rode the server-scope event stream: active when turn 1
    // claimed the thread, idle once the queue drained.
    let server_events = wait_for_event(&client, &format!("{base}/events"), |e| {
        e["type"] == "session.activity" && e["active"] == false
    })
    .await;
    assert!(
        server_events
            .iter()
            .any(|e| e["type"] == "session.activity" && e["active"] == true)
    );
    let sessions: Vec<serde_json::Value> = client
        .get(format!("{base}/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions[0]["active"], false);
}

/// A queue on one session keeps draining while the user works in another:
/// session A's turn is gated (its queue holds two prompts) while session B
/// runs a full turn — then A drains in order without anyone looking at it.
#[tokio::test]
async fn queued_prompts_drain_on_background_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("gated", Arc::new(GatedProvider { gate: gate.clone() }))
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(1), // skip the tool-call turn
                }),
            )
            .with_default_model("gated/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut threads = Vec::new();
    for (title, model) in [("A", "gated/test-model"), ("B", "scripted/test-model")] {
        let session: serde_json::Value = client
            .post(format!("{base}/sessions"))
            .json(&serde_json::json!({"workspace_id": ws["id"], "title": title}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let thread: serde_json::Value = client
            .post(format!("{base}/threads"))
            .json(&serde_json::json!({"session_id": session["id"], "model": model}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        threads.push(thread["id"].as_str().unwrap().to_string());
    }
    let (thread_a, thread_b) = (&threads[0], &threads[1]);

    // Session A: one running (gated) turn plus two queued prompts.
    for content in ["a-one", "a-two", "a-three"] {
        client
            .post(format!("{base}/threads/{thread_a}/messages"))
            .json(&serde_json::json!({"content": content}))
            .send()
            .await
            .unwrap();
    }

    // Session B is fully interactive while A's queue waits.
    client
        .post(format!("{base}/threads/{thread_b}/messages"))
        .json(&serde_json::json!({"content": "b-one"}))
        .send()
        .await
        .unwrap();
    let events_b = format!("{base}/threads/{thread_b}/events");
    wait_for_event(&client, &events_b, |e| e["type"] == "turn.completed").await;

    // A's turn is still gated; its queue is untouched.
    let queue_a: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_a}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue_a.len(), 2, "B's activity must not consume A's queue");

    // Release A; its queue drains in order with nobody watching the thread.
    gate.add_permits(3);
    let events_a = format!("{base}/threads/{thread_a}/events");
    let events = wait_for_event(&client, &events_a, |e| {
        e["type"] == "turn.completed" && e["turn"] == 3
    })
    .await;
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "user.message")
        .map(|e| e["content"].as_str().unwrap())
        .collect();
    assert_eq!(user_messages, ["a-one", "a-two", "a-three"]);
}

/// Prompts left in the queue by a crash wait for an explicit kick: a crash
/// may have cut the in-flight turn short, so the queue must NOT auto-run at
/// startup — it drains only once the user hits "Send now" (queue/dispatch).
#[tokio::test]
async fn leftover_queue_waits_for_explicit_dispatch_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store.clone(), tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(1), // text-only turns
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Resume"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();

    // What a crash mid-drain leaves behind: rows in queued_prompts, no
    // active dispatcher.
    store
        .enqueue_prompt(thread_id, "left-behind-1", &[])
        .unwrap();
    store
        .enqueue_prompt(thread_id, "left-behind-2", &[])
        .unwrap();

    // Nothing runs on its own — the server never auto-resumes a queue.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue.len(), 2, "leftover prompts must wait for the user");

    // "Send now" drains the leftovers in order.
    let resp = client
        .post(format!("{base}/threads/{thread_id}/queue/dispatch"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    let events_url = format!("{base}/threads/{thread_id}/events");
    let events = wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 2
    })
    .await;
    let user_messages: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "user.message")
        .map(|e| e["content"].as_str().unwrap())
        .collect();
    assert_eq!(user_messages, ["left-behind-1", "left-behind-2"]);

    let queue: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/queue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(queue.is_empty());
}

/// The "@"-mention path list: every worktree file plus directories with a
/// trailing '/', gitignored and hidden entries excluded.
#[tokio::test]
async fn worktree_paths_for_mentions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Mentions"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap();
    let worktree = std::path::PathBuf::from(session["worktree_path"].as_str().unwrap());

    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(worktree.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir_all(worktree.join("target")).unwrap();
    std::fs::write(worktree.join("target/junk.o"), "o").unwrap();
    std::fs::write(worktree.join(".gitignore"), "target/\n").unwrap();

    let paths: Vec<String> = client
        .get(format!("{base}/sessions/{session_id}/paths"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(paths.contains(&"src/".to_string()), "{paths:?}");
    assert!(paths.contains(&"src/main.rs".to_string()), "{paths:?}");
    // Gitignored and hidden entries stay out.
    assert!(!paths.iter().any(|p| p.starts_with("target")), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with(".git")), "{paths:?}");
    // Sorted, so the popup's unfiltered view is stable.
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);

    // Unknown session: 404.
    let missing = client
        .get(format!("{base}/sessions/nope/paths"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

/// Integrated terminal: open a shell in the session worktree, type a
/// command, watch the output stream, resize, and kill.
#[cfg(unix)]
#[tokio::test]
async fn terminal_shell_in_session_worktree() {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(ScriptedProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Terminal"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap();

    let term: serde_json::Value = client
        .post(format!("{base}/sessions/{session_id}/terminal"))
        .json(&serde_json::json!({"cols": 100, "rows": 30}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let term_id = term["id"].as_str().unwrap().to_string();
    assert_eq!(term["session_id"], *session_id);
    assert_eq!(term["cols"], 100);
    assert_eq!(term["exited"], false);

    // Re-open returns the same live terminal.
    let again: serde_json::Value = client
        .post(format!("{base}/sessions/{session_id}/terminal"))
        .json(&serde_json::json!({"cols": 80, "rows": 24}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["id"], *term_id);

    // The plural API creates and lists independent terminal tabs without
    // changing the singular endpoint's default-terminal behavior.
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/sessions/{session_id}/terminals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let second: serde_json::Value = client
        .post(format!("{base}/sessions/{session_id}/terminals"))
        .json(&serde_json::json!({"cols": 90, "rows": 25}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_id = second["id"].as_str().unwrap();
    assert_ne!(second_id, term_id);
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/sessions/{session_id}/terminals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0]["id"], *term_id);
    assert_eq!(listed[1]["id"], *second_id);
    assert_eq!(
        client
            .delete(format!("{base}/terminals/{second_id}"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let listed: Vec<serde_json::Value> = client
        .get(format!("{base}/sessions/{session_id}/terminals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], *term_id);

    // The shell starts in the worktree: `ls` shows the checked-out README.
    let resp = client
        .post(format!("{base}/terminals/{term_id}/input"))
        .json(&serde_json::json!({"data": b64.encode("ls\r")}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Follow the output SSE until the README shows up.
    let out_url = format!("{base}/terminals/{term_id}/output?after=0");
    let collected = tokio::time::timeout(Duration::from_secs(20), async {
        let resp = client.get(&out_url).send().await.unwrap();
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut out: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                if let Some(data) = line.strip_prefix("data:")
                    && let Ok(bytes) = b64.decode(data.trim())
                {
                    out.extend_from_slice(&bytes);
                }
            }
            if String::from_utf8_lossy(&out).contains("README.md") {
                return out;
            }
        }
        panic!("terminal stream ended without README.md; got: {out:?}");
    })
    .await
    .expect("timed out waiting for terminal output");
    assert!(String::from_utf8_lossy(&collected).contains("README.md"));

    // Resize.
    let resp = client
        .post(format!("{base}/terminals/{term_id}/resize"))
        .json(&serde_json::json!({"cols": 120, "rows": 40}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // An already-exited terminal still announces the absolute replay start,
    // replays its retained bytes, and only then emits `exit`. The marker has
    // no id so legacy EventSource clients keep their last output-event id.
    let resp = client
        .post(format!("{base}/terminals/{term_id}/input"))
        .json(&serde_json::json!({
            "data": b64.encode("printf 'replay-exit'; exit\r")
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let mut terminal_exited = false;
    for _ in 0..100 {
        let terminals: Vec<serde_json::Value> = client
            .get(format!("{base}/sessions/{session_id}/terminals"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        terminal_exited = terminals[0]["exited"] == true;
        if terminal_exited {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(terminal_exited, "terminal did not exit in time");

    let replay = tokio::time::timeout(Duration::from_secs(10), async {
        client
            .get(&out_url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    })
    .await
    .expect("timed out reading exited terminal replay");
    let replay = replay.replace("\r\n", "\n");
    let events: Vec<_> = replay
        .split("\n\n")
        .filter(|event| !event.trim().is_empty())
        .collect();
    let first = events.first().expect("replay-start event");
    assert!(first.lines().any(|line| line == "event: replay-start"));
    assert!(first.lines().any(|line| line == "data: {\"offset\":0}"));
    assert!(!first.lines().any(|line| line.starts_with("id:")));
    assert!(
        events
            .last()
            .is_some_and(|event| event.lines().any(|line| line == "event: exit"))
    );
    let replayed: Vec<u8> = events
        .iter()
        .skip(1)
        .flat_map(|event| {
            event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .filter_map(|data| b64.decode(data.trim()).ok())
                .flatten()
        })
        .collect();
    assert!(String::from_utf8_lossy(&replayed).contains("replay-exit"));

    // Kill; input to a dead terminal 404s, and reopening spawns a new one.
    let resp = client
        .delete(format!("{base}/terminals/{term_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let resp = client
        .post(format!("{base}/terminals/{term_id}/input"))
        .json(&serde_json::json!({"data": b64.encode("x")}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let fresh: serde_json::Value = client
        .post(format!("{base}/sessions/{session_id}/terminal"))
        .json(&serde_json::json!({"cols": 80, "rows": 24}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(fresh["id"], *term_id);
}

/// The automation-template catalog: non-empty, every entry ready to
/// pre-fill the create form, and the static /templates segment doesn't
/// shadow (or get shadowed by) the /{id} routes.
#[tokio::test]
async fn automation_templates_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let templates: Vec<serde_json::Value> = client
        .get(format!("{base}/automations/templates"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!templates.is_empty());
    for t in &templates {
        assert_ne!(t["id"], "");
        assert_ne!(t["name"], "");
        assert_ne!(t["description"], "");
        assert_ne!(t["prompt"], "");
        assert!(["hourly", "daily", "weekly"].contains(&t["schedule"]["kind"].as_str().unwrap()));
    }

    // The parameterized routes still resolve: an unknown automation id
    // 404s rather than being eaten by the static /templates route.
    let resp = client
        .put(format!("{base}/automations/nope"))
        .json(&serde_json::json!({
            "name": "x", "prompt": "y", "workspace_id": "w",
            "schedule": {"kind": "daily", "time": "09:00"}, "enabled": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

struct FailingAutomationProvider;

#[async_trait::async_trait]
impl Provider for FailingAutomationProvider {
    fn id(&self) -> &str {
        "automation-failure"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Api("automation provider failed".into()))
    }
}

#[tokio::test]
async fn automation_records_the_turn_outcome_not_just_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("automation-failure", Arc::new(FailingAutomationProvider))
            .with_default_model("automation-failure/test"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let automation: serde_json::Value = client
        .post(format!("{base}/automations"))
        .json(&serde_json::json!({
            "name": "Fail after dispatch",
            "prompt": "run",
            "workspace_id": workspace["id"],
            "permission_mode": "yolo",
            "thinking_level": "high",
            "schedule": {"kind": "daily", "time": "09:00"},
            "enabled": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(automation["permission_mode"], "yolo");
    assert_eq!(automation["thinking_level"], "high");
    let automation_id = automation["id"].as_str().unwrap();
    let resp = client
        .post(format!("{base}/automations/{automation_id}/run"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);

    let recorded = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let automations: Vec<serde_json::Value> = client
                .get(format!("{base}/automations"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let current = automations
                .into_iter()
                .find(|a| a["id"] == automation_id)
                .unwrap();
            if !current["last_error"].as_str().unwrap_or("").is_empty() {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("automation outcome was not recorded");
    assert!(
        recorded["last_error"]
            .as_str()
            .unwrap()
            .contains("automation provider failed"),
        "{recorded}"
    );
    assert!(recorded["last_session_id"].is_string());
    let session_id = recorded["last_session_id"].as_str().unwrap();
    let threads: Vec<serde_json::Value> = client
        .get(format!("{base}/threads?session_id={session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["permission_mode"], "yolo");
    assert_eq!(threads[0]["model_options"]["thinking_level"], "high");
}

/// Session naming settings persist through the protocol, and missing model
/// assets always degrade to the deterministic heuristic instead of blocking
/// creation.
#[tokio::test]
async fn session_title_settings_and_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let config_file = tmp.path().join("config.toml");
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_config_file(Some(config_file.clone())),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/config/git-worktrees"))
        .send()
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .contains_key(trouve_protocol::EVENT_CURSOR_HEADER)
    );
    let settings: serde_json::Value = response.json().await.unwrap();
    assert_eq!(settings["derive_branch_name_from_session_title"], false);
    assert_eq!(settings["title_model_load_behavior"], "auto");
    assert_eq!(settings["title_model_resource_policy"], "cpu_ram_only");
    assert_eq!(settings["title_model"]["state"], "not_installed");

    let response = client
        .put(format!("{base}/config/git-worktrees"))
        .json(&serde_json::json!({
            "derive_branch_name_from_session_title": true,
            "title_model_load_behavior": "off",
            "title_model_resource_policy": "gpu_cpu_ram"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .contains_key(trouve_protocol::EVENT_CURSOR_HEADER)
    );
    let settings: serde_json::Value = response.json().await.unwrap();
    assert_eq!(settings["derive_branch_name_from_session_title"], true);
    assert_eq!(settings["title_model_load_behavior"], "off");
    assert_eq!(settings["title_model_resource_policy"], "gpu_cpu_ram");

    // Requests from clients predating the additive branch-naming option must
    // preserve an explicit opt-in rather than resetting it to the default.
    let settings: serde_json::Value = client
        .put(format!("{base}/config/git-worktrees"))
        .json(&serde_json::json!({
            "title_model_load_behavior": "off",
            "title_model_resource_policy": "gpu_cpu_ram"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(settings["derive_branch_name_from_session_title"], true);
    let response = client
        .delete(format!("{base}/config/git-worktrees/title-model/install"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(
        std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("title_model_load_behavior = \"off\"")
    );
    assert!(
        std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("title_model_resource_policy = \"gpu_cpu_ram\"")
    );
    assert!(
        std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("derive_branch_name_from_session_title = true")
    );
    assert!(
        engine
            .store()
            .events_after(&trouve_protocol::Scope::Server, 0)
            .unwrap()
            .iter()
            .any(|envelope| matches!(
                envelope.event,
                trouve_protocol::Event::GitWorktreeSettingsUpdated { .. }
            ))
    );

    let title: serde_json::Value = client
        .post(format!("{base}/session-title"))
        .json(&serde_json::json!({
            "prompt": "When initially naming a new session, can the app create an intelligent summarized title based on the prompt instead of just using the prompt as-is?"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(title["source"], "heuristic");
    assert_eq!(
        title["title"],
        "Create intelligent summarized title from prompt"
    );
}

#[tokio::test]
async fn code_review_execution_settings_persist_and_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let config_file = tmp.path().join("config.toml");
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_config_file(Some(config_file.clone())),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/config/code-review"))
        .send()
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .contains_key(trouve_protocol::EVENT_CURSOR_HEADER)
    );
    let settings: serde_json::Value = response.json().await.unwrap();
    assert_eq!(settings["max_parallel_reviews"], 2);
    assert_eq!(settings["total_timeout_seconds"], 900);
    assert_eq!(settings["reviewer_timeout_seconds"], 600);
    assert_eq!(settings["coordinator_timeout_seconds"], 300);

    let response = client
        .put(format!("{base}/config/code-review"))
        .json(&serde_json::json!({
            "max_parallel_reviews": 4,
            "total_timeout_seconds": 1_200,
            "reviewer_timeout_seconds": 720,
            "coordinator_timeout_seconds": 360
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        response
            .headers()
            .contains_key(trouve_protocol::EVENT_CURSOR_HEADER)
    );
    let settings: serde_json::Value = response.json().await.unwrap();
    assert_eq!(settings["max_parallel_reviews"], 4);
    assert_eq!(settings["total_timeout_seconds"], 1_200);
    assert_eq!(settings["reviewer_timeout_seconds"], 720);
    assert_eq!(settings["coordinator_timeout_seconds"], 360);

    let invalid = client
        .put(format!("{base}/config/code-review"))
        .json(&serde_json::json!({
            "max_parallel_reviews": 4,
            "total_timeout_seconds": 600,
            "reviewer_timeout_seconds": 601,
            "coordinator_timeout_seconds": 300
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    let excessive_concurrency = client
        .put(format!("{base}/config/code-review"))
        .json(&serde_json::json!({
            "max_parallel_reviews": trouve_protocol::MAX_PARALLEL_REVIEWS + 1,
            "total_timeout_seconds": 1_200,
            "reviewer_timeout_seconds": 720,
            "coordinator_timeout_seconds": 360
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(excessive_concurrency.status(), reqwest::StatusCode::OK);
    let compatible_settings: serde_json::Value = excessive_concurrency.json().await.unwrap();
    assert_eq!(
        compatible_settings["max_parallel_reviews"],
        trouve_protocol::MAX_PARALLEL_REVIEWS
    );

    let persisted = std::fs::read_to_string(&config_file).unwrap();
    assert!(persisted.contains("code_review_max_parallel_reviews = 32"));
    assert!(persisted.contains("code_review_timeout_seconds = 1200"));
    assert!(persisted.contains("code_review_reviewer_timeout_seconds = 720"));
    assert!(persisted.contains("code_review_coordinator_timeout_seconds = 360"));
    assert!(
        engine
            .store()
            .events_after(&trouve_protocol::Scope::Server, 0)
            .unwrap()
            .iter()
            .any(|envelope| matches!(
                envelope.event,
                trouve_protocol::Event::CodeReviewSettingsUpdated { .. }
            ))
    );
}

/// GitHub Enterprise hosts: the integration always lists github.com
/// first, added hosts get their own entry (persisted to config),
/// duplicates and bad hostnames are rejected, and removal works —
/// github.com itself can't be removed or added.
#[tokio::test]
async fn github_enterprise_host_crud() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let config_file = tmp.path().join("config.toml");
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_config_file(Some(config_file.clone())),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    // Fresh state: only github.com, which is not removable.
    let gh: serde_json::Value = client
        .get(format!("{base}/integrations/github"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hosts = gh["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0]["host"], "github.com");
    assert_eq!(hosts[0]["removable"], false);
    // The built-in shared OAuth app: sign-in works with zero config.
    assert_eq!(hosts[0]["oauth_available"], true);

    // Add an enterprise host (scheme and trailing slash are tolerated).
    let gh: serde_json::Value = client
        .post(format!("{base}/integrations/github/hosts"))
        .json(&serde_json::json!({"host": "https://GHES.Example.com/", "client_id": "Iv1.abc"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hosts = gh["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[1]["host"], "ghes.example.com");
    assert_eq!(hosts[1]["removable"], true);
    assert_eq!(hosts[1]["oauth_available"], true);
    // The host landed in config.toml.
    assert!(
        std::fs::read_to_string(&config_file)
            .unwrap()
            .contains("ghes.example.com")
    );

    // Duplicates conflict; garbage and github.com itself are rejected.
    for (body, status) in [
        (serde_json::json!({"host": "ghes.example.com"}), 409),
        (serde_json::json!({"host": "not a hostname"}), 400),
        (serde_json::json!({"host": "github.com"}), 400),
    ] {
        let resp = client
            .post(format!("{base}/integrations/github/hosts"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), status, "{body}");
    }

    // Pasted-token authentication is not an integration point.
    let resp = client
        .put(format!("{base}/integrations/github"))
        .json(&serde_json::json!({"token": "x", "host": "unknown.example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);

    // Remove the host; github.com can't be removed.
    let gh: serde_json::Value = client
        .delete(format!("{base}/integrations/github/hosts/ghes.example.com"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(gh["hosts"].as_array().unwrap().len(), 1);
    let resp = client
        .delete(format!("{base}/integrations/github/hosts/github.com"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// The local-models enable toggle and the install-lifecycle endpoints:
/// disabling unregisters the "local" provider (persisted), cancels 404
/// when nothing is in flight, uninstall is a no-op for absent managed
/// installs, and restart 409s with no server running.
#[tokio::test]
async fn local_enable_toggle_and_install_lifecycle_endpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    // Enabled by default; the sidecar is stopped and the provider listed.
    let local: serde_json::Value = client
        .get(format!("{base}/local"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local["enabled"], true);
    assert_eq!(local["server_status"], "stopped");
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let has_local = |p: &serde_json::Value| {
        p["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "local")
    };
    assert!(has_local(&providers));

    // Disable: reflected in status, and the provider disappears.
    let resp = client
        .put(format!("{base}/local/enabled"))
        .json(&serde_json::json!({"enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let local: serde_json::Value = client
        .get(format!("{base}/local"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local["enabled"], false);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!has_local(&providers));

    // Re-enable restores the provider.
    let resp = client
        .put(format!("{base}/local/enabled"))
        .json(&serde_json::json!({"enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let providers: serde_json::Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(has_local(&providers));

    // Nothing is downloading/installing: cancels 404.
    let resp = client
        .delete(format!("{base}/clis/codex/install"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = client
        .delete(format!("{base}/local/models/qwen2.5-coder-3b/download"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Uninstall: unknown CLI 404s; a known CLI with no managed install is
    // a clean no-op.
    let resp = client
        .delete(format!("{base}/clis/not-a-cli"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = client
        .delete(format!("{base}/clis/codex"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // No llama-server running: restart conflicts.
    let resp = client
        .post(format!("{base}/local/server/restart"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
}

/// Drives search_transcript: turn 1 plants a fact; turn 2 searches for it
/// (plus a bad-scope probe), then reads the matched turn in full.
struct RecallProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for RecallProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let done = Ok(ProviderEvent::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        });
        let events: Vec<Result<ProviderEvent, ProviderError>> = match call {
            // Turn 1: acknowledge the fact (also searchable later).
            0 => vec![
                Ok(ProviderEvent::TextDelta(
                    "Noted: 74656 is the magic number.".into(),
                )),
                done,
            ],
            // Turn 2, iteration 1: search for it, plus a bad scope.
            1 => vec![
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "s1".into(),
                    name: "search_transcript".into(),
                    arguments: serde_json::json!({"query": "magic number"}),
                })),
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "s2".into(),
                    name: "search_transcript".into(),
                    arguments: serde_json::json!({"query": "x", "scope": "galaxy"}),
                })),
                done,
            ],
            // Turn 2, iteration 2: read the matched turn in full.
            2 => vec![
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "s3".into(),
                    name: "search_transcript".into(),
                    arguments: serde_json::json!({"turn": 1}),
                })),
                done,
            ],
            _ => vec![
                Ok(ProviderEvent::TextDelta("Recovered: 74656.".into())),
                done,
            ],
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// search_transcript: snippets are turn-stamped, scopes validate, and turn
/// mode replays one turn's messages in full.
#[tokio::test]
async fn search_transcript_recovers_history() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider(
                "scripted",
                Arc::new(RecallProvider {
                    calls: AtomicUsize::new(0),
                }),
            )
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Recall"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap().to_string();
    let events_url = format!("{base}/threads/{thread_id}/events");

    // Turn 1 plants the fact.
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "remember the magic number is 74656"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 1
    })
    .await;

    // Turn 2 recovers it.
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "what was the magic number?"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let events = wait_for_event(&client, &events_url, |e| {
        e["type"] == "turn.completed" && e["turn"] == 2
    })
    .await;

    let results = tool_results(&events);
    // The search found turn 1's user message and assistant reply.
    let search = results.iter().find(|(id, _)| *id == "s1").unwrap().1;
    let matches = search["matches"].as_array().unwrap();
    assert!(matches.len() >= 2, "{search}");
    assert_eq!(matches[0]["turn"], 1);
    assert_eq!(matches[0]["role"], "user");
    assert!(
        matches[0]["snippet"].as_str().unwrap().contains("74656"),
        "{search}"
    );
    assert!(matches.iter().any(|m| m["role"] == "assistant"));
    assert_eq!(search["truncated"], false);
    // Scope names validate.
    let bad = results.iter().find(|(id, _)| *id == "s2").unwrap().1;
    assert!(
        bad["error"].as_str().unwrap().contains("unknown scope"),
        "{bad}"
    );
    // Turn mode replays the full messages of turn 1.
    let full = results.iter().find(|(id, _)| *id == "s3").unwrap().1;
    let messages = full["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| {
        m["role"] == "user"
            && m["content"]
                .as_str()
                .unwrap()
                .contains("remember the magic number")
    }));
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"].as_str().unwrap().contains("Noted")),
        "{full}"
    );
    // ... and the model could answer from it.
    assert!(events.iter().any(|e| e["type"] == "assistant.message"
        && e["content"].as_str().unwrap().contains("Recovered: 74656")));
}

/// Drives the spawn tool family end-to-end. The parent turn spawns a child
/// agent, pokes spawn_output with a bogus id (denied: not its child), waits
/// on the real child, then summarizes. The child delegates to a grandchild
/// and collects that result before answering.
struct SpawnProvider {
    /// "spawn_thread" (same session) or "spawn_session" (fresh worktree).
    spawn_tool: &'static str,
}

#[async_trait::async_trait]
impl Provider for SpawnProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn stream_chat(
        &self,
        _model: &str,
        messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let users: String = messages
            .iter()
            .filter_map(|m| match m {
                Message::User(c) => Some(c.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let results: String = messages
            .iter()
            .filter_map(|m| match m {
                Message::ToolResult { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let done = Ok(ProviderEvent::Completed {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            },
        });
        let events: Vec<Result<ProviderEvent, ProviderError>> = if users.contains("grandchild task")
        {
            vec![
                Ok(ProviderEvent::TextDelta(
                    "Grandchild done: the nested answer is 21.".into(),
                )),
                done,
            ]
        } else if users.contains("child task") {
            // The child agent delegates once, waits for that grandchild, then
            // completes its own durable transcript.
            if results.contains("Grandchild done") {
                vec![
                    Ok(ProviderEvent::TextDelta(
                        "Child done: the answer is 42.".into(),
                    )),
                    done,
                ]
            } else if !results.contains("thread_id") {
                vec![
                    Ok(ProviderEvent::ToolCall(ToolCallRequest {
                        id: "c1".into(),
                        name: "spawn_thread".into(),
                        arguments: serde_json::json!({"prompt": "grandchild task"}),
                    })),
                    done,
                ]
            } else {
                let grandchild_id = results
                    .split("\"thread_id\":\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
                    .to_string();
                vec![
                    Ok(ProviderEvent::ToolCall(ToolCallRequest {
                        id: "c2".into(),
                        name: "spawn_output".into(),
                        arguments: serde_json::json!({
                            "thread_id": grandchild_id,
                            "wait_ms": 25_000
                        }),
                    })),
                    done,
                ]
            }
        } else if !results.contains("thread_id") {
            // Parent iteration 1: spawn the child.
            let mut args = serde_json::json!({"prompt": "child task: compute the answer"});
            if self.spawn_tool == "spawn_session" {
                args["title"] = "Sub experiment".into();
            }
            vec![
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "p1".into(),
                    name: self.spawn_tool.into(),
                    arguments: args,
                })),
                done,
            ]
        } else if !results.contains("Child done") {
            // Parent iteration 2: a bogus collect (denied), then the real
            // one, blocking until the child finishes.
            let child_id = results
                .split("\"thread_id\":\"")
                .nth(1)
                .unwrap()
                .split('"')
                .next()
                .unwrap()
                .to_string();
            vec![
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "p2".into(),
                    name: "spawn_output".into(),
                    arguments: serde_json::json!({"thread_id": "th_bogus"}),
                })),
                Ok(ProviderEvent::ToolCall(ToolCallRequest {
                    id: "p3".into(),
                    name: "spawn_output".into(),
                    arguments: serde_json::json!({"thread_id": child_id, "wait_ms": 25_000}),
                })),
                done,
            ]
        } else {
            vec![
                Ok(ProviderEvent::TextDelta(
                    "Parent: the child reported 42.".into(),
                )),
                done,
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// Shared setup for the spawn tests: server + workspace + session + thread.
/// Returns (base url, client, session json, parent thread id).
async fn spawn_test_setup(
    tmp: &tempfile::TempDir,
    spawn_tool: &'static str,
) -> (String, reqwest::Client, serde_json::Value, String) {
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);

    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_provider("scripted", Arc::new(SpawnProvider { spawn_tool }))
            .with_default_model("scripted/test-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Parent work"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap().to_string();
    (base, client, session, thread_id)
}

/// The completed tool results of a turn's event list, by tool call id.
fn tool_results(events: &[serde_json::Value]) -> Vec<(&str, &serde_json::Value)> {
    events
        .iter()
        .filter(|e| e["type"] == "tool.completed")
        .map(|e| (e["call_id"].as_str().unwrap(), &e["result"]))
        .collect()
}

/// spawn_thread: a child agent on a new thread in the same session, running
/// concurrently with the parent's turn, recursively delegating once, and
/// collected with spawn_output — plus authorization and hierarchy checks.
#[tokio::test]
async fn spawn_thread_child_agent_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, client, session, thread_id) = spawn_test_setup(&tmp, "spawn_thread").await;
    let session_id = session["id"].as_str().unwrap();

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "spawn a child worker"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let events = wait_for_event(
        &client,
        &format!("{base}/threads/{thread_id}/events"),
        |e| e["type"] == "turn.completed",
    )
    .await;

    let results = tool_results(&events);
    // The spawn returned the child's coordinates without blocking the turn.
    let spawn = results.iter().find(|(id, _)| *id == "p1").unwrap().1;
    let child_id = spawn["thread_id"].as_str().unwrap().to_string();
    assert_eq!(spawn["session_id"], session["id"]);
    // Collecting someone else's (or a made-up) thread is refused.
    let bogus = results.iter().find(|(id, _)| *id == "p2").unwrap().1;
    assert!(
        bogus["error"].as_str().unwrap().contains("not a child"),
        "{bogus}"
    );
    // The real collect waited for the child and folded its result.
    let output = results.iter().find(|(id, _)| *id == "p3").unwrap().1;
    assert_eq!(output["status"], "completed", "{output}");
    assert!(
        output["last_message"]
            .as_str()
            .unwrap()
            .contains("Child done"),
        "{output}"
    );
    assert!(output["usage"]["output_tokens"].as_u64().unwrap() > 0);
    // ... and the parent's final answer used it.
    assert!(events.iter().any(|e| e["type"] == "assistant.message"
        && e["content"].as_str().unwrap().contains("child reported 42")));

    // The child rides the same session and is marked as agent-spawned.
    let threads: Vec<serde_json::Value> = client
        .get(format!("{base}/threads?session_id={session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let child = threads.iter().find(|t| t["id"] == child_id).unwrap();
    assert_eq!(child["spawned"], true, "{child}");
    assert_eq!(child["mode"], "code");
    assert_eq!(child["title"], "Subagent: Child task compute answer");
    let subagents: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{thread_id}/subagents"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(subagents.len(), 1, "{subagents:?}");
    assert_eq!(subagents[0]["id"], child_id);
    assert_eq!(subagents[0]["spawned"], true);
    let descendants: Vec<serde_json::Value> = client
        .get(format!(
            "{base}/threads/{thread_id}/subagents?recursive=true"
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(descendants.len(), 2, "{descendants:?}");
    assert!(descendants.iter().any(|thread| thread["id"] == child_id));
    let grandchild = descendants
        .iter()
        .find(|thread| thread["id"] != child_id)
        .expect("recursive listing should include the grandchild");
    let grandchild_id = grandchild["id"].as_str().unwrap();
    let child_subagents: Vec<serde_json::Value> = client
        .get(format!("{base}/threads/{child_id}/subagents"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(child_subagents.len(), 1, "{child_subagents:?}");
    assert_eq!(child_subagents[0]["id"], grandchild_id);
    let subagent = events
        .iter()
        .find(|event| event["type"] == "subagent.spawned")
        .expect("parent transcript should link the spawned child");
    assert_eq!(subagent["thread_id"], child_id);
    assert_eq!(subagent["session_id"], session_id);
    assert_eq!(subagent["prompt"], "child task: compute the answer");
    assert_eq!(subagent["model"], child["model"]);
    let parent = threads
        .iter()
        .find(|t| t["id"] == thread_id.as_str())
        .unwrap();
    assert!(!parent["spawned"].as_bool().unwrap_or(false));

    let grandchild_events = wait_for_event(
        &client,
        &format!("{base}/threads/{grandchild_id}/events"),
        |event| event["type"] == "turn.completed",
    )
    .await;
    assert!(grandchild_events.iter().any(|event| {
        event["type"] == "assistant.message"
            && event["content"]
                .as_str()
                .is_some_and(|content| content.contains("Grandchild done"))
    }));
}

/// spawn_session: a child agent in a fresh worktree session branched from
/// the parent session's branch, fully isolated, collected with spawn_output.
#[tokio::test]
async fn spawn_session_child_agent_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let (base, client, session, thread_id) = spawn_test_setup(&tmp, "spawn_session").await;

    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "spawn an isolated experiment"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let events = wait_for_event(
        &client,
        &format!("{base}/threads/{thread_id}/events"),
        |e| e["type"] == "turn.completed",
    )
    .await;

    let results = tool_results(&events);
    let spawn = results
        .iter()
        .find(|(id, _)| *id == "p1")
        .unwrap_or_else(|| panic!("missing spawn_session result in events: {events:#?}"))
        .1;
    let child_thread_id = spawn["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("spawn_session did not return a thread: {spawn}"));
    let child_session_id = spawn["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("spawn_session did not return a session: {spawn}"));
    assert_ne!(child_session_id, session["id"].as_str().unwrap());
    // The child is based on the parent's latest checkpoint commit (its
    // actual work), not the session branch — checkpoints never move the
    // branch, so basing on the branch would show the child nothing. Expect
    // a resolved commit hash rather than the branch name.
    let based_on = spawn["based_on"].as_str().unwrap();
    assert_ne!(based_on, session["branch"].as_str().unwrap());
    assert_eq!(
        based_on.len(),
        40,
        "based_on should be a commit hash: {based_on}"
    );
    assert!(based_on.chars().all(|c| c.is_ascii_hexdigit()));
    let output = results.iter().find(|(id, _)| *id == "p3").unwrap().1;
    assert_eq!(output["status"], "completed", "{output}");
    assert!(
        output["last_message"]
            .as_str()
            .unwrap()
            .contains("Child done"),
        "{output}"
    );

    // A real session: its own branch off the parent's, its own worktree,
    // the requested title, and a spawned thread inheriting the parent mode.
    let child_session: serde_json::Value = client
        .get(format!("{base}/sessions/{child_session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(child_session["title"], "Sub experiment");
    // base_ref is the parent's checkpoint commit (see based_on above), not
    // the branch name.
    assert_eq!(child_session["base_ref"], based_on);
    assert_ne!(child_session["branch"], session["branch"]);
    let child_worktree = child_session["worktree_path"].as_str().unwrap();
    assert_ne!(child_worktree, session["worktree_path"].as_str().unwrap());
    assert!(Path::new(child_worktree).join("README.md").exists());

    let threads: Vec<serde_json::Value> = client
        .get(format!("{base}/threads?session_id={child_session_id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let child = threads.iter().find(|t| t["id"] == child_thread_id).unwrap();
    assert_eq!(child["spawned"], true, "{child}");
    assert_eq!(child["mode"], "code");
}

#[tokio::test]
async fn secured_router_enforces_loopback_host_and_internal_token() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let backend = Arc::new(ScriptedBackend::new());
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let mut config = Config::default();
    config.providers.insert(
        "fake-agent".into(),
        trouve_core::config::ProviderConfig {
            kind: "claude-cli".into(),
            command: Some("/definitely/not/a/fake-agent-test-binary".into()),
            ..Default::default()
        },
    );
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &config)
            .with_config_dir(None)
            .with_backend("fake-agent", backend.clone())
            .with_default_model("fake-agent/agent-model"),
    );

    let security = trouve_server::ServerSecurity {
        require_loopback_host: true,
        internal_token: Some("bridge-secret".to_string()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    engine.set_base_url(&format!("http://{addr}"));
    let router = trouve_server::build_secured_router(engine, security);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    // Loopback API requests do not require authentication.
    let resp = client.get(format!("{base}/info")).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    // Non-loopback Host header (DNS-rebinding attempt) -> 403.
    let resp = client
        .get(format!("{base}/info"))
        .header(reqwest::header::HOST, "attacker.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);

    // GitHub's public webhook route bypasses the loopback-host check; its
    // handler still rejects unsigned payloads before processing.
    let resp = client
        .post(format!("http://{addr}/github/webhooks"))
        .header(reqwest::header::HOST, "hooks.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2025-03-26"}
    });
    let ws: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path": repo.to_str().unwrap()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"workspace_id": ws["id"], "title": "Secured bridge"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({"session_id": session["id"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thread_id = thread["id"].as_str().unwrap();
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content": "hold for approval"}))
        .send()
        .await
        .unwrap();
    let events_url = format!("{base}/threads/{thread_id}/events");
    wait_for_event(&client, &events_url, |event| {
        event["type"] == "approval.requested"
    })
    .await;
    let internal = backend
        .bridge_urls_seen
        .lock()
        .unwrap()
        .last()
        .cloned()
        .flatten()
        .expect("active turn receives an exact bridge capability");
    let unauthenticated = internal.replace("&bridge_token=bridge-secret", "");
    assert_ne!(unauthenticated, internal);
    let resp = client
        .post(&unauthenticated)
        .json(&initialize)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);
    let resp = client
        .post(internal)
        .json(&initialize)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

// --- connectivity -------------------------------------------------------------

/// A provider that only exists to publish a one-model catalog.
struct StaticModelProvider {
    id: &'static str,
}

#[async_trait::async_trait]
impl Provider for StaticModelProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
        vec![trouve_protocol::ModelInfo {
            id: format!("{}/m", self.id),
            display_name: self.id.into(),
            context_window: 100_000,
            supports_tools: true,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            options_schema: serde_json::json!({"type": "object", "properties": {}}),
        }]
    }

    async fn stream_chat(
        &self,
        _model: &str,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Request("catalog-only provider".into()))
    }
}

/// Offline behavior: `/v1/info` reports the state, `/v1/models` keeps only
/// models that run without internet (the local provider) — remote providers
/// and vendor backends disappear instead of degrading to fallback catalogs —
/// and each transition lands exactly once in the server-scope event log.
#[tokio::test]
async fn offline_filters_models_and_reports_connectivity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            // Stands in for the built-in local provider (same "local" id).
            .with_provider("local", Arc::new(StaticModelProvider { id: "local" }))
            .with_provider("remote", Arc::new(StaticModelProvider { id: "remote" }))
            .with_backend("agent-a", Arc::new(HandoffBackend::new("agent-a"))),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let client = reqwest::Client::new();

    let model_ids = || {
        let client = client.clone();
        let url = format!("{base}/models");
        async move {
            let models: Vec<serde_json::Value> =
                client.get(url).send().await.unwrap().json().await.unwrap();
            models
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let online_flag = || {
        let client = client.clone();
        let url = format!("{base}/info");
        async move {
            let info: serde_json::Value =
                client.get(url).send().await.unwrap().json().await.unwrap();
            info["online"].as_bool().unwrap()
        }
    };

    // Online (the default without a probe): everything is listed.
    assert!(online_flag().await);
    assert_eq!(model_ids().await, ["agent-a/m", "local/m", "remote/m"]);

    // Offline: only the local provider survives; no fallback entries.
    engine.set_online(false);
    assert!(!online_flag().await);
    assert_eq!(model_ids().await, ["local/m"]);

    // Recovery restores the full list.
    engine.set_online(true);
    assert!(online_flag().await);
    assert_eq!(model_ids().await, ["agent-a/m", "local/m", "remote/m"]);

    // Both transitions were logged, and repeating the current state is not
    // a transition (no duplicate events).
    engine.set_online(true);
    let events = engine
        .store()
        .events_after(&trouve_protocol::Scope::Server, 0)
        .unwrap();
    let transitions: Vec<bool> = events
        .iter()
        .filter_map(|env| match env.event {
            trouve_protocol::Event::ConnectivityChanged { online } => Some(online),
            _ => None,
        })
        .collect();
    assert_eq!(transitions, [false, true]);

    // The SSE stream delivers the transition like any other server event.
    engine.set_online(false);
    let seen = wait_for_event(&client, &format!("{base}/events"), |event| {
        event["type"] == "server.connectivity_changed" && event["online"] == false
    })
    .await;
    assert!(!seen.is_empty());
}

#[tokio::test]
async fn code_review_dashboard_and_repository_policy_round_trip() {
    struct ReviewRouterProvider;

    #[async_trait::async_trait]
    impl Provider for ReviewRouterProvider {
        fn id(&self) -> &str {
            "anthropic"
        }

        fn models(&self) -> Vec<trouve_protocol::ModelInfo> {
            vec![trouve_protocol::ModelInfo {
                id: "anthropic/claude".into(),
                display_name: "Claude".into(),
                context_window: 100_000,
                supports_tools: true,
                input_price_per_mtok: None,
                output_price_per_mtok: None,
                options_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reasoning_effort": {
                            "type": "string",
                            "enum": ["low", "high"],
                            "default": "low"
                        }
                    }
                }),
            }]
        }

        async fn stream_chat(
            &self,
            _model: &str,
            _messages: &[Message],
            _tools: &[ToolSpec],
            _options: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<EventStream, ProviderError> {
            unreachable!("repository round-trip validation never starts a model turn")
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(Some(tmp.path().join("config")))
            .with_provider("anthropic", Arc::new(ReviewRouterProvider)),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1/code-review");
    let client = reqwest::Client::new();

    let empty_response = client.get(&base).send().await.unwrap();
    let empty_cursor = empty_response
        .headers()
        .get(trouve_protocol::EVENT_CURSOR_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let empty: serde_json::Value = empty_response.json().await.unwrap();
    assert_eq!(empty["app"]["configured"], false);
    assert_eq!(empty["repositories"], serde_json::json!([]));
    assert!(empty["reviewers"].as_array().unwrap().len() >= 12);
    assert!(
        empty["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reviewer| reviewer["id"] == "correctness" && reviewer["built_in"] == true)
    );
    assert!(
        empty["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reviewer| reviewer["id"] != "review")
    );
    let default_correctness = empty["reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reviewer| reviewer["id"] == "correctness")
        .unwrap()
        .clone();

    let response = client
        .put(format!("http://{addr}/v1/personas/correctness"))
        .json(&serde_json::json!({
            "display_name": "Correctness",
            "group": "reviewer",
            "system_prompt": "Check correctness.",
            "allowed_tools": [],
            "read_only": true,
            "default_model": "anthropic/claude",
            "default_thinking_level": "high"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    let custom_id = "widget-invariants";
    let response = client
        .put(format!("http://{addr}/v1/personas/{custom_id}"))
        .json(&serde_json::json!({
            "display_name": "Widget invariants",
            "group": "reviewer",
            "system_prompt": "Check every widget state transition.",
            "allowed_tools": [],
            "read_only": true,
            "default_model": "openai/gpt-5",
            "default_thinking_level": "medium"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    let response = client
        .put(format!("{base}/repository"))
        .json(&serde_json::json!({
            "installation_id": 7,
            "repository": "acme/widgets",
            "mode": "automatic",
            "model": "anthropic/claude",
            "coordinator_thinking_level": "high",
            "router_model": "anthropic/claude",
            "router_thinking_level": "low",
            "prompt": "focus on concurrency",
            "reviewer_ids": ["correctness", custom_id],
            "routing_mode": "additive",
            "semantic_routing": true,
            "included_reviewer_ids": [custom_id, "reliability"],
            "excluded_reviewer_ids": ["operations"],
            "reviewer_overrides": [
                {
                    "reviewer_id": "correctness",
                    "model": "anthropic/claude",
                    "thinking_level": "low",
                    "prompt_mode": "append",
                    "prompt": "Focus on widget lifecycle boundaries."
                },
                {
                    "reviewer_id": custom_id,
                    "prompt_mode": "replace",
                    "prompt": "Apply the repository's widget state machine."
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());

    let dashboard_response = client.get(&base).send().await.unwrap();
    let dashboard_cursor = dashboard_response
        .headers()
        .get(trouve_protocol::EVENT_CURSOR_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(dashboard_cursor > empty_cursor);
    let dashboard: serde_json::Value = dashboard_response.json().await.unwrap();
    assert_eq!(dashboard["repositories"][0]["repository"], "acme/widgets");
    assert_eq!(dashboard["repositories"][0]["mode"], "automatic");
    assert_eq!(dashboard["repositories"][0]["model"], "anthropic/claude");
    assert_eq!(
        dashboard["repositories"][0]["coordinator_thinking_level"],
        "high"
    );
    assert_eq!(
        dashboard["repositories"][0]["router_model"],
        "anthropic/claude"
    );
    assert_eq!(dashboard["repositories"][0]["router_thinking_level"], "low");
    assert_eq!(
        dashboard["repositories"][0]["reviewer_ids"],
        serde_json::json!(["correctness", custom_id])
    );
    assert_eq!(dashboard["repositories"][0]["routing_mode"], "additive");
    assert_eq!(dashboard["repositories"][0]["semantic_routing"], true);
    assert_eq!(
        dashboard["repositories"][0]["included_reviewer_ids"],
        serde_json::json!([custom_id, "reliability"])
    );
    assert_eq!(
        dashboard["repositories"][0]["excluded_reviewer_ids"],
        serde_json::json!(["operations"])
    );
    assert_eq!(
        dashboard["repositories"][0]["reviewer_overrides"][0]["model"],
        "anthropic/claude"
    );
    assert_eq!(
        dashboard["repositories"][0]["reviewer_overrides"][0]["thinking_level"],
        "low"
    );
    assert_eq!(
        dashboard["repositories"][0]["reviewer_overrides"][1]["prompt_mode"],
        "replace"
    );
    assert!(
        dashboard["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reviewer| reviewer["id"] == "correctness"
                && reviewer["model"] == "anthropic/claude"
                && reviewer["default_thinking_level"] == "high")
    );

    let deleted = client
        .delete(format!("http://{addr}/v1/personas/{custom_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let reset_built_in = client
        .delete(format!("http://{addr}/v1/personas/correctness"))
        .send()
        .await
        .unwrap();
    assert_eq!(reset_built_in.status(), reqwest::StatusCode::NO_CONTENT);

    let dashboard: serde_json::Value = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        dashboard["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reviewer| reviewer["id"] != custom_id)
    );
    let reset_correctness = dashboard["reviewers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reviewer| reviewer["id"] == "correctness")
        .unwrap();
    assert_eq!(reset_correctness, &default_correctness);
    assert_eq!(
        dashboard["repositories"][0]["reviewer_ids"],
        serde_json::json!(["correctness"])
    );
    assert_eq!(
        dashboard["repositories"][0]["included_reviewer_ids"],
        serde_json::json!(["reliability"])
    );
    assert_eq!(
        dashboard["repositories"][0]["reviewer_overrides"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn code_review_job_overview_loads_task_content_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );
    let queued = engine
        .store()
        .enqueue_code_review_job(&NewCodeReviewJob {
            dedupe_key: "acme/widgets#42:lazy-detail".into(),
            installation_id: 7,
            repository: "acme/widgets".into(),
            pull_number: 42,
            pull_title: "Ship widgets".into(),
            pull_body: String::new(),
            pull_url: "https://github.com/acme/widgets/pull/42".into(),
            head_sha: "2222222222222222222222222222222222222222".into(),
            review_base_sha: "1111111111111111111111111111111111111111".into(),
            base_ref: "main".into(),
            head_ref: "ship".into(),
            scope: trouve_protocol::CodeReviewJobScope::Incremental,
            trigger: "automatic".into(),
            retry_of: None,
            model: Some("provider/model".into()),
            coordinator_thinking_level: Some("medium".into()),
            router_model: Some("provider/router".into()),
            router_thinking_level: Some("low".into()),
            prompt: "Review it".into(),
            reviewers: Vec::new(),
            routing_mode: trouve_protocol::CodeReviewRoutingMode::Manual,
            semantic_routing: false,
            included_reviewer_ids: Vec::new(),
            excluded_reviewer_ids: Vec::new(),
            config_hash: "config".into(),
        })
        .unwrap()
        .unwrap();
    engine.store().claim_code_review_job().unwrap().unwrap();
    engine
        .store()
        .save_code_review_routing_decisions(
            &queued.id,
            &[trouve_protocol::CodeReviewRoutingDecision {
                batch_index: 0,
                reviewer_id: "correctness".into(),
                reviewer_name: "Correctness".into(),
                selected: true,
                reasons: vec![trouve_protocol::CodeReviewRoutingReason {
                    source: trouve_protocol::CodeReviewRoutingSource::Core,
                    detail: "selected by the repository's Manual persona set".into(),
                }],
            }],
        )
        .unwrap();
    let task = engine
        .store()
        .create_code_review_task(&NewCodeReviewTask {
            job_id: queued.id.clone(),
            role: trouve_protocol::CodeReviewTaskRole::Reviewer,
            reviewer_id: Some("correctness".into()),
            reviewer_name: "Correctness".into(),
            batch_index: 0,
            batch_count: 1,
            model: Some("provider/model".into()),
            prompt: "Retained task prompt".into(),
        })
        .unwrap();
    engine
        .store()
        .start_code_review_task(&task.id, "session", "thread", "provider/model")
        .unwrap()
        .unwrap();
    engine
        .store()
        .append_code_review_task_output(
            &task.id,
            trouve_protocol::CodeReviewOutputStream::Assistant,
            "retained assistant output",
        )
        .unwrap();
    let snapshot_event = engine
        .store()
        .append_event(
            trouve_protocol::Scope::CodeReviewJob(queued.id.clone()),
            trouve_protocol::Event::CodeReviewJobUpdated {
                job_id: queued.id.clone(),
            },
        )
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{addr}/v1/code-review/jobs/{}", queued.id);
    let client = reqwest::Client::new();

    let overview: serde_json::Value = client
        .get(format!("{base}?include_task_content=false"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(overview["tasks"][0]["id"], task.id);
    assert_eq!(overview["tasks"][0]["status"], "running");
    assert_eq!(overview["event_cursor"], snapshot_event.cursor);
    assert_eq!(overview["job"]["routing_mode"], "manual");
    assert_eq!(overview["job"]["model"], "provider/model");
    assert_eq!(overview["job"]["router_model"], "provider/router");
    assert_eq!(overview["job"]["router_thinking_level"], "low");
    assert_eq!(overview["job"]["coordinator_thinking_level"], "medium");
    assert_eq!(
        overview["routing_decisions"][0]["reviewer_id"],
        "correctness"
    );
    assert_eq!(
        overview["routing_decisions"][0]["reasons"][0]["source"],
        "core"
    );
    assert!(overview["tasks"][0]["prompt"].is_null());
    assert!(overview["tasks"][0]["output"].is_null());

    let full: serde_json::Value = client
        .get(&base)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(full["tasks"][0]["prompt"], "Retained task prompt");
    assert_eq!(full["tasks"][0]["output"], "retained assistant output");

    let retained: serde_json::Value = client
        .get(format!("{base}/tasks/{}", task.id))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(retained["prompt"], "Retained task prompt");
    assert_eq!(retained["output"], "retained assistant output");

    let wrong_job = client
        .get(format!(
            "http://{addr}/v1/code-review/jobs/not-this-job/tasks/{}",
            task.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_job.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn provider_login_callback_endpoint_validates_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let endpoint = format!("http://{addr}/v1/providers/claude-code/login/callback");
    let client = reqwest::Client::new();

    let malformed = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "callback_url": "http://localhost/callback\ninjected"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);

    let absent = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "callback_url": "http://localhost/callback?code=test&state=test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(absent.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn server_projection_returns_cached_state_with_a_resume_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db/trouve.db")).unwrap();
    let expected_cursor = store
        .append_event(
            trouve_protocol::Scope::Server,
            trouve_protocol::Event::GithubPullRequestsUpdated {
                pull_requests: trouve_protocol::GithubPrList {
                    viewer: "octocat".into(),
                    host: "github.com".into(),
                    prs: Vec::new(),
                },
            },
        )
        .unwrap()
        .cursor;
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default()).with_config_dir(None),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let response = reqwest::get(format!("http://{addr}/v1/server-projection"))
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(trouve_protocol::EVENT_CURSOR_HEADER)
            .unwrap(),
        expected_cursor.to_string().as_str()
    );
    let projection: trouve_protocol::ServerProjection = response.json().await.unwrap();
    assert_eq!(projection.github_pull_requests.len(), 1);
    assert_eq!(projection.github_pull_requests[0].cursor, expected_cursor);
    assert!(projection.session_pull_requests.is_empty());
}
