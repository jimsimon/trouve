import { describe, expect, it } from "vitest";

import type { ThreadChatItem } from "../state/thread-view-model.js";
import {
  activityGroupSummary,
  buildChatLayout,
  isContextCompactionTool,
  isStandaloneCommandUnit,
  type AgentActivityItem,
} from "./chat-layout.js";

const output = { text: "", omitted: false, bytes: 0 } as const;

describe("buildChatLayout", () => {
  it("identifies deterministic commands as standalone transcript units", () => {
    const command: ThreadChatItem = {
      id: "command-1",
      kind: "command",
      name: "status",
      arguments: "",
      output: "Ready",
    };
    const unit = buildChatLayout([command]).units[0];
    expect(unit).toBeDefined();
    expect(isStandaloneCommandUnit(unit!)).toBe(true);

    const turnUnit = buildChatLayout([
      { id: "u1", kind: "user", turn: 1, content: "Run", attachments: [] },
      command,
    ]).units[0];
    expect(turnUnit).toBeDefined();
    expect(isStandaloneCommandUnit(turnUnit!)).toBe(false);
  });

  it("gives post-command activity in the same turn a distinct unit id", () => {
    const layout = buildChatLayout([
      { id: "before", kind: "thinking", turn: 3, content: "Before", complete: true },
      {
        id: "command-1",
        kind: "command",
        name: "status",
        arguments: "",
        output: "Ready",
      },
      { id: "after", kind: "thinking", turn: 3, content: "After", complete: false },
    ]);

    expect(layout.units.map((unit) => unit.id)).toEqual([
      "turn:3",
      "standalone:command-1",
      "turn:3:segment:after",
    ]);
    expect(new Set(layout.units.map((unit) => unit.id)).size).toBe(layout.units.length);
  });

  it("groups each prompt and assistant/work run into one turn", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 1, content: "Do it", attachments: [] },
      { id: "a1", kind: "assistant", turn: 1, content: "First", complete: true },
      { id: "t1", kind: "tool", callId: "c1", tool: "read", args: { path: "a.rs" }, status: "ok", result: null, output },
      { id: "a2", kind: "assistant", turn: 1, content: "Done", complete: true },
      { id: "s1", kind: "turn-status", turn: 1, state: { kind: "completed", usage: { input_tokens: 0, output_tokens: 0 } } },
      { id: "u2", kind: "user", turn: 2, content: "Next", attachments: [] },
    ];

    const layout = buildChatLayout(items);
    expect(layout.units.map((unit) => unit.kind)).toEqual(["turn", "turn"]);
    expect(layout.units[0]).toMatchObject({
      kind: "turn",
      turn: 1,
      prompt: { id: "u1" },
      items: [{ id: "a1" }, { id: "t1" }, { id: "a2" }],
      status: { id: "s1" },
    });
    expect(layout.units[1]).toMatchObject({ kind: "turn", divider: true });
    expect(layout.unitIdForItem.get("u1")).toBe(layout.units[0]?.id);
    expect(layout.unitIdForItem.get("t1")).toBe(layout.units[0]?.id);
    expect(layout.unitIdForItem.get("s1")).toBe(layout.units[0]?.id);
  });

  it("keeps tools with their prompt when a turn begins with tools", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 4, content: "Search", attachments: [] },
      { id: "t1", kind: "tool", callId: "c1", tool: "search", args: {}, status: "running", result: undefined, output },
      { id: "t2", kind: "tool", callId: "c2", tool: "read", args: {}, status: "running", result: undefined, output },
    ];
    expect(buildChatLayout(items).units[0]).toMatchObject({
      kind: "turn",
      turn: 4,
      prompt: { id: "u1" },
      items: [{ id: "t1" }, { id: "t2" }],
    });
  });

  it("keeps paged progress separate from a later-turn prompt", () => {
    const items: ThreadChatItem[] = [
      {
        id: "p4",
        kind: "progress",
        turn: 4,
        content: "Checking the adapter",
        complete: true,
      },
      { id: "u5", kind: "user", turn: 5, content: "Continue", attachments: [] },
    ];

    const layout = buildChatLayout(items);
    expect(layout.units).toHaveLength(2);
    expect(layout.units[0]).toMatchObject({
      turn: 4,
      prompt: undefined,
      items: [{ id: "p4", kind: "progress" }],
    });
    expect(layout.units[1]).toMatchObject({
      turn: 5,
      prompt: { id: "u5" },
      items: [],
    });
  });

  it("keeps compaction between adjacent work runs in the same agent card", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 4, content: "Continue", attachments: [] },
      { id: "t1", kind: "tool", callId: "c1", tool: "read", args: {}, status: "ok", result: null, output },
      { id: "c1", kind: "compaction", turn: 4, state: { kind: "completed", messagesCompacted: 12 } },
      { id: "t2", kind: "tool", callId: "c2", tool: "edit", args: {}, status: "ok", result: null, output },
    ];

    expect(buildChatLayout(items).units[0]).toMatchObject({
      kind: "turn",
      turn: 4,
      items: [{ id: "t1" }, { id: "c1" }, { id: "t2" }],
    });
  });

  it("keeps todo lifecycle rows in their explicit turn", () => {
    const items: ThreadChatItem[] = [
      { id: "u4", kind: "user", turn: 4, content: "Continue", attachments: [] },
      {
        id: "todo4",
        kind: "todo",
        turn: 4,
        todoId: "verify",
        content: "Run the checks",
        state: "completed",
      },
    ];
    expect(buildChatLayout(items).units[0]).toMatchObject({
      turn: 4,
      items: [{ id: "todo4", kind: "todo" }],
    });
  });

  it("retains a todo tool card when lifecycle rows lack exact call causality", () => {
    const todoTool: ThreadChatItem = {
      id: "todo-tool",
      kind: "tool",
      callId: "todo-call",
      tool: "mcpToolCall",
      args: {
        tool: "mcp__trouve__todo_write",
        arguments: {
          todos: [{ id: "verify", content: "Run checks", status: "completed" }],
        },
      },
      status: "ok",
      result: null,
      output,
    };
    const lifecycle: ThreadChatItem = {
      id: "todo-completed",
      kind: "todo",
      turn: 4,
      todoId: "verify",
      content: "Run checks",
      state: "completed",
    };

    const projected = buildChatLayout([
      { id: "u4", kind: "user", turn: 4, content: "Continue", attachments: [] },
      todoTool,
      lifecycle,
    ]);
    expect(projected.units[0]?.items).toEqual([todoTool, lifecycle]);
    expect(projected.unitIdForItem.has("todo-tool")).toBe(true);

    const legacy = buildChatLayout([
      { id: "u4", kind: "user", turn: 4, content: "Continue", attachments: [] },
      todoTool,
    ]);
    expect(legacy.units[0]?.items).toEqual([todoTool]);
  });

  it("retains parallel, interleaved, multiple, and no-op TODO calls", () => {
    const todoTool = (id: string): ThreadChatItem => ({
      id,
      kind: "tool",
      callId: id,
      tool: "todo_write",
      args: { todos: [] },
      status: "ok",
      result: null,
      output,
    });
    const todoLifecycle = (id: string, todoId: string): ThreadChatItem => ({
      id,
      kind: "todo",
      turn: 4,
      todoId,
      content: todoId,
      state: "completed",
    });
    const layout = buildChatLayout([
      { id: "u4", kind: "user", turn: 4, content: "Continue", attachments: [] },
      todoTool("parallel-one"),
      {
        id: "interleaved-shell",
        kind: "tool",
        callId: "shell-call",
        tool: "shell",
        args: { command: "true" },
        status: "ok",
        result: null,
        output,
      },
      todoLifecycle("lifecycle-one", "one"),
      todoTool("parallel-two"),
      todoLifecycle("lifecycle-two", "two"),
      todoTool("no-op"),
    ]);
    expect(layout.units[0]?.items.map((item) => item.id)).toEqual([
      "parallel-one",
      "interleaved-shell",
      "lifecycle-one",
      "parallel-two",
      "lifecycle-two",
      "no-op",
    ]);
  });

  it("retains failed and third-party TODO calls beside lifecycle rows", () => {
    const todoTool = (
      id: string,
      tool: string,
      args: unknown,
      status: "ok" | "error",
    ): ThreadChatItem => ({
      id,
      kind: "tool",
      callId: id,
      tool,
      args,
      status,
      result: status === "ok" ? null : { error: "failed" },
      output,
    });
    const lifecycle: ThreadChatItem = {
      id: "todo-completed",
      kind: "todo",
      turn: 4,
      todoId: "verify",
      content: "Run checks",
      state: "completed",
    };
    const layout = buildChatLayout([
      { id: "u4", kind: "user", turn: 4, content: "Continue", attachments: [] },
      todoTool("failed-native", "todo_write", { todos: [] }, "error"),
      todoTool("external-direct", "mcp__linear__todo_write", { todos: [] }, "ok"),
      todoTool("external-wrapped", "mcpToolCall", {
        serverName: "linear",
        toolName: "todo_write",
        arguments: { todos: [] },
      }, "ok"),
      lifecycle,
    ]);

    expect(layout.units[0]?.items.map((item) => item.id)).toEqual([
      "failed-native",
      "external-direct",
      "external-wrapped",
      "todo-completed",
    ]);
  });

  it("keeps steering in its active turn between the output it redirects", () => {
    const items: ThreadChatItem[] = [
      { id: "u5", kind: "user", turn: 5, content: "Begin", attachments: [] },
      { id: "x1", kind: "thinking", turn: 5, content: "Before", complete: true },
      {
        id: "st5",
        kind: "steered",
        turn: 5,
        content: "Prioritize tests",
        attachments: [],
      },
      { id: "x2", kind: "thinking", turn: 5, content: "After", complete: false },
    ];

    const layout = buildChatLayout(items);
    expect(layout.units).toHaveLength(1);
    expect(layout.units[0]).toMatchObject({
      turn: 5,
      prompt: { id: "u5" },
      items: [{ id: "x1" }, { id: "st5" }, { id: "x2" }],
    });
    expect(layout.unitIdForItem.get("st5")).toBe("turn:5");
  });

  it("promotes a linked subagent and suppresses its redundant spawn tool row", () => {
    const items: ThreadChatItem[] = [
      { id: "u6", kind: "user", turn: 6, content: "Delegate", attachments: [] },
      {
        id: "spawn-tool",
        kind: "tool",
        callId: "call_spawn",
        tool: "spawn_thread",
        args: { prompt: "Review the host" },
        status: "ok",
        result: { thread_id: "th_child" },
        output,
      },
      {
        id: "subagent",
        kind: "subagent",
        turn: 6,
        threadId: "th_child",
        sessionId: "se_parent",
        prompt: "Review the host",
        model: "codex/gpt-5.6-terra",
        callId: "call_spawn",
      },
    ];

    expect(buildChatLayout(items).units[0]).toMatchObject({
      turn: 6,
      items: [{ id: "subagent", kind: "subagent" }],
    });
    expect(buildChatLayout(items).unitIdForItem.has("spawn-tool")).toBe(false);
  });

  it("suppresses child-agent output polling while retaining delegation nodes", () => {
    const subagent: ThreadChatItem = {
      id: "subagent",
      kind: "subagent",
      turn: 6,
      threadId: "th_child",
      sessionId: "se_parent",
      prompt: "Review the host",
      model: "codex/gpt-5.6-terra",
    };
    const collection: ThreadChatItem = {
      id: "spawn-output",
      kind: "tool",
      callId: "call_output",
      tool: "mcpToolCall",
      args: {
        tool: "mcp__trouve__spawn_output",
        arguments: { thread_id: "th_child", wait_ms: 30_000 },
      },
      status: "ok",
      result: { thread_id: "th_child", status: "completed" },
      output,
    };

    const layout = buildChatLayout([
      { id: "u6", kind: "user", turn: 6, content: "Delegate", attachments: [] },
      subagent,
      collection,
    ]);
    expect(layout.units[0]?.items).toEqual([subagent]);
    expect(layout.unitIdForItem.has("spawn-output")).toBe(false);
  });

  it("retains failed and third-party spawn_output tool calls", () => {
    const tool = (id: string, name: string, status: "ok" | "error"): ThreadChatItem => ({
      id,
      kind: "tool",
      callId: id,
      tool: name,
      args: { thread_id: "th_child" },
      status,
      result: status === "error" ? { error: "collection failed" } : {},
      output,
    });
    const layout = buildChatLayout([
      { id: "u6", kind: "user", turn: 6, content: "Delegate", attachments: [] },
      tool("failed", "spawn_output", "error"),
      tool("external", "mcp__example__spawn_output", "ok"),
    ]);
    expect(layout.units[0]?.items.map((item) => item.id)).toEqual(["failed", "external"]);
  });

  it("associates the event-folded leading status with its turn", () => {
    const items: ThreadChatItem[] = [
      { id: "s7", kind: "turn-status", turn: 7, state: { kind: "completed", usage: { input_tokens: 2, output_tokens: 1 } } },
      { id: "u7", kind: "user", turn: 7, content: "Build", attachments: [] },
      { id: "a7", kind: "assistant", turn: 7, content: "Done", complete: true },
    ];
    const layout = buildChatLayout(items);
    expect(layout.units.map((unit) => unit.kind)).toEqual(["turn"]);
    expect(layout.units[0]).toMatchObject({
      prompt: { id: "u7" },
      items: [{ id: "a7" }],
      status: { id: "s7" },
    });
    expect(layout.unitIdForItem.get("s7")).toBe(layout.units[0]?.id);
  });

  it("promotes a recovered subagent prompt ahead of earlier thought activity", () => {
    const items: ThreadChatItem[] = [
      { id: "thought", kind: "thinking", turn: 8, content: "Finishing quickly.", complete: true },
      { id: "status", kind: "turn-status", turn: 8, state: { kind: "completed", usage: { input_tokens: 1, output_tokens: 1 } } },
      { id: "prompt", kind: "user", turn: 8, content: "Recovered after completion.", attachments: [] },
    ];
    expect(buildChatLayout(items).units[0]).toMatchObject({
      prompt: { id: "prompt", content: "Recovered after completion." },
      items: [{ id: "thought" }],
      status: { id: "status" },
    });
  });

  it("keeps a terminal failure in the affected turn", () => {
    const items: ThreadChatItem[] = [
      { id: "s8", kind: "turn-status", turn: 8, state: { kind: "failed", error: "boom" } },
      { id: "u8", kind: "user", turn: 8, content: "Build", attachments: [] },
      { id: "a8", kind: "assistant", turn: 8, content: "Partial", complete: true },
    ];
    expect(buildChatLayout(items).units).toHaveLength(1);
    expect(buildChatLayout(items).units[0]).toMatchObject({
      kind: "turn",
      prompt: { id: "u8" },
      items: [{ id: "a8" }],
      status: { id: "s8", state: { kind: "failed" } },
    });
  });

  it("lets a leading tool inherit the next explicit turn in a bounded page", () => {
    const items: ThreadChatItem[] = [
      { id: "t1", kind: "tool", callId: "c1", tool: "read", args: {}, status: "ok", result: null, output },
      { id: "a9", kind: "assistant", turn: 9, content: "Done", complete: true },
    ];
    expect(buildChatLayout(items).units[0]).toMatchObject({
      id: "turn:9",
      turn: 9,
      items: [{ id: "t1" }, { id: "a9" }],
    });
  });
});

