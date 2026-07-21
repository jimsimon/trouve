//! Provider-neutral turn coordination.
//!
//! Native chat providers and vendor-agent backends keep their own execution
//! mechanics, but report one common attempt outcome here. The persisted
//! transcript and session worktree are the handoff boundary between them.

use super::*;

impl Engine {
    pub(super) async fn run_routed_turn(
        self: &Arc<Self>,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let content = prompt.content.clone();
        let attachments = prompt.attachments.clone();
        let tools_enabled = self.store.queued_prompt_tools_enabled(&prompt.id)?;
        let session = self
            .store
            .session(&thread.session_id)?
            .context("session vanished")?;
        let workspace = self
            .store
            .workspace(&session.workspace_id)?
            .context("workspace vanished")?;
        let scope = Scope::Thread(thread.id.clone());
        let worktree = PathBuf::from(&session.worktree_path);
        let tool_ctx = ToolCtx {
            worktree: worktree.clone(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&workspace.path)),
        };

        let modes =
            modes::resolve_modes(self.config_dir.as_deref(), Some(Path::new(&workspace.path)));
        let mode = modes::find_mode(&modes, &thread.mode)
            .cloned()
            .unwrap_or_else(modes::fallback_mode);

        let mut candidates = self
            .resolve_model_candidates(&thread.model)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let total_candidates = candidates.len();
        candidates.truncate(MAX_ROUTE_ATTEMPTS_PER_TURN);
        let first_route = candidates.first().context("model route disappeared")?;
        let capacity_model = format!("{}/{}", first_route.provider_id, first_route.provider_model);
        let background = self.store.is_code_review_thread(&thread.id)?;
        let capacity_started = Instant::now();
        let turn_capacity = self.turn_scheduler.acquire_global(background).await?;
        let first_route_capacity = self
            .turn_scheduler
            .acquire_provider(&capacity_model, background)
            .await?;
        let capacity_wait_ms = capacity_started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        if background {
            self.store
                .set_code_review_task_provider_wait(&thread.id, capacity_wait_ms)?;
        }
        self.store.append_event(
            scope.clone(),
            Event::TurnCapacityAcquired {
                turn,
                wait_ms: capacity_wait_ms,
                background,
            },
        )?;

        let concurrent_child = mode.read_only && self.store.spawn_parent(&thread.id)?.is_some();
        let lock = self.session_lock(&session.id);
        let _read_guard;
        let _write_guard;
        if concurrent_child {
            _read_guard = None;
            _write_guard = None;
        } else if mode.read_only {
            _read_guard = Some(lock.read().await);
            _write_guard = None;
        } else {
            _read_guard = None;
            _write_guard = Some(lock.write().await);
        }
        let _turn_capacity = turn_capacity;

        let has_native = candidates
            .iter()
            .any(|candidate| matches!(candidate.executor, ModelExecutor::Native(_)));
        let failover_context_window = candidates
            .iter()
            .map(|candidate| candidate.info.context_window)
            .filter(|window| *window > 0)
            .min()
            .unwrap_or(first_route.info.context_window);
        let history_before = self.store.messages(&thread.id)?;

        self.store.append_event(
            scope.clone(),
            Event::TurnStarted {
                turn,
                mode: mode.id.clone(),
                model: thread.model.clone(),
            },
        )?;
        self.store.append_event(
            scope.clone(),
            Event::ModelRouteSelected {
                turn,
                model: thread.model.clone(),
                provider_id: first_route.provider_id.clone(),
                provider_model: first_route.provider_model.clone(),
                reason: "initial".into(),
            },
        )?;
        self.store.append_event(
            scope.clone(),
            Event::UserMessage {
                turn,
                content: content.clone(),
                attachments: attachments.clone(),
            },
        )?;

