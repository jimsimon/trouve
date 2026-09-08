import { describe, expect, it } from "vitest";

import {
  DEFAULT_CHAT_PREFERENCES,
  effectiveChatCollapsePreferences,
} from "../services/chat-preferences.js";
import {
  activityRunItems,
  MAX_ACTIVITY_RUN_ITEMS,
  planAgentBody,
  TURN_SEGMENT_TARGET_ITEMS,
  segmentTurnSpans,
  turnSegmentId,
  turnSegmentUnitId,
  type AgentBodySpan,
} from "./agent-body-plan.js";
import type { AgentChatItem } from "./chat-layout.js";

const output = { text: "", omitted: false, bytes: 0 } as const;

const tool = (id: string, status: "ok" | "running" | "awaiting-approval" = "ok"): AgentChatItem => ({
  id,
  kind: "tool",
  callId: `call-${id}`,
  tool: "read",
  args: {},
  status,
  result: null,
  output,
});

const thinking = (id: string): AgentChatItem => ({
  id,
  kind: "thinking",
  turn: 1,
  content: "hmm",
  complete: true,
});

const assistant = (id: string, content = "text"): AgentChatItem => ({
  id,
  kind: "assistant",
  turn: 1,
  content,
  complete: true,
});

const compaction = (id: string): AgentChatItem => ({
  id,
  kind: "compaction",
  turn: 1,
  state: { kind: "completed", messagesCompacted: 3 },
});

const subagent = (id: string): AgentChatItem => ({
  id,
  kind: "subagent",
  turn: 1,
  threadId: "th_child",
  sessionId: "se_child",
  prompt: "look into it",
  model: "claude/opus",
});

const spanIds = (items: readonly AgentChatItem[], spans: readonly AgentBodySpan[]) =>
  spans.map((span) => ({
    kind: span.kind === "activity" ? span.activity : span.kind,
    ids: items.slice(span.start, span.end).map((item) => item.id),
  }));

describe("planAgentBody", () => {
  const collapse = effectiveChatCollapsePreferences(DEFAULT_CHAT_PREFERENCES);

  it("groups consecutive tools into runs and keeps thinking as its own row", () => {
    const items = [
      thinking("th1"),
      tool("t1"),
      tool("t2"),
      thinking("th2"),
      assistant("a1", "part one"),
      assistant("a2", "part two"),
      tool("t3"),
    ];
    expect(spanIds(items, planAgentBody(items, collapse))).toEqual([
      { kind: "thinking", ids: ["th1"] },
      { kind: "run", ids: ["t1", "t2"] },
      { kind: "thinking", ids: ["th2"] },
      { kind: "assistant", ids: ["a1", "a2"] },
      { kind: "run", ids: ["t3"] },
    ]);
  });

  it("isolates approval tools and marks compaction rail connections", () => {
    const items = [
      tool("t1"),
      compaction("c1"),
      tool("t2", "awaiting-approval"),
      tool("t3"),
    ];
    const spans = planAgentBody(items, collapse);
    expect(spanIds(items, spans)).toEqual([
      { kind: "run", ids: ["t1"] },
      { kind: "compaction", ids: ["c1"] },
      { kind: "approval", ids: ["t2"] },
      { kind: "run", ids: ["t3"] },
    ]);
    expect(spans[1]).toMatchObject({ kind: "compaction", legacy: false, connectAfter: true });
  });

  it("ends a tool run at a subagent node instead of absorbing it", () => {
    const items = [tool("t1"), subagent("s1"), tool("t2")];
    expect(spanIds(items, planAgentBody(items, collapse))).toEqual([
      { kind: "run", ids: ["t1"] },
      { kind: "node", ids: ["s1"] },
      { kind: "run", ids: ["t2"] },
    ]);
  });

  it("folds thinking into runs when the preference allows it", () => {
    const items = [thinking("th1"), tool("t1"), thinking("th2"), tool("t2")];
    const merged = effectiveChatCollapsePreferences({
      ...DEFAULT_CHAT_PREFERENCES,
      collapseThinkingWithTools: true,
    });
    expect(spanIds(items, planAgentBody(items, merged))).toEqual([
      { kind: "run", ids: ["th1", "t1", "th2", "t2"] },
    ]);
  });

  it("renders every tool individually when sequential grouping is off", () => {
    const items = [tool("t1"), tool("t2")];
    const split = effectiveChatCollapsePreferences({
      ...DEFAULT_CHAT_PREFERENCES,
      collapseSequentialToolCalls: false,
    });
    expect(spanIds(items, planAgentBody(items, split))).toEqual([
      { kind: "tool", ids: ["t1"] },
      { kind: "tool", ids: ["t2"] },
    ]);
  });

  it("chunks an oversized tool run so no span exceeds one virtual row", () => {
    const items: AgentChatItem[] = [];
    for (let index = 0; index < MAX_ACTIVITY_RUN_ITEMS * 3 + 5; index += 1) {
      items.push(tool(`t${index}`));
    }
    const spans = planAgentBody(items, collapse);
    expect(spans.length).toBe(4);
    expect(spans.every((span) => span.kind === "activity" && span.activity === "run")).toBe(true);
    expect(spans.map((span) => span.end - span.start)).toEqual([
      MAX_ACTIVITY_RUN_ITEMS,
      MAX_ACTIVITY_RUN_ITEMS,
      MAX_ACTIVITY_RUN_ITEMS,
      5,
    ]);
    expect(spans[0]?.start).toBe(0);
    expect(spans.at(-1)?.end).toBe(items.length);
    // Chunks start at fixed offsets, so a group keeps its identity (first
    // item id) as more tools stream into the same run.
    const shorter = planAgentBody(items.slice(0, MAX_ACTIVITY_RUN_ITEMS + 1), collapse);
    expect(shorter.map((span) => span.start)).toEqual([0, MAX_ACTIVITY_RUN_ITEMS]);
    const segments = segmentTurnSpans(spans, items.length);
    expect(segments.every((segment) =>
      segment.itemEnd - segment.itemStart <= TURN_SEGMENT_TARGET_ITEMS)).toBe(true);
  });

  it("returns run items in order", () => {
    const items = [tool("t1"), tool("t2")];
    const [span] = planAgentBody(items, collapse);
    expect(span).toBeDefined();
    expect(activityRunItems(items, span!, false).map((item) => item.id)).toEqual(["t1", "t2"]);
  });
});

