//! Provider-neutral turn coordination.
//!
//! Native chat providers and vendor-agent backends keep their own execution
//! mechanics, but report one common attempt outcome here. The persisted
//! transcript and session worktree are the handoff boundary between them.

use super::*;

pub(super) fn unfinished_collaborator_reason(
    cancelled: bool,
    attempt_error: Option<&anyhow::Error>,
    backend_error: Option<&BackendError>,
) -> String {
    if cancelled {
        "turn cancelled".to_string()
    } else if let Some(error) = attempt_error {
        format!("parent turn event processing failed: {error}")
    } else if let Some(error) = backend_error {
        format!("parent backend stream failed: {error}")
    } else {
        "parent turn ended before collaborator completion".to_string()
    }
}

impl Engine {
    pub(super) async fn run_routed_turn(
        self: &Arc<Self>,
        thread: &Thread,
        turn: u64,
        prompt: &trouve_protocol::QueuedPrompt,
        cancel: tokio_util::sync::CancellationToken,
        prompt_persisted: &AtomicBool,
        active_attempt: &Mutex<Option<RoutedAttemptSnapshot>>,
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
        let canonical_worktree = worktree.canonicalize()?;
        let tool_ctx = ToolCtx {
            cancel: cancel.clone(),
            worktree: worktree.clone(),
            canonical_worktree: Some(canonical_worktree),
            read_only_roots: crate::skills::trusted_read_roots(
                self.config_dir.as_deref(),
                Some(Path::new(&workspace.path)),
            )
            .into(),
            thread_id: thread.id.clone(),
            todos: Arc::new(Mutex::new(thread.todos.clone())),
            config_dir: self.config_dir.clone(),
            workspace_root: Some(PathBuf::from(&workspace.path)),
            edit_strategy: edit_strategy_for_model(&thread.model),
            background_mutation_lease: None,
        };

        let personas = self.resolve_personas(Some(Path::new(&workspace.path)))?;
        let mode = personas::find_persona(&personas, &thread.mode)
            .cloned()
            .unwrap_or_else(personas::fallback_persona);

        // Accept the turn durably before automatic route discovery or
        // capacity acquisition. If either setup step fails, the dispatcher
        // can now publish a visible TurnFailed for the accepted user message.
        if !prompt_persisted.load(Ordering::Acquire) {
            let shell_options = self.store.thread_model_options(&thread.id)?;
            let thinking_level = resolved_thinking_level(&shell_options, None);
            let message = accepted_prompt_message(prompt)?;
            self.store
                .append_events_accepting_claimed_prompt(
                    &thread.id,
                    turn,
                    &prompt.id,
                    message,
                    vec![
                        Event::TurnStarted {
                            turn,
                            mode: thread.mode.clone(),
                            model: thread.model.clone(),
                            thinking_level,
                            // A later failover may select an adapter without
                            // steering, so advertise conservatively.
                            supports_steering: false,
                        },
                        Event::UserMessage {
                            turn,
                            content: prompt.content.clone(),
                            attachments: prompt.attachments.clone(),
                        },
                    ],
                )
                .await?;
            prompt_persisted.store(true, Ordering::Release);
        }

        let mut candidates = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            candidates = self.resolve_model_candidates(thread) => {
                candidates.map_err(|error| anyhow!(error.to_string()))?
            }
        };
        anyhow::ensure!(!candidates.is_empty(), "model route disappeared");
        let selection_info = model_info_for_routed_selection(routed_model_info(
            thread.model.clone(),
            candidates.clone(),
        ));
        let total_candidates = candidates.len();
        let concurrent_child = mode.read_only && self.store.spawn_parent(&thread.id)?.is_some();
        // Session lifecycle always precedes turn/provider capacity. Concrete
        // and automatic routes therefore share one lock order and cannot
        // deadlock while session deletion or restore waits for the write lock.
        let session_lifecycle = self.session_lock(&session.id);
        let _session_lifecycle_guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("turn cancelled"),
            guard = session_lifecycle.read() => guard,
        };
        let background = self.store.is_code_review_thread(&thread.id)?;
        let mut first_candidate = 0;
        let first_route_capacity = loop {
            let route = candidates.get(first_candidate).with_context(|| {
                format!(
                    "no provider is currently able to run {}; every eligible route is cooling down",
                    thread.model
                )
            })?;
            let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
            if let Some(capacity) = self
                .turn_scheduler
                .acquire_routed(&capacity_model, background, &cancel)
                .await?
            {
                break capacity;
            }
            first_candidate += 1;
        };
        if first_candidate > 0 {
            candidates.drain(..first_candidate);
        }
        candidates.truncate(MAX_ROUTE_ATTEMPTS_PER_TURN);
        let first_route = candidates.first().context("model route disappeared")?;
        let capacity_wait_ms = first_route_capacity.wait_ms;
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

        let has_native = candidates
            .iter()
            .any(|candidate| matches!(candidate.executor, ModelExecutor::Native(_)));
        let failover_context_window = candidates
            .iter()
            .map(|candidate| candidate.info.context_window)
            .filter(|window| *window > 0)
            .min()
            .unwrap_or(first_route.info.context_window);
        self.store.append_event(
            scope.clone(),
            Event::ModelRouteSelected {
                turn,
                model: thread.model.clone(),
                provider_id: first_route.provider_id.clone(),
                provider_model: first_route.provider_model.clone(),
                reason: trouve_protocol::ModelRouteReason::Initial,
            },
        )?;
        // Compaction summarizes only earlier transcript rows and preserves
        // the accepted current user message as the final row. If a backend is
        // selected first and later hands off to native execution, the native
        // route uses the full persisted transcript for that exceptional
        // continuation.
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
                    &cancel,
                )
                .await
        {
            tracing::warn!("compaction failed for {}: {error}", thread.id);
        }
        // The first backend attempt must see the same post-compaction
        // transcript as native execution, excluding the current user message
        // that is carried separately in the backend prompt.
        let mut history_before = self.store.messages(&thread.id)?;
        let current_user = history_before
            .pop()
            .context("accepted user message is missing before backend routing")?;
        let expected_user =
            serde_json::to_value(accepted_user_message(&prompt.content, &prompt.attachments)?)?;
        anyhow::ensure!(
            current_user == expected_user,
            "accepted user message is not the final transcript row before backend routing"
        );

        // Materialization stays behind ToolExecutor and happens once before
        // any cross-adapter handoff, so every route sees the same safe paths.
        let materialized = self
            .materialize_attachments_for_turn(&session, &attachments, &cancel)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
        let (images, files): (Vec<_>, Vec<_>) = materialized
            .into_iter()
            .partition(|file| file.attachment.mime.starts_with("image/"));
        let backend_files = files
            .iter()
            .map(|file| (file.attachment.clone(), file.relative_path.clone()))
            .collect::<Vec<_>>();
        let backend_content = annotate_attachments(content, &backend_files);
        let backend_attachments: Vec<trouve_agents::TurnAttachment> = images
            .into_iter()
            .map(|file| trouve_agents::TurnAttachment {
                name: file.attachment.name,
                mime: file.attachment.mime,
                bytes: file.bytes,
                local_path: Some(file.absolute_path),
            })
            .collect();
        if !self.store.finish_queued_prompt(&prompt.id)? {
            bail!("queued prompt {} vanished before turn start", prompt.id);
        }
        self.emit_queue(&thread.id)?;

        let mut specs = Vec::new();
        if has_native && tools_enabled {
            specs = tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("turn cancelled"),
                specs = self.executor.specs(&tool_ctx) => specs,
            }
            .into_iter()
            .filter(|spec| mode.allowed_tools.is_empty() || mode.allowed_tools.contains(&spec.name))
            .collect();
            specs.push(ask_question_spec());
            specs.push(search_transcript_spec());
            let spawn_allowed = |name: &str| {
                mode.allowed_tools.is_empty() || mode.allowed_tools.iter().any(|tool| tool == name)
            };
            if self.thread_can_spawn_subagents(&thread.id)? {
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
        let stored_model_options = model_options_for_schema(
            &self.store.thread_model_options(&thread.id)?,
            &selection_info,
        );
        let permission = backend_permission_policy(tools_enabled, mode.read_only);
        let github_repository = self.github_repository_for_session(&session).ok();
        let mut recorded_prs = if github_repository.is_some() {
            self.recorded_session_pr_numbers(&session.id)?
        } else {
            HashSet::new()
        };
        let mut accounting = TurnAccounting::default();
        let mut native_iterations_left = MAX_ITERATIONS;
        let mut route_capacity = Some(first_route_capacity);
        let mut attempted_candidates = 0usize;
        let mut failover_reason = None;
        let mut last_failure = None::<(String, String, String)>;

        for (route_index, route) in candidates.iter().enumerate() {
            if route_index > 0 {
                // Never reserve the next provider while the previous route's
                // provider/global permits are still live (global limit 1 is a
                // supported configuration).
                drop(route_capacity.take());
                let capacity_model = format!("{}/{}", route.provider_id, route.provider_model);
                let Some(capacity) = self
                    .turn_scheduler
                    .acquire_routed(&capacity_model, background, &cancel)
                    .await?
                else {
                    continue;
                };
                route_capacity = Some(capacity);
                self.store.append_event(
                    scope.clone(),
                    Event::ModelRouteSelected {
                        turn,
                        model: thread.model.clone(),
                        provider_id: route.provider_id.clone(),
                        provider_model: route.provider_model.clone(),
                        reason: failover_reason
                            .unwrap_or(trouve_protocol::ModelRouteReason::RouteFailover),
                    },
                )?;
            }
            let retrying = route_index > 0;
            attempted_candidates += 1;
            // Assign order only after this route owns provider capacity. The
            // global hybrid clock prevents concurrent admissions from tying
            // even when the wall clock has microsecond resolution.
            let attempt_order = self.turn_scheduler.next_attempt_order();
            *active_attempt.lock().unwrap() = Some(RoutedAttemptSnapshot {
                provider_id: route.provider_id.clone(),
                provider_model: route.provider_model.clone(),
                provider_generation: route.provider_generation,
                attempt_order,
            });
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
                    .await
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
                        &stored_model_options,
                        retrying,
                        permission,
                        github_repository.as_ref(),
                        &mut recorded_prs,
                        &mut accounting,
                        tools_enabled,
                        &cancel,
                    )
                    .await
                }
            };
            active_attempt.lock().unwrap().take();
            let result = result?;
            drop(route_capacity.take());

            match result {
                RouteAttemptResult::Completed => {
                    self.with_current_provider_generation(
                        &route.provider_id,
                        route.provider_generation,
                        || {
                            self.turn_scheduler.record_outcome(
                                &route.provider_id,
                                None,
                                attempt_order,
                            );
                            self.store.record_route_success(
                                &route.provider_id,
                                &route.provider_model,
                                attempt_order,
                            )?;
                            if automatic_model_name(&thread.model).is_some() {
                                self.store.set_thread_route_affinity(
                                    &thread.id,
                                    &route.provider_id,
                                    &route.provider_model,
                                )?;
                            }
                            Ok(())
                        },
                    )?;
                    self.record_routed_usage(&session.id, &thread.id, turn, &mut accounting, true)?;
                    let checkpoint_id = if concurrent_child {
                        None
                    } else {
                        self.maybe_checkpoint(&session, thread, turn, &cancel)
                            .await?
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
                    self.record_routed_usage(
                        &session.id,
                        &thread.id,
                        turn,
                        &mut accounting,
                        false,
                    )?;
                    return Ok(());
                }
                RouteAttemptResult::Failed(failure) => {
                    last_failure = Some((
                        route.provider_id.clone(),
                        route.provider_model.clone(),
                        failure.message.clone(),
                    ));
                    let (base, max) = failure.kind.cooldown();
                    let health = self.with_current_provider_generation(
                        &route.provider_id,
                        route.provider_generation,
                        || {
                            self.turn_scheduler.record_outcome(
                                &route.provider_id,
                                Some(&failure.message),
                                attempt_order,
                            );
                            self.store
                                .record_route_failure(
                                    &route.provider_id,
                                    &route.provider_model,
                                    attempt_order,
                                    base,
                                    max,
                                )
                                .and_then(|health| {
                                    self.store.clear_thread_route_affinity_if_matches(
                                        &thread.id,
                                        &route.provider_id,
                                        &route.provider_model,
                                    )?;
                                    Ok(health)
                                })
                        },
                    )?;
                    if let Some(health) = health {
                        tracing::warn!(
                            model = %thread.model,
                            provider = %route.provider_id,
                            failures = health.consecutive_failures,
                            retry_after = health.retry_after,
                            error = %failure.message,
                            "model route opened its circuit"
                        );
                    }
                    let has_next = route_index + 1 < candidates.len();
                    if !failure.safe_to_retry {
                        self.record_routed_usage(
                            &session.id,
                            &thread.id,
                            turn,
                            &mut accounting,
                            false,
                        )?;
                        if automatic_model_name(&thread.model).is_some() && has_next {
                            bail!(
                                "automatic model {} failed on {}/{} and cannot safely switch providers: {}",
                                thread.model,
                                route.provider_id,
                                route.provider_model,
                                failure.message,
                            );
                        }
                        bail!(
                            "selected provider route {}/{} failed: {}",
                            route.provider_id,
                            route.provider_model,
                            failure.message,
                        );
                    }
                    failover_reason = Some(failure.kind.failover_reason());
                    if !has_next {
                        self.record_routed_usage(
                            &session.id,
                            &thread.id,
                            turn,
                            &mut accounting,
                            false,
                        )?;
                        let untried = total_candidates.saturating_sub(attempted_candidates);
                        if automatic_model_name(&thread.model).is_some() {
                            if untried == 0 {
                                bail!(
                                    "no provider is currently able to run {}; tried {} route(s). Last error from {}/{}: {}",
                                    thread.model,
                                    attempted_candidates,
                                    route.provider_id,
                                    route.provider_model,
                                    failure.message,
                                );
                            }
                            bail!(
                                "unable to route {} after {} route attempts; {} alternate route(s) remain deferred until the next turn. Last error from {}/{}: {}",
                                thread.model,
                                attempted_candidates,
                                untried,
                                route.provider_id,
                                route.provider_model,
                                failure.message,
                            );
                        }
                        bail!(
                            "selected provider route {}/{} failed: {}",
                            route.provider_id,
                            route.provider_model,
                            failure.message,
                        );
                    }
                }
            }
        }

        self.record_routed_usage(&session.id, &thread.id, turn, &mut accounting, false)?;
        if let Some((provider_id, provider_model, message)) = last_failure {
            bail!(
                "no provider is currently able to run {}; tried {} route(s). Last error from {}/{}: {}",
                thread.model,
                attempted_candidates,
                provider_id,
                provider_model,
                message,
            );
        }
        bail!(
            "no provider is currently able to run {}; every remaining route is cooling down",
            thread.model
        )
    }

    pub(super) fn record_routed_usage(
        &self,
        session_id: &str,
        thread_id: &str,
        turn: u64,
        accounting: &mut TurnAccounting,
        record_empty: bool,
    ) -> Result<()> {
        accounting.finalize_cost();
        if record_empty
            || accounting.usage.input_tokens > 0
            || accounting.usage.output_tokens > 0
            || accounting.usage.cached_input_tokens > 0
            || accounting.usage.cost_usd.is_some()
        {
            self.store.record_usage(
                session_id,
                thread_id,
                turn,
                &accounting.usage,
                accounting.context_input_tokens,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_native_route(
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
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
        let model_options = model_options_for_schema(stored_model_options, &route.info);
        // Keep one sanitized transcript in memory for the provider tool loop.
        // Every assistant/tool message is still persisted immediately, then
        // appended here for the next iteration without re-reading and
        // deserializing the entire thread.
        let mut messages = vec![Message::System(system.to_string())];
        for payload in self.store.messages(&thread.id)? {
            messages.push(serde_json::from_value(payload)?);
        }
        let mut messages = sanitize_transcript(messages);
        if retrying {
            messages.push(Message::User(
                "Another provider could not continue this turn. Continue the in-progress \
                 response from the transcript and current worktree without repeating \
                 completed text, tool calls, or edits."
                    .into(),
            ));
        }
        let mut side_effect_started = false;

        while *iterations_left > 0 {
            if cancel.is_cancelled() {
                return Ok(RouteAttemptResult::Cancelled);
            }
            *iterations_left -= 1;

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
                    let mut completed = false;
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
                                completed = true;
                                accounting.add_native(&self.model_catalog, route, &usage);
                            }
                        }
                    }
                    if error.is_none() && !cancel.is_cancelled() && !completed {
                        error = Some(trouve_providers::ProviderError::Request(
                            "provider stream ended before a completion event".into(),
                        ));
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
                }
                if !text.is_empty() || !reasoning.is_empty() {
                    self.store.append_message(
                        &thread.id,
                        &serde_json::to_value(Message::Assistant {
                            content: text,
                            tool_calls: Vec::new(),
                            reasoning,
                        })?,
                    )?;
                }
                return Ok(RouteAttemptResult::Failed(native_attempt_failure(
                    error,
                    side_effect_started,
                )));
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
                let assistant = Message::Assistant {
                    content: text,
                    tool_calls: tool_calls.clone(),
                    reasoning,
                };
                self.store
                    .append_message(&thread.id, &serde_json::to_value(&assistant)?)?;
                messages.push(assistant);
            }
            if tool_calls.is_empty() {
                return Ok(RouteAttemptResult::Completed);
            }
            // Classify before dispatch: after a mutation-capable call begins,
            // its outcome may be unknown if the next provider request fails.
            // Engine-served read/question helpers are the only calls outside
            // ToolExecutor that are known not to change durable state.
            side_effect_started |= tool_calls.iter().any(|call| match call.name.as_str() {
                "ask_question" | "search_transcript" | "spawn_output" => false,
                "spawn_thread" | "spawn_session" => true,
                _ => self.executor.tool_mutates(&call.name) != Some(false),
            });
            // Keep the same read-only concurrency and mutation barriers used
            // by concrete-model turns. Route failover must not change tool
            // scheduling semantics merely because the model was automatic.
            let results = self
                .handle_tool_calls_parallel(
                    session, thread, turn, mode, tool_ctx, tool_calls, cancel,
                )
                .await;
            for (call_id, result) in results {
                let (result_content, images) = result?;
                let result = Message::ToolResult {
                    call_id,
                    content: result_content,
                    images,
                };
                self.store
                    .append_message(&thread.id, &serde_json::to_value(&result)?)?;
                messages.push(result);
            }
        }

        self.run_native_iteration_summary(
            thread,
            turn,
            route,
            system,
            &model_options,
            accounting,
            side_effect_started,
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
        side_effect_started: bool,
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
                let mut completed = false;
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
                            completed = true;
                            accounting.add_native(&self.model_catalog, route, &usage);
                        }
                        Ok(ProviderEvent::ToolCall(_)) => {}
                        Err(provider_error) => {
                            error = Some(provider_error);
                            break;
                        }
                    }
                }
                if error.is_none() && !cancel.is_cancelled() && !completed {
                    error = Some(trouve_providers::ProviderError::Request(
                        "provider stream ended before a completion event".into(),
                    ));
                }
                error
            }
        };
        if cancel.is_cancelled() {
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !text.is_empty() || !reasoning.is_empty() {
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
        if let Some(error) = error {
            if !text.is_empty() {
                self.store.append_event(
                    scope.clone(),
                    Event::AssistantMessage {
                        turn,
                        content: text.clone(),
                    },
                )?;
            }
            if !text.is_empty() || !reasoning.is_empty() {
                self.store.append_message(
                    &thread.id,
                    &serde_json::to_value(Message::Assistant {
                        content: text,
                        tool_calls: Vec::new(),
                        reasoning,
                    })?,
                )?;
            }
            return Ok(RouteAttemptResult::Failed(native_attempt_failure(
                error,
                side_effect_started,
            )));
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
        self: &Arc<Self>,
        session: &Session,
        thread: &Thread,
        turn: u64,
        mode: &AgentPersona,
        route: &ModelCandidate,
        initial_content: &str,
        attachments: &[trouve_agents::TurnAttachment],
        history_before: &[serde_json::Value],
        stored_model_options: &serde_json::Map<String, serde_json::Value>,
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
        let effective_read_only = !tools_enabled || mode.read_only;
        // Backends that can remove every native tool must honor the strict
        // tool-free contract. Others remain usable for read/search activity,
        // but run read-only with no mounted MCP servers.
        let strict_tool_free =
            backend_strict_tool_free_policy(tools_enabled, backend.supports_tool_free_turns());
        let scope = Scope::Thread(thread.id.clone());
        let backend_id = &route.provider_id;
        let payloads = if retrying {
            self.store.messages(&thread.id)?
        } else {
            history_before.to_vec()
        };
        let submitted_transcript_messages =
            u64::try_from(payloads.len().saturating_add(usize::from(!retrying)))
                .context("backend transcript length exceeds u64")?;
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
        let mut active_vendor_session = vendor_session.clone();
        if let Some(vendor_session_id) = active_vendor_session.as_deref() {
            self.bridged_tool_owners
                .bind_vendor_thread(&thread.id, vendor_session_id, &thread.id)
                .map_err(anyhow::Error::msg)?;
        }
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
        let full_tool_bridge = mcp_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.bridge_tools);
        if full_tool_bridge {
            if !instructions.is_empty() {
                instructions.push_str("\n\n");
            }
            instructions.push_str(crate::tools::VENDOR_TOOL_BRIDGE_GUIDANCE);
        }
        // A full bridge exposes user MCP servers through ToolExecutor. Direct
        // mounting would bypass trouve's permission and mutation lanes.
        let mcp_servers = if tools_enabled && !full_tool_bridge {
            self.mcp_servers_for(session)?
        } else {
            Vec::new()
        };
        let model_options = model_options_for_schema(stored_model_options, &route.info);
        let backend_turn = BackendTurn {
            cancel: cancel.clone(),
            thread_id: thread.id.clone(),
            worktree: PathBuf::from(&session.worktree_path),
            session: vendor_session,
            model: route.provider_model.clone(),
            model_options,
            prompt: attempt_prompt,
            attachments: attachments.to_vec(),
            instructions: (!instructions.is_empty()).then_some(instructions),
            permission,
            tool_free: strict_tool_free,
            mcp_bridge,
            mcp_servers,
        };
        let startup_activity = backend.startup_activity(&backend_turn).await;
        if matches!(
            startup_activity,
            Some(BackendStartupActivity::ConnectingTools)
        ) {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::ConnectingTools,
                    },
                )
                .await?;
        }
        let mut stream = match backend.run_turn(backend_turn).await {
            Ok(stream) => stream,
            Err(BackendError::Cancelled) if cancel.is_cancelled() => {
                return Ok(RouteAttemptResult::Cancelled);
            }
            Err(error) => {
                return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                    error, false,
                )));
            }
        };
        if startup_activity.is_some() {
            self.store
                .append_event_async(
                    scope.clone(),
                    Event::TurnPhaseChanged {
                        turn,
                        phase: TurnPhase::Processing,
                    },
                )
                .await?;
        }

        let mut text = String::new();
        let mut segment = String::new();
        let mut attempt_usage = Usage::default();
        let mut backend_error = None;
        let mut attempt_error = None;
        let mut backend_cancelled = false;
        let mut backend_completed = false;
        let mut open_tools = HashSet::new();
        let mut side_effect_started = false;
        let mut tool_calls = HashMap::<String, (String, serde_json::Value)>::new();
        let mut github_creation_output = HashMap::<String, String>::new();
        let mut vendor_threads = HashMap::<String, String>::new();
        if let Some(vendor_session_id) = active_vendor_session.as_ref() {
            vendor_threads.insert(vendor_session_id.clone(), thread.id.clone());
        }
        let mut collaborators = HashMap::<String, BackendCollaboratorProjection>::new();
        let mut collaborator_claims = BackendCollaboratorClaims::new(&self.active_threads);
        let mut pending_backend_approvals = futures::stream::FuturesUnordered::new();
        let mut backend_approval_cancels =
            HashMap::<String, tokio_util::sync::CancellationToken>::new();
        let mut backend_mutation_permits =
            HashMap::<String, tokio::sync::OwnedRwLockWriteGuard<()>>::new();
        let mut suppressed_bridge_calls = HashSet::new();
        let mut persisted = Vec::new();
        let mut persist_deadline = None;
        let event_loop_result: Result<()> = async {
          loop {
            let flush_at = persist_deadline.unwrap_or_else(Instant::now);
            let input = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep_until(flush_at.into()), if persist_deadline.is_some() => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                    persist_deadline = None;
                    continue;
                }
                approval = pending_backend_approvals.next(), if !pending_backend_approvals.is_empty() => {
                    BackendLoopInput::Approval(
                        approval.expect("non-empty approval queue must yield an outcome")
                    )
                }
                event = stream.next() => BackendLoopInput::Event(event),
            };
            let event = match input {
                BackendLoopInput::Event(None) => {
                    if !backend_completed && !cancel.is_cancelled() {
                        backend_error = Some(BackendError::Protocol(
                            "backend stream ended before a completion event".into(),
                        ));
                    }
                    break;
                }
                BackendLoopInput::Approval(outcome) => {
                    let BackendApprovalOutcome {
                        owner_thread_id,
                        call_id,
                        responder,
                        approved,
                        mutation_permit,
                    } = outcome;
                    if let Some(owner_thread_id) = owner_thread_id {
                        let Some(collaborator) = collaborators
                            .values_mut()
                            .find(|collaborator| collaborator.thread.id == owner_thread_id)
                        else {
                            let _ = responder.send(false);
                            continue;
                        };
                        collaborator.approval_cancels.remove(&call_id);
                        if collaborator.terminal {
                            let _ = responder.send(false);
                            continue;
                        }
                        let approved = match approved {
                            Ok(approved) => approved,
                            Err(error) => {
                                let _ = responder.send(false);
                                attempt_error = Some(error);
                                break;
                            }
                        };
                        if approved {
                            if let Some(permit) = mutation_permit {
                                collaborator
                                    .mutation_permits
                                    .insert(call_id.clone(), permit);
                            }
                            if responder.send(true).is_err() {
                                collaborator.mutation_permits.remove(&call_id);
                            }
                        } else {
                            let _ = responder.send(false);
                        }
                        continue;
                    }
                    backend_approval_cancels.remove(&call_id);
                    let approved = match approved {
                        Ok(approved) => approved,
                        Err(error) => {
                            let _ = responder.send(false);
                            attempt_error = Some(error);
                            break;
                        }
                    };
                    if approved {
                        if let Some(permit) = mutation_permit {
                            backend_mutation_permits.insert(call_id.clone(), permit);
                        }
                        if responder.send(true).is_err() {
                            backend_mutation_permits.remove(&call_id);
                        }
                    } else {
                        let _ = responder.send(false);
                    }
                    continue;
                }
                BackendLoopInput::Event(Some(Ok(event))) => event,
                BackendLoopInput::Event(Some(Err(BackendError::Cancelled)))
                    if cancel.is_cancelled() =>
                {
                    backend_cancelled = true;
                    break;
                }
                BackendLoopInput::Event(Some(Err(error))) => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                    backend_error = Some(error);
                    break;
                }
            };
            match event {
                BackendEvent::SessionStarted { session_id } => {
                    active_vendor_session = Some(session_id.clone());
                    vendor_threads.insert(session_id.clone(), thread.id.clone());
                    self.bridged_tool_owners
                        .bind_vendor_thread(&thread.id, &session_id, &thread.id)
                        .map_err(anyhow::Error::msg)?;
                    if tools_enabled {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        self.store.set_backend_session_at_watermark(
                            &thread.id,
                            backend_id,
                            &session_id,
                            submitted_transcript_messages,
                        )?;
                    }
                }
                BackendEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    segment.push_str(&delta);
                    persisted.push(Event::AssistantDelta { turn, text: delta });
                }
                BackendEvent::ProgressDelta(delta) => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::AssistantProgress { turn, text: delta });
                }
                BackendEvent::ProgressCompleted => {
                    persisted.push(Event::AssistantProgressCompleted { turn });
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
                BackendEvent::ThinkingCompleted => {
                    persisted.push(Event::AssistantThinkingCompleted { turn });
                }
                BackendEvent::ToolStarted {
                    call_id,
                    tool,
                    mut args,
                } => {
                    if let Some((nested_tool, _)) = trouve_bridge_wrapper_call(&tool, &args) {
                        side_effect_started |=
                            self.executor.tool_mutates(nested_tool) != Some(false);
                        if suppressed_bridge_calls.insert(call_id.clone())
                            && let Some(vendor_thread_id) = active_vendor_session.as_deref()
                        {
                            self.announce_trouve_bridge_wrapper(
                                &thread.id,
                                vendor_thread_id,
                                &thread.id,
                                &call_id,
                                &tool,
                                &args,
                            );
                        }
                        continue;
                    }
                    if strict_tool_free {
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        side_effect_started = true;
                        backend_error = Some(BackendError::Protocol(format!(
                            "backend requested tool {tool} during a tool-free turn"
                        )));
                        break;
                    }
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
                    if suppressed_bridge_calls.contains(&call_id) {
                        continue;
                    }
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
                BackendEvent::TodosUpdated { todos } => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    self.store.update_thread_todos(&thread.id, &todos)?;
                    persisted.push(Event::TodosUpdated { todos });
                }
                BackendEvent::UsageUpdated { usage } => {
                    persisted.push(Event::TurnUsageUpdated { turn, usage });
                }
                BackendEvent::CompactionStarted => {
                    if !segment.is_empty() {
                        persisted.push(Event::AssistantMessage {
                            turn,
                            content: std::mem::take(&mut segment),
                        });
                    }
                    persisted.push(Event::CompactionStarted { turn });
                }
                BackendEvent::CompactionCompleted => {
                    persisted.push(Event::CompactionCompleted {
                        turn,
                        messages_compacted: 0,
                    });
                }
                BackendEvent::CompactionFailed => {
                    persisted.push(Event::CompactionFailed { turn });
                }
                BackendEvent::CollaboratorStarted {
                    session_id,
                    parent_session_id,
                    name,
                    access,
                    prompt,
                    model,
                    thinking_level,
                } => {
                    flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                    let vendor_session_id = session_id.clone();
                    let prompt_announced =
                        prompt.as_deref().is_some_and(|prompt| !prompt.is_empty());
                    self.start_backend_collaborator_claimed(
                        session,
                        thread,
                        backend_id,
                        session_id,
                        &parent_session_id,
                        name,
                        access,
                        prompt,
                        model,
                        thinking_level,
                        &mut collaborator_claims,
                        &mut vendor_threads,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(owner_thread_id) = vendor_threads.get(&vendor_session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &vendor_session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &vendor_session_id,
                        &mut collaborators,
                    )
                    .await?;
                    if prompt_announced
                        && let Some(collaborator) = collaborators.get_mut(&vendor_session_id)
                    {
                        flush_backend_event_batch(
                            &self.store,
                            &Scope::Thread(collaborator.thread.id.clone()),
                            &mut collaborator.persisted,
                        )
                        .await?;
                    }
                }
                BackendEvent::CollaboratorEvent {
                    session_id,
                    turn_id,
                    mut event,
                } => {
                    if strict_tool_free {
                        event = match event {
                            BackendCollaboratorEvent::ToolStarted { tool, .. } => {
                                side_effect_started = true;
                                backend_error = Some(BackendError::Protocol(format!(
                                    "backend collaborator requested tool {tool} during a tool-free turn"
                                )));
                                break;
                            }
                            BackendCollaboratorEvent::ApprovalNeeded {
                                tool, responder, ..
                            } => {
                                let _ = responder.send(false);
                                backend_error = Some(BackendError::Protocol(format!(
                                    "backend collaborator requested approval for {tool} during a tool-free turn"
                                )));
                                break;
                            }
                            event => event,
                        };
                    }
                    if matches!(
                        &event,
                        BackendCollaboratorEvent::ToolStarted { .. }
                            | BackendCollaboratorEvent::ApprovalNeeded { .. }
                    ) {
                        side_effect_started = true;
                    }
                    if !collaborators.contains_key(&session_id) {
                        let parent_session_id = active_vendor_session
                            .as_deref()
                            .unwrap_or_default()
                            .to_string();
                        self.start_backend_collaborator_claimed(
                            session,
                            thread,
                            backend_id,
                            session_id.clone(),
                            &parent_session_id,
                            None,
                            BackendCollaboratorAccess::Inherit,
                            None,
                            None,
                            None,
                            &mut collaborator_claims,
                            &mut vendor_threads,
                            &mut collaborators,
                        )
                        .await?;
                    }
                    if let Some(owner_thread_id) = vendor_threads.get(&session_id) {
                        self.bridged_tool_owners
                            .bind_vendor_thread(&thread.id, &session_id, owner_thread_id)
                            .map_err(anyhow::Error::msg)?;
                    }
                    if let Some(collaborator) = collaborators.get(&session_id)
                        && !collaborator_claims.claim(&collaborator.thread.id, &session.id)
                    {
                        bail!(
                            "cannot route provider collaborator {} while another turn owns it",
                            collaborator.thread.id
                        );
                    }
                    let completed_successfully =
                        matches!(&event, BackendCollaboratorEvent::Completed { .. });
                    let terminal_thread =
                        if let Some(collaborator) = collaborators.get_mut(&session_id) {
                            self.prepare_backend_collaborator_turn(
                                session,
                                backend_id,
                                collaborator,
                                turn_id.as_deref(),
                            )
                            .await?;
                            if !self.suppress_collaborator_bridge_wrapper(
                                &thread.id,
                                &session_id,
                                collaborator,
                                &event,
                            ) {
                                self.persist_backend_collaborator_event(
                                    session,
                                    mode,
                                    backend_id,
                                    collaborator,
                                    event,
                                    cancel,
                                )
                                .await?;
                            }
                            if let Some(approval) = collaborator.pending_approval.take() {
                                let owner_thread_id = approval.thread.id.clone();
                                let approval_call_id = approval.call_id.clone();
                                let approval_cancel = cancel.child_token();
                                collaborator
                                    .approval_cancels
                                    .insert(approval_call_id, approval_cancel.clone());
                                pending_backend_approvals.push(self.pending_backend_approval(
                                    session.clone(),
                                    approval.thread,
                                    approval.turn,
                                    effective_read_only || approval.mode.read_only,
                                    approval.call_id,
                                    approval.tool,
                                    approval.args,
                                    approval.responder,
                                    approval_cancel,
                                    !full_tool_bridge,
                                    Some(owner_thread_id),
                                ));
                            }
                            collaborator
                                .terminal
                                .then(|| collaborator.thread.id.clone())
                        } else {
                            None
                        };
                    self.publish_backend_collaborator_spawn(
                        thread,
                        turn,
                        &session_id,
                        &mut collaborators,
                    )
                    .await?;
                    if let Some(thread_id) = terminal_thread {
                        collaborator_claims.release(&thread_id);
                        if completed_successfully {
                            self.dispatch_queue(&thread_id)
                                .map_err(|error| anyhow!(error.to_string()))?;
                        }
                    }
                }
                BackendEvent::ToolCompleted {
                    call_id,
                    ok,
                    result,
                } => {
                    backend_mutation_permits.remove(&call_id);
                    if suppressed_bridge_calls.remove(&call_id) {
                        continue;
                    }
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
                        execution_duration_ms: None,
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
                    if strict_tool_free {
                        let _ = responder.send(false);
                        flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                        backend_error = Some(BackendError::Protocol(format!(
                            "backend requested approval for {tool} during a tool-free turn"
                        )));
                        break;
                    }
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
                    let approval_cancel = cancel.child_token();
                    backend_approval_cancels.insert(call_id.clone(), approval_cancel.clone());
                    pending_backend_approvals.push(self.pending_backend_approval(
                        session.clone(),
                        thread.clone(),
                        turn,
                        effective_read_only,
                        call_id,
                        tool,
                        args,
                        responder,
                        approval_cancel,
                        !full_tool_bridge,
                        None,
                    ));
                    continue;
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
                        .ask_user_questions(&thread.id, turn, &request_id, title, questions, cancel)
                        .await?;
                    let _ = responder.send(answers);
                }
                BackendEvent::Completed { usage } => {
                    backend_completed = true;
                    attempt_usage.input_tokens += usage.input_tokens;
                    attempt_usage.output_tokens += usage.output_tokens;
                    attempt_usage.cached_input_tokens += usage.cached_input_tokens;
                    if let Some(cost) = usage.cost_usd {
                        attempt_usage.cost_usd = Some(attempt_usage.cost_usd.unwrap_or(0.0) + cost);
                    }
                    if usage.context_window.is_some() {
                        attempt_usage.context_window = usage.context_window;
                    }
                    if usage.context_input_tokens.is_some() {
                        attempt_usage.context_input_tokens = usage.context_input_tokens;
                    }
                }
            }
            let collaborator_pending = collaborators
                .values()
                .any(|collaborator| !collaborator.persisted.is_empty());
            if persisted.len()
                + collaborators
                    .values()
                    .map(|collaborator| collaborator.persisted.len())
                    .sum::<usize>()
                >= STREAM_EVENT_BATCH_MAX
            {
                flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
                flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
                persist_deadline = None;
            } else if (!persisted.is_empty() || collaborator_pending) && persist_deadline.is_none()
            {
                persist_deadline = Some(Instant::now() + STREAM_EVENT_BATCH_WINDOW);
            }
          }
          Ok(())
        }
        .await;
        if let Err(error) = event_loop_result {
            attempt_error = Some(error);
        }
        drop(stream);
        backend_mutation_permits.clear();
        flush_backend_collaborator_batches(&self.store, &mut collaborators).await?;
        for collaborator in collaborators.values_mut() {
            if !collaborator.terminal {
                let reason = unfinished_collaborator_reason(
                    cancel.is_cancelled() || backend_cancelled,
                    attempt_error.as_ref(),
                    backend_error.as_ref(),
                );
                self.finish_backend_collaborator(session, backend_id, collaborator, Err(reason))
                    .await?;
            }
            collaborator_claims.release(&collaborator.thread.id);
        }
        deny_pending_backend_approvals(
            &mut pending_backend_approvals,
            &mut backend_approval_cancels,
            &mut collaborators,
        )
        .await;
        accounting.add_backend(&attempt_usage);

        if attempt_error.is_some() || backend_error.is_some() {
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
                        content: text.clone(),
                        tool_calls: Vec::new(),
                        reasoning: Vec::new(),
                    })?,
                )?;
            }
            for call_id in open_tools.drain() {
                persisted.push(Event::ToolCompleted {
                    call_id,
                    status: ToolStatus::Aborted,
                    result: serde_json::json!({
                        "error": "provider route ended during tool execution"
                    }),
                    execution_duration_ms: None,
                });
            }
            flush_backend_event_batch(&self.store, &scope, &mut persisted).await?;
            if tools_enabled {
                let seen = self.store.messages(&thread.id)?.len() as u64;
                self.store.mark_backend_seen(&thread.id, backend_id, seen)?;
            }
        }

        if let Some(error) = attempt_error {
            self.record_routed_usage(&session.id, &thread.id, turn, accounting, false)?;
            return Err(error);
        }

        if let Some(error) = backend_error {
            return Ok(RouteAttemptResult::Failed(backend_attempt_failure(
                error,
                side_effect_started,
            )));
        }

        if cancel.is_cancelled() || backend_cancelled {
            if !segment.is_empty() {
                persisted.push(Event::AssistantMessage {
                    turn,
                    content: segment,
                });
            }
            for call_id in open_tools {
                persisted.push(Event::ToolCompleted {
                    call_id,
                    status: ToolStatus::Aborted,
                    result: serde_json::json!({
                        "error": "turn cancelled during tool execution"
                    }),
                    execution_duration_ms: None,
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
