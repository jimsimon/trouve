import { describe, expect, it } from "vitest";

import type { ThreadChatItem } from "../state/thread-view-model.js";
import {
  chatFindMatches,
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
    expect(chatFindUnitIds(items, "assistant", false)).toEqual([]);
    expect(chatFindUnitIds(items, "assistant:1", false)).toEqual([]);
    expect(chatFindUnitIds(items, "turn:2", false)).toEqual([]);
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
        result: { summary: "Nested fence recovered", exit_code: 11, success: false },
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
    expect(chatFindUnitIds(structured, "11", false)).toEqual(["turn:0:tool:1"]);
    expect(chatFindUnitIds(structured, "false", false)).toEqual(["turn:0:tool:1"]);
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

  it("reuses searchable projections while item revisions are unchanged", () => {
    let reads = 0;
    const args: Record<string, unknown> = {};
    Object.defineProperty(args, "needle", {
      enumerable: true,
      get: () => {
        reads += 1;
        return "Cached projection";
      },
    });
    const structured: ThreadChatItem[] = [{
      id: "tool:cached",
      kind: "tool",
      callId: "call-cached",
      tool: "read_file",
      args,
      status: "ok",
      result: null,
      output: { text: "", bytes: 0, omitted: false },
    }];

    expect(chatFindUnitIds(structured, "cached", false)).toEqual(["turn:0:tool:cached"]);
    expect(chatFindUnitIds(structured, "projection", false)).toEqual(["turn:0:tool:cached"]);
    expect(reads).toBe(1);
    const tool = structured[0];
    if (tool?.kind !== "tool") throw new Error("missing cached tool fixture");
    tool.args = { needle: "Updated projection" };
    expect(chatFindUnitIds(structured, "updated", false)).toEqual(["turn:0:tool:cached"]);
  });

  it("shares one fair traversal budget across all uncached transcript items", () => {
    let reads = 0;
    const structured: ThreadChatItem[] = Array.from({ length: 3 }, (_, toolIndex) => {
      const args: Record<string, unknown> = {};
      for (let index = 0; index < 5_000; index += 1) {
        Object.defineProperty(args, `padding-${index}`, {
          enumerable: true,
          get: () => {
            reads += 1;
            return "padding";
          },
        });
      }
      return {
        id: `tool:aggregate-${toolIndex}`,
        kind: "tool",
        callId: `call-aggregate-${toolIndex}`,
        tool: "read_file",
        args,
        status: "ok",
        result: null,
        output: { text: "", bytes: 0, omitted: false },
      };
    });

    structured.push({
      id: "user:after-aggregate",
      kind: "user",
      turn: 1,
      content: "Tail match remains searchable",
      attachments: [],
    });

    const result = chatFindMatches(structured, "tail match", false);
    expect(result).toEqual({ unitIds: ["turn:1"], incomplete: true });
    expect(reads).toBeLessThan(15_000);
    const coldReads = reads;
    expect(chatFindMatches(structured, "tail match", false)).toEqual(result);
    expect(reads - coldReads).toBeLessThan(15_000);
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
