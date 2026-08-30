import type { components as ProtocolComponents } from "../generated/protocol.js";
import type {
  ProtocolEventEnvelope,
  ProtocolThreadToolDetails,
  ProtocolThreadViewSnapshot,
} from "../services/protocol-client.js";
import {
  appendBoundedToolOutput,
  emptyToolOutput,
  type ToolOutputBuffer,
} from "./tool-output.js";

type Attachment = ProtocolComponents["schemas"]["Attachment"];
type CommandInfo = ProtocolComponents["schemas"]["CommandInfo"];
type Question = ProtocolComponents["schemas"]["Question"];
type QuestionAnswer = ProtocolComponents["schemas"]["QuestionAnswer"];
export type QueuedPrompt = ProtocolComponents["schemas"]["QueuedPrompt"];
export type TodoItem = ProtocolComponents["schemas"]["TodoItem"];
type Usage = ProtocolComponents["schemas"]["Usage"];
type TurnPhase = ProtocolComponents["schemas"]["TurnPhase"];
type ThreadViewItem = ProtocolThreadViewSnapshot["items"][number];

const accumulateLiveUsage = (
  total: Usage | undefined,
  latest: Usage,
): Usage => {
  if (total === undefined) return { ...latest };
  const totalCost = total.cost_usd;
  const latestCost = latest.cost_usd;
  const totalCachedInputTokens = total.cached_input_tokens;
  const latestCachedInputTokens = latest.cached_input_tokens;
  const contextInputTokens = latest.context_input_tokens ?? total.context_input_tokens;
  const contextWindow = latest.context_window ?? total.context_window;
  return {
    input_tokens: total.input_tokens + latest.input_tokens,
    output_tokens: total.output_tokens + latest.output_tokens,
    ...(totalCachedInputTokens == null && latestCachedInputTokens == null
      ? {}
      : { cached_input_tokens: (totalCachedInputTokens ?? 0) + (latestCachedInputTokens ?? 0) }),
    ...(totalCost == null && latestCost == null
      ? {}
      : { cost_usd: (totalCost ?? 0) + (latestCost ?? 0) }),
    ...(contextInputTokens == null ? {} : { context_input_tokens: contextInputTokens }),
    ...(contextWindow == null ? {} : { context_window: contextWindow }),
  };
};

const usageWithLiveContext = (usage: Usage, live: Usage | undefined): Usage => {
  const merged = { ...usage };
  if (merged.context_input_tokens == null && live?.context_input_tokens != null) {
    merged.context_input_tokens = live.context_input_tokens;
  }
  if (merged.context_window == null && live?.context_window != null) {
    merged.context_window = live.context_window;
  }
  return merged;
};

/** Constant-space queue revision observation scoped to one pending request. */
export interface QueueRevisionTracker {
  readonly queueChanged: () => boolean;
  readonly close: () => void;
}

export type ToolCallStatus =
  | "awaiting-approval"
  | "running"
  | "ok"
  | "error"
  | "denied"
  | "aborted";

export type TurnState =
  | {
      readonly kind: "waiting-for-capacity";
      readonly startedAt?: string;
    }
  | {
      readonly kind: "running";
      readonly startedAt?: string;
      readonly usage?: Usage;
    }
  | {
      readonly kind: "completed";
      readonly usage: Usage;
      readonly checkpointId?: string;
    }
  | { readonly kind: "failed"; readonly error: string }
  | { readonly kind: "cancelled" };

export type CompactionState =
  | { readonly kind: "running" }
  | { readonly kind: "completed"; readonly messagesCompacted: number }
  | { readonly kind: "failed" };

export type TodoLifecycleState = "started" | "completed" | "cancelled" | "skipped";

