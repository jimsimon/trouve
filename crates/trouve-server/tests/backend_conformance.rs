//! Cross-path conformance tests for the seamless agent experience.
//!
//! A raw provider and a vendor-owned backend intentionally have different
//! internal loops. This suite holds them to the same protocol-visible turn
//! contract instead of exposing that implementation detail to clients.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use trouve_agents::{
    AgentBackend, BackendError, BackendEvent, BackendEventStream, BackendLogin, BackendPermission,
    BackendStatus, BackendTurn,
};
use trouve_core::Engine;
use trouve_core::config::Config;
use trouve_core::store::Store;
use trouve_protocol::{ModelInfo, Usage};
use trouve_providers::{EventStream, Message, Provider, ProviderError, ProviderEvent, ToolSpec};

const ANSWER: &str = "The two execution paths present one experience.";
const THINKING: &str = "Checking the shared contract.";

fn model(id: &str, name: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        display_name: name.into(),
        context_window: 100_000,
        supports_tools: true,
        input_price_per_mtok: None,
        output_price_per_mtok: None,
        options_schema: serde_json::json!({"type":"object", "properties":{}}),
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 12,
        output_tokens: 7,
        cached_input_tokens: 3,
        context_input_tokens: Some(15),
        context_window: Some(100_000),
        cost_usd: None,
    }
}

#[derive(Default)]
struct ProviderObservation {
    saw_system_instructions: bool,
    saw_user_prompt: bool,
}

struct ConformanceProvider {
    observation: Arc<Mutex<ProviderObservation>>,
}

#[async_trait::async_trait]
impl Provider for ConformanceProvider {
    fn id(&self) -> &str {
        "raw"
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![model("raw/model", "Raw provider")]
    }

    async fn stream_chat(
        &self,
        _model: &str,
        messages: &[Message],
        _tools: &[ToolSpec],
        _options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<EventStream, ProviderError> {
        let mut observation = self.observation.lock().unwrap();
        observation.saw_system_instructions = messages
            .iter()
            .any(|message| matches!(message, Message::System(text) if !text.trim().is_empty()));
        observation.saw_user_prompt = messages
            .iter()
            .any(|message| matches!(message, Message::User(text) if text == "verify parity"));
        drop(observation);
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ProviderEvent::ThinkingDelta(THINKING.into())),
            Ok(ProviderEvent::TextDelta(ANSWER.into())),
            Ok(ProviderEvent::Completed { usage: usage() }),
        ])))
    }
}

#[derive(Default)]
struct BackendObservation {
    saw_mode_instructions: bool,
    saw_user_prompt: bool,
    permission: Option<BackendPermission>,
}

struct ConformanceBackend {
    observation: Arc<Mutex<BackendObservation>>,
}

#[async_trait::async_trait]
impl AgentBackend for ConformanceBackend {
    fn id(&self) -> &str {
        "vendor"
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![model("vendor/model", "Vendor backend")]
    }

    fn status(&self) -> BackendStatus {
        BackendStatus {
            installed: true,
            has_credentials: true,
        }
    }

    async fn start_login(&self) -> Result<BackendLogin, BackendError> {
        Err(BackendError::Auth("test backend needs no login".into()))
    }

    async fn run_turn(&self, turn: BackendTurn) -> Result<BackendEventStream, BackendError> {
        let mut observation = self.observation.lock().unwrap();
        observation.saw_mode_instructions = turn
            .instructions
            .as_deref()
            .is_some_and(|instructions| !instructions.trim().is_empty());
        observation.saw_user_prompt = turn.prompt == "verify parity";
        observation.permission = Some(turn.permission);
        drop(observation);
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(BackendEvent::SessionStarted {
                session_id: "vendor-session".into(),
            }),
            Ok(BackendEvent::ThinkingDelta(THINKING.into())),
            Ok(BackendEvent::ThinkingCompleted),
            Ok(BackendEvent::TextDelta(ANSWER.into())),
            Ok(BackendEvent::Completed { usage: usage() }),
        ])))
    }
}

