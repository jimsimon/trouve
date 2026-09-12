import { describe, expect, it } from "vitest";

import {
  isLiveTaskStatus,
  RETAINED_TOOL_OUTPUT_CALL_ID,
  RetainedTranscriptClient,
  retainedTaskSnapshot,
  retainedThreadId,
  type RetainedTask,
} from "./review-transcript";

const task = (overrides: Partial<RetainedTask> = {}): RetainedTask => ({
  id: "task-1",
  status: "succeeded",
  model: "openai/gpt-5",
  prompt: "Review this diff.",
  output: "# Findings\n\nNone.",
  thinking: "Looking at the diff…",
  tool_output: "$ git diff\n+1 -0",
  created_at: "2026-09-08T10:00:00Z",
  started_at: "2026-09-08T10:00:05Z",
  elapsed_ms: 42_000,
  input_tokens: 1200,
  cached_input_tokens: 300,
  output_tokens: 80,
  ...overrides,
});

describe("retainedTaskSnapshot", () => {
  it("folds the retained columns into one completed turn in transcript order", () => {
    const snapshot = retainedTaskSnapshot(task());
    expect(snapshot.items.map((item) => item.kind)).toEqual([
      "user",
      "thinking",
      "tool_call",
      "assistant",
      "turn_status",
    ]);
    expect(snapshot.items[0]).toMatchObject({ kind: "user", turn: 1, content: "Review this diff.", attachments: [] });
    expect(snapshot.items[1]).toMatchObject({ kind: "thinking", complete: true, content: "Looking at the diff…" });
    expect(snapshot.items[2]).toMatchObject({
      kind: "tool_call",
      call_id: RETAINED_TOOL_OUTPUT_CALL_ID,
      tool: "tool_output",
      status: "ok",
      result: "$ git diff\n+1 -0",
    });
    expect(snapshot.items[3]).toMatchObject({ kind: "assistant", complete: true, content: "# Findings\n\nNone." });
    expect(snapshot.items[4]).toMatchObject({
      kind: "turn_status",
      state: { state: "completed", usage: { input_tokens: 1200, output_tokens: 80, cached_input_tokens: 300 } },
    });
    expect(snapshot.turn_running).toBe(false);
    expect(snapshot.turn_models).toEqual({ "1": "openai/gpt-5" });
    expect(snapshot.turn_started_at).toEqual({ "1": "2026-09-08T10:00:05Z" });
    expect(snapshot.turn_duration_ms).toEqual({ "1": 42_000 });
  });

  it("omits empty columns and reports failures through the turn status", () => {
    const snapshot = retainedTaskSnapshot(
      task({ status: "failed", thinking: undefined, tool_output: undefined, output: "", error: "timed out" }),
    );
    expect(snapshot.items.map((item) => item.kind)).toEqual(["user", "assistant", "turn_status"]);
    expect(snapshot.items.at(-1)).toMatchObject({ state: { state: "failed", error: "timed out" } });
  });

  it("keeps a running task's turn open without a duration", () => {
    const snapshot = retainedTaskSnapshot(task({ status: "running", output: "partial" }));
    expect(snapshot.turn_running).toBe(true);
    expect(snapshot.turn_duration_ms).toBeUndefined();
    expect(snapshot.items.find((item) => item.kind === "assistant")).toMatchObject({ complete: false });
    expect(snapshot.items.at(-1)).toMatchObject({ state: { state: "running" } });
    expect(retainedTaskSnapshot(task({ status: "queued", output: undefined })).items.at(-1)).toMatchObject({
      state: { state: "waiting_for_capacity" },
    });
  });
});

describe("RetainedTranscriptClient", () => {
  it("serves the snapshot once and opens an idle stream", async () => {
    const client = new RetainedTranscriptClient(task());
    const view = await client.threadView();
    expect(view.cursor).toBe(0);
    expect(view.value.items).toHaveLength(5);
    let opened = 0;
    const stream = await client.threadEvents(retainedThreadId("task-1"), { onOpen: () => { opened += 1; } });
    expect(opened).toBe(1);
    expect(() => { stream.start(); stream.close(); }).not.toThrow();
    await expect(client.threadToolDetails()).rejects.toThrow(/deferred tool details/u);
  });
});

describe("isLiveTaskStatus", () => {
  it("treats only queued and running tasks as having a live thread", () => {
    expect(isLiveTaskStatus("running")).toBe(true);
    expect(isLiveTaskStatus("queued")).toBe(true);
    for (const status of ["succeeded", "failed", "cancelled", "stale"]) {
      expect(isLiveTaskStatus(status)).toBe(false);
    }
  });
});