describe("segmentTurnSpans", () => {
  const collapse = effectiveChatCollapsePreferences(DEFAULT_CHAT_PREFERENCES);

  it("keeps a small turn in one segment", () => {
    const items = [thinking("th1"), tool("t1"), assistant("a1")];
    const spans = planAgentBody(items, collapse);
    expect(segmentTurnSpans(spans, items.length)).toEqual([
      {
        index: 0,
        spanStart: 0,
        spanEnd: spans.length,
        itemStart: 0,
        itemEnd: items.length,
        first: true,
        last: true,
      },
    ]);
  });

  it("splits a long stream into item-budgeted segments covering every span", () => {
    const items: AgentChatItem[] = [];
    for (let index = 0; index < 100; index += 1) {
      items.push(thinking(`th${index}`), tool(`t${index}`));
    }
    const spans = planAgentBody(items, collapse);
    const segments = segmentTurnSpans(spans, items.length, 10);
    expect(segments.length).toBeGreaterThan(5);
    expect(segments[0]).toMatchObject({ first: true, last: false, itemStart: 0 });
    expect(segments.at(-1)).toMatchObject({ first: false, last: true, itemEnd: items.length });
    let expectedSpan = 0;
    let expectedItem = 0;
    for (const segment of segments) {
      expect(segment.spanStart).toBe(expectedSpan);
      expect(segment.itemStart).toBe(expectedItem);
      expect(segment.spanEnd).toBeGreaterThan(segment.spanStart);
      expectedSpan = segment.spanEnd;
      expectedItem = segment.itemEnd;
    }
    expect(expectedSpan).toBe(spans.length);
    expect(expectedItem).toBe(items.length);
  });

  it("never cuts next to a compaction boundary", () => {
    const items: AgentChatItem[] = [];
    for (let index = 0; index < 5; index += 1) items.push(tool(`before${index}`), thinking(`tb${index}`));
    items.push(compaction("c1"));
    for (let index = 0; index < 5; index += 1) items.push(tool(`after${index}`), thinking(`ta${index}`));
    const spans = planAgentBody(items, collapse);
    const compactionIndex = spans.findIndex((span) => span.kind === "compaction");
    const segments = segmentTurnSpans(spans, items.length, 1);
    for (const segment of segments) {
      expect(segment.spanStart).not.toBe(compactionIndex);
      expect(segment.spanStart).not.toBe(compactionIndex + 1);
    }
  });

  it("keeps the first segment id equal to the unit id", () => {
    expect(turnSegmentId("turn:7", 0)).toBe("turn:7");
    expect(turnSegmentId("turn:7", 3)).toBe("turn:7:seg:3");
    expect(turnSegmentUnitId("turn:7:seg:3")).toBe("turn:7");
    expect(turnSegmentUnitId("turn:7")).toBe("turn:7");
  });
});