        // Compaction must run before this turn's user message joins the
        // provider transcript. If a backend is selected first and later
        // hands off to native execution, the native route uses the full
        // persisted transcript for that exceptional continuation.
        if let Some(native_route) = candidates
            .iter()
            .find(|candidate| matches!(candidate.executor, ModelExecutor::Native(_)))
            && let ModelExecutor::Native(provider) = &native_route.executor
            && let Err(error) = self
                .maybe_compact(
                    thread,
                    turn,
                    provider,
                    &native_route.provider_model,
                    failover_context_window,
                )
                .await
        {
            tracing::warn!("compaction failed for {}: {error}", thread.id);
        }

        let resolved = self.resolve_attachments(&attachments);
        let routed_attachments = if has_native {
            // Native tools only accept worktree-relative paths. Materialize
            // up front so a backend -> native handoff can see every original
            // attachment without mutating the transcript mid-turn.
            materialize_attachments(&worktree, &resolved)
        } else {
            resolved
        };
        let (images, files): (Vec<_>, Vec<_>) = routed_attachments
            .iter()
            .cloned()
            .partition(|(attachment, _)| attachment.mime.starts_with("image/"));
        let stored_content = annotate_attachments(content.clone(), &routed_attachments);
        let backend_content = annotate_attachments(content, &files);
        let backend_attachments: Vec<trouve_agents::TurnAttachment> = images
            .into_iter()
            .map(|(attachment, path)| trouve_agents::TurnAttachment {
                name: attachment.name,
                mime: attachment.mime,
                path,
            })
            .collect();
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::User(stored_content))?,
        )?;
        if !self.store.finish_queued_prompt(&prompt.id)? {
            bail!("queued prompt {} vanished before turn start", prompt.id);
        }
        self.emit_queue(&thread.id)?;

        let mut specs = Vec::new();
        if has_native && tools_enabled {
            specs = self
                .executor
                .specs(&tool_ctx)
                .await
                .into_iter()
                .filter(|spec| {
                    mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&spec.name)
                })
                .collect();
            specs.push(ask_question_spec());
            specs.push(search_transcript_spec());
            let spawn_allowed = |name: &str| {
                mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|tool| tool == name)
            };
            if self.store.spawn_parent(&thread.id)?.is_none() {
                if spawn_allowed("spawn_thread") {
                    specs.push(spawn_thread_spec());
                }
                if spawn_allowed("spawn_session") {
                    specs.push(spawn_session_spec());
                }
                if spawn_allowed("spawn_thread") || spawn_allowed("spawn_session") {
                    specs.push(spawn_output_spec());
                }
            }
        }
        let system = context::system_prompt(
            &mode,
            self.config_dir.as_deref(),
            Path::new(&workspace.path),
        );
        let stored_model_options = self.store.thread_model_options(&thread.id)?;
        let permission = if mode.read_only {
            BackendPermission::ReadOnly
        } else {
            match thread.permission_mode {
                trouve_protocol::PermissionMode::Yolo => BackendPermission::Yolo,
                _ => BackendPermission::Ask,
            }
        };
        let github_repository = self.github_repository_for_session(&session).ok();
        let mut recorded_prs = if github_repository.is_some() {
            self.recorded_session_pr_numbers(&session.id)?
        } else {
            HashSet::new()
        };
        let mut accounting = TurnAccounting::default();
        let mut native_iterations_left = MAX_ITERATIONS;
        let attempted_candidates = candidates.len();
        let mut route_capacity = Some(first_route_capacity);

        for (route_index, route) in candidates.iter().enumerate() {
            if route_index > 0 {
                let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
                route_capacity = Some(
                    self.turn_scheduler
                        .acquire_provider(&capacity_model, background)
                        .await?,
                );
            }
            let retrying = route_index > 0;
            let result = match &route.executor {
                ModelExecutor::Native(_) => {
                    self.run_native_route(
                        &session,
                        thread,
                        turn,
                        &mode,
                        &tool_ctx,
                        route,
                        &specs,
                        &system,
                        &stored_model_options,
                        retrying,
                        &mut native_iterations_left,
                        &mut accounting,
                        tools_enabled,
                        &cancel,
                    )
                    .await?
                }
                ModelExecutor::Backend(_) => {
                    self.run_backend_route(
                        &session,
                        thread,
                        turn,
                        &mode,
                        route,
                        &backend_content,
                        &backend_attachments,
                        &history_before,
                        retrying,
                        permission,
                        github_repository.as_ref(),
                        &mut recorded_prs,
                        &mut accounting,
                        tools_enabled,
                        &cancel,
                    )
                    .await?
                }
            };
            drop(route_capacity.take());

            match result {
                RouteAttemptResult::Completed => {
                    self.turn_scheduler.record_outcome(&route.provider_id, None);
                    self.store
                        .record_route_success(&route.provider_id, &route.provider_model)?;
                    accounting.finalize_cost();
                    self.store.record_usage(
                        &session.id,
                        &thread.id,
                        turn,
                        &accounting.usage,
                        accounting.context_input_tokens,
                    )?;
                    let checkpoint_id = if concurrent_child {
                        None
                    } else {
                        self.maybe_checkpoint(&session, thread, turn).await?
                    };
                    self.store.append_event(
                        scope,
                        Event::TurnCompleted {
                            turn,
                            usage: accounting.usage,
                            checkpoint_id,
                        },
                    )?;
                    return Ok(());
                }
                RouteAttemptResult::Cancelled => {
                    accounting.finalize_cost();
                    if accounting.usage.input_tokens > 0
                        || accounting.usage.output_tokens > 0
                        || accounting.usage.cached_input_tokens > 0
                    {
                        self.store.record_usage(
                            &session.id,
                            &thread.id,
                            turn,
                            &accounting.usage,
                            accounting.context_input_tokens,
                        )?;
                    }
                    return Ok(());
                }
                RouteAttemptResult::Failed(failure) => {
                    self.turn_scheduler
                        .record_outcome(&route.provider_id, Some(&failure.message));
                    let (base, max) = failure.kind.cooldown();
                    let health = self.store.record_route_failure(
                        &route.provider_id,
                        &route.provider_model,
                        base,
                        max,
                    )?;
                    tracing::warn!(
                        model = %thread.model,
                        provider = %route.provider_id,
                        failures = health.consecutive_failures,
                        retry_after = health.retry_after,
                        error = %failure.message,
                        "model route opened its circuit"
                    );
                    let has_next = route_index + 1 < candidates.len();
                    if !failure.safe_to_retry || !has_next {
                        let untried = total_candidates.saturating_sub(attempted_candidates);
                        if failure.safe_to_retry && untried > 0 {
                            bail!(
                                "{}; stopped after {} route attempts ({} alternate routes remain untried and will be considered next turn)",
                                failure.message,
                                attempted_candidates,
                                untried,
                            );
                        }
                        bail!(failure.message);
                    }
                    let next = &candidates[route_index + 1];
                    self.store.append_event(
                        scope.clone(),
                        Event::ModelRouteSelected {
                            turn,
                            model: thread.model.clone(),
                            provider_id: next.provider_id.clone(),
                            provider_model: next.provider_model.clone(),
                            reason: failure.kind.failover_reason().into(),
                        },
                    )?;
                }
            }
        }

        bail!("no model route completed the turn")
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_native_route(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        tool_ctx: &ToolCtx,
        route: &ModelCandidate,
        specs: &[ToolSpec],
        system: &str,
        stored_model_options: &serde_json::Map<String, serde_json::Value>,
        retrying: bool,
        iterations_left: &mut usize,
        accounting: &mut TurnAccounting,
        tools_enabled: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Native(provider) = &route.executor else {
            unreachable!("native route helper received a backend")
        };
        let scope = Scope::Thread(thread.id.clone());
        let mut model_options = stored_model_options.clone();
        normalize_thinking_option(&mut model_options, Some(&route.info));
        let mut continuation_needed = retrying;

        while *iterations_left > 0 {
            if cancel.is_cancelled() {
                return Ok(RouteAttemptResult::Cancelled);
            }
            *iterations_left -= 1;
            let mut messages = vec![Message::System(system.to_string())];
            for payload in self.store.messages(&thread.id)? {
                messages.push(serde_json::from_value(payload)?);
            }
            let mut messages = sanitize_transcript(messages);
            if continuation_needed {
                messages.push(Message::User(
                    "Another provider could not continue this turn. Continue the in-progress \
                     response from the transcript and current worktree without repeating \
                     completed text, tool calls, or edits."
                        .into(),
                ));
            }

            let mut text = String::new();
            let mut tool_calls = Vec::new();
            let mut reasoning = Vec::new();
            let attempt_error = match provider
                .stream_chat(&route.provider_model, &messages, specs, &model_options)
                .await
            {
                Err(error) => Some(error),
                Ok(stream) => {
                    let mut stream = trouve_providers::coalesce_event_stream(stream);
                    let mut error = None;
                    loop {
                        let event = tokio::select! {
                            biased;
                            _ = cancel.cancelled() => break,
                            event = stream.next() => match event {
                                Some(event) => event,
                                None => break,
                            },
                        };
                        match event {
                            Err(provider_error) => {
                                error = Some(provider_error);
                                break;
                            }
                            Ok(ProviderEvent::TextDelta(delta)) => {
                                text.push_str(&delta);
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantDelta { turn, text: delta },
                                )?;
                            }
                            Ok(ProviderEvent::ThinkingDelta(delta)) => {
                                self.store.append_event(
                                    scope.clone(),
                                    Event::AssistantThinking { turn, text: delta },
                                )?;
                            }
                            Ok(ProviderEvent::Reasoning(block)) => reasoning.push(block),
                            Ok(ProviderEvent::ToolCall(call)) => tool_calls.push(call),
                            Ok(ProviderEvent::Completed { usage }) => {
                                accounting.add_native(&self.model_catalog, route, &usage);
                            }
                        }
                    }
                    error
                }
            };

            if let Some(error) = attempt_error {
                if !text.is_empty() {
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )?;
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning: Vec::new(),
                        })?,
                    )?;
                }
                return Ok(RouteAttemptResult::Failed(native_attempt_failure(error)));
            }

            if cancel.is_cancelled() {
                if !text.is_empty() {
                    self.store.append_event(
                        scope.clone(),
                        Event::AssistantMessage {
                            turn,
                            content: text.clone(),
                        },
                    )?;
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                return Ok(RouteAttemptResult::Cancelled);
            }

            continuation_needed = false;
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !tools_enabled && !tool_calls.is_empty() {
                tracing::warn!(
                    thread_id = %thread.id,
                    turn,
                    "provider requested a tool during a tool-free turn; ignoring the request"
                );
                tool_calls.clear();
            }
            if !text.is_empty() || !tool_calls.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: tool_calls.clone(),
                        reasoning,
                    })?,
                )?;
            }
            if tool_calls.is_empty() {
                return Ok(RouteAttemptResult::Completed);
            }
            for call in tool_calls {
                let (result_content, images) = self
                    .handle_tool_call(session, thread, turn, mode, tool_ctx, &call, cancel)
                    .await?;
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::ToolResult {
                        call_id: call.id,
                        content: result_content,
                        images,
                    })?,
                )?;
            }
        }

        self.run_native_iteration_summary(
            thread,
            turn,
            route,
            system,
            &model_options,
            accounting,
            cancel,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_native_iteration_summary(
        &self,
        thread: &Thread,
        turn: u64,
        route: &ModelCandidate,
        system: &str,
        model_options: &serde_json::Map<String, serde_json::Value>,
        accounting: &mut TurnAccounting,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Native(provider) = &route.executor else {
            unreachable!("native summary helper received a backend")
        };
        let scope = Scope::Thread(thread.id.clone());
        let mut messages = vec![Message::System(system.to_string())];
        for payload in self.store.messages(&thread.id)? {
            messages.push(serde_json::from_value(payload)?);
        }
        let mut messages = sanitize_transcript(messages);
        messages.push(Message::User(format!(
            "You reached the hard {MAX_ITERATIONS}-step limit for this turn. Do not call any \
             more tools. Give the user a concise progress report based on the tool results \
             above, clearly identify unfinished work, and ask them to continue in a new turn."
        )));
        let mut text = String::new();
        let mut reasoning = Vec::new();
        let error = match provider
            .stream_chat(&route.provider_model, &messages, &[], model_options)
            .await
        {
            Err(error) => Some(error),
            Ok(stream) => {
                let mut stream = trouve_providers::coalesce_event_stream(stream);
                let mut error = None;
                loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        event = stream.next() => match event {
                            Some(event) => event,
                            None => break,
                        },
                    };
                    match event {
                        Ok(ProviderEvent::TextDelta(delta)) => {
                            text.push_str(&delta);
                            self.store.append_event(
                                scope.clone(),
                                Event::AssistantDelta { turn, text: delta },
                            )?;
                        }
                        Ok(ProviderEvent::ThinkingDelta(delta)) => {
                            self.store.append_event(
                                scope.clone(),
                                Event::AssistantThinking { turn, text: delta },
                            )?;
                        }
                        Ok(ProviderEvent::Reasoning(block)) => reasoning.push(block),
                        Ok(ProviderEvent::Completed { usage }) => {
                            accounting.add_native(&self.model_catalog, route, &usage);
                        }
                        Ok(ProviderEvent::ToolCall(_)) => {}
                        Err(provider_error) => {
                            error = Some(provider_error);
                            break;
                        }
                    }
                }
                error
            }
        };
        if cancel.is_cancelled() {
            return Ok(RouteAttemptResult::Cancelled);
        }
        if let Some(error) = error {
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Failed(native_attempt_failure(error)));
        }
        if text.trim().is_empty() {
            text = format!(
                "Reached the {MAX_ITERATIONS}-step limit for one turn and stopped mid-task. \
                 Send another message to continue."
            );
        }
        self.store.append_event(
            scope,
            Event::AssistantMessage {
                turn,
                content: text.clone(),
            },
        )?;
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning,
            })?,
        )?;
        Ok(RouteAttemptResult::Completed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_backend_route(
        &self,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentMode,
        route: &ModelCandidate,
        initial_content: &str,
        attachments: &[trouve_agents::TurnAttachment],
        history_before: &[serde_json::Value],
        retrying: bool,
        permission: BackendPermission,
        github_repository: Option<&(String, String, String)>,
        recorded_prs: &mut HashSet<u64>,
        accounting: &mut TurnAccounting,
        tools_enabled: bool,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<RouteAttemptResult> {
        let ModelExecutor::Backend(backend) = &route.executor else {
            unreachable!("backend route helper received a native provider")
        };
        let scope = Scope::Thread(thread.id.clone());
        let backend_id = &route.provider_id;
        let payloads = if retrying {
            self.store.messages(&thread.id)?
        } else {
            history_before.to_vec()
        };
        let resume = if tools_enabled {
            self.store.backend_session(&thread.id, backend_id)?
        } else {
            None
        };
        let unseen = match &resume {
            Some((_, seen)) => payloads.get(*seen as usize..).unwrap_or(&payloads),
            None => &payloads,
        };
        let handoff = {
            let messages: Vec<Message> = unseen
                .iter()
                .filter_map(|payload| serde_json::from_value(payload.clone()).ok())
                .collect();
            render_history_digest(&messages, resume.is_some())
        };
        let vendor_session = resume.map(|(id, _)| id);
        let attempt_prompt = if retrying {
            let continuation = "Another provider could not continue this turn. Continue the \
                in-progress task from the transcript and current worktree. Do not repeat \
                completed text, commands, or edits; inspect state when unsure.";
            match handoff {
                Some(digest) => format!("{digest}\n\n{continuation}"),
                None => continuation.into(),
            }
        } else {
            match handoff {
                Some(digest) => format!("{digest}\n\n{initial_content}"),
                None => initial_content.into(),
            }
        };

        let mcp_bridge = tools_enabled
            .then(|| self.mcp_bridge_for(backend_id, &thread.id))
            .flatten();
        let mut instructions = mode.system_prompt.trim().to_string();
        if mcp_bridge.is_some() {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_SEARCH_GUIDANCE);
        }
        let mut model_options = self.store.thread_model_options(&thread.id)?;
        normalize_thinking_option(&mut model_options, Some(&route.info));
        let backend_turn = BackendTurn {
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: route.provider_model.clone(),
            model_options,
            prompt: attempt_prompt,
            attachments: attachments.to_vec(),
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            tool_free: !tools_enabled,
            mcp_bridge,
            mcp_servers: if tools_enabled {
                self.mcp_servers_for(session)?
            } else {
                Vec::new()
            },
        };
        let mut stream = match backend.run_turn(backend_turn).await {
            Ok(stream) => stream,
            Err(error) => {
                return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                    error, false,
                )));
            }
        };

        let mut text = String::new();
        let mut segment = String::new();
        let mut attempt_usage = Usage::default();
        let mut backend_error = None;
        let mut open_tools = HashSet::new();
        let mut side_effect_started = false;
        let mut tool_calls = HashMap::<String, (String, serde_json::Value)>::new();
        let mut github_creation_output = HashMap::<String, String>::new();
        let mut persisted = Vec::new();
        let mut persist_deadline = None;
        loop {
            let flush_at = persist_deadline.unwrap_or_else(Instant::now);
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    continue;
                }
                event = stream.next() => match event {
                    Some(event) => event,
                    None => break,
                },
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    backend_error = Some(error);
                    break;
                }
            };
            match event {
                BackendEvent::SessionStarted { session_id } => {
                    if tools_enabled {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        self.store
                            .set_backend_session(&thread.id, backend_id, &session_id)?;
                    }
                }
                BackendEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    segment.push_str(&delta);
                    persisted.push(Event::AssistantDelta { turn, text: delta });
                }
                BackendEvent::ThinkingDelta(delta) => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantThinking { turn, text: delta });
                }
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    mut args,
                } => {
                    side_effect_started = true;
                    open_tools.insert(call_id.clone());
                    tool_calls.insert(call_id.clone(), (tool.clone(), args.clone()));
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    annotate_edit_lines(Path::new(&session.worktree_path), &mut args);
                    if !self.tool_card_exists(&thread.id, turn, &call_id) {
                        persisted.push(Event::ToolRequested {
                            turn,
                            call_id: call_id.clone(),
                            tool,
                            args,
                            requires_approval: false,
                        });
                    }
                    persisted.push(Event::ToolStarted { call_id });
                }
                BackendEvent::ToolOutput { call_id, chunk } => {
                    if let Some((_, owner, repo)) = github_repository
                        && let Some((tool, args)) = tool_calls.get(&call_id)
                        && requests_pull_request_creation(tool, args, owner, repo)
                    {
                        github_creation_output
                            .entry(call_id.clone())
                            .or_default()
                            .push_str(&chunk);
                    }
                    persisted.push(Event::ToolOutput { call_id, chunk });
                }
                BackendEvent::CommandsUpdated { commands } => {
                    persisted.push(Event::CommandsUpdated { commands });
                }
                BackendEvent::ToolCompleted {
                    call_id,
                    ok,
                    result,
                } => {
                    open_tools.remove(&call_id);
                    let status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Error
                    };
                    let todos = match tool_calls.get(&call_id) {
                        Some((tool, args)) => self.persist_todos_from_result(
                            &thread.id,
                            tool,
                            status,
                            &result,
                            Some(args),
                        )?,
                        None => None,
                    };
                    if ok
                        && let Some(repository @ (host, owner, repo)) = github_repository
                        && let Some((tool, args)) = tool_calls.get(&call_id)
                        && requests_pull_request_creation(tool, args, owner, repo)
                    {
                        let mut numbers = pr_numbers_in_value(args, host, owner, repo);
                        numbers.extend(pr_numbers_in_value(&result, host, owner, repo));
                        if let Some(output) = github_creation_output.remove(&call_id) {
                            numbers.extend(crate::github::pr_numbers_in_text(
                                &output, host, owner, repo,
                            ));
                        }
                        self.record_session_pr_numbers(
                            &session.id,
                            repository,
                            numbers,
                            recorded_prs,
                        )?;
                    } else {
                        github_creation_output.remove(&call_id);
                    }
                    persisted.push(Event::ToolCompleted {
                        call_id,
                        status,
                        result,
                    });
                    if let Some(todos) = todos {
                        persisted.push(Event::TodosUpdated { todos });
                    }
                }
                BackendEvent::ApprovalNeeded {
                    call_id,
                    tool,
                    args,
                    responder,
                } => {
                    side_effect_started = true;
                    open_tools.insert(call_id.clone());
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    let approved = self
                        .gate_backend_approval(session, thread, turn, mode, &call_id, &tool, &args)
                        .await?;
                    let _ = responder.send(approved);
                }
                BackendEvent::QuestionsNeeded {
                    request_id,
                    title,
                    questions,
                    responder,
                } => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    persist_deadline = None;
                    let answers = self
                        .ask_user_questions(&thread.id, turn, &request_id, title, questions)
                        .await?;
                    let _ = responder.send(answers);
                }
                BackendEvent::Completed { usage } => {
                    attempt_usage.input_tokens += usage.input_tokens;
                    attempt_usage.output_tokens += usage.output_tokens;
                    attempt_usage.cached_input_tokens += usage.cached_input_tokens;
                    if let Some(cost) = usage.cost_usd {
                        attempt_usage.cost_usd = Some(attempt_usage.cost_usd.unwrap_or(0.0) + cost);
                    }
                    if usage.context_window.is_some() {
                        attempt_usage.context_window = usage.context_window;
                    }
                }
            }
            if persisted.len() >= BACKEND_EVENT_BATCH_MAX {
                flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                persist_deadline = None;
            } else if !persisted.is_empty() && persist_deadline.is_none() {
                persist_deadline = Some(Instant::now() + BACKEND_EVENT_BATCH_WINDOW);
            }
        }
        drop(stream);
        accounting.add_backend(&attempt_usage);

        if let Some(error) = backend_error {
            if !segment.is_empty() {
                persisted.push(Event::AssistantMessage {
                    turn,
                    content: std::mem::take(&mut segment),
                });
            }
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            for call_id in open_tools {
                persisted.push(Event::ToolCompleted {
                    call_id,
                    status: ToolStatus::Aborted,
                    result: serde_json::json!({
                        "error": "provider route ended during tool execution"
                    }),
                });
            }
            flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
            if tools_enabled {
                let seen = self.store.messages(&thread.id)?.len() as u64;
                self.store.mark_backend_seen(&thread.id, backend_id, seen)?;
            }
            return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                error,
                side_effect_started,
            )));
        }

        if cancel.is_cancelled() {
            if !segment.is_empty() {
                persisted.push(Event::AssistantMessage {
                    turn,
                    content: segment,
                });
            }
            flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
            if !text.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Cancelled);
        }

        if !segment.is_empty() {
            persisted.push(Event::AssistantMessage {
                turn,
                content: segment,
            });
        }
        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
        self.store.append_message(
            &thread.id,
            &serde_json::to_value(Message::Assistant {
                content: text,
                tool_calls: Vec::new(),
                reasoning: Vec::new(),
            })?,
        )?;
        if tools_enabled {
            let seen = self.store.messages(&thread.id)?.len() as u64;
            self.store.mark_backend_seen(&thread.id, backend_id, seen)?;
        }
        Ok(RouteAttemptResult::Completed)
    }
}
