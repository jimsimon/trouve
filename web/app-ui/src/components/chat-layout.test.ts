import { describe, expect, it } from "vitest";

import type { ThreadChatItem } from "../state/thread-view-model.js";
import {
  activityGroupSummary,
  buildChatLayout,
  isContextCompactionTool,
  type AgentActivityItem,
} from "./chat-layout.js";

const output = { text: "", omitted: false, bytes: 0 } as const;

describe("buildChatLayout", () => {
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
      "Edited 1 file, read 1 file, ran 1 command, thought 1 time",
    );
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
    ])).toBe("Updated 1 todo");
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
