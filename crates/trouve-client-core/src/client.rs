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

    // --- queued prompts ---------------------------------------------------

    pub async fn list_queue(&self, thread_id: &str) -> Result<Vec<trouve_protocol::QueuedPrompt>> {
        self.get_json(&format!("/threads/{thread_id}/queue")).await
    }

    pub async fn update_queued_prompt(&self, prompt_id: &str, content: &str) -> Result<()> {
        let path = format!("/queue/{prompt_id}");
        let resp = self
            .http
            .patch(format!("{}{path}", self.base))
            .json(&trouve_protocol::UpdateQueuedPromptRequest {
                content: content.into(),
            })
            .send()
            .await
            .with_context(|| format!("PATCH {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    pub async fn delete_queued_prompt(&self, prompt_id: &str) -> Result<()> {
        self.delete(&format!("/queue/{prompt_id}")).await
    }

    /// Full desired order (every queued prompt id, first to run first).
    pub async fn reorder_queue(
        &self,
        thread_id: &str,
        ids: &[String],
    ) -> Result<Vec<trouve_protocol::QueuedPrompt>> {
        self.put_json(
            &format!("/threads/{thread_id}/queue"),
            &trouve_protocol::ReorderQueueRequest { ids: ids.to_vec() },
        )
        .await
    }

    /// Kick an idle thread into draining its queue. Prompts left queued by
    /// a restart/crash or paused by a failed turn wait for this explicit
    /// resume ("Send now").
    pub async fn dispatch_queue(&self, thread_id: &str) -> Result<TurnAccepted> {
        self.post_json(
            &format!("/threads/{thread_id}/queue/dispatch"),
            &serde_json::json!({}),
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

    /// Answer (or skip, `answers: None`) a pending question request.
    pub async fn resolve_question(
        &self,
        request_id: &str,
        answers: Option<Vec<trouve_protocol::QuestionAnswer>>,
    ) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/questions", self.base))
            .json(&trouve_protocol::ResolveQuestionRequest {
                request_id: request_id.into(),
                answers,
            })
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("question answer failed: {}", resp.status());
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

    /// Modes with provenance (builtin / customized / custom / workspace).
    pub async fn list_mode_infos(&self, workspace_id: Option<&str>) -> Result<Vec<ModeInfo>> {
        match workspace_id {
            Some(id) => {
                self.get_json(&format!("/mode-infos?workspace_id={id}"))
                    .await
            }
            None => self.get_json("/mode-infos").await,
        }
    }

    /// Create or update a user-level mode; a built-in id customizes that
    /// built-in.
    pub async fn upsert_mode(&self, id: &str, req: &UpsertModeRequest) -> Result<()> {
        let resp = self
            .http
            .put(format!("{}/modes/{id}", self.base))
            .json(req)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .json::<ErrorBody>()
                .await
                .map(|e| e.message)
                .unwrap_or_else(|_| status.to_string());
            bail!("saving mode failed: {message}");
        }
        Ok(())
    }

    /// Delete a custom mode / reset a customized built-in.
    pub async fn delete_mode(&self, id: &str) -> Result<()> {
        self.delete(&format!("/modes/{id}")).await
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

    pub async fn list_clis(&self) -> Result<CliList> {
        self.get_json("/clis").await
    }

    pub async fn start_cli_install(&self, id: &str) -> Result<()> {
        self.post_empty(&format!("/clis/{id}/install")).await
    }

    pub async fn cli_install_status(&self, id: &str) -> Result<CliInstallStatus> {
        self.get_json(&format!("/clis/{id}/install")).await
    }

    /// Cancel an in-flight CLI install.
    pub async fn cancel_cli_install(&self, id: &str) -> Result<()> {
        self.delete(&format!("/clis/{id}/install")).await
    }

    /// Remove the managed install of a CLI (PATH installs are untouched).
    pub async fn uninstall_cli(&self, id: &str) -> Result<()> {
        self.delete(&format!("/clis/{id}")).await
    }

    /// Local ("offline / integrated") inference status: hardware, runtime,
    /// running server, and model download/fit state.
    pub async fn local_status(&self) -> Result<LocalStatus> {
        self.get_json("/local").await
    }

    /// Register a custom GGUF (HuggingFace repo + file). Surfaces the
    /// server's validation message on failure.
    pub async fn add_local_model(&self, req: &AddLocalModelRequest) -> Result<()> {
        let path = "/local/models";
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(req)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            if let Ok(body) = resp.json::<ErrorBody>().await {
                bail!("{}", body.message);
            }
            bail!("{path}: {status}");
        }
        Ok(())
    }

    pub async fn start_local_model_download(&self, id: &str) -> Result<()> {
        self.post_empty(&format!("/local/models/{id}/download"))
            .await
    }

    /// Cancel an in-flight model download (the partial file is deleted).
    pub async fn cancel_local_model_download(&self, id: &str) -> Result<()> {
        self.delete(&format!("/local/models/{id}/download")).await
    }

    pub async fn delete_local_model(&self, id: &str) -> Result<()> {
        self.delete(&format!("/local/models/{id}")).await
    }

    pub async fn stop_local_server(&self) -> Result<()> {
        self.post_empty("/local/server/stop").await
    }

    /// Restart llama-server with the model it is serving (background;
    /// poll `local_status` for server_status).
    pub async fn restart_local_server(&self) -> Result<()> {
        self.post_empty("/local/server/restart").await
    }

    /// Enable or disable local models (disable stops the sidecar and
    /// removes the "local" provider).
    pub async fn set_local_enabled(&self, enabled: bool) -> Result<()> {
        self.put_empty(
            "/local/enabled",
            &trouve_protocol::SetLocalEnabledRequest { enabled },
        )
        .await
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

    // --- integrated terminal -------------------------------------------

    /// Open (or re-attach to) the session's shell terminal.
    pub async fn open_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalInfo> {
        self.post_json(
            &format!("/sessions/{session_id}/terminal"),
            &OpenTerminalRequest { cols, rows },
        )
        .await
    }

    /// Write raw bytes (already key-encoded) to the terminal's PTY.
    pub async fn terminal_input(&self, terminal_id: &str, bytes: &[u8]) -> Result<()> {
        use base64::Engine as _;
        let body = TerminalInputRequest {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        let path = format!("/terminals/{terminal_id}/input");
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    pub async fn terminal_resize(&self, terminal_id: &str, cols: u16, rows: u16) -> Result<()> {
        let path = format!("/terminals/{terminal_id}/resize");
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&TerminalResizeRequest { cols, rows })
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        if !resp.status().is_success() {
            bail!("{path}: {}", resp.status());
        }
        Ok(())
    }

    /// Kill the terminal's shell (the next open starts a fresh one).
    pub async fn kill_terminal(&self, terminal_id: &str) -> Result<()> {
        self.delete(&format!("/terminals/{terminal_id}")).await
    }

    /// Follow a terminal's output from byte offset `after`, invoking
    /// `on_chunk` with (end offset, raw bytes). Returns `(offset, exited)`:
    /// `exited: true` means the shell is gone; `false` means the stream
    /// dropped or lagged and the caller may reconnect from `offset`.
    pub async fn follow_terminal(
        &self,
        terminal_id: &str,
        after: u64,
        mut on_chunk: impl FnMut(u64, Vec<u8>) -> std::ops::ControlFlow<()>,
    ) -> Result<(u64, bool)> {
        use base64::Engine as _;
        let resp = self
            .http
            .get(format!(
                "{}/terminals/{terminal_id}/output?after={after}",
                self.base
            ))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!("terminal output: {}", resp.status());
        }
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut last = after;
        let mut id: Option<u64> = None;
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                if let Some(v) = line.strip_prefix("id:") {
                    id = v.trim().parse().ok();
                } else if line == "event: exit" {
                    return Ok((last, true));
                } else if line == "event: lagged" {
                    return Ok((last, false));
                } else if let Some(data) = line.strip_prefix("data:") {
                    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim())
                    else {
                        continue;
                    };
                    if let Some(end) = id.take() {
                        last = end;
                    }
                    if !bytes.is_empty() && on_chunk(last, bytes).is_break() {
                        return Ok((last, true));
                    }
                }
            }
        }
        Ok((last, false))
    }

    pub async fn session_pr(&self, session_id: &str) -> Result<Option<PrInfo>> {
        self.get_json(&format!("/sessions/{session_id}/pr")).await
    }

    /// All PRs spawned from the session branch (open first, newest first).
    pub async fn session_prs(&self, session_id: &str) -> Result<Vec<PrInfo>> {
        self.get_json(&format!("/sessions/{session_id}/prs")).await
    }

    /// User + workspace MCP servers; `probe` spawns each one for a health
    /// check, so expect the call to take a few seconds.
    pub async fn list_mcp_servers(
        &self,
        workspace_id: Option<&str>,
        probe: bool,
    ) -> Result<Vec<McpServerInfo>> {
        let mut path = format!("/mcp-servers?probe={probe}");
        if let Some(id) = workspace_id {
            path.push_str(&format!("&workspace_id={id}"));
        }
        self.get_json(&path).await
    }

    pub async fn upsert_mcp_server(&self, name: &str, req: &UpsertMcpServerRequest) -> Result<()> {
        let resp = self
            .http
            .put(format!("{}/mcp-servers/{name}", self.base))
            .json(req)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .json::<ErrorBody>()
                .await
                .map(|e| e.message)
                .unwrap_or_else(|_| status.to_string());
            bail!("saving MCP server failed: {message}");
        }
        Ok(())
    }

    pub async fn delete_mcp_server(
        &self,
        name: &str,
        scope: &str,
        workspace_id: Option<&str>,
    ) -> Result<()> {
        let mut path = format!("/mcp-servers/{name}?scope={scope}");
        if let Some(id) = workspace_id {
            path.push_str(&format!("&workspace_id={id}"));
        }
        self.delete(&path).await
    }

    pub async fn mcp_server_logs(&self, name: &str) -> Result<McpLogs> {
        self.get_json(&format!("/mcp-servers/{name}/logs")).await
    }

    /// The effective MCP config a turn in this session would see: all
    /// scopes merged, each entry tagged with the winning layer ("app-wide",
    /// "workspace", or "branch"); disabled tombstones included.
    pub async fn session_mcp_servers(&self, session_id: &str) -> Result<Vec<McpServerInfo>> {
        self.get_json(&format!("/sessions/{session_id}/mcp-servers"))
            .await
    }

    /// Subscription usage per configured agent backend. Codex answers via
    /// its app-server (may spawn it), so this can take a couple of seconds.
    pub async fn subscription_health(&self) -> Result<Vec<SubscriptionHealth>> {
        self.get_json("/subscriptions").await
    }

    pub async fn github_integration(&self) -> Result<GithubIntegration> {
        self.get_json("/integrations/github").await
    }

    /// Store the GitHub token server-side; an empty token removes it.
    pub async fn set_github_token(&self, token: &str) -> Result<GithubIntegration> {
        self.put_json(
            "/integrations/github",
            &SetGithubTokenRequest {
                token: token.to_string(),
            },
        )
        .await
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
