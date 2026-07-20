//! Typed protocol client (HTTP commands + SSE event stream).

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use trouve_protocol::*;

#[derive(Clone)]
pub struct ProtocolClient {
    base: String,
    http: reqwest::Client,
}

impl ProtocolClient {
    pub fn new(server: &str) -> Self {
        Self::with_token(server, None)
    }

    /// Build a client that sends `Authorization: Bearer <token>` on every
    /// request (the local server requires it). `None` disables auth (tests
    /// and unauthenticated servers).
    pub fn with_token(server: &str, token: Option<String>) -> Self {
        let http = match token.filter(|t| !t.is_empty()) {
            Some(token) => {
                let mut headers = reqwest::header::HeaderMap::new();
                let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .expect("bearer token is valid header value");
                value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, value);
                reqwest::Client::builder()
                    .default_headers(headers)
                    .build()
                    .expect("reqwest client builds")
            }
            None => reqwest::Client::new(),
        };
        Self {
            base: format!("{}/v1", server.trim_end_matches('/')),
            http,
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
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        decode_empty(resp, path).await
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
        decode_empty(resp, path).await
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        decode_empty(resp, path).await
    }

    async fn delete_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .http
            .delete(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("DELETE {path}"))?;
        decode(resp, path).await
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

    /// Refresh account-relevant PR snapshots for every configured GitHub
    /// host. Results arrive on the persisted server event stream.
    pub async fn refresh_github_prs(&self) -> Result<()> {
        self.post_empty("/github/prs/refresh").await
    }

    pub async fn close_workspace(&self, workspace_id: &str) -> Result<()> {
        self.delete(&format!("/workspaces/{workspace_id}")).await
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
        self.send_message_with(thread_id, content, Vec::new()).await
    }

    /// Send a prompt with attachment uploads (base64 bytes; stored
    /// server-side and passed to the agent).
    pub async fn send_message_with(
        &self,
        thread_id: &str,
        content: &str,
        attachments: Vec<trouve_protocol::AttachmentUpload>,
    ) -> Result<TurnAccepted> {
        self.post_json(
            &format!("/threads/{thread_id}/messages"),
            &SendMessageRequest {
                content: content.into(),
                attachments,
            },
        )
        .await
    }

