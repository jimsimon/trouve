//! End-to-end protocol test: a scripted provider drives the real server,
//! event streams, approval flow, checkpointing, and undo — no network, no
//! real model.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use trouve_core::config::Config;
use trouve_core::store::Store;
use trouve_core::Engine;
use trouve_protocol::Usage;
use trouve_providers::{
    EventStream, Message, Provider, ProviderError, ProviderEvent, ToolCallRequest, ToolSpec,
};

/// Turn 1: asks to write hello.txt, then finishes with a message.
struct ScriptedProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Provider for ScriptedProvider {
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
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
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
    assert!(session["branch"]
        .as_str()
        .unwrap()
        .starts_with("trouve/test-session"));
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

    // Approve; the turn then finishes with a checkpoint.
    let resp = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({"call_id": call_id, "decision": "approve"}))
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
    assert_eq!(completed["usage"]["input_tokens"], 30);
    assert!(events
        .iter()
        .any(|e| e["type"] == "tool.completed" && e["status"] == "ok"));
    assert_eq!(
        std::fs::read_to_string(Path::new(&worktree).join("hello.txt")).unwrap(),
        "hi\n"
    );

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
    assert!(events
        .iter()
        .any(|e| e["type"] == "thread.compaction_started"));
    let completed = events
        .iter()
        .find(|e| e["type"] == "thread.compaction_completed")
        .expect("compaction completes");
    assert!(completed["messages_compacted"].as_u64().unwrap() >= 2);
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
    assert!(branches["branches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b == "main"));

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

    // Thread creation succeeds even with an unconfigured model (validation
    // is deferred to send time), then PATCH switches mode/model.
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session_id,
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

    // Known-provider presets: static catalog with prefill data.
    let known: serde_json::Value = client
        .get(format!("{base}/providers/known"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let known = known.as_array().unwrap();
    assert!(known.len() >= 10);
    let openrouter = known
        .iter()
        .find(|k| k["id"] == "openrouter")
        .expect("openrouter preset");
    assert_eq!(openrouter["kind"], "openai-compat");
    assert_eq!(openrouter["base_url"], "https://openrouter.ai/api/v1");
    assert_eq!(openrouter["api_key_env"], "OPENROUTER_API_KEY");
    assert_eq!(openrouter["auth"], "api-key");
    assert!(known.iter().any(|k| k["id"] == "anthropic"));
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
    assert_eq!(cursor_api["api_key_env"], "CURSOR_API_KEY");
    // The direct-Codex client is flagged experimental (undocumented endpoint).
    let codex_api = known
        .iter()
        .find(|k| k["id"] == "codex-api")
        .expect("codex-api preset");
    assert_eq!(codex_api["kind"], "codex-responses");
    assert_eq!(codex_api["auth"], "cli");
    assert_eq!(codex_api["experimental"], true);

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
    assert!(providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "openrouter"));

    // Default model change persists.
    let resp = client
        .put(format!("{base}/config/default-model"))
        .json(&serde_json::json!({"model": "scripted/test-model"}))
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
    assert!(!providers["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == "openrouter"));
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
    assert!(events
        .iter()
        .any(|e| e["type"] == "tool.completed" && e["status"] == "denied"));
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
        .json(&serde_json::json!({"request_id": "bogus", "answers": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Answer: single choice + a multi-choice with an "Other" free-form.
    let resp = client
        .post(format!("{base}/questions"))
        .json(&serde_json::json!({
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
            Message::ToolResult { call_id, content } if call_id == "q_call_1" => Some(content),
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

/// Scripted `AgentBackend`: every turn asks for approval of one "command",
/// writes a file to the worktree when approved, and completes with usage.
/// Records the vendor session id it was resumed with, per turn.
struct ScriptedBackend {
    sessions_seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl ScriptedBackend {
    fn new() -> Self {
        Self {
            sessions_seen: std::sync::Mutex::new(Vec::new()),
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
    let engine = Arc::new(
        Engine::new(store, tmp.path().join("data"), &Config::default())
            .with_config_dir(None)
            .with_backend("fake-agent", backend.clone())
            .with_default_model("fake-agent/agent-model"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
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
    assert!(models
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "fake-agent/agent-model"));

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

    let resp = client
        .post(format!("{base}/approvals"))
        .json(&serde_json::json!({"call_id": call_id, "decision": "approve"}))
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
    assert!(events
        .iter()
        .any(|e| e["type"] == "tool.completed" && e["status"] == "ok"));
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

    // Embedded MCP bridge endpoint: a bridged vendor agent can list and
    // call trouve tools for this thread through the engine's gate.
    let mcp_url = format!("http://{addr}/internal/threads/{thread_id}/mcp?tools=1&approval=1");
    let mcp = |body: serde_json::Value| {
        let client = client.clone();
        let url = mcp_url.clone();
        async move {
            client
                .post(url)
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
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
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file") && names.contains(&"write_file"));
    assert!(names.contains(&"approval_prompt"));

    // Non-mutating call runs without approval (thread is yolo by now anyway).
    let called = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "list_dir", "arguments": {"path": "."}}
    }))
    .await;
    assert!(
        called["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("agent.txt"),
        "{called}"
    );

    // Vendor-executed tool gating (Claude's permission-prompt hook): the
    // thread is yolo by now, so the gate auto-approves.
    let verdict = mcp(serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "approval_prompt",
                   "arguments": {"tool_name": "Bash", "input": {"command": "ls"}}}
    }))
    .await;
    let text = verdict["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"behavior\":\"allow\""), "{verdict}");

    // CLI-kind provider CRUD: upsert reports auth "cli"; login relays the
    // vendor flow (here: a bogus binary, so it fails with 400).
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
    // Claude Code's login is an interactive TUI; we surface instructions.
    assert_eq!(resp.status(), 400);
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
    let second = send("two").await;
    assert_eq!(second["queued"], true);
    assert_eq!(second["turn"], 0);
    send("three").await;

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
    let id_two = queue[0]["id"].as_str().unwrap().to_string();
    let id_three = queue[1]["id"].as_str().unwrap().to_string();

    // Edit a queued prompt.
    let resp = client
        .patch(format!("{base}/queue/{id_two}"))
        .json(&serde_json::json!({"content": "two v2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

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
    store.enqueue_prompt(thread_id, "left-behind-1").unwrap();
    store.enqueue_prompt(thread_id, "left-behind-2").unwrap();

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
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(bytes) = b64.decode(data.trim()) {
                        out.extend_from_slice(&bytes);
                    }
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
