import type { components as ProtocolComponents } from "../generated/protocol.js";
import type { ProtocolEventEnvelope } from "../services/protocol-client.js";
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

export type ToolCallStatus =
  | "awaiting-approval"
  | "running"
  | "ok"
  | "error"
  | "denied"
  | "aborted";

export type TurnState =
  | { readonly kind: "running" }
  | { readonly kind: "completed"; readonly usage: Usage }
  | { readonly kind: "failed"; readonly error: string }
  | { readonly kind: "cancelled" };

export type ThreadChatItem =
  | {
      readonly id: string;
      readonly kind: "user";
      readonly turn: number;
      readonly content: string;
      readonly attachments: readonly Attachment[];
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
      readonly kind: "thinking";
      readonly turn: number;
      content: string;
      complete: boolean;
    }
  | {
      readonly id: string;
      readonly kind: "tool";
      readonly callId: string;
      readonly tool: string;
      readonly args: unknown;
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

/** Replay-equivalent projection of one thread's durable event stream.
 * This mirrors trouve-client-core's ThreadViewModel without sharing Rust
 * process state across the protocol boundary. */
export class ThreadViewModel {
  readonly items: ThreadChatItem[] = [];
  readonly pendingApprovals: string[] = [];
  readonly pendingQuestions: string[] = [];
  readonly turnModels = new Map<number, string>();
  readonly turnStartedAt = new Map<number, string>();
  readonly turnDurationMs = new Map<number, number>();

  cursor = 0;
  lastUsage: Usage | undefined;
  lastUsageCursor = 0;
  compacting = false;
  turnRunning = false;
  thinking = false;
  commands: readonly CommandInfo[] = [];
  queue: readonly QueuedPrompt[] = [];
  todos: readonly TodoItem[] = [];

  replaceQueue(prompts: readonly QueuedPrompt[]): void {
    this.queue = prompts;
  }

  replaceTodos(todos: readonly TodoItem[]): void {
    this.todos = todos.map((todo) => ({ ...todo }));
  }

  apply(envelope: ProtocolEventEnvelope): boolean {
    this.cursor = envelope.cursor;
    switch (envelope.type) {
      case "turn.started":
        this.turnRunning = true;
        this.turnModels.set(envelope.turn, envelope.model);
        this.turnStartedAt.set(envelope.turn, envelope.ts);
        this.items.push({
          id: `turn:${envelope.turn}`,
          kind: "turn-status",
          turn: envelope.turn,
          state: { kind: "running" },
        });
        return true;
      case "thread.compaction_started":
        this.compacting = true;
        return true;
      case "thread.commands_updated":
        this.commands = envelope.commands;
        return true;
      case "thread.queue_updated":
        this.queue = envelope.prompts;
        return true;
      case "thread.todos_updated":
        this.replaceTodos(envelope.todos);
        return true;
      case "thread.compaction_completed":
        this.compacting = false;
        return true;
      case "user.message":
        this.items.push({
          id: `user:${envelope.turn}`,
          kind: "user",
          turn: envelope.turn,
          content: envelope.content,
          attachments: envelope.attachments ?? [],
        });
        return true;
      case "assistant.thinking": {
        this.thinking = true;
        const current = this.findTrailingOpen("thinking", envelope.turn);
        if (current?.kind === "thinking") current.content += envelope.text;
        else {
          this.items.push({
            id: this.nextItemId(`thinking:${envelope.turn}`),
            kind: "thinking",
            turn: envelope.turn,
            content: envelope.text,
            complete: false,
          });
        }
        return true;
      }
      case "assistant.delta": {
        this.finishThinking();
        const current = this.findTrailingOpen("assistant", envelope.turn);
        if (current?.kind === "assistant") current.content += envelope.text;
        else {
          this.items.push({
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
        this.finishThinking();
        const current = this.findTrailingOpen("assistant", envelope.turn);
        if (current?.kind === "assistant") {
          current.content = envelope.content;
          current.complete = true;
        } else {
          this.items.push({
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
        this.finishThinking();
        this.items.push({
          id: `tool:${envelope.call_id}`,
          kind: "tool",
          callId: envelope.call_id,
          tool: envelope.tool,
          args: envelope.args,
          status: envelope.requires_approval ? "awaiting-approval" : "running",
          result: undefined,
          output: emptyToolOutput(),
          startedAt: envelope.ts,
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
          const startedAt = tool.startedAt === undefined
            ? Number.NaN
            : Date.parse(tool.startedAt);
          const completedAt = Date.parse(envelope.ts);
          if (Number.isFinite(startedAt) && Number.isFinite(completedAt)) {
            tool.durationMs = Math.max(0, completedAt - startedAt);
          }
        }
        this.removePending(this.pendingApprovals, envelope.call_id);
        return tool !== undefined;
      }
      case "question.requested":
        this.finishThinking();
        if (!this.pendingQuestions.includes(envelope.request_id)) {
          this.pendingQuestions.push(envelope.request_id);
        }
        this.items.push({
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
      case "turn.completed":
        this.turnRunning = false;
        this.compacting = false;
        this.finishThinking();
        this.pendingQuestions.length = 0;
        this.lastUsage = envelope.usage;
        this.lastUsageCursor = envelope.cursor;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        return this.replaceRunningTurn(envelope.turn, {
          kind: "completed",
          usage: envelope.usage,
        });
      case "turn.failed":
        this.turnRunning = false;
        this.compacting = false;
        this.finishThinking();
        this.pendingQuestions.length = 0;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        return this.replaceRunningTurn(envelope.turn, {
          kind: "failed",
          error: envelope.error,
        });
      case "turn.cancelled": {
        this.turnRunning = false;
        this.compacting = false;
        this.finishThinking();
        this.pendingQuestions.length = 0;
        this.recordTurnDuration(envelope.turn, envelope.ts);
        return this.replaceRunningTurn(envelope.turn, { kind: "cancelled" });
      }
      default:
        return false;
    }
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

  findQuestions(
    requestId: string,
  ): Extract<ThreadChatItem, { kind: "questions" }> | undefined {
    const item = this.#findLast(
      (candidate) =>
        candidate.kind === "questions" && candidate.requestId === requestId,
    );
    return item?.kind === "questions" ? item : undefined;
  }

  private findTrailingOpen(
    kind: "assistant" | "thinking",
    turn: number,
  ): ThreadChatItem | undefined {
    return this.#findLast(
      (item) => item.kind === kind && item.turn === turn && !item.complete,
    );
  }

  private finishThinking(): void {
    this.thinking = false;
    const item = this.#findLast((candidate) => candidate.kind === "thinking");
    if (item?.kind === "thinking") item.complete = true;
  }

  private recordTurnDuration(turn: number, endedAt: string): void {
    const startedAt = this.turnStartedAt.get(turn);
    if (startedAt === undefined) return;
    const duration = Date.parse(endedAt) - Date.parse(startedAt);
    if (Number.isFinite(duration)) this.turnDurationMs.set(turn, Math.max(0, duration));
  }

  private replaceRunningTurn(turn: number, state: TurnState): boolean {
    const item = this.#findLast(
      (candidate) =>
        candidate.kind === "turn-status" &&
        candidate.turn === turn &&
        candidate.state.kind === "running",
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
