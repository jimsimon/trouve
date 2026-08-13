import { describe, expect, it } from "vitest";

import { emptyToolOutput } from "../state/tool-output.js";
import type { ThreadChatItem, TurnState } from "../state/thread-view-model.js";
import {
  compactRunningElapsed,
  runningAgentActivity,
  runningToolActivityLabel,
  type RunningAgentActivityInput,
} from "./agent-activity-model.js";

const status = (turn: number, state: TurnState): ThreadChatItem => ({
  id: `status-${turn}-${state.kind}`,
  kind: "turn-status",
  turn,
  state,
});

const prompt = (turn: number): ThreadChatItem => ({
  id: `prompt-${turn}`,
  kind: "user",
  turn,
  content: "hello",
  attachments: [],
});

const tool = (
  name: string,
  args: unknown,
  toolStatus: Extract<ThreadChatItem, { kind: "tool" }>["status"],
): ThreadChatItem => ({
  id: `tool-${name}`,
  kind: "tool",
  callId: `call-${name}`,
  tool: name,
  args,
  status: toolStatus,
  result: undefined,
  output: emptyToolOutput(),
});

const presentation = (
  overrides: Partial<RunningAgentActivityInput> = {},
) => runningAgentActivity({
  items: [],
  turnRunning: true,
  thinking: false,
  compacting: false,
  turnModels: new Map(),
  turnStartedAt: new Map(),
  nowMs: 0,
  ...overrides,
});

describe("agent activity presentation", () => {
  it("formats elapsed waits compactly", () => {
    expect(compactRunningElapsed(-1_000)).toBe("0s");
    expect(compactRunningElapsed(59_999)).toBe("59s");
    expect(compactRunningElapsed(63_000)).toBe("1m 3s");
    expect(compactRunningElapsed(3_723_000)).toBe("1h 2m 3s");
  });

  it("distinguishes model startup, first-response waits, and a long wait", () => {
    const startedAt = "2026-07-31T16:00:00.000Z";
    const startedMs = Date.parse(startedAt);
    const input = {
      items: [status(2, { kind: "running", startedAt }), prompt(2)],
      turnModels: new Map([[2, "codex/gpt-5.6-sol"]]),
      turnStartedAt: new Map([[2, startedAt]]),
    };

    expect(presentation({ ...input, nowMs: startedMs + 1_000 })).toEqual({
      label: "Starting gpt-5.6-sol…",
      detail: "Preparing the model request.",
    });
    expect(presentation({ ...input, nowMs: startedMs + 42_000 })).toEqual({
      label: "Waiting for first response from gpt-5.6-sol · 42s",
      detail: "The turn is running, but no model output has arrived yet.",
    });
    expect(presentation({ ...input, nowMs: startedMs + 188_000 })).toEqual({
      label: "Still waiting for gpt-5.6-sol · 3m 8s",
      detail: "No model output has arrived yet. You can keep waiting or cancel and retry.",
    });
  });

  it("describes each durable running phase", () => {
    const marker = status(3, { kind: "running" });
    const model = new Map([[3, "openai/o3"]]);
    expect(presentation({ items: [marker], compacting: true })).toEqual({
      label: "Compacting context…",
      detail: "Preparing a shorter conversation history before contacting the model.",
    });
    expect(presentation({
      items: [marker, {
        id: "questions",
        kind: "questions",
        requestId: "request",
        title: undefined,
        questions: [],
        answers: undefined,
      }],
    })).toEqual({
      label: "Waiting for your answer…",
      detail: "The agent will continue after you answer or skip its questions.",
    });
    expect(presentation({
      items: [marker, tool("shell", {}, "awaiting-approval")],
    })).toEqual({
      label: "Waiting for approval…",
      detail: "The agent will continue after the pending tool request is resolved.",
    });
    expect(presentation({
      items: [status(3, { kind: "waiting-for-capacity" })],
    })).toEqual({
      label: "Waiting for model capacity…",
      detail: "",
    });
    expect(presentation({ items: [marker], thinking: true, turnModels: model })).toEqual({
      label: "Thinking…",
      detail: "o3 is streaming its reasoning.",
    });
    expect(presentation({
      items: [marker, tool("read_file", {}, "running")],
    })).toEqual({ label: "Reading files…", detail: "" });
  });

  it("ignores stale activity from earlier turns and recognizes response gaps", () => {
    const items: ThreadChatItem[] = [
      tool("WebSearch", {}, "running"),
      status(4, { kind: "running" }),
      prompt(4),
    ];
    expect(presentation({ items })).toEqual({
      label: "Starting model…",
      detail: "Preparing the model request.",
    });
    items.push({
      id: "answer",
      kind: "assistant",
      turn: 4,
      content: "Working on it.",
      complete: true,
    });
    expect(presentation({ items, turnModels: new Map([[4, "codex/gpt-5"]]) })).toEqual({
      label: "Waiting for gpt-5…",
      detail: "The model is between visible response or tool events.",
    });
  });

  it("does not present activity for an idle turn", () => {
    expect(presentation({ turnRunning: false })).toBeUndefined();
  });
});

describe("running tool activity labels", () => {
  it.each([
    ["Edit", {}, "Editing files…"],
    ["mcp__trouve__search", {}, "Searching through code…"],
    ["WebSearch", {}, "Searching the web…"],
    ["execute", { title: "Web Search" }, "Searching the web…"],
    ["commandExecution", {}, "Running commands…"],
    ["read_file", {}, "Reading files…"],
    ["web_fetch", {}, "Fetching web content…"],
    ["todo_write", {}, "Updating the plan…"],
    ["Task", {}, "Delegating work…"],
    [
      "mcpToolCall",
      { serverName: "github", toolName: "list_issues" },
      "Using github…",
    ],
  ])("maps %s to its current work", (name, args, expected) => {
    expect(runningToolActivityLabel(name, args)).toBe(expected);
  });
});