export type ThreadChatItem =
  | {
      readonly id: string;
      readonly kind: "user";
      readonly turn: number;
      readonly content: string;
      readonly attachments: readonly Attachment[];
      /** Server-dispatched attach turn for background agent activity. */
      readonly background?: boolean;
    }
  | {
      readonly id: string;
      readonly kind: "steered";
      readonly turn: number;
      readonly content: string;
      readonly attachments: readonly Attachment[];
    }
  | {
      readonly id: string;
      readonly kind: "subagent";
      readonly turn: number;
      readonly threadId: string;
      readonly sessionId: string;
      readonly prompt: string;
      readonly model: string;
      readonly callId?: string;
    }
  | {
      readonly id: string;
      readonly kind: "assistant";
      readonly turn: number;
      content: string;
      complete: boolean;
    }
  | {
      readonly id: string;
      readonly kind: "progress";
      readonly turn: number;
      content: string;
      complete: boolean;
    }
  | {
      readonly id: string;
      readonly kind: "thinking";
      readonly turn: number;
      content: string;
      complete: boolean;
    }
  | {
      readonly id: string;
      readonly kind: "compaction";
      readonly turn: number;
      state: CompactionState;
    }
  | {
      readonly id: string;
      readonly kind: "todo";
      readonly turn: number;
      readonly todoId: string;
      readonly content: string;
      readonly state: TodoLifecycleState;
    }
  | {
      readonly id: string;
      readonly kind: "tool";
      readonly callId: string;
      readonly tool: string;
      args: unknown;
      detailsDeferred?: boolean;
      status: ToolCallStatus;
      result: unknown | undefined;
      output: ToolOutputBuffer;
      startedAt?: string;
      durationMs?: number;
    }
  | {
      readonly id: string;
      readonly kind: "turn-status";
      readonly turn: number;
      state: TurnState;
    }
  | {
      readonly id: string;
      readonly kind: "questions";
      readonly requestId: string;
      readonly title: string | undefined;
      readonly questions: readonly Question[];
      /** undefined = pending, null = skipped, array = answered. */
      answers: readonly QuestionAnswer[] | null | undefined;
    };

const terminalToolStatus = (status: ToolCallStatus): boolean =>
  status === "ok" ||
  status === "error" ||
  status === "denied" ||
  status === "aborted";

interface TodoTransition {
  readonly todo: TodoItem;
  readonly state: TodoLifecycleState;
}

const todoTransitions = (
  previous: readonly TodoItem[],
  current: readonly TodoItem[],
): readonly TodoTransition[] => {
  const previousById = new Map(previous.map((todo) => [todo.id, todo]));
  const currentIds = new Set(current.map((todo) => todo.id));
  const transitions: TodoTransition[] = [];
  for (const todo of current) {
    const previousStatus = previousById.get(todo.id)?.status;
    if (todo.status === "in_progress" && previousStatus !== "in_progress") {
      transitions.push({ todo, state: "started" });
    } else if (todo.status === "completed" && previousStatus !== "completed") {
      transitions.push({ todo, state: "completed" });
    } else if (todo.status === "cancelled" && previousStatus !== "cancelled") {
      transitions.push({ todo, state: "cancelled" });
    }
  }
  for (const todo of previous) {
    if (
      !currentIds.has(todo.id)
      && todo.status !== "completed"
      && todo.status !== "cancelled"
    ) {
      transitions.push({ todo, state: "skipped" });
    }
  }
  return transitions;
};

const replaceNumericMap = <T>(
  target: Map<number, T>,
  source: Readonly<Record<string, T>> | undefined,
): void => {
  target.clear();
  for (const [rawKey, value] of Object.entries(source ?? {})) {
    const key = Number(rawKey);
    if (Number.isSafeInteger(key) && key >= 0) target.set(key, value);
  }
};

const latestNumericMapKey = (map: ReadonlyMap<number, unknown>): number | undefined => {
  let latest: number | undefined;
  for (const key of map.keys()) {
    if (latest === undefined || key > latest) latest = key;
  }
  return latest;
};

/** Replay-equivalent projection of one thread's durable event stream.
 * This mirrors trouve-client-core's ThreadViewModel without sharing Rust
 * process state across the protocol boundary. */
export class ThreadViewModel {
  readonly items: ThreadChatItem[] = [];
  readonly pendingApprovals: string[] = [];
  readonly pendingQuestions: string[] = [];
  readonly turnModels = new Map<number, string>();
  readonly turnThinkingLevels = new Map<number, string>();
  readonly turnSteerable = new Map<number, boolean>();
  readonly turnStartedAt = new Map<number, string>();
  readonly turnDurationMs = new Map<number, number>();
  readonly #admittedBeforeStart = new Set<number>();
  #queueRevision = 0;
  #completedUsage: Usage | undefined;
  #activeTurnUsage: { readonly turn: number; readonly usage: Usage } | undefined;

  cursor = 0;
  /** Absolute folded-item position of `items[0]`. */
  itemOffset = 0;
  /** Complete folded-item count at the latest snapshot/live event. */
  totalItems = 0;
  hasOlder = false;
  snapshotLoaded = false;
  lastUsage: Usage | undefined;
  lastUsageCursor = 0;
  compacting = false;
  turnRunning = false;
  turnPhase: TurnPhase | undefined;
  thinking = false;
  commands: readonly CommandInfo[] = [];
  queue: readonly QueuedPrompt[] = [];
  todos: readonly TodoItem[] = [];