fn init_repo(directory: &Path) {
    let run = |arguments: &[&str]| {
        let mut command = Command::new("git");
        command.arg("-C").arg(directory).args(arguments);
        assert!(
            trouve_process::output(&mut command)
                .unwrap()
                .status
                .success(),
            "git {arguments:?} failed"
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "conformance@example.com"]);
    run(&["config", "user.name", "Conformance"]);
    std::fs::write(directory.join("README.md"), "# conformance\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-m", "init"]);
}

async fn wait_for_completion(client: &reqwest::Client, url: &str) -> Vec<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let response = client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let mut bytes = response.bytes_stream();
        let mut buffer = String::new();
        let mut events = Vec::new();
        while let Some(chunk) = bytes.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim().to_string();
                buffer.drain(..=newline);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let event: serde_json::Value = serde_json::from_str(data.trim()).unwrap();
                let complete = event["type"] == "turn.completed";
                events.push(event);
                if complete {
                    return events;
                }
            }
        }
        panic!("event stream ended before turn.completed")
    })
    .await
    .expect("timed out waiting for conformance turn")
}

async fn run_visible_turn(
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    selected_model: &str,
) -> Vec<serde_json::Value> {
    let thread: serde_json::Value = client
        .post(format!("{base}/threads"))
        .json(&serde_json::json!({
            "session_id": session_id,
            "model": selected_model,
            "mode": "plan"
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
    client
        .post(format!("{base}/threads/{thread_id}/messages"))
        .json(&serde_json::json!({"content":"verify parity"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_for_completion(client, &format!("{base}/threads/{thread_id}/events")).await
}

#[derive(Debug, PartialEq, Eq)]
struct VisibleTurn {
    user_message: String,
    thinking: String,
    thinking_completed: bool,
    answer: String,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    context_input_tokens: u64,
    completed: bool,
    failed: bool,
}

fn exact_event_index(events: &[serde_json::Value], kind: &str) -> Result<usize, String> {
    let indices = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| (event["type"] == kind).then_some(index))
        .collect::<Vec<_>>();
    if indices.len() != 1 {
        return Err(format!(
            "expected exactly one {kind} event, observed {}",
            indices.len()
        ));
    }
    Ok(indices[0])
}

fn required_u64(event: &serde_json::Value, pointer: &str) -> Result<u64, String> {
    event
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| format!("event omitted integral {pointer}"))
}

fn fold_visible_turn(events: &[serde_json::Value]) -> Result<VisibleTurn, String> {
    let user_index = exact_event_index(events, "user.message")?;
    let thinking_indices = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| (event["type"] == "assistant.thinking").then_some(index))
        .collect::<Vec<_>>();
    if thinking_indices.is_empty() {
        return Err("expected at least one assistant.thinking event".into());
    }
    let thinking_completed_index = exact_event_index(events, "assistant.thinking_completed")?;
    let answer_index = exact_event_index(events, "assistant.message")?;
    let completed_index = exact_event_index(events, "turn.completed")?;
    let conflicting_terminals = events
        .iter()
        .filter(|event| {
            matches!(
                event["type"].as_str(),
                Some("turn.failed" | "turn.cancelled")
            )
        })
        .count();
    if conflicting_terminals != 0 {
        return Err(format!(
            "completed turn also contained {conflicting_terminals} failure or cancellation event(s)"
        ));
    }
    if completed_index + 1 != events.len() {
        return Err("turn.completed was not the final observed event".into());
    }
    if user_index >= thinking_indices[0]
        || thinking_indices
            .iter()
            .any(|index| *index >= thinking_completed_index)
        || thinking_completed_index >= answer_index
        || answer_index >= completed_index
    {
        return Err("visible turn lifecycle events were out of order".into());
    }

    let user_message = events[user_index]["content"]
        .as_str()
        .ok_or_else(|| "user.message omitted content".to_string())?
        .to_string();
    let thinking = thinking_indices
        .iter()
        .map(|index| {
            events[*index]["text"]
                .as_str()
                .ok_or_else(|| "assistant.thinking omitted text".to_string())
        })
        .collect::<Result<String, _>>()?;
    let answer = events[answer_index]["content"]
        .as_str()
        .ok_or_else(|| "assistant.message omitted content".to_string())?
        .to_string();
    let completed = &events[completed_index];
    Ok(VisibleTurn {
        user_message,
        thinking,
        thinking_completed: true,
        answer,
        input_tokens: required_u64(completed, "/usage/input_tokens")?,
        cached_input_tokens: required_u64(completed, "/usage/cached_input_tokens")?,
        output_tokens: required_u64(completed, "/usage/output_tokens")?,
        context_input_tokens: required_u64(completed, "/usage/context_input_tokens")?,
        completed: true,
        failed: false,
    })
}

fn valid_visible_turn_events() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({"type":"user.message", "content":"verify parity"}),
        serde_json::json!({"type":"assistant.thinking", "text":THINKING}),
        serde_json::json!({"type":"assistant.thinking_completed"}),
        serde_json::json!({"type":"assistant.message", "content":ANSWER}),
        serde_json::json!({
            "type":"turn.completed",
            "usage":{
                "input_tokens":12,
                "cached_input_tokens":3,
                "output_tokens":7,
                "context_input_tokens":15
            }
        }),
    ]
}

#[test]
fn visible_turn_fold_rejects_malformed_lifecycle_histories() {
    let valid = valid_visible_turn_events();
    assert!(fold_visible_turn(&valid).is_ok());

    let mut duplicate_message = valid.clone();
    duplicate_message.insert(
        duplicate_message.len() - 1,
        serde_json::json!({"type":"assistant.message", "content":"duplicate"}),
    );
    assert!(
        fold_visible_turn(&duplicate_message)
            .unwrap_err()
            .contains("exactly one assistant.message")
    );

    let missing_boundary = valid
        .iter()
        .filter(|event| event["type"] != "assistant.thinking_completed")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        fold_visible_turn(&missing_boundary)
            .unwrap_err()
            .contains("exactly one assistant.thinking_completed")
    );

    let mut contradictory_terminal = valid.clone();
    contradictory_terminal.insert(
        contradictory_terminal.len() - 1,
        serde_json::json!({"type":"turn.failed"}),
    );
    assert!(
        fold_visible_turn(&contradictory_terminal)
            .unwrap_err()
            .contains("failure or cancellation")
    );

    let mut out_of_order = valid;
    out_of_order.swap(2, 3);
    assert!(
        fold_visible_turn(&out_of_order)
            .unwrap_err()
            .contains("out of order")
    );
}

