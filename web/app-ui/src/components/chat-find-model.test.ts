import { describe, expect, it } from "vitest";

import type { ThreadChatItem } from "../state/thread-view-model.js";
import {
  chatFindUnitIds,
  reconcileChatFind,
  stepChatFindIndex,
} from "./chat-find-model.js";

const items: ThreadChatItem[] = [
  {
    id: "user:1",
    kind: "user",
    turn: 1,
    content: "Hello from the Agent",
    attachments: [],
  },
  {
    id: "assistant:1",
    kind: "assistant",
    turn: 1,
    content: "First answer",
    complete: true,
  },
  {
    id: "user:2",
    kind: "user",
    turn: 2,
    content: "Inspect src/SearchPanel.ts",
    attachments: [],
  },
  {
    id: "assistant:2",
    kind: "assistant",
    turn: 2,
    content: "The agent found it",
    complete: false,
  },
];

describe("chat find model", () => {
  it("matches literal text by turn in both case modes", () => {
    expect(chatFindUnitIds(items, "agent", false)).toEqual(["turn:1", "turn:2"]);
    expect(chatFindUnitIds(items, "Agent", true)).toEqual(["turn:1"]);
    expect(chatFindUnitIds(items, "agent", true)).toEqual(["turn:2"]);
    expect(chatFindUnitIds(items, "  SearchPanel  ", false)).toEqual(["turn:2"]);
    expect(chatFindUnitIds(items, "", false)).toEqual([]);
  });

  it("searches structured tool and question content", () => {
    const structured: ThreadChatItem[] = [
      {
        id: "tool:1",
        kind: "tool",
        callId: "call-1",
        tool: "read_file",
        args: { path: "src/parser.rs" },
        status: "ok",
        result: { summary: "Nested fence recovered" },
        output: { text: "line output", bytes: 11, omitted: false },
      },
      {
        id: "questions:1",
        kind: "questions",
        requestId: "request-1",
        title: "Choose branch",
        questions: [{ id: "branch", prompt: "Which branch?", options: [] }],
        answers: undefined,
      },
    ];
    expect(chatFindUnitIds(structured, "parser.rs", false)).toEqual(["turn:0:tool:1"]);
    expect(chatFindUnitIds(structured, "which branch", false)).toEqual([
      "turn:0:tool:1",
    ]);
  });

  it("stops traversing structured content at the node budget", () => {
    const args: Record<string, unknown> = {};
    for (let index = 0; index < 20_000; index += 1) {
      args[`padding-${index}`] = "padding";
    }
    Object.defineProperty(args, "unvisited", {
      enumerable: true,
      get: () => {
        throw new Error("the traversal read beyond its node budget");
      },
    });
    const structured: ThreadChatItem[] = [{
      id: "tool:bounded",
      kind: "tool",
      callId: "call-bounded",
      tool: "read_file",
      args,
      status: "ok",
      result: null,
      output: { text: "", bytes: 0, omitted: false },
    }];
    expect(chatFindUnitIds(structured, "not present", false)).toEqual([]);
  });

  it("preserves an active match during streaming and wraps navigation", () => {
    expect(reconcileChatFind(["a", "b", "c"], "b")).toEqual({
      unitIds: ["a", "b", "c"],
      activeIndex: 1,
    });
    expect(reconcileChatFind(["a", "c"], "b").activeIndex).toBe(0);
    expect(reconcileChatFind(["a", "b"], "b", true).activeIndex).toBe(0);
    expect(stepChatFindIndex(3, 2, 1)).toBe(0);
    expect(stepChatFindIndex(3, 0, -1)).toBe(2);
    expect(stepChatFindIndex(0, 0, 1)).toBe(-1);
  });
});
