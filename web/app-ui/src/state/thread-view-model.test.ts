import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import type {
  ProtocolEventEnvelope,
  ProtocolThreadViewSnapshot,
} from "../services/protocol-client.js";
import { ThreadReplayBatcher } from "../services/thread-ingress.js";
import { ThreadViewModel, type TodoItem } from "./thread-view-model.js";

const envelope = (
  cursor: number,
  event: Record<string, unknown>,
  ts = `2026-08-01T12:00:0${cursor}Z`,
): ProtocolEventEnvelope =>
  ({ cursor, scope: { thread: "th_1" }, ts, ...event }) as ProtocolEventEnvelope;

describe("ThreadViewModel", () => {
  it("distinguishes scheduler waiting from an actively running provider turn", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.started",
      turn: 4,
      mode: "code",
      model: "codex/gpt-5.6-sol",
      thinking_level: "max",
      supports_steering: true,
    }));
    expect(vm.items.at(-1)).toMatchObject({
      kind: "turn-status",
      turn: 4,
      state: { kind: "waiting-for-capacity" },
    });

    vm.apply(envelope(2, {
      type: "turn.capacity_acquired",
      turn: 4,
      wait_ms: 125,
      background: false,
    }));
    expect(vm.items.at(-1)).toMatchObject({
      kind: "turn-status",
      turn: 4,
      state: { kind: "running" },
    });
  });

  it("replays an early capacity event without regressing the visible state", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.capacity_acquired",
      turn: 4,
      wait_ms: 0,
      background: false,
    }));
    vm.apply(envelope(2, {
      type: "turn.started",
      turn: 4,
      mode: "code",
      model: "codex/gpt-5.6-sol",
      thinking_level: "max",
      supports_steering: true,
    }));
    expect(vm.items.at(-1)).toMatchObject({
      kind: "turn-status",
      turn: 4,
      state: { kind: "running" },
    });
  });

  it("matches the shared Rust/web projection fixture", () => {
    const fixture = JSON.parse(
      readFileSync(
        new URL(
          "../../../../crates/trouve-client-core/fixtures/thread-turn.json",
          import.meta.url,
        ),
        "utf8",
      ),
    ) as {
      readonly events: readonly ProtocolEventEnvelope[];
      readonly expected: Record<string, unknown>;
    };
    const vm = new ThreadViewModel();
    for (const event of fixture.events) vm.apply(event);

    const turn = vm.items.find((item) => item.kind === "turn-status");
    const assistant = vm.items.find((item) => item.kind === "assistant");
    const thinking = vm.items.find((item) => item.kind === "thinking");
    const tool = vm.items.find((item) => item.kind === "tool");
    const questions = vm.items.find((item) => item.kind === "questions");
    const questionResolution =
      questions?.kind !== "questions" || questions.answers === undefined
        ? "pending"
        : questions.answers === null
          ? "skipped"
          : "answered";

    expect({
      cursor: vm.cursor,
      turn_running: vm.turnRunning,
      thinking: vm.thinking,
      pending_approvals: vm.pendingApprovals,
      pending_questions: vm.pendingQuestions,
      item_kinds: vm.items.map((item) => item.kind),
      turn_state: turn?.kind === "turn-status" ? turn.state.kind : null,
      assistant_text: assistant?.kind === "assistant" ? assistant.content : null,
      thinking_text: thinking?.kind === "thinking" ? thinking.content : null,
      thinking_complete:
        thinking?.kind === "thinking" ? thinking.complete : null,
      tool_status: tool?.kind === "tool" ? tool.status : null,
      tool_result: tool?.kind === "tool" ? tool.result : null,
      tool_output: tool?.kind === "tool" ? tool.output.text : null,
      tool_output_omitted: tool?.kind === "tool" ? tool.output.omitted : null,
      question_resolution: questionResolution,
      command_names: vm.commands.map((command) => command.name),
      todo_statuses: vm.todos.map((todo) => todo.status),
      turn_duration_ms: vm.turnDurationMs.get(7),
      last_usage:
        vm.lastUsage === undefined
          ? null
          : {
              input_tokens: vm.lastUsage.input_tokens,
              output_tokens: vm.lastUsage.output_tokens,
              cached_input_tokens: vm.lastUsage.cached_input_tokens ?? 0,
              cost_usd: vm.lastUsage.cost_usd ?? null,
              context_window: vm.lastUsage.context_window ?? null,
            },
    }).toEqual(fixture.expected);
  });

  it("keeps steering as a top-level causal boundary between thought output", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.started",
      turn: 3,
      mode: "code",
      model: "codex/gpt-5.6-sol",
      thinking_level: "max",
      supports_steering: true,
    }));
    vm.apply(envelope(2, {
      type: "assistant.thinking",
      turn: 3,
      text: "Following the original direction.",
    }));
    vm.apply(envelope(3, {
      type: "turn.steered",
      turn: 3,
      content: "Prioritize the narrow layout.",
      attachments: [],
    }));
    vm.apply(envelope(4, {
      type: "assistant.thinking",
      turn: 3,
      text: " Continue with the revised direction.",
    }));
    vm.apply(envelope(5, {
      type: "assistant.thinking_completed",
      turn: 3,
    }));

    expect(vm.turnSteerable.get(3)).toBe(true);
    expect(vm.thinking).toBe(false);
    expect(vm.items).toMatchObject([
      { kind: "turn-status", turn: 3 },
      {
        kind: "thinking",
        turn: 3,
        content: "Following the original direction.",
        complete: true,
      },
      {
        kind: "steered",
        turn: 3,
        content: "Prioritize the narrow layout.",
        attachments: [],
      },
      {
        kind: "thinking",
        turn: 3,
        content: " Continue with the revised direction.",
        complete: true,
      },
    ]);
  });

  it("projects a linked subagent as a top-level parent-turn boundary", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "assistant.thinking",
      turn: 3,
      text: "Delegating the focused review.",
    }));
    vm.apply(envelope(2, {
      type: "subagent.spawned",
      turn: 3,
      thread_id: "th_child",
      session_id: "se_child",
      prompt: "Review the native host lifecycle.",
      model: "codex/gpt-5.6-terra",
      call_id: "call_spawn",
    }));

    expect(vm.thinking).toBe(false);
    expect(vm.items).toMatchObject([
      { kind: "thinking", complete: true },
      {
        kind: "subagent",
        turn: 3,
        threadId: "th_child",
        sessionId: "se_child",
        prompt: "Review the native host lifecycle.",
        model: "codex/gpt-5.6-terra",
        callId: "call_spawn",
      },
    ]);
  });

  it("installs a folded snapshot without replaying its historical deltas", () => {
    const snapshot: ProtocolThreadViewSnapshot = {
      item_offset: 40,
      total_items: 43,
      has_older: true,
      items: [
        {
          kind: "turn_status",
          turn: 7,
          state: {
            state: "completed",
            usage: { input_tokens: 20, output_tokens: 5 },
            checkpoint_id: "cp_snapshot_7",
          },
        },
        {
          kind: "assistant",
          turn: 7,
          content: "Final folded answer",
          complete: true,
        },
        {
          kind: "tool_call",
          call_id: "call_snapshot",
          tool: "shell",
          args: { command: "cargo test" },
          status: "awaiting_approval",
        },
      ],
      pending_approvals: ["call_snapshot"],
      last_usage: { input_tokens: 20, output_tokens: 5 },
      turn_models: { "7": "openai/gpt-5.6" },
      turn_thinking_levels: { "7": "high" },
      turn_started_at: { "7": "2026-08-01T12:00:00Z" },
      turn_duration_ms: { "7": 4_000 },
      commands: [{ name: "review", description: "Review changes" }],
      todos: [{ id: "done", content: "Fold history", status: "completed" }],
    };

    const view = ThreadViewModel.fromSnapshot(91, snapshot);

    expect(view).toMatchObject({
      cursor: 91,
      itemOffset: 40,
      totalItems: 43,
      hasOlder: true,
      snapshotLoaded: true,
      lastUsageCursor: 91,
      turnRunning: false,
    });
    expect(view.items).toMatchObject([
      {
        id: "snapshot:40",
        kind: "turn-status",
        state: { kind: "completed", checkpointId: "cp_snapshot_7" },
      },
      { id: "snapshot:41", kind: "assistant", content: "Final folded answer" },
      {
        id: "snapshot:42",
        kind: "tool",
        status: "awaiting-approval",
      },
    ]);
    expect(view.turnModels.get(7)).toBe("openai/gpt-5.6");
    expect(view.turnThinkingLevels.get(7)).toBe("high");
    expect(view.turnDurationMs.get(7)).toBe(4_000);
  });

  it("retains a completed tool duration from a folded snapshot", () => {
    const view = ThreadViewModel.fromSnapshot(9, {
      items: [{
        kind: "tool_call",
        call_id: "completed",
        tool: "shell",
        args: { command: "cargo test" },
        status: "ok",
        duration_ms: 50,
      }],
    });

    expect(view.items[0]).toMatchObject({
      kind: "tool",
      status: "ok",
      durationMs: 50,
    });
  });

  it("hydrates deferred historical tool details without replacing the page", () => {
    const view = ThreadViewModel.fromSnapshot(9, {
      item_offset: 12,
      total_items: 13,
      has_older: true,
      items: [{
        kind: "tool_call",
        call_id: "deferred",
        tool: "read_file",
        args: { path: "README.md" },
        details_deferred: true,
        status: "ok",
      }],
    });

    expect(view.findTool("deferred")).toMatchObject({
      detailsDeferred: true,
      args: { path: "README.md" },
      result: undefined,
    });
    expect(view.replaceToolDetails({
      call_id: "deferred",
      args: { path: "README.md", content: "full arguments" },
      result: { content: "full result" },
    })).toBe(true);
    expect(view.findTool("deferred")).toMatchObject({
      detailsDeferred: false,
      args: { path: "README.md", content: "full arguments" },
      result: { content: "full result" },
    });
    expect(view.itemOffset).toBe(12);
  });

  it("prefers executor timing over durable event latency", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "tool.requested",
      turn: 1,
      call_id: "measured",
      tool: "read_file",
      args: { path: "README.md" },
      requires_approval: false,
    }, "2026-08-01T12:00:00.000Z"));
    view.apply(envelope(2, {
      type: "tool.started",
      call_id: "measured",
    }, "2026-08-01T12:00:00.100Z"));
    view.apply(envelope(3, {
      type: "tool.completed",
      call_id: "measured",
      status: "ok",
      result: {},
      execution_duration_ms: 7,
    }, "2026-08-01T12:00:00.845Z"));

    expect(view.items[0]).toMatchObject({
      kind: "tool",
      durationMs: 7,
    });
  });

  it("excludes approval wait from fallback tool duration", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "tool.requested",
      turn: 1,
      call_id: "approved",
      tool: "shell",
      args: { command: "true" },
      requires_approval: true,
    }, "2026-08-01T12:00:00.000Z"));
    view.apply(envelope(2, {
      type: "approval.resolved",
      call_id: "approved",
      decision: "approve",
    }, "2026-08-01T12:00:10.000Z"));
    view.apply(envelope(3, {
      type: "tool.completed",
      call_id: "approved",
      status: "ok",
      result: {},
    }, "2026-08-01T12:00:10.250Z"));
    expect(view.findTool("approved")).toMatchObject({ durationMs: 250 });
  });

  it("matches canonical cancellation by removing the active turn shell", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "turn.started",
      turn: 3,
      mode: "code",
      model: "test/model",
    }));
    view.apply(envelope(2, { type: "turn.cancelled", turn: 3 }));
    expect(view.turnRunning).toBe(false);
    expect(view.items).toEqual([]);
    expect(view.totalItems).toBe(0);
  });

  it("aborts an unmatched provider wait when its turn is cancelled", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "turn.started",
      turn: 3,
      mode: "code",
      model: "test/model",
    }, "2026-08-01T12:00:00.000Z"));
    view.apply(envelope(2, {
      type: "tool.requested",
      turn: 3,
      call_id: "wait",
      tool: "collabAgentToolCall",
      args: { tool: "wait" },
      requires_approval: false,
    }, "2026-08-01T12:00:00.010Z"));
    view.apply(envelope(3, {
      type: "tool.started",
      call_id: "wait",
    }, "2026-08-01T12:00:00.020Z"));
    view.apply(envelope(4, {
      type: "turn.cancelled",
      turn: 3,
    }, "2026-08-01T12:00:00.270Z"));

    expect(view.turnRunning).toBe(false);
    expect(view.items).toEqual([expect.objectContaining({
      kind: "tool",
      callId: "wait",
      status: "aborted",
      durationMs: 250,
    })]);
  });

  it("closes thinking at tool and steering causal boundaries", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, { type: "assistant.thinking", turn: 2, text: "before tool" }));
    view.apply(envelope(2, {
      type: "tool.requested",
      turn: 2,
      call_id: "read",
      tool: "read_file",
      args: {},
      requires_approval: false,
    }));
    view.apply(envelope(3, { type: "assistant.thinking", turn: 2, text: "after tool" }));
    view.apply(envelope(4, {
      type: "turn.steered",
      turn: 2,
      content: "new direction",
      attachments: [],
    }));
    view.apply(envelope(5, { type: "assistant.thinking", turn: 2, text: "after steer" }));
    expect(view.items.map((item) => item.kind)).toEqual([
      "thinking", "tool", "thinking", "steered", "thinking",
    ]);
  });

  it("keeps authored progress separate from provider reasoning", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "assistant.progress",
      turn: 2,
      text: "Checking the adapter.",
    }));
    view.apply(envelope(2, { type: "assistant.progress_completed", turn: 2 }));
    view.apply(envelope(3, {
      type: "assistant.thinking",
      turn: 2,
      text: "The streams have different semantics.",
    }));
    view.apply(envelope(4, { type: "assistant.thinking_completed", turn: 2 }));

    expect(view.items).toEqual([
      expect.objectContaining({
        kind: "progress",
        content: "Checking the adapter.",
        complete: true,
      }),
      expect.objectContaining({
        kind: "thinking",
        content: "The streams have different semantics.",
        complete: true,
      }),
    ]);
  });

  it("restores a completed compaction boundary from a folded snapshot", () => {
    const view = ThreadViewModel.fromSnapshot(12, {
      item_offset: 8,
      total_items: 9,
      has_older: true,
      compacting: false,
      items: [{
        kind: "compaction",
        turn: 4,
        state: { state: "completed", messages_compacted: 27 },
      }],
    });

    expect(view.items).toEqual([{
      id: "snapshot:8",
      kind: "compaction",
      turn: 4,
      state: { kind: "completed", messagesCompacted: 27 },
    }]);
  });

  it("preserves live todos when an older-compatible snapshot omits them", () => {
    const view = new ThreadViewModel();
    view.apply(envelope(1, {
      type: "thread.todos_updated",
      todos: [{ id: "live", content: "Keep me", status: "in_progress" }],
    }));

    view.replaceSnapshot(2, { items: [] });

    expect(view.todos).toEqual([
      { id: "live", content: "Keep me", status: "in_progress" },
    ]);
  });

  it("prepends only a contiguous folded page and keeps absolute item ids stable", () => {
    const newest: ProtocolThreadViewSnapshot = {
      item_offset: 2,
      total_items: 4,
      has_older: true,
      items: [
        { kind: "user", turn: 2, content: "new", attachments: [] },
        { kind: "assistant", turn: 2, content: "answer", complete: true },
      ],
    };
    const view = ThreadViewModel.fromSnapshot(20, newest);
    const newestIds = view.items.map(({ id }) => id);

    expect(view.prependSnapshot({
      item_offset: 0,
      total_items: 4,
      has_older: false,
      items: [
        { kind: "user", turn: 1, content: "old", attachments: [] },
        { kind: "assistant", turn: 1, content: "earlier", complete: true },
      ],
    })).toBe(true);
    expect(view.itemOffset).toBe(0);
    expect(view.hasOlder).toBe(false);
    expect(view.items.map(({ id }) => id)).toEqual([
      "snapshot:0",
      "snapshot:1",
      ...newestIds,
    ]);

    expect(view.prependSnapshot({
      item_offset: 8,
      items: [{ kind: "user", turn: 9, content: "gap", attachments: [] }],
    })).toBe(false);
  });

  it("merges a fresh tail without discarding prefetched history", () => {
    const view = ThreadViewModel.fromSnapshot(20, {
      item_offset: 4,
      total_items: 6,
      has_older: true,
      items: [
        { kind: "user", turn: 3, content: "old tail", attachments: [] },
        { kind: "assistant", turn: 3, content: "old answer", complete: true },
      ],
    });
    expect(view.prependSnapshot({
      item_offset: 2,
      total_items: 6,
      has_older: true,
      items: [
        { kind: "user", turn: 2, content: "prefetched", attachments: [] },
        { kind: "assistant", turn: 2, content: "history", complete: true },
      ],
    })).toBe(true);

    view.mergeTailSnapshot(25, {
      item_offset: 4,
      total_items: 7,
      has_older: true,
      items: [
        { kind: "user", turn: 3, content: "fresh tail", attachments: [] },
        { kind: "assistant", turn: 3, content: "fresh answer", complete: true },
        { kind: "user", turn: 4, content: "new item", attachments: [] },
      ],
    });

    expect(view.itemOffset).toBe(2);
    expect(view.totalItems).toBe(7);
    expect(view.items.map(({ id }) => id)).toEqual([
      "snapshot:2",
      "snapshot:3",
      "snapshot:4",
      "snapshot:5",
      "snapshot:6",
    ]);
    expect(view.items.map((item) => "content" in item ? item.content : "")).toEqual([
      "prefetched",
      "history",
      "fresh tail",
      "fresh answer",
      "new item",
    ]);
  });

  it("bounds retained history while preserving absolute ids", () => {
    const view = ThreadViewModel.fromSnapshot(20, {
      item_offset: 10,
      total_items: 15,
      has_older: true,
      items: Array.from({ length: 5 }, (_, index) => ({
        kind: "user" as const,
        turn: index,
        content: `item ${index}`,
        attachments: [],
      })),
    });

    view.trimHistory(3);

    expect(view.itemOffset).toBe(12);
    expect(view.totalItems).toBe(15);
    expect(view.hasOlder).toBe(true);
    expect(view.items.map(({ id }) => id)).toEqual([
      "snapshot:12",
      "snapshot:13",
      "snapshot:14",
    ]);
  });

  it("folds a streamed turn into stable user, assistant, and terminal status items", () => {
    const vm = new ThreadViewModel();
    const events = [
      envelope(1, {
        type: "turn.started",
        turn: 1,
        mode: "code",
        model: "openai/gpt-5.6",
        thinking_level: "max",
      }),
      envelope(2, { type: "user.message", turn: 1, content: "Build it", attachments: [] }),
      envelope(3, { type: "assistant.thinking", turn: 1, text: "first " }),
      envelope(4, { type: "assistant.thinking", turn: 1, text: "thought" }),
      envelope(5, { type: "assistant.delta", turn: 1, text: "Working" }),
      envelope(6, { type: "assistant.delta", turn: 1, text: " now" }),
      envelope(7, { type: "assistant.message", turn: 1, content: "Working now." }),
      envelope(
        8,
        {
          type: "turn.completed",
          turn: 1,
          usage: { input_tokens: 10, output_tokens: 4 },
          checkpoint_id: "cp_after_1",
        },
        "2026-08-01T12:00:08Z",
      ),
    ];
    for (const event of events) vm.apply(event);

    expect(vm.cursor).toBe(8);
    expect(vm.lastUsageCursor).toBe(8);
    expect(vm.turnRunning).toBe(false);
    expect(vm.turnThinkingLevels.get(1)).toBe("max");
    expect(vm.thinking).toBe(false);
    expect(vm.items).toMatchObject([
      {
        kind: "turn-status",
        state: { kind: "completed", checkpointId: "cp_after_1" },
      },
      { kind: "user", content: "Build it" },
      { kind: "thinking", content: "first thought", complete: true },
      { kind: "assistant", content: "Working now.", complete: true },
    ]);
    expect(vm.turnDurationMs.get(1)).toBe(7_000);
  });

  it("closes the live thinking phase on its explicit provider boundary", () => {
    const vm = new ThreadViewModel();
    expect(vm.apply(envelope(1, {
      type: "assistant.thinking",
      turn: 2,
      text: "Waiting for another event.",
    }))).toBe(true);
    expect(vm.thinking).toBe(true);

    expect(vm.apply(envelope(2, {
      type: "assistant.thinking_completed",
      turn: 2,
    }))).toBe(true);
    expect(vm.thinking).toBe(false);
    expect(vm.items).toMatchObject([
      { kind: "thinking", content: "Waiting for another event.", complete: true },
    ]);
  });

  it("uses an interleaved tool request as a thought boundary", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "assistant.thinking",
      turn: 2,
      text: "The final overlap pass is still",
    }));
    vm.apply(envelope(2, {
      type: "tool.requested",
      turn: 2,
      call_id: "search",
      tool: "search_transcript",
      args: { query: "Stopping" },
      requires_approval: false,
    }));
    vm.apply(envelope(3, {
      type: "assistant.thinking",
      turn: 2,
      text: " running.",
    }));
    vm.apply(envelope(4, {
      type: "assistant.thinking_completed",
      turn: 2,
    }));

    expect(vm.items.filter((item) => item.kind === "thinking")).toMatchObject([
      {
        kind: "thinking",
        content: "The final overlap pass is still",
        complete: true,
      },
      {
        kind: "thinking",
        content: " running.",
        complete: true,
      },
    ]);
    expect(vm.thinking).toBe(false);
  });

  it("accumulates live turn usage while retaining the latest context measurement", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.started",
      turn: 1,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }));
    vm.apply(envelope(2, {
      type: "turn.capacity_acquired",
      turn: 1,
      wait_ms: 0,
      background: false,
    }));
    vm.apply(envelope(3, {
      type: "turn.usage_updated",
      turn: 1,
      usage: {
        input_tokens: 10_000,
        output_tokens: 500,
        cached_input_tokens: 80_000,
        context_input_tokens: 90_000,
        context_window: 258_400,
        cost_usd: 0.01,
      },
    }));
    vm.apply(envelope(4, {
      type: "turn.usage_updated",
      turn: 1,
      usage: {
        input_tokens: 2_000,
        output_tokens: 250,
        cached_input_tokens: 5_000,
        context_input_tokens: 70_000,
        cost_usd: 0.02,
      },
    }));

    expect(vm.turnRunning).toBe(true);
    expect(vm.lastUsageCursor).toBe(4);
    expect(vm.lastUsage).toMatchObject({
      input_tokens: 12_000,
      output_tokens: 750,
      cached_input_tokens: 85_000,
      context_input_tokens: 70_000,
      context_window: 258_400,
    });
    expect(vm.lastUsage?.cost_usd).toBeCloseTo(0.03);
    expect(vm.items).toMatchObject([
      {
        kind: "turn-status",
        state: {
          kind: "running",
          startedAt: "2026-08-01T12:00:01Z",
          usage: {
            input_tokens: 12_000,
            output_tokens: 750,
            cached_input_tokens: 85_000,
            context_input_tokens: 70_000,
            context_window: 258_400,
          },
        },
      },
    ]);
    const runningTurn = vm.items.find(
      (item) => item.kind === "turn-status" && item.state.kind === "running",
    );
    expect(runningTurn?.kind === "turn-status" && runningTurn.state.kind === "running"
      ? runningTurn.state.usage?.cost_usd
      : undefined).toBeCloseTo(0.03);

    vm.apply(envelope(5, {
      type: "turn.completed",
      turn: 1,
      usage: {
        input_tokens: 12_000,
        output_tokens: 750,
        cached_input_tokens: 85_000,
        cost_usd: 0.03,
      },
    }));
    expect(vm.lastUsage).toMatchObject({
      input_tokens: 12_000,
      output_tokens: 750,
      context_input_tokens: 70_000,
      context_window: 258_400,
    });
    vm.apply(envelope(6, {
      type: "turn.started",
      turn: 2,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }));
    vm.apply(envelope(7, {
      type: "turn.capacity_acquired",
      turn: 2,
      wait_ms: 0,
      background: false,
    }));
    expect(vm.lastUsage).toMatchObject({
      input_tokens: 12_000,
      output_tokens: 750,
      context_input_tokens: 70_000,
      context_window: 258_400,
    });
    expect(vm.items.at(-1)).toMatchObject({
      kind: "turn-status",
      turn: 2,
      state: { kind: "running" },
    });
    expect(vm.items.at(-1)).not.toHaveProperty("state.usage");
    vm.apply(envelope(8, {
      type: "turn.usage_updated",
      turn: 2,
      usage: {
        input_tokens: 300,
        output_tokens: 20,
        cached_input_tokens: 1_000,
        context_input_tokens: 1_300,
      },
    }));

    const turnStates = vm.items.filter((item) => item.kind === "turn-status");
    expect(turnStates).toMatchObject([
      {
        turn: 1,
        state: {
          kind: "completed",
          usage: {
            input_tokens: 12_000,
            output_tokens: 750,
            context_input_tokens: 70_000,
            context_window: 258_400,
          },
        },
      },
      {
        turn: 2,
        state: {
          kind: "running",
          usage: { input_tokens: 300, output_tokens: 20 },
        },
      },
    ]);
    vm.apply(envelope(9, {
      type: "turn.failed",
      turn: 2,
      error: "provider failed",
    }));
    expect(vm.lastUsage).toMatchObject({
      input_tokens: 12_000,
      output_tokens: 750,
      context_input_tokens: 70_000,
      context_window: 258_400,
    });
    expect(vm.items.at(-1)).toMatchObject({
      kind: "turn-status",
      turn: 2,
      state: { kind: "failed" },
    });

    vm.apply(envelope(10, {
      type: "turn.started",
      turn: 3,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }));
    vm.apply(envelope(11, {
      type: "turn.capacity_acquired",
      turn: 3,
      wait_ms: 0,
      background: false,
    }));
    vm.apply(envelope(12, {
      type: "turn.usage_updated",
      turn: 3,
      usage: {
        input_tokens: 400,
        output_tokens: 30,
      },
    }));
    vm.apply(envelope(13, {
      type: "turn.cancelled",
      turn: 3,
    }));
    expect(vm.lastUsage).toMatchObject({
      input_tokens: 12_000,
      output_tokens: 750,
      context_input_tokens: 70_000,
      context_window: 258_400,
    });
  });

  it("restores live usage and start time on a running snapshot turn", () => {
    const view = ThreadViewModel.fromSnapshot(17, {
      items: [{ kind: "turn_status", turn: 3, state: { state: "running" } }],
      turn_running: true,
      last_usage: { input_tokens: 30, output_tokens: 8 },
      active_usage: { input_tokens: 40, output_tokens: 12 },
      turn_started_at: { "3": "2026-08-01T12:00:00Z" },
    });

    expect(view.items).toMatchObject([{
      kind: "turn-status",
      turn: 3,
      state: {
        kind: "running",
        startedAt: "2026-08-01T12:00:00Z",
        usage: { input_tokens: 40, output_tokens: 12 },
      },
    }]);
    expect(view.lastUsage).toMatchObject({ input_tokens: 40, output_tokens: 12 });
  });

  it("keeps prior context off a new snapshot turn before its first usage update", () => {
    const view = ThreadViewModel.fromSnapshot(18, {
      items: [{ kind: "turn_status", turn: 4, state: { state: "running" } }],
      turn_running: true,
      last_usage: {
        input_tokens: 30,
        output_tokens: 8,
        context_input_tokens: 38,
        context_window: 100,
      },
      turn_started_at: { "4": "2026-08-01T12:01:00Z" },
    });

    expect(view.lastUsage).toMatchObject({
      input_tokens: 30,
      output_tokens: 8,
      context_input_tokens: 38,
      context_window: 100,
    });
    expect(view.items).toMatchObject([{
      kind: "turn-status",
      turn: 4,
      state: {
        kind: "running",
        startedAt: "2026-08-01T12:01:00Z",
      },
    }]);
    expect(view.items[0]).not.toHaveProperty("state.usage");
  });

  it("attaches bridged approvals to tool cards and keeps denials terminal", () => {
    const vm = new ThreadViewModel();
    vm.apply(
      envelope(1, {
        type: "tool.requested",
        turn: 1,
        call_id: "call_1",
        tool: "shell",
        args: { cmd: "cargo test" },
        requires_approval: false,
      }),
    );
    vm.apply(envelope(2, { type: "approval.requested", turn: 1, call_id: "call_1" }));
    expect(vm.findTool("call_1")?.status).toBe("awaiting-approval");
    expect(vm.pendingApprovals).toEqual(["call_1"]);

    vm.apply(
      envelope(3, { type: "approval.resolved", call_id: "call_1", decision: "deny" }),
    );
    vm.apply(envelope(4, { type: "tool.started", call_id: "call_1" }));
    vm.apply(
      envelope(5, {
        type: "tool.completed",
        call_id: "call_1",
        status: "error",
        result: { error: "user denied" },
      }),
    );
    expect(vm.findTool("call_1")).toMatchObject({
      status: "denied",
      result: { error: "user denied" },
    });
    expect(vm.pendingApprovals).toEqual([]);
  });

  it("folds bounded live output into an existing tool card", () => {
    const vm = new ThreadViewModel();
    vm.apply(
      envelope(1, {
        type: "tool.requested",
        turn: 1,
        call_id: "call_output",
        tool: "shell",
        args: { cmd: "cargo test" },
        requires_approval: false,
      }),
    );
    vm.apply(
      envelope(2, { type: "tool.output", call_id: "call_output", chunk: "running " }),
    );
    vm.apply(
      envelope(3, { type: "tool.output", call_id: "call_output", chunk: "tests\n" }),
    );

    expect(vm.findTool("call_output")?.output).toEqual({
      text: "running tests\n",
      bytes: 14,
      omitted: false,
    });
  });

  it("measures tool execution from start to completion for chat metadata", () => {
    const vm = new ThreadViewModel();
    vm.apply(
      envelope(1, {
        type: "tool.requested",
        turn: 1,
        call_id: "timed",
        tool: "shell",
        args: { command: "cargo test" },
        requires_approval: false,
      }, "2026-08-01T12:00:01Z"),
    );
    vm.apply(
      envelope(2, { type: "tool.started", call_id: "timed" }, "2026-08-01T12:00:03Z"),
    );
    vm.apply(
      envelope(3, {
        type: "tool.completed",
        call_id: "timed",
        status: "ok",
        result: { exit_code: 0 },
      }, "2026-08-01T12:00:08Z"),
    );
    expect(vm.findTool("timed")?.durationMs).toBe(5_000);
  });

  it("ignores unknown output and output delivered after completion", () => {
    const vm = new ThreadViewModel();
    expect(
      vm.apply(envelope(1, { type: "tool.output", call_id: "missing", chunk: "ignored" })),
    ).toBe(false);
    expect(vm.items).toEqual([]);
    expect(vm.cursor).toBe(1);

    vm.apply(
      envelope(2, {
        type: "tool.requested",
        turn: 1,
        call_id: "completed",
        tool: "shell",
        args: { cmd: "true" },
        requires_approval: false,
      }),
    );
    vm.apply(
      envelope(3, {
        type: "tool.completed",
        call_id: "completed",
        status: "ok",
        result: null,
      }),
    );
    expect(
      vm.apply(
        envelope(4, {
          type: "tool.output",
          call_id: "completed",
          chunk: "too late",
        }),
      ),
    ).toBe(false);
    expect(vm.findTool("completed")?.output).toEqual({
      text: "",
      bytes: 0,
      omitted: false,
    });
    expect(vm.cursor).toBe(4);
  });

  it("tracks question, command, queue, todo, and compaction replacement state", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, { type: "thread.compaction_started", turn: 2 }));
    vm.apply(
      envelope(2, {
        type: "thread.compaction_completed",
        turn: 2,
        messages_compacted: 10,
      }),
    );
    vm.apply(
      envelope(3, {
        type: "thread.commands_updated",
        commands: [{ name: "review", description: "Review changes" }],
      }),
    );
    vm.apply(envelope(4, { type: "thread.queue_updated", prompts: [] }));
    vm.apply(
      envelope(5, {
        type: "thread.todos_updated",
        todos: [{ id: "one", content: "Wire UI", status: "in_progress" }],
      }),
    );
    vm.apply(
      envelope(6, {
        type: "question.requested",
        turn: 2,
        request_id: "question_1",
        title: "Target",
        questions: [
          {
            id: "q1",
            prompt: "Ship where?",
            options: [{ id: "pwa", label: "PWA" }],
          },
        ],
      }),
    );
    expect(vm.compacting).toBe(false);
    expect(vm.pendingQuestions).toEqual(["question_1"]);
    expect(vm.items.find((item) => item.kind === "compaction")).toMatchObject({
      kind: "compaction",
      turn: 2,
      state: { kind: "completed", messagesCompacted: 10 },
    });

    vm.apply(
      envelope(7, {
        type: "question.resolved",
        request_id: "question_1",
        answers: [{ question_id: "q1", selected_option_ids: ["pwa"] }],
      }),
    );
    expect(vm.compacting).toBe(false);
    expect(vm.commands).toHaveLength(1);
    expect(vm.todos).toHaveLength(1);
    expect(vm.findQuestions("question_1")?.answers).toEqual([
      { question_id: "q1", selected_option_ids: ["pwa"] },
    ]);
  });

  it("tracks queue projection changes for the lifetime of a pending submission", () => {
    const vm = new ThreadViewModel();
    const queueRevision = vm.trackQueueRevision();
    const queued = {
      id: "queued-delayed-response",
      thread_id: "th_1",
      position: 1,
      content: "same text as an older prompt",
      created_at: "2026-08-01T12:00:00Z",
      attachments: [],
    };

    vm.apply(envelope(1, { type: "thread.queue_updated", prompts: [queued] }));
    vm.apply(envelope(2, { type: "thread.queue_updated", prompts: [] }));
    expect(queueRevision.queueChanged()).toBe(true);

    // The HTTP send response can arrive here. Its stable id remains recorded
    // until the request owner explicitly closes the tracker.
    vm.apply(envelope(3, {
      type: "user.message",
      turn: 4,
      content: queued.content,
      attachments: [],
    }));
    expect(vm.queue).toEqual([]);
    expect(queueRevision.queueChanged()).toBe(true);
    queueRevision.close();
    expect(queueRevision.queueChanged()).toBe(false);
  });

  it("marks an unfinished compaction stopped when normal output resumes", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, { type: "thread.compaction_started", turn: 2 }));
    vm.apply(envelope(2, { type: "assistant.delta", turn: 2, text: "Continuing" }));

    expect(vm.compacting).toBe(false);
    expect(vm.items[0]).toMatchObject({
      kind: "compaction",
      turn: 2,
      state: { kind: "failed" },
    });
  });

  it("persists an explicit failed compaction boundary", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, { type: "thread.compaction_started", turn: 2 }));
    vm.apply(envelope(2, { type: "thread.compaction_failed", turn: 2 }));

    expect(vm.compacting).toBe(false);
    expect(vm.items[0]).toMatchObject({
      kind: "compaction",
      turn: 2,
      state: { kind: "failed" },
    });
  });

  it("defensively applies full todo replacements in protocol order", () => {
    const vm = new ThreadViewModel();
    const snapshot: TodoItem[] = [
      { id: "first", content: "First", status: "pending" },
      { id: "second", content: "Second", status: "in_progress" },
    ];
    vm.replaceTodos(snapshot);

    expect(vm.todos).toEqual(snapshot);
    expect(vm.todos).not.toBe(snapshot);
    expect(vm.todos[0]).not.toBe(snapshot[0]);

    vm.apply(envelope(8, {
      type: "thread.todos_updated",
      todos: [
        { id: "second", content: "Second", status: "completed" },
        { id: "third", content: "Third", status: "pending" },
      ],
    }));
    expect(vm.todos).toEqual([
      { id: "second", content: "Second", status: "completed" },
      { id: "third", content: "Third", status: "pending" },
    ]);
  });

  it("projects todo lifecycle updates into the active turn", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.started",
      turn: 9,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }));
    vm.apply(envelope(2, {
      type: "thread.todos_updated",
      todos: [
        { id: "one", content: "Implement", status: "in_progress" },
        { id: "two", content: "Verify", status: "pending" },
      ],
    }));
    vm.apply(envelope(3, {
      type: "thread.todos_updated",
      todos: [{ id: "one", content: "Implement", status: "completed" }],
    }));

    expect(vm.items.filter((item) => item.kind === "todo")).toMatchObject([
      { kind: "todo", turn: 9, todoId: "one", state: "started" },
      { kind: "todo", turn: 9, todoId: "one", state: "completed" },
      { kind: "todo", turn: 9, todoId: "two", state: "skipped" },
    ]);
  });

  it("hydrates materialized todo updates from a folded snapshot", () => {
    const view = ThreadViewModel.fromSnapshot(12, {
      items: [{
        kind: "todo_update",
        turn: 4,
        todo_id: "verify",
        content: "Run the checks",
        state: "cancelled",
      }],
    });
    expect(view.items).toMatchObject([{
      kind: "todo",
      turn: 4,
      todoId: "verify",
      content: "Run the checks",
      state: "cancelled",
    }]);
  });

  it("produces the same projection during replay and live delivery", () => {
    const events = [
      envelope(1, { type: "turn.started", turn: 3, mode: "plan", model: "m" }),
      envelope(2, { type: "user.message", turn: 3, content: "plan", attachments: [] }),
      envelope(3, { type: "assistant.message", turn: 3, content: "done" }),
      envelope(4, { type: "turn.cancelled", turn: 3 }),
    ];
    const live = new ThreadViewModel();
    const replay = new ThreadViewModel();
    for (const event of events) live.apply(event);
    const batches: ProtocolEventEnvelope[][] = [];
    const replayBatcher = new ThreadReplayBatcher((batch) => {
      batches.push([...batch]);
      for (const event of batch) replay.apply(event);
    });
    for (const event of events) replayBatcher.receive(event);
    replayBatcher.flush();

    expect(batches).toEqual([events]);
    expect(replay.items).toEqual(live.items);
    expect(replay.cursor).toBe(live.cursor);
    expect(replay.turnRunning).toBe(false);
    expect(replay.items.find((item) => item.kind === "turn-status")).toBeUndefined();
  });

  it("removes a cancelled turn shell to match canonical snapshots", () => {
    const vm = new ThreadViewModel();
    vm.apply(
      envelope(
        1,
        { type: "turn.started", turn: 9, mode: "code", model: "m" },
        "2026-08-01T12:00:00Z",
      ),
    );
    vm.apply(
      envelope(
        2,
        { type: "turn.cancelled", turn: 9 },
        "2026-08-01T12:00:01.250Z",
      ),
    );

    expect(vm.items).toEqual([]);
    expect(vm.turnDurationMs.get(9)).toBe(1_250);
    expect(vm.turnRunning).toBe(false);
  });
});
