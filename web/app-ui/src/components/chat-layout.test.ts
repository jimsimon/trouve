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
  it("groups an uninterrupted assistant/work run into one agent card", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 1, content: "Do it", attachments: [] },
      { id: "a1", kind: "assistant", turn: 1, content: "First", complete: true },
      { id: "t1", kind: "tool", callId: "c1", tool: "read", args: { path: "a.rs" }, status: "ok", result: null, output },
      { id: "a2", kind: "assistant", turn: 1, content: "Done", complete: true },
      { id: "s1", kind: "turn-status", turn: 1, state: { kind: "completed", usage: { input_tokens: 0, output_tokens: 0 } } },
      { id: "u2", kind: "user", turn: 2, content: "Next", attachments: [] },
    ];

    const layout = buildChatLayout(items);
    expect(layout.units.map((unit) => unit.kind)).toEqual(["user", "agent", "user"]);
    expect(layout.units[1]).toMatchObject({
      kind: "agent",
      turn: 1,
      items: [{ id: "a1" }, { id: "t1" }, { id: "a2" }],
    });
    expect(layout.units[2]).toMatchObject({ kind: "user", divider: true });
    expect(layout.unitIdForItem.get("t1")).toBe(layout.units[1]?.id);
  });

  it("synthesizes an agent card when a turn begins with tools", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 4, content: "Search", attachments: [] },
      { id: "t1", kind: "tool", callId: "c1", tool: "search", args: {}, status: "running", result: undefined, output },
      { id: "t2", kind: "tool", callId: "c2", tool: "read", args: {}, status: "running", result: undefined, output },
    ];
    expect(buildChatLayout(items).units[1]).toMatchObject({ kind: "agent", turn: 4 });
  });

  it("keeps compaction between adjacent work runs in the same agent card", () => {
    const items: ThreadChatItem[] = [
      { id: "u1", kind: "user", turn: 4, content: "Continue", attachments: [] },
      { id: "t1", kind: "tool", callId: "c1", tool: "read", args: {}, status: "ok", result: null, output },
      { id: "c1", kind: "compaction", turn: 4, state: { kind: "completed", messagesCompacted: 12 } },
      { id: "t2", kind: "tool", callId: "c2", tool: "edit", args: {}, status: "ok", result: null, output },
    ];

    expect(buildChatLayout(items).units[1]).toMatchObject({
      kind: "agent",
      turn: 4,
      items: [{ id: "t1" }, { id: "c1" }, { id: "t2" }],
    });
  });

  it("associates the event-folded leading status with its later agent card", () => {
    const items: ThreadChatItem[] = [
      { id: "s7", kind: "turn-status", turn: 7, state: { kind: "completed", usage: { input_tokens: 2, output_tokens: 1 } } },
      { id: "u7", kind: "user", turn: 7, content: "Build", attachments: [] },
      { id: "a7", kind: "assistant", turn: 7, content: "Done", complete: true },
    ];
    const layout = buildChatLayout(items);
    expect(layout.units.map((unit) => unit.kind)).toEqual(["user", "agent"]);
    expect(layout.unitIdForItem.get("s7")).toBe(layout.units[1]?.id);
  });

  it("places a terminal failure after the affected turn instead of before its prompt", () => {
    const items: ThreadChatItem[] = [
      { id: "s8", kind: "turn-status", turn: 8, state: { kind: "failed", error: "boom" } },
      { id: "u8", kind: "user", turn: 8, content: "Build", attachments: [] },
      { id: "a8", kind: "assistant", turn: 8, content: "Partial", complete: true },
    ];
    expect(buildChatLayout(items).units.map((unit) => unit.kind))
      .toEqual(["user", "agent", "status"]);
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
});
