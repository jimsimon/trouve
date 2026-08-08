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

  it("keeps steering as a top-level boundary and closes preceding thought output", () => {
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

    expect(vm.turnSteerable.get(3)).toBe(true);
    expect(vm.thinking).toBe(false);
    expect(vm.items).toMatchObject([
      { kind: "turn-status", turn: 3 },
      { kind: "thinking", turn: 3, complete: true },
      {
        kind: "steered",
        turn: 3,
        content: "Prioritize the narrow layout.",
        attachments: [],
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

  it("applies live context usage without completing the running turn", () => {
    const vm = new ThreadViewModel();
    vm.apply(envelope(1, {
      type: "turn.started",
      turn: 1,
      mode: "code",
      model: "codex/gpt-5.6-sol",
    }));
    vm.apply(envelope(2, {
      type: "turn.usage_updated",
      turn: 1,
      usage: {
        input_tokens: 10_000,
        output_tokens: 500,
        cached_input_tokens: 80_000,
        context_input_tokens: 90_000,
        context_window: 258_400,
      },
    }));

    expect(vm.turnRunning).toBe(true);
    expect(vm.lastUsageCursor).toBe(2);
    expect(vm.lastUsage?.context_input_tokens).toBe(90_000);
    expect(vm.items).toMatchObject([
      {
        kind: "turn-status",
        state: {
          kind: "running",
          startedAt: "2026-08-01T12:00:01Z",
          usage: {
            input_tokens: 10_000,
            output_tokens: 500,
            context_input_tokens: 90_000,
          },
        },
      },
    ]);
  });

  it("restores live usage and start time on a running snapshot turn", () => {
    const view = ThreadViewModel.fromSnapshot(17, {
      items: [{ kind: "turn_status", turn: 3, state: { state: "running" } }],
      turn_running: true,
      last_usage: { input_tokens: 40, output_tokens: 12 },
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
    expect(replay.items.find((item) => item.kind === "turn-status"))
      .toMatchObject({ state: { kind: "cancelled" } });
  });

  it("keeps cancellation as an explicit interrupted terminal state", () => {
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

    expect(vm.items).toMatchObject([
      { kind: "turn-status", turn: 9, state: { kind: "cancelled" } },
    ]);
    expect(vm.turnDurationMs.get(9)).toBe(1_250);
    expect(vm.turnRunning).toBe(false);
  });
});