    /// Fetch the raw bytes of a stored prompt attachment. The request uses
    /// the same authenticated client as every other protocol operation.
    pub async fn attachment_bytes(&self, attachment_id: &str) -> Result<Vec<u8>> {
        let path = format!("/attachments/{attachment_id}");
        let resp = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .json::<ErrorBody>()
                .await
                .map(|e| e.message)
                .unwrap_or_else(|_| status.to_string());
            bail!("{path}: {message}");
        }
        Ok(resp
            .bytes()
            .await
            .with_context(|| format!("reading {path}"))?
            .to_vec())
    }

    // --- queued prompts ---------------------------------------------------

    pub async fn list_queue(&self, thread_id: &str) -> Result<Vec<trouve_protocol::QueuedPrompt>> {
        self.get_json(&format!("/threads/{thread_id}/queue")).await
    }

    pub async fn update_queued_prompt(&self, prompt_id: &str, content: &str) -> Result<()> {
        self.update_queued_prompt_with(
            prompt_id,
            trouve_protocol::UpdateQueuedPromptRequest {
                content: content.into(),
                retained_attachment_ids: None,
                attachments: Vec::new(),
            },
        )
        .await
    }

    pub async fn update_queued_prompt_with(
        &self,
        prompt_id: &str,
        request: trouve_protocol::UpdateQueuedPromptRequest,
    ) -> Result<()> {
        let path = format!("/queue/{prompt_id}");
        let resp = self
            .http
            .patch(format!("{}{path}", self.base))
            .json(&request)
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

    /// Interrupt the turn currently running on a thread.
    pub async fn cancel_turn(&self, thread_id: &str) -> Result<()> {
        self.post_empty(&format!("/threads/{thread_id}/cancel"))
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

    /// Search HuggingFace for GGUF repos, with per-file hardware-fit
    /// guidance computed on the server's hardware.
    pub async fn search_local_models(
        &self,
        query: &str,
    ) -> Result<Vec<trouve_protocol::LocalSearchResult>> {
        self.get_json(&format!("/local/search?q={}", urlencode(query)))
            .await
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

    pub async fn set_default_model(
        &self,
        model: &str,
        default_thinking_level: Option<&str>,
    ) -> Result<()> {
        self.put_empty(
            "/config/default-model",
            &SetDefaultModelRequest {
                model: model.into(),
                default_thinking_level: default_thinking_level.map(String::from),
            },
        )
        .await
    }

    /// Set the global default permission mode for new threads (used by
    /// modes without a default of their own).
    pub async fn set_default_permission_mode(&self, permission_mode: PermissionMode) -> Result<()> {
        self.put_empty(
            "/config/default-permission-mode",
            &SetDefaultPermissionModeRequest { permission_mode },
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

    /// Every worktree path (files, dirs with trailing '/'), for "@" mentions.
    pub async fn session_paths(&self, session_id: &str) -> Result<Vec<String>> {
        self.get_json(&format!("/sessions/{session_id}/paths"))
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
        let mut buf = LineBuffer::default();
        let mut last = after;
        let mut id: Option<u64> = None;
        while let Some(chunk) = stream.next().await {
            buf.push(&chunk?);
            while let Some(line) = buf.next_line() {
                let line = line.trim();
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

    /// All PRs associated with the session (open first, newest first).
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

    /// Subscription usage per configured subscription provider. Some
    /// providers answer through vendor CLIs, so this can take a few seconds.
    pub async fn subscription_health(&self) -> Result<Vec<SubscriptionHealth>> {
        self.get_json("/subscriptions").await
    }

    pub async fn github_integration(&self) -> Result<GithubIntegration> {
        self.get_json("/integrations/github").await
    }

    /// Register a self-hosted GitHub Enterprise instance for OAuth sign-in.
    pub async fn add_github_host(&self, host: &str, client_id: &str) -> Result<GithubIntegration> {
        self.post_json(
            "/integrations/github/hosts",
            &trouve_protocol::AddGithubHostRequest {
                host: host.to_string(),
                client_id: client_id.to_string(),
            },
        )
        .await
    }

    /// Remove an enterprise host (github.com can't be removed).
    pub async fn remove_github_host(&self, host: &str) -> Result<GithubIntegration> {
        self.delete_json(&format!("/integrations/github/hosts/{host}"))
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

    // --- automations -----------------------------------------------------------

    pub async fn list_automations(&self) -> Result<Vec<trouve_protocol::Automation>> {
        self.get_json("/automations").await
    }

    /// Pre-canned automations for common development tasks (static catalog).
    pub async fn automation_templates(&self) -> Result<Vec<trouve_protocol::AutomationTemplate>> {
        self.get_json("/automations/templates").await
    }

    pub async fn create_automation(
        &self,
        req: &trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation> {
        self.post_json("/automations", req).await
    }

    pub async fn update_automation(
        &self,
        id: &str,
        req: &trouve_protocol::UpsertAutomationRequest,
    ) -> Result<trouve_protocol::Automation> {
        let path = format!("/automations/{id}");
        let resp = self
            .http
            .put(format!("{}{path}", self.base))
            .json(req)
            .send()
            .await
            .with_context(|| format!("PUT {path}"))?;
        decode(resp, &path).await
    }

    pub async fn delete_automation(&self, id: &str) -> Result<()> {
        self.delete(&format!("/automations/{id}")).await
    }

    /// Fire an automation immediately (runs in the background server-side).
    pub async fn run_automation(&self, id: &str) -> Result<()> {
        self.post_empty(&format!("/automations/{id}/run")).await
    }

    /// Follow the server-scope event stream (session/workspace lifecycle,
    /// automation runs) from `after`. Same contract as
    /// [`Self::follow_thread_events`].
    pub async fn follow_server_events(
        &self,
        after: u64,
        on_event: impl FnMut(EventEnvelope) -> std::ops::ControlFlow<()>,
    ) -> Result<u64> {
        self.follow_sse(
            format!("{}/events?after={after}", self.base),
            after,
            on_event,
        )
        .await
    }

    /// Follow a thread's event stream from `after`, invoking `on_event` for
    /// each envelope. Returns when the stream ends or the callback errors.
    pub async fn follow_thread_events(
        &self,
        thread_id: &str,
        after: u64,
        on_event: impl FnMut(EventEnvelope) -> std::ops::ControlFlow<()>,
    ) -> Result<u64> {
        self.follow_sse(
            format!("{}/threads/{thread_id}/events?after={after}", self.base),
            after,
            on_event,
        )
        .await
    }

    /// Consume one SSE stream of [`EventEnvelope`]s, returning the last
    /// cursor seen.
    async fn follow_sse(
        &self,
        url: String,
        after: u64,
        mut on_event: impl FnMut(EventEnvelope) -> std::ops::ControlFlow<()>,
    ) -> Result<u64> {
        let resp = self.http.get(url).send().await?;
        let mut stream = resp.bytes_stream();
        let mut buf = LineBuffer::default();
        let mut last = after;
        while let Some(chunk) = stream.next().await {
            buf.push(&chunk?);
            while let Some(line) = buf.next_line() {
                let Some(data) = line.trim().strip_prefix("data:") else {
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

/// Buffers raw SSE bytes and yields complete lines, decoding each only once
/// whole — decoding per network chunk would corrupt multi-byte UTF-8 (and
/// thus drop the whole JSON envelope) when a character straddles a chunk
/// boundary. Lines split on `\n`, which is never part of a multi-byte
/// sequence.
#[derive(Default)]
struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    fn next_line(&mut self) -> Option<String> {
        let pos = self.buf.iter().position(|&b| b == b'\n')?;
        let line: Vec<u8> = self.buf.drain(..=pos).collect();
        Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned())
    }
}

fn urlencode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'/' | b'~') {
            encoded.push(*byte as char);
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

async fn decode<T: serde::de::DeserializeOwned>(resp: reqwest::Response, path: &str) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await?;
    if !status.is_success() {
        return Err(response_error(path, status, &bytes));
    }
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {path} response"))
}

async fn decode_empty(resp: reqwest::Response, path: &str) -> Result<()> {
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let bytes = resp.bytes().await?;
    Err(response_error(path, status, &bytes))
}

fn response_error(path: &str, status: reqwest::StatusCode, bytes: &[u8]) -> anyhow::Error {
    let message = serde_json::from_slice::<ErrorBody>(bytes)
        .map(|error| error.message)
        .unwrap_or_else(|_| {
            let message = String::from_utf8_lossy(bytes);
            if message.is_empty() {
                status.to_string()
            } else {
                message.into_owned()
            }
        });
    anyhow::anyhow!("{path}: {message} ({status})")
}

#[cfg(test)]
mod tests {
    use super::{ProtocolClient, response_error, urlencode};

    #[test]
    fn urlencode_percent_encodes_utf8_bytes() {
        assert_eq!(urlencode("src/café.rs"), "src/caf%C3%A9.rs");
        assert_eq!(urlencode("🙂 notes"), "%F0%9F%99%82%20notes");
        assert_eq!(urlencode("a/b~c"), "a/b~c");
    }

    #[test]
    fn empty_response_preserves_structured_server_error() {
        let error = response_error(
            "/github/prs/refresh",
            reqwest::StatusCode::BAD_REQUEST,
            br#"{"code":"bad_request","message":"github.com: API rate limit exceeded"}"#,
        );
        assert_eq!(
            error.to_string(),
            "/github/prs/refresh: github.com: API rate limit exceeded (400 Bad Request)"
        );
    }

    #[test]
    fn bodyless_error_response_uses_http_status_as_message() {
        let error = response_error("/github/prs/refresh", reqwest::StatusCode::BAD_REQUEST, b"");
        assert_eq!(
            error.to_string(),
            "/github/prs/refresh: 400 Bad Request (400 Bad Request)"
        );
    }

    #[test]
    fn non_json_error_response_preserves_raw_body() {
        let error = response_error(
            "/github/prs/refresh",
            reqwest::StatusCode::BAD_GATEWAY,
            b"upstream unavailable",
        );
        assert_eq!(
            error.to_string(),
            "/github/prs/refresh: upstream unavailable (502 Bad Gateway)"
        );
    }

    #[tokio::test]
    #[ignore = "binds a loopback socket; run with TROUVE_E2E=1 cargo test -- --ignored"]
    async fn empty_post_has_no_json_body() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        if std::env::var("TROUVE_E2E").ok().as_deref() != Some("1") {
            eprintln!("skipping: set TROUVE_E2E=1 to run network tests");
            return;
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "request ended before its headers");
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let (headers, buffered_body) = request.split_at(header_end);
            let headers = std::str::from_utf8(headers).unwrap();
            assert!(headers.starts_with("POST /v1/empty HTTP/1.1\r\n"));

            let mut content_length = None;
            let mut has_transfer_encoding = false;
            let mut has_content_type = false;
            for line in headers.lines().skip(1) {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = Some(value.trim().parse::<u64>().unwrap());
                } else if name.eq_ignore_ascii_case("transfer-encoding") {
                    has_transfer_encoding = true;
                } else if name.eq_ignore_ascii_case("content-type") {
                    has_content_type = true;
                }
            }
            assert!(content_length.is_none_or(|length| length == 0));
            assert!(!has_transfer_encoding);
            assert!(!has_content_type);
            assert!(buffered_body.is_empty());
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        ProtocolClient::new(&format!("http://{addr}"))
            .post_empty("/empty")
            .await
            .unwrap();
        server.await.unwrap();
    }
}