describe("activityGroupSummary", () => {
  it("matches the retained activity categories and distinct-path counting", () => {
    const items: AgentActivityItem[] = [
      { id: "e1", kind: "tool", callId: "e1", tool: "edit", args: { path: "a.rs" }, status: "ok", result: null, output },
      { id: "e2", kind: "tool", callId: "e2", tool: "write_file", args: { file_path: "a.rs" }, status: "ok", result: null, output },
      { id: "r1", kind: "tool", callId: "r1", tool: "read", args: { path: "b.rs" }, status: "ok", result: null, output },
      { id: "c1", kind: "tool", callId: "c1", tool: "Bash", args: {}, status: "ok", result: null, output },
      { id: "x1", kind: "thinking", turn: 1, content: "hmm", complete: true },
    ];
    expect(activityGroupSummary(items)).toBe(
      "Edited 1 file, read 1 file, ran 1 command, reasoned 1 time",
    );
  });

  it("counts every hashline section as an edit to its header path", () => {
    const items: AgentActivityItem[] = [{
      id: "h1",
      kind: "tool",
      callId: "h1",
      tool: "hashline_edit",
      args: {
        input: "[src/lib.rs#A1B2C3D4E5F6]\nPUT 1:\n+updated\n[README.md#123456789ABC]\nCUT 2\n",
      },
      status: "ok",
      result: null,
      output,
    }];
    expect(activityGroupSummary(items)).toBe("Edited 2 files");
  });

  it("separates code and transcript searches from generic tool calls", () => {
    const tool = (id: string, name: string, args: unknown = {}): AgentActivityItem => ({
      id,
      kind: "tool",
      callId: id,
      tool: name,
      args,
      status: "ok",
      result: null,
      output,
    });
    const items: AgentActivityItem[] = [
      tool("search", "mcp__trouve__search"),
      tool("related", "mcpToolCall", {
        tool: "mcp__trouve__find_related",
        arguments: { file_path: "src/main.ts", line: 10 },
      }),
      tool("transcript", "mcp__trouve__search_transcript"),
      tool("other", "mcp__example__custom_tool"),
    ];

    expect(activityGroupSummary(items)).toBe(
      "Ran 2 code searches, ran 1 transcript search, called 1 tool",
    );
  });

  it("counts third-party MCP basename collisions as generic tool calls", () => {
    const item: AgentActivityItem = {
      id: "external-search",
      kind: "tool",
      callId: "external-search",
      tool: "mcp__example__search",
      args: { query: "record" },
      status: "ok",
      result: null,
      output,
    };
    expect(activityGroupSummary([item])).toBe("Called 1 tool");
  });

  it("keeps wrapped third-party basename collisions in the generic category", () => {
    const wrapped = (id: string, wrapper: string): AgentActivityItem => ({
      id,
      kind: "tool",
      callId: id,
      tool: wrapper,
      args: {
        serverName: "example",
        toolName: "search",
        arguments: { query: "record" },
      },
      status: "ok",
      result: null,
      output,
    });
    expect(activityGroupSummary([
      wrapped("mcp", "mcpToolCall"),
      wrapped("dynamic", "dynamicToolCall"),
    ])).toBe("Called 2 tools");
  });

  it("recognizes legacy context-compaction tool names", () => {
    const item = (tool: string): AgentActivityItem => ({
      id: tool,
      kind: "tool",
      callId: tool,
      tool,
      args: {},
      status: "ok",
      result: null,
      output,
    });
    expect(isContextCompactionTool(item("contextCompaction"))).toBe(true);
    expect(isContextCompactionTool(item("mcp__trouve__context_compaction"))).toBe(true);
    expect(isContextCompactionTool(item("compact_context"))).toBe(true);
    expect(isContextCompactionTool(item("commandExecution"))).toBe(false);
  });

  it("keeps collapsed compaction visible in the activity summary", () => {
    const legacy: AgentActivityItem = {
      id: "legacy",
      kind: "tool",
      callId: "legacy",
      tool: "contextCompaction",
      args: {},
      status: "ok",
      result: null,
      output,
    };
    expect(activityGroupSummary([
      { id: "c1", kind: "compaction", turn: 1, state: { kind: "completed", messagesCompacted: 8 } },
    ])).toBe("Compacted context");
    expect(activityGroupSummary([legacy])).toBe("Compacted context");
  });

  it("includes collapsed todo updates in the activity summary", () => {
    expect(activityGroupSummary([
      {
        id: "todo1",
        kind: "todo",
        turn: 1,
        todoId: "verify",
        content: "Run the checks",
        state: "started",
      },
      {
        id: "todo2",
        kind: "todo",
        turn: 1,
        todoId: "verify",
        content: "Run the checks",
        state: "completed",
      },
    ])).toBe("Updated 1 TODO");
  });

  it("describes repeated same-state todo updates with their lifecycle action", () => {
    const completed = Array.from({ length: 5 }, (_, index): AgentActivityItem => ({
      id: `todo${index}`,
      kind: "todo",
      turn: 1,
      todoId: `todo-${index}`,
      content: `Task ${index + 1}`,
      state: "completed",
    }));
    expect(activityGroupSummary(completed)).toBe("Completed 5 TODOs");
  });
});
