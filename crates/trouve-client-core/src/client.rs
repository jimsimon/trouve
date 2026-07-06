//! Typed protocol client (HTTP commands + SSE event stream).

use anyhow::{bail, Context, Result};
use futures::StreamExt;
use trouve_protocol::*;

#[derive(Clone)]
pub struct ProtocolClient {
    base: String,
    http: reqwest::Client,
}

impl ProtocolClient {
    pub fn new(server: &str) -> Self {
        Self {
            base: format!("{}/v1", server.trim_end_matches('/')),
            http: reqwest::Client::new(),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        decode(resp, path).await
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        decode(resp, path).await
    }

    async fn post_empty(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&serde_json::json!({}))
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    async fn patch_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T> {
        let resp = self
            .http
            .patch(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        decode(resp, path).await
    }

    async fn put_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> Result<T> {
        let resp = self
            .http
            .put(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .with_context(|| format!("PUT {path}"))?;
        decode(resp, path).await
    }

    async fn put_empty(&self, path: &str, body: &impl serde::Serialize) -> Result<()> {
        let resp = self
            .http
            .put(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .with_context(|| format!("PUT {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    pub async fn info(&self) -> Result<ServerInfo> {
        self.get_json("/info").await
    }

    pub async fn register_workspace(&self, path: &str) -> Result<Workspace> {
        self.post_json(
            "/workspaces",
            &RegisterWorkspaceRequest {
                path: path.into(),
                name: None,
            },
        )
        .await
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.get_json("/workspaces").await
    }

    pub async fn workspace_branches(&self, workspace_id: &str) -> Result<BranchList> {
        self.get_json(&format!("/workspaces/{workspace_id}/branches"))
            .await
    }

    pub async fn create_session(&self, req: &CreateSessionRequest) -> Result<Session> {
        self.post_json("/sessions", req).await
    }

    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        self.get_json("/sessions").await
    }

    pub async fn update_session(
        &self,
        session_id: &str,
        req: &UpdateSessionRequest,
    ) -> Result<Session> {
        self.patch_json(&format!("/sessions/{session_id}"), req)
            .await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        self.delete(&format!("/sessions/{session_id}")).await
    }

    pub async fn create_thread(&self, req: &CreateThreadRequest) -> Result<Thread> {
        self.post_json("/threads", req).await
    }

    pub async fn update_thread(
        &self,
        thread_id: &str,
        req: &UpdateThreadRequest,
    ) -> Result<Thread> {
        self.patch_json(&format!("/threads/{thread_id}"), req).await
    }

    pub async fn list_threads(&self, session_id: &str) -> Result<Vec<Thread>> {
        self.get_json(&format!("/threads?session_id={session_id}"))
            .await
    }

    pub async fn send_message(&self, thread_id: &str, content: &str) -> Result<TurnAccepted> {
        self.post_json(
            &format!("/threads/{thread_id}/messages"),
            &SendMessageRequest {
                content: content.into(),
            },
        )
        .await
    }

    pub async fn resolve_approval(&self, call_id: &str, decision: ApprovalDecision) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/approvals", self.base))
            .json(&ResolveApprovalRequest {
                call_id: call_id.into(),
                decision,
            })
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("approval failed: {}", resp.status());
        }
        Ok(())
    }

    pub async fn undo(&self, session_id: &str) -> Result<()> {
        self.post_empty(&format!("/sessions/{session_id}/undo"))
            .await
    }

    pub async fn redo(&self, session_id: &str) -> Result<()> {
        self.post_empty(&format!("/sessions/{session_id}/redo"))
            .await
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        self.get_json("/models").await
    }

    pub async fn list_modes(&self, workspace_id: Option<&str>) -> Result<Vec<AgentMode>> {
        match workspace_id {
            Some(id) => self.get_json(&format!("/modes?workspace_id={id}")).await,
            None => self.get_json("/modes").await,
        }
    }

    pub async fn list_providers(&self) -> Result<ProvidersResponse> {
        self.get_json("/providers").await
    }

    pub async fn known_providers(&self) -> Result<Vec<KnownProvider>> {
        self.get_json("/providers/known").await
    }

    pub async fn start_login(&self, id: &str) -> Result<LoginStarted> {
        self.post_json(&format!("/providers/{id}/login"), &serde_json::json!({}))
            .await
    }

    pub async fn login_status(&self, id: &str) -> Result<LoginStatus> {
        self.get_json(&format!("/providers/{id}/login")).await
    }

    pub async fn upsert_provider(
        &self,
        id: &str,
        req: &UpsertProviderRequest,
    ) -> Result<ProviderInfo> {
        self.put_json(&format!("/providers/{id}"), req).await
    }

    pub async fn delete_provider(&self, id: &str) -> Result<()> {
        self.delete(&format!("/providers/{id}")).await
    }

    pub async fn set_default_model(&self, model: &str) -> Result<()> {
        self.put_empty(
            "/config/default-model",
            &SetDefaultModelRequest {
                model: model.into(),
            },
        )
        .await
    }

    pub async fn session_diff(&self, session_id: &str) -> Result<SessionDiff> {
        self.get_json(&format!("/sessions/{session_id}/diff")).await
    }

    pub async fn session_files(&self, session_id: &str, path: &str) -> Result<Vec<DirEntry>> {
        self.get_json(&format!(
            "/sessions/{session_id}/files?path={}",
            urlencode(path)
        ))
        .await
    }

    pub async fn session_file(&self, session_id: &str, path: &str) -> Result<FileContent> {
        self.get_json(&format!(
            "/sessions/{session_id}/file?path={}",
            urlencode(path)
        ))
        .await
    }

    pub async fn session_usage(&self, session_id: &str) -> Result<UsageSummary> {
        self.get_json(&format!("/sessions/{session_id}/usage"))
            .await
    }

    pub async fn session_pr(&self, session_id: &str) -> Result<Option<PrInfo>> {
        self.get_json(&format!("/sessions/{session_id}/pr")).await
    }

    pub async fn create_session_pr(
        &self,
        session_id: &str,
        req: &CreatePrRequest,
    ) -> Result<PrInfo> {
        self.post_json(&format!("/sessions/{session_id}/pr"), req)
            .await
    }

    pub async fn merge_session_pr(&self, session_id: &str, method: Option<&str>) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/sessions/{session_id}/pr/merge", self.base))
            .json(&MergePrRequest {
                method: method.map(String::from),
            })
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("merge failed: {}", resp.status());
        }
        Ok(())
    }

    /// Follow a thread's event stream from `after`, invoking `on_event` for
    /// each envelope. Returns when the stream ends or the callback errors.
    pub async fn follow_thread_events(
        &self,
        thread_id: &str,
        after: u64,
        mut on_event: impl FnMut(EventEnvelope) -> std::ops::ControlFlow<()>,
    ) -> Result<u64> {
        let resp = self
            .http
            .get(format!(
                "{}/threads/{thread_id}/events?after={after}",
                self.base
            ))
            .send()
            .await?;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut last = after;
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(envelope) = serde_json::from_str::<EventEnvelope>(data.trim()) else {
                    continue;
                };
                last = envelope.cursor;
                if on_event(envelope).is_break() {
                    return Ok(last);
                }
            }
        }
        Ok(last)
    }
}

fn urlencode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}

async fn decode<T: serde::de::DeserializeOwned>(resp: reqwest::Response, path: &str) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        let message = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|e| e.message)
            .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).to_string());
        bail!("{path}: {message} ({status})");
    }
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {path} response"))
}