  static fromSnapshot(
    cursor: number,
    snapshot: ProtocolThreadViewSnapshot,
  ): ThreadViewModel {
    const view = new ThreadViewModel();
    view.replaceSnapshot(cursor, snapshot);
    return view;
  }

  /** Replace replay-built state with the server's current folded tail. */
  replaceSnapshot(cursor: number, snapshot: ProtocolThreadViewSnapshot): void {
    this.#admittedBeforeStart.clear();
    const itemOffset = snapshot.item_offset ?? 0;
    this.items.splice(
      0,
      this.items.length,
      ...snapshot.items.map((item, index) =>
        this.#snapshotItem(item, itemOffset + index)),
    );
    this.pendingApprovals.splice(
      0,
      this.pendingApprovals.length,
      ...(snapshot.pending_approvals ?? []),
    );
    this.pendingQuestions.splice(
      0,
      this.pendingQuestions.length,
      ...(snapshot.pending_questions ?? []),
    );
    replaceNumericMap(this.turnModels, snapshot.turn_models);
    replaceNumericMap(this.turnThinkingLevels, snapshot.turn_thinking_levels);
    replaceNumericMap(this.turnSteerable, snapshot.turn_steerable);
    replaceNumericMap(this.turnStartedAt, snapshot.turn_started_at);
    replaceNumericMap(this.turnDurationMs, snapshot.turn_duration_ms);
    this.cursor = cursor;
    this.itemOffset = itemOffset;
    this.totalItems = Math.max(
      snapshot.total_items ?? 0,
      itemOffset + snapshot.items.length,
    );
    this.hasOlder = snapshot.has_older ?? itemOffset > 0;
    this.snapshotLoaded = true;
    const activeUsage = snapshot.active_usage ?? undefined;
    this.#completedUsage = snapshot.last_usage ?? undefined;
    this.#activeTurnUsage = undefined;
    this.lastUsage = activeUsage ?? this.#completedUsage;
    this.lastUsageCursor = this.lastUsage === undefined ? 0 : cursor;
    this.compacting = snapshot.compacting ?? false;
    this.turnRunning = snapshot.turn_running ?? false;
    this.turnPhase = snapshot.turn_phase ?? undefined;
    if (this.turnRunning) {
      const activeTurn = this.#findLast(
        (item) =>
          item.kind === "turn-status"
          && (
            item.state.kind === "waiting-for-capacity"
            || item.state.kind === "running"
          ),
      );
      const activeTurnNumber = activeTurn?.kind === "turn-status"
        ? activeTurn.turn
        : latestNumericMapKey(this.turnStartedAt);
      this.#activeTurnUsage = activeUsage === undefined || activeTurnNumber === undefined
        ? undefined
        : { turn: activeTurnNumber, usage: activeUsage };
      if (activeTurn?.kind === "turn-status") {
        const startedAt = this.turnStartedAt.get(activeTurn.turn);
        activeTurn.state = activeTurn.state.kind === "running"
          ? {
              kind: "running",
              ...(startedAt === undefined ? {} : { startedAt }),
              ...(activeUsage === undefined ? {} : { usage: activeUsage }),
            }
          : {
              kind: "waiting-for-capacity",
              ...(startedAt === undefined ? {} : { startedAt }),
            };
      }
    }
    this.thinking = snapshot.thinking ?? false;
    this.commands = [...(snapshot.commands ?? [])];
    this.replaceQueue(snapshot.queue ?? []);
    // Protocol 3.1 snapshots predate the todo projection. Preserve any live
    // todo events already folded when an older-compatible snapshot omits it.
    if (snapshot.todos !== undefined) this.replaceTodos(snapshot.todos);
  }

  /** Merge a fresh folded tail into an already-prefetched history window.
   * Absolute snapshot ids make the retained prefix stable across reconnects. */
  mergeTailSnapshot(cursor: number, snapshot: ProtocolThreadViewSnapshot): void {
    if (!this.snapshotLoaded) {
      this.replaceSnapshot(cursor, snapshot);
      return;
    }
    const previousOffset = this.itemOffset;
    const previousEnd = previousOffset + this.items.length;
    const nextOffset = snapshot.item_offset ?? 0;
    const retained = nextOffset >= previousOffset && nextOffset <= previousEnd
      ? this.items.slice(0, nextOffset - previousOffset)
      : [];
    const previousTotal = this.totalItems;
    this.replaceSnapshot(cursor, snapshot);
    if (retained.length > 0) {
      this.items.splice(0, 0, ...retained);
      this.itemOffset = previousOffset;
      this.hasOlder = previousOffset > 0;
    }
    this.totalItems = Math.max(previousTotal, this.totalItems);
  }

