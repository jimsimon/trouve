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
use trouve_protocol::{ModelInfo, Scope, Usage};
use trouve_providers::{EventStream, Message, Provider, ProviderError, ProviderEvent, ToolSpec};

const ANSWER: &str = "The two execution paths present one experience.";
const THINKING: &str = "Checking the shared contract.";

fn model(id: &str, name: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        display_name: name.into(),
        context_window: 100_000,
        supports_tools: true,
        supports_images: true,
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
    system: String,
    user_messages: Vec<String>,
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
        observation.system = messages
            .iter()
            .find_map(|message| match message {
                Message::System(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        observation.user_messages = messages
            .iter()
            .filter_map(|message| match message {
                Message::User(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        drop(observation);
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ProviderEvent::ThinkingStarted {
                id: "reasoning".into(),
            }),
            Ok(ProviderEvent::ThinkingDelta {
                id: "reasoning".into(),
                text: THINKING.into(),
            }),
            Ok(ProviderEvent::ThinkingCompleted {
                id: "reasoning".into(),
            }),
            Ok(ProviderEvent::TextDelta(ANSWER.into())),
            Ok(ProviderEvent::Completed { usage: usage() }),
        ])))
    }
}

#[derive(Default)]
struct BackendObservation {
    instructions: String,
    prompt: String,
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
        observation.instructions = turn.instructions.clone().unwrap_or_default();
        observation.prompt = turn.prompt.clone();
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
    engine: &Engine,
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    selected_model: &str,
) -> Vec<serde_json::Value> {
    run_visible_turn_with(
        engine,
        client,
        base,
        session_id,
        selected_model,
        "verify parity",
    )
    .await
}

async fn run_visible_turn_with(
    engine: &Engine,
    client: &reqwest::Client,
    base: &str,
    session_id: &str,
    selected_model: &str,
    content: &str,
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
        .json(&serde_json::json!({"content":content}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    wait_for_completion(client, &format!("{base}/threads/{thread_id}/events")).await;
    engine
        .store()
        .events_after(&Scope::Thread(thread_id.to_string()), 0)
        .unwrap()
        .into_iter()
        .map(|envelope| serde_json::to_value(envelope).unwrap())
        .collect()
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

    let mut post_terminal = valid.clone();
    post_terminal.push(serde_json::json!({
        "type":"assistant.progress",
        "text":"late output"
    }));
    assert!(
        fold_visible_turn(&post_terminal)
            .unwrap_err()
            .contains("final observed event")
    );

    let mut out_of_order = valid;
    out_of_order.swap(2, 3);
    assert!(
        fold_visible_turn(&out_of_order)
            .unwrap_err()
            .contains("out of order")
    );
}

struct Harness {
    /// Owns the repository and data directories for the harness lifetime.
    temporary: tempfile::TempDir,
    repository: std::path::PathBuf,
    engine: Arc<Engine>,
    client: reqwest::Client,
    base: String,
    session_id: String,
    provider_observation: Arc<Mutex<ProviderObservation>>,
    backend_observation: Arc<Mutex<BackendObservation>>,
}

async fn harness() -> Harness {
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
    let router = trouve_server::build_router(engine.clone());
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
    let session_id = session["id"].as_str().unwrap().to_string();
    Harness {
        temporary,
        repository,
        engine,
        client,
        base,
        session_id,
        provider_observation,
        backend_observation,
    }
}

#[tokio::test]
async fn raw_provider_and_vendor_backend_share_the_visible_turn_contract() {
    let Harness {
        temporary,
        engine,
        client,
        base,
        session_id,
        provider_observation,
        backend_observation,
        ..
    } = harness().await;

    let raw = run_visible_turn(&engine, &client, &base, &session_id, "raw/model").await;
    let vendor = run_visible_turn(&engine, &client, &base, &session_id, "vendor/model").await;
    assert_eq!(
        fold_visible_turn(&raw).expect("raw provider emitted a malformed visible lifecycle"),
        fold_visible_turn(&vendor).expect("vendor backend emitted a malformed visible lifecycle")
    );

    let raw_observation = provider_observation.lock().unwrap();
    assert!(!raw_observation.system.trim().is_empty());
    assert!(
        raw_observation
            .user_messages
            .iter()
            .any(|text| text == "verify parity")
    );
    let vendor_observation = backend_observation.lock().unwrap();
    assert!(!vendor_observation.instructions.trim().is_empty());
    assert_eq!(vendor_observation.prompt, "verify parity");
    assert_eq!(
        vendor_observation.permission,
        Some(BackendPermission::ReadOnly)
    );
    drop(temporary);
}

/// Skills are discovered by the engine from `.agents/skills`, published to
/// clients as slash commands, advertised to both execution paths, and a
/// leading `/skill` is expanded before any model sees the prompt while the
/// transcript keeps the user's words.
#[tokio::test]
async fn engine_owned_skills_reach_raw_providers_and_vendor_backends_alike() {
    let Harness {
        temporary,
        repository,
        engine,
        client,
        base,
        session_id,
        provider_observation,
        backend_observation,
        ..
    } = harness().await;
    let skill_dir = repository.join(".agents/skills/ship");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ship\ndescription: Ship a release\n---\n# Ship\n\nRun the release checklist.\n",
    )
    .unwrap();

    let roster_of = |events: &[serde_json::Value]| -> Vec<Vec<String>> {
        events
            .iter()
            .filter(|event| event["type"] == "thread.commands_updated")
            .map(|event| {
                event["commands"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|command| command["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .collect()
    };

    let raw = run_visible_turn_with(
        &engine,
        &client,
        &base,
        &session_id,
        "raw/model",
        "/ship to staging",
    )
    .await;
    // Once at thread creation, once at turn start; both name the skill.
    let raw_rosters = roster_of(&raw);
    assert_eq!(raw_rosters.len(), 2, "{raw:#?}");
    assert!(
        raw_rosters
            .iter()
            .all(|roster| roster == &["ship".to_string()])
    );
    let transcript_message = raw
        .iter()
        .find(|event| event["type"] == "user.message")
        .unwrap();
    assert_eq!(transcript_message["content"], "/ship to staging");
    {
        let observation = provider_observation.lock().unwrap();
        assert!(observation.system.contains("## Available skills"));
        assert!(observation.system.contains("<invoked_skill name=\"ship\""));
        assert!(observation.system.contains("Run the release checklist."));
        assert!(observation.system.contains("to staging"));
        assert_eq!(
            observation.user_messages,
            vec!["/ship to staging".to_string()]
        );
    }

    let vendor = run_visible_turn_with(
        &engine,
        &client,
        &base,
        &session_id,
        "vendor/model",
        "/ship to staging",
    )
    .await;
    assert_eq!(roster_of(&vendor).len(), 2);
    let transcript_message = vendor
        .iter()
        .find(|event| event["type"] == "user.message")
        .unwrap();
    assert_eq!(transcript_message["content"], "/ship to staging");
    let observation = backend_observation.lock().unwrap();
    assert!(observation.instructions.contains("## Available skills"));
    assert!(observation.instructions.contains("**ship**"));
    assert!(
        observation
            .prompt
            .starts_with("The user invoked the `ship` skill")
    );
    assert!(observation.prompt.contains("Run the release checklist."));
    assert!(observation.prompt.ends_with("to staging"));
    assert!(!observation.prompt.contains("/ship to staging"));
    drop(temporary);
}