#[tokio::test]
async fn raw_provider_and_vendor_backend_share_the_visible_turn_contract() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repo");
    std::fs::create_dir(&repository).unwrap();
    init_repo(&repository);

    let provider_observation = Arc::new(Mutex::new(ProviderObservation::default()));
    let backend_observation = Arc::new(Mutex::new(BackendObservation::default()));
    let engine = Arc::new(
        Engine::new(
            Store::open(&temporary.path().join("db/trouve.db")).unwrap(),
            temporary.path().join("data"),
            &Config {
                local_enabled: Some(false),
                ..Default::default()
            },
        )
        .with_config_dir(None)
        .with_provider(
            "raw",
            Arc::new(ConformanceProvider {
                observation: provider_observation.clone(),
            }),
        )
        .with_backend(
            "vendor",
            Arc::new(ConformanceBackend {
                observation: backend_observation.clone(),
            }),
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = trouve_server::build_router(engine);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let base = format!("http://{address}/v1");
    let client = reqwest::Client::new();

    let workspace: serde_json::Value = client
        .post(format!("{base}/workspaces"))
        .json(&serde_json::json!({"path":repository}))
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
        .json(&serde_json::json!({"workspace_id":workspace["id"]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let session_id = session["id"].as_str().unwrap();

    let raw = run_visible_turn(&client, &base, session_id, "raw/model").await;
    let vendor = run_visible_turn(&client, &base, session_id, "vendor/model").await;
    assert_eq!(
        fold_visible_turn(&raw).expect("raw provider emitted a malformed visible lifecycle"),
        fold_visible_turn(&vendor).expect("vendor backend emitted a malformed visible lifecycle")
    );

    let raw_observation = provider_observation.lock().unwrap();
    assert!(raw_observation.saw_system_instructions);
    assert!(raw_observation.saw_user_prompt);
    let vendor_observation = backend_observation.lock().unwrap();
    assert!(vendor_observation.saw_mode_instructions);
    assert!(vendor_observation.saw_user_prompt);
    assert_eq!(
        vendor_observation.permission,
        Some(BackendPermission::ReadOnly)
    );
}