  /** Prepend one contiguous folded history page without replacing live state. */
  prependSnapshot(snapshot: ProtocolThreadViewSnapshot): boolean {
    const itemOffset = snapshot.item_offset ?? 0;
    const pageEnd = itemOffset + snapshot.items.length;
    if (pageEnd !== this.itemOffset) return false;
    if (snapshot.items.length === 0) {
      this.hasOlder = false;
      return true;
    }
    const olderItems = snapshot.items.map((item, index) =>
      this.#snapshotItem(item, itemOffset + index));
    this.items.splice(0, 0, ...olderItems);
    this.itemOffset = itemOffset;
    this.totalItems = Math.max(
      this.totalItems,
      snapshot.total_items ?? 0,
      pageEnd,
    );
    this.hasOlder = snapshot.has_older ?? itemOffset > 0;
    return true;
  }

  #snapshotItem(item: ThreadViewItem, absoluteIndex: number): ThreadChatItem {
    const id = `snapshot:${absoluteIndex}`;
    switch (item.kind) {
      case "user":
        return {
          id,
          kind: "user",
          turn: item.turn,
          content: item.content,
          attachments: item.attachments,
          background: item.background ?? false,
        };
      case "steered":
        return {
          id,
          kind: "steered",
          turn: item.turn,
          content: item.content,
          attachments: item.attachments,
        };
      case "subagent":
        return {
          id,
          kind: "subagent",
          turn: item.turn,
          threadId: item.thread_id,
          sessionId: item.session_id,
          prompt: item.prompt,
          model: item.model,
          ...(item.call_id == null ? {} : { callId: item.call_id }),
        };
      case "assistant":
        return {
          id,
          kind: "assistant",
          turn: item.turn,
          content: item.content,
          complete: item.complete,
        };
      case "progress":
        return {
          id,
          kind: "progress",
          turn: item.turn,
          content: item.content,
          complete: item.complete,
        };
      case "thinking":
        return {
          id,
          kind: "thinking",
          turn: item.turn,
          content: item.content,
          complete: item.complete,
        };
      case "compaction": {
        let state: CompactionState;
        switch (item.state.state) {
          case "running":
            state = { kind: "running" };
            break;
          case "completed":
            state = {
              kind: "completed",
              messagesCompacted: item.state.messages_compacted,
            };
            break;
          case "failed":
            state = { kind: "failed" };
            break;
        }
        return { id, kind: "compaction", turn: item.turn, state };
      }
      case "todo_update":
        return {
          id,
          kind: "todo",
          turn: item.turn,
          todoId: item.todo_id,
          content: item.content,
          state: item.state,
        };
      case "tool_call":
        return {
          id,
          kind: "tool",
          callId: item.call_id,
          tool: item.tool,
          args: item.args,
          detailsDeferred: item.details_deferred ?? false,
          status: item.status === "awaiting_approval"
            ? "awaiting-approval"
            : item.status,
          result: item.result,
          output: emptyToolOutput(),
          ...(item.duration_ms == null ? {} : { durationMs: item.duration_ms }),
        };
      case "turn_status": {
        let state: TurnState;
        switch (item.state.state) {
          case "waiting_for_capacity":
            state = { kind: "waiting-for-capacity" };
            break;
          case "running":
            state = { kind: "running" };
            break;
          case "completed":
            state = {
              kind: "completed",
              usage: item.state.usage,
              ...(item.state.checkpoint_id == null
                ? {}
                : { checkpointId: item.state.checkpoint_id }),
            };
            break;
          case "failed":
            state = { kind: "failed", error: item.state.error };
            break;
        }
        return { id, kind: "turn-status", turn: item.turn, state };
      }
      case "questions":
        return {
          id,
          kind: "questions",
          requestId: item.request_id,
          title: item.title ?? undefined,
          questions: item.questions,
          answers: item.resolved === true ? item.answers ?? null : undefined,
        };
    }
  }

  replaceQueue(prompts: readonly QueuedPrompt[]): void {
    this.#queueRevision += 1;
    this.queue = [...prompts];
  }

  trackQueueRevision(): QueueRevisionTracker {
    const revision = this.#queueRevision;
    let closed = false;
    return {
      queueChanged: () => !closed && this.#queueRevision !== revision,
      close: () => {
        closed = true;
      },
    };
  }

  replaceTodos(todos: readonly TodoItem[]): void {
    this.todos = todos.map((todo) => ({ ...todo }));
  }

