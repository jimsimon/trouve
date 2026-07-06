//! Thin protocol client and the interactive chat loop.

use std::io::Write as _;

use anyhow::{bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};

pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(server: String) -> Self {
        Self {
            base: format!("{}/v1", server.trim_end_matches('/')),
            http: reqwest::Client::new(),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path} (is the server running? try `trouve serve`)"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let value: Value = resp.json().await?;
        if !status.is_success() {
            bail!(
                "{path}: {} ({})",
                value["message"].as_str().unwrap_or("error"),
                status
            );
        }
        Ok(value)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("GET {path} (is the server running? try `trouve serve`)"))?;
        let status = resp.status();
        let value: Value = resp.json().await?;
        if !status.is_success() {
            bail!(
                "{path}: {} ({})",
                value["message"].as_str().unwrap_or("error"),
                status
            );
        }
        Ok(value)
    }

    pub async fn register_workspace(&self, path: &str) -> Result<Value> {
        self.post("/workspaces", json!({"path": path})).await
    }

    pub async fn list_models(&self) -> Result<Vec<Value>> {
        Ok(self
            .get("/models")
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Value>> {
        Ok(self
            .get("/workspaces")
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub async fn create_session(
        &self,
        workspace_id: &str,
        title: Option<&str>,
        base_ref: Option<&str>,
    ) -> Result<Value> {
        let mut body = json!({"workspace_id": workspace_id});
        if let Some(t) = title {
            body["title"] = t.into();
        }
        if let Some(r) = base_ref {
            body["base_ref"] = r.into();
        }
        self.post("/sessions", body).await
    }

    pub async fn list_sessions(&self) -> Result<Vec<Value>> {
        Ok(self
            .get("/sessions")
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}/sessions/{id}", self.base))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("delete failed: {}", resp.status());
        }
        Ok(())
    }

    pub async fn undo(&self, id: &str) -> Result<()> {
        self.post(&format!("/sessions/{id}/undo"), json!({}))
            .await?;
        Ok(())
    }

    pub async fn redo(&self, id: &str) -> Result<()> {
        self.post(&format!("/sessions/{id}/redo"), json!({}))
            .await?;
        Ok(())
    }

    pub async fn create_thread(
        &self,
        session_id: &str,
        mode: Option<&str>,
        model: Option<&str>,
        permissions: Option<&str>,
    ) -> Result<Value> {
        let mut body = json!({"session_id": session_id});
        if let Some(m) = mode {
            body["mode"] = m.into();
        }
        if let Some(m) = model {
            body["model"] = m.into();
        }
        if let Some(p) = permissions {
            body["permission_mode"] = p.into();
        }
        self.post("/threads", body).await
    }

    pub async fn send_message(&self, thread_id: &str, content: &str) -> Result<Value> {
        self.post(
            &format!("/threads/{thread_id}/messages"),
            json!({"content": content}),
        )
        .await
    }

    pub async fn resolve_approval(&self, call_id: &str, decision: &str) -> Result<()> {
        self.post(
            "/approvals",
            json!({"call_id": call_id, "decision": decision}),
        )
        .await?;
        Ok(())
    }

    /// Open the thread event stream after `cursor` and hand each event to
    /// `on_event`; returns when the callback says stop.
    pub async fn follow_events(
        &self,
        thread_id: &str,
        cursor: u64,
        mut on_event: impl AsyncFnMut(Value) -> Result<EventFlow>,
    ) -> Result<u64> {
        let resp = self
            .http
            .get(format!(
                "{}/threads/{thread_id}/events?after={cursor}",
                self.base
            ))
            .send()
            .await?;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut last = cursor;
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let event: Value = serde_json::from_str(data.trim())?;
                if let Some(c) = event["cursor"].as_u64() {
                    last = c;
                }
                if on_event(event).await? == EventFlow::Stop {
                    return Ok(last);
                }
            }
        }
        Ok(last)
    }
}

#[derive(PartialEq)]
pub enum EventFlow {
    Continue,
    Stop,
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        bail!("stdin closed");
    }
    Ok(line.trim().to_string())
}

fn compact(v: &Value, max: usize) -> String {
    let s = v.to_string();
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Interactive chat: send a message, stream the turn, prompt for approvals.
pub async fn chat(
    api: &Api,
    session_id: &str,
    mode: Option<&str>,
    model: Option<&str>,
    permissions: Option<&str>,
) -> Result<()> {
    let thread = api
        .create_thread(session_id, mode, model, permissions)
        .await?;
    let thread_id = thread["id"].as_str().unwrap().to_string();
    println!(
        "thread {thread_id} — mode={} model={} permissions={}\n(ctrl-d to exit)",
        thread["mode"].as_str().unwrap_or("?"),
        thread["model"].as_str().unwrap_or("?"),
        thread["permission_mode"].as_str().unwrap_or("?"),
    );

    let mut cursor = 0u64;
    loop {
        let Ok(input) = read_line("\n> ") else {
            println!();
            return Ok(());
        };
        if input.is_empty() {
            continue;
        }
        api.send_message(&thread_id, &input).await?;

        cursor = api
            .follow_events(&thread_id, cursor, async |event| {
                match event["type"].as_str().unwrap_or("") {
                    "assistant.delta" => {
                        print!("{}", event["text"].as_str().unwrap_or(""));
                        std::io::stdout().flush()?;
                    }
                    "assistant.message" => println!(),
                    "tool.requested" => {
                        println!(
                            "\n[tool] {} {}",
                            event["tool"].as_str().unwrap_or("?"),
                            compact(&event["args"], 200)
                        );
                    }
                    "approval.requested" => {
                        let call_id = event["call_id"].as_str().unwrap_or("").to_string();
                        let decision = loop {
                            match read_line("approve? [y]es / [a]lways / [n]o: ")?.as_str() {
                                "y" | "yes" => break "approve",
                                "a" | "always" => break "always_approve",
                                "n" | "no" => break "deny",
                                _ => continue,
                            }
                        };
                        api.resolve_approval(&call_id, decision).await?;
                    }
                    "tool.completed" => {
                        println!(
                            "[tool] -> {} {}",
                            event["status"].as_str().unwrap_or("?"),
                            compact(&event["result"], 200)
                        );
                    }
                    "turn.completed" => {
                        let usage = &event["usage"];
                        println!(
                            "\n[turn done — in:{} out:{} tokens{}]",
                            usage["input_tokens"],
                            usage["output_tokens"],
                            event["checkpoint_id"]
                                .as_str()
                                .map(|c| format!(", checkpoint {c}"))
                                .unwrap_or_default()
                        );
                        return Ok(EventFlow::Stop);
                    }
                    "turn.failed" => {
                        println!(
                            "\n[turn failed: {}]",
                            event["error"].as_str().unwrap_or("?")
                        );
                        return Ok(EventFlow::Stop);
                    }
                    _ => {}
                }
                Ok(EventFlow::Continue)
            })
            .await?;
    }
}