  apply(envelope: ProtocolEventEnvelope): boolean {
    this.cursor = envelope.cursor;
    switch (envelope.type) {
      case "turn.admitted":
      case "turn.capacity_acquired": {
        const waitingTurn = this.#findLast(
          (item) =>
            item.kind === "turn-status"
            && item.turn === envelope.turn
            && item.state.kind === "waiting-for-capacity",
        );
        if (waitingTurn?.kind === "turn-status") {
          const startedAt = this.turnStartedAt.get(envelope.turn);
          waitingTurn.state = {
            kind: "running",
            ...(startedAt === undefined ? {} : { startedAt }),
          };
          return true;
        }
        this.#admittedBeforeStart.add(envelope.turn);
        return false;
      }
      case "turn.started": {
        this.turnRunning = true;
        this.#activeTurnUsage = undefined;
        this.turnPhase = "processing";
        this.turnModels.set(envelope.turn, envelope.model);
        if (envelope.thinking_level == null) {
          this.turnThinkingLevels.delete(envelope.turn);
        } else {
          this.turnThinkingLevels.set(envelope.turn, envelope.thinking_level);
        }
        this.turnSteerable.set(envelope.turn, envelope.supports_steering ?? false);
        this.turnStartedAt.set(envelope.turn, envelope.ts);
        const admitted = this.#admittedBeforeStart.delete(envelope.turn);
        this.appendItem({
          id: `turn:${envelope.turn}`,
          kind: "turn-status",
          turn: envelope.turn,
          state: admitted
            ? { kind: "running", startedAt: envelope.ts }
            : { kind: "waiting-for-capacity", startedAt: envelope.ts },
        });
        return true;
      }
      case "turn.phase_changed":
        this.turnPhase = envelope.phase;
        return true;
      case "thread.compaction_started":
        this.compacting = true;
        this.appendItem({
          id: `compaction:${envelope.turn}:${envelope.cursor}`,
          kind: "compaction",
          turn: envelope.turn,
          state: { kind: "running" },
        });
        return true;
      case "thread.commands_updated":
        this.commands = envelope.commands;
        return true;
      case "thread.queue_updated":
        this.replaceQueue(envelope.prompts);
        return true;
      case "thread.todos_updated": {
        const turn = this.activeTurn();
        const transitions = todoTransitions(this.todos, envelope.todos);
        this.replaceTodos(envelope.todos);
        if (turn !== undefined) {
          for (const [index, transition] of transitions.entries()) {
            this.appendItem({
              id: `todo:${turn}:${transition.todo.id}:${transition.state}:${envelope.cursor}:${index}`,
              kind: "todo",
              turn,
              todoId: transition.todo.id,
              content: transition.todo.content,
              state: transition.state,
            });
          }
        }
        return true;
      }
      case "thread.compaction_completed": {
        this.compacting = false;
        const compaction = this.findRunningCompaction(envelope.turn);
        if (compaction !== undefined) {
          compaction.state = {
            kind: "completed",
            messagesCompacted: envelope.messages_compacted,
          };
        } else {
          // Handles a completion arriving after a snapshot produced by an
          // older protocol that only carried the transient busy flag.
          this.appendItem({
            id: `compaction:${envelope.turn}:${envelope.cursor}`,
            kind: "compaction",
            turn: envelope.turn,
            state: {
              kind: "completed",
              messagesCompacted: envelope.messages_compacted,
            },
          });
        }
        return true;
      }
      case "thread.compaction_failed": {
        this.compacting = false;
        const compaction = this.findRunningCompaction(envelope.turn);
        if (compaction !== undefined) {
          compaction.state = { kind: "failed" };
        } else {
          this.appendItem({
            id: `compaction:${envelope.turn}:${envelope.cursor}`,
            kind: "compaction",
            turn: envelope.turn,
            state: { kind: "failed" },
          });
        }
        return true;
      }
      case "user.message":
        this.appendItem({
          id: `user:${envelope.turn}`,
          kind: "user",
          turn: envelope.turn,
          content: envelope.content,
          attachments: envelope.attachments ?? [],
          background: envelope.background ?? false,
        });
        return true;
      case "turn.background_activity":
        this.appendItem({
          id: `background:${envelope.turn}`,
          kind: "user",
          turn: envelope.turn,
          content: "",
          attachments: [],
          background: true,
        });
        return true;
      case "turn.steered":
        this.finishProgress();
        this.finishThinking();
        this.appendItem({
          id: `steered:${envelope.turn}:${envelope.cursor}`,
          kind: "steered",
          turn: envelope.turn,
          content: envelope.content,
          attachments: envelope.attachments ?? [],
        });
        return true;
      case "subagent.spawned":
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        this.appendItem({
          id: `subagent:${envelope.thread_id}:${envelope.cursor}`,
          kind: "subagent",
          turn: envelope.turn,
          threadId: envelope.thread_id,
          sessionId: envelope.session_id,
          prompt: envelope.prompt,
          model: envelope.model,
          ...(envelope.call_id == null ? {} : { callId: envelope.call_id }),
        });
        return true;
      case "assistant.progress": {
        this.failOpenCompaction(envelope.turn);
        this.finishThinking();
        const current = this.findTrailingOpen("progress", envelope.turn);
        if (current?.kind === "progress") current.content += envelope.text;
        else {
          this.appendItem({
            id: this.nextItemId(`progress:${envelope.turn}`),
            kind: "progress",
            turn: envelope.turn,
            content: envelope.text,
            complete: false,
          });
        }
        return true;
      }
      case "assistant.progress_completed":
        return this.finishProgress();
      case "assistant.thinking": {
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.thinking = true;
        const current = this.findTrailingOpen("thinking", envelope.turn);
        if (current?.kind === "thinking") current.content += envelope.text;
        else {
          this.appendItem({
            id: this.nextItemId(`thinking:${envelope.turn}`),
            kind: "thinking",
            turn: envelope.turn,
            content: envelope.text,
            complete: false,
          });
        }
        return true;
      }
      case "assistant.thinking_completed":
        return this.finishThinking();
      case "assistant.delta": {
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        const current = this.findTrailingOpen("assistant", envelope.turn);
        if (current?.kind === "assistant") current.content += envelope.text;
        else {
          this.appendItem({
            id: this.nextItemId(`assistant:${envelope.turn}`),
            kind: "assistant",
            turn: envelope.turn,
            content: envelope.text,
            complete: false,
          });
        }
        return true;
      }
      case "assistant.message": {
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        const current = this.findTrailingOpen("assistant", envelope.turn);
        if (current?.kind === "assistant") {
          current.content = envelope.content;
          current.complete = true;
        } else {
          this.appendItem({
            id: this.nextItemId(`assistant:${envelope.turn}`),
            kind: "assistant",
            turn: envelope.turn,
            content: envelope.content,
            complete: true,
          });
        }
        return true;
      }
      case "tool.requested":
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        this.appendItem({
          id: `tool:${envelope.call_id}`,
          kind: "tool",
          callId: envelope.call_id,
          tool: envelope.tool,
          args: envelope.args,
          detailsDeferred: false,
          status: envelope.requires_approval ? "awaiting-approval" : "running",
          result: undefined,
          output: emptyToolOutput(),
          ...(envelope.requires_approval ? {} : { startedAt: envelope.ts }),
        });
        return true;
      case "approval.requested": {
        if (!this.pendingApprovals.includes(envelope.call_id)) {
          this.pendingApprovals.push(envelope.call_id);
        }
        const tool = this.findTool(envelope.call_id);
        if (tool !== undefined) tool.status = "awaiting-approval";
        return tool !== undefined;
      }
      case "approval.resolved": {
        this.removePending(this.pendingApprovals, envelope.call_id);
        const tool = this.findTool(envelope.call_id);
        if (tool !== undefined) {
          tool.status = envelope.decision === "deny" ? "denied" : "running";
          if (envelope.decision !== "deny" && tool.startedAt === undefined) {
            tool.startedAt = envelope.ts;
          }
        }
        return tool !== undefined;
      }
      case "tool.started": {
        const tool = this.findTool(envelope.call_id);
        if (
          tool !== undefined &&
          !terminalToolStatus(tool.status) &&
          tool.status !== "awaiting-approval"
        ) {
          tool.startedAt = envelope.ts;
          tool.status = "running";
        }
        return tool !== undefined;
      }
      case "tool.output": {
        const tool = this.findTool(envelope.call_id);
        if (
          tool === undefined ||
          terminalToolStatus(tool.status) ||
          envelope.chunk === ""
        ) return false;
        tool.output = appendBoundedToolOutput(tool.output, envelope.chunk);
        return true;
      }
      case "tool.completed": {
        const tool = this.findTool(envelope.call_id);
        if (tool !== undefined) {
          if (tool.status !== "denied") tool.status = envelope.status;
          tool.result = envelope.result;
          if (envelope.execution_duration_ms != null) {
            tool.durationMs = envelope.execution_duration_ms;
          } else {
            const startedAt = tool.startedAt === undefined
              ? Number.NaN
              : Date.parse(tool.startedAt);
            const completedAt = Date.parse(envelope.ts);
            if (Number.isFinite(startedAt) && Number.isFinite(completedAt)) {
              tool.durationMs = Math.max(0, completedAt - startedAt);
            }
          }
        }
        this.removePending(this.pendingApprovals, envelope.call_id);
        return tool !== undefined;
      }
      case "question.requested":
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        if (!this.pendingQuestions.includes(envelope.request_id)) {
          this.pendingQuestions.push(envelope.request_id);
        }
        this.appendItem({
          id: `questions:${envelope.request_id}`,
          kind: "questions",
          requestId: envelope.request_id,
          ...(envelope.title == null ? { title: undefined } : { title: envelope.title }),
          questions: envelope.questions,
          answers: undefined,
        });
        return true;
      case "question.resolved": {
        this.removePending(this.pendingQuestions, envelope.request_id);
        const questions = this.findQuestions(envelope.request_id);
        if (questions !== undefined) questions.answers = envelope.answers ?? null;
        return questions !== undefined;
      }
      case "turn.usage_updated": {
        const runningTurn = this.#findLast(
          (item) =>
            item.kind === "turn-status" &&
            item.turn === envelope.turn &&
            item.state.kind === "running",
        );
        const usage = accumulateLiveUsage(
          this.#activeTurnUsage?.turn === envelope.turn
            ? this.#activeTurnUsage.usage
            : undefined,
          envelope.usage,
        );
        this.#activeTurnUsage = { turn: envelope.turn, usage };
        this.lastUsage = usage;
        this.lastUsageCursor = envelope.cursor;
        if (runningTurn?.kind === "turn-status" && runningTurn.state.kind === "running") {
          runningTurn.state = { ...runningTurn.state, usage };
        }
        return true;
      }
      case "turn.completed": {
        this.#admittedBeforeStart.delete(envelope.turn);
        this.turnRunning = false;
        this.turnPhase = undefined;
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        const toolsChanged = this.abortOpenTools(envelope.ts);
        this.pendingQuestions.length = 0;
        const usage = usageWithLiveContext(
          envelope.usage,
          this.#activeTurnUsage?.turn === envelope.turn
            ? this.#activeTurnUsage.usage
            : undefined,
        );
        this.#activeTurnUsage = undefined;
        this.#completedUsage = usage;
        this.lastUsage = usage;
        this.lastUsageCursor = envelope.cursor;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        return this.replaceActiveTurn(envelope.turn, {
          kind: "completed",
          usage,
          ...(envelope.checkpoint_id == null
            ? {}
            : { checkpointId: envelope.checkpoint_id }),
        }) || toolsChanged;
      }
      case "turn.failed": {
        this.#admittedBeforeStart.delete(envelope.turn);
        this.turnRunning = false;
        this.turnPhase = undefined;
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        const toolsChanged = this.abortOpenTools(envelope.ts);
        this.pendingQuestions.length = 0;
        this.#activeTurnUsage = undefined;
        this.lastUsage = this.#completedUsage;
        this.lastUsageCursor = this.lastUsage === undefined ? 0 : envelope.cursor;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        return this.replaceActiveTurn(envelope.turn, {
          kind: "failed",
          error: envelope.error,
        }) || toolsChanged;
      }
      case "turn.cancelled": {
        this.#admittedBeforeStart.delete(envelope.turn);
        this.turnRunning = false;
        this.turnPhase = undefined;
        this.failOpenCompaction(envelope.turn);
        this.finishProgress();
        this.finishThinking();
        const toolsChanged = this.abortOpenTools(envelope.ts);
        this.pendingQuestions.length = 0;
        this.#activeTurnUsage = undefined;
        this.lastUsage = this.#completedUsage;
        this.lastUsageCursor = this.lastUsage === undefined ? 0 : envelope.cursor;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        const index = this.items.findIndex((item) =>
          item.kind === "turn-status" && item.turn === envelope.turn);
        if (index < 0) return toolsChanged;
        this.items.splice(index, 1);
        this.totalItems = Math.max(this.itemOffset + this.items.length, this.totalItems - 1);
        return true;
      }
      default:
        return false;
    }
  }

  private appendItem(item: ThreadChatItem): void {
    this.items.push(item);
    this.totalItems += 1;
  }

  private activeTurn(): number | undefined {
    const active = this.#findLast(
      (item) =>
        item.kind === "turn-status"
        && (
          item.state.kind === "waiting-for-capacity"
          || item.state.kind === "running"
        ),
    );
    return active?.kind === "turn-status" ? active.turn : undefined;
  }

  #findLast(predicate: (item: ThreadChatItem) => boolean): ThreadChatItem | undefined {
    for (let index = this.items.length - 1; index >= 0; index -= 1) {
      const item = this.items[index];
      if (item !== undefined && predicate(item)) return item;
    }
    return undefined;
  }

  findTool(callId: string): Extract<ThreadChatItem, { kind: "tool" }> | undefined {
    const item = this.#findLast(
      (candidate) => candidate.kind === "tool" && candidate.callId === callId,
    );
    return item?.kind === "tool" ? item : undefined;
  }

  replaceToolDetails(details: ProtocolThreadToolDetails): boolean {
    const tool = this.findTool(details.call_id);
    if (tool === undefined) return false;
    tool.args = details.args;
    tool.result = details.result;
    tool.detailsDeferred = false;
    return true;
  }

  /** Bound inactive transcript caches without changing absolute item ids. */
  trimHistory(maxItems: number): void {
    const retained = Math.max(1, Math.floor(maxItems));
    if (this.items.length <= retained) return;
    const removed = this.items.length - retained;
    this.items.splice(0, removed);
    this.itemOffset += removed;
    this.hasOlder = this.itemOffset > 0;
  }

  findQuestions(
    requestId: string,
  ): Extract<ThreadChatItem, { kind: "questions" }> | undefined {
    const item = this.#findLast(
      (candidate) =>
        candidate.kind === "questions" && candidate.requestId === requestId,
    );
    return item?.kind === "questions" ? item : undefined;
  }

  private findRunningCompaction(
    turn: number,
  ): Extract<ThreadChatItem, { kind: "compaction" }> | undefined {
    const item = this.#findLast(
      (candidate) =>
        candidate.kind === "compaction"
        && candidate.turn === turn
        && candidate.state.kind === "running",
    );
    return item?.kind === "compaction" ? item : undefined;
  }

  private failOpenCompaction(turn: number): boolean {
    this.compacting = false;
    const compaction = this.findRunningCompaction(turn);
    if (compaction === undefined) return false;
    compaction.state = { kind: "failed" };
    return true;
  }

  private findTrailingOpen(
    kind: "assistant" | "progress" | "thinking",
    turn: number,
  ): ThreadChatItem | undefined {
    return this.#findLast(
      (item) => item.kind === kind && item.turn === turn && !item.complete,
    );
  }

  private finishThinking(): boolean {
    const wasThinking = this.thinking;
    this.thinking = false;
    const item = this.#findLast(
      (candidate) => candidate.kind === "thinking" && !candidate.complete,
    );
    if (item?.kind !== "thinking") return wasThinking;
    item.complete = true;
    return true;
  }

  private finishProgress(): boolean {
    const item = this.#findLast(
      (candidate) => candidate.kind === "progress" && !candidate.complete,
    );
    if (item?.kind !== "progress") return false;
    item.complete = true;
    return true;
  }

  /**
   * A thread has at most one active turn, so every non-terminal tool row
   * belongs to the turn that is ending. Provider control-plane calls can be
   * interrupted without a matching tool.completed event; preserve the row,
   * but never leave a terminal transcript looking active.
   */
  private abortOpenTools(endedAt: string): boolean {
    const ended = Date.parse(endedAt);
    let changed = false;
    for (const item of this.items) {
      if (item.kind !== "tool" || terminalToolStatus(item.status)) continue;
      item.status = "aborted";
      if (item.durationMs === undefined && item.startedAt !== undefined) {
        const started = Date.parse(item.startedAt);
        if (Number.isFinite(ended) && Number.isFinite(started)) {
          item.durationMs = Math.max(0, ended - started);
        }
      }
      changed = true;
    }
    this.pendingApprovals.length = 0;
    return changed;
  }

  private recordTurnDuration(turn: number, endedAt: string): void {
    const startedAt = this.turnStartedAt.get(turn);
    if (startedAt === undefined) return;
    const duration = Date.parse(endedAt) - Date.parse(startedAt);
    if (Number.isFinite(duration)) this.turnDurationMs.set(turn, Math.max(0, duration));
  }

  private replaceActiveTurn(turn: number, state: TurnState): boolean {
    const item = this.#findLast(
      (candidate) =>
        candidate.kind === "turn-status" &&
        candidate.turn === turn &&
        (
          candidate.state.kind === "waiting-for-capacity"
          || candidate.state.kind === "running"
        ),
    );
    if (item?.kind !== "turn-status") return false;
    item.state = state;
    return true;
  }

  private removePending(values: string[], value: string): void {
    let index = values.indexOf(value);
    while (index >= 0) {
      values.splice(index, 1);
      index = values.indexOf(value);
    }
  }

  private nextItemId(prefix: string): string {
    let ordinal = 1;
    const ids = new Set(this.items.map((item) => item.id));
    while (ids.has(`${prefix}:${ordinal}`)) ordinal += 1;
    return `${prefix}:${ordinal}`;
  }
}
