import { describe, expect, it } from "vitest";

import type { ProtocolEventEnvelope } from "../services/protocol-client.js";
import { ThreadViewModel } from "../state/thread-view-model.js";
import {
  droppedQueueIds,
  effectiveQueueDropPlacement,
  queueControlState,
  queueFocusAfterDelete,
  queuePreview,
  reorderedQueueIds,
  shouldMaterializeAcceptedQueuedPrompt,
} from "./queue-controls.js";

const queue = [{ id: "one" }, { id: "two" }, { id: "three" }];

describe("queue controls", () => {
  it("does not resurrect a removed response id or confuse an identical old prompt", () => {
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      undefined,
      "queued-1",
      [],
    )).toBe(false);
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      "queued-1",
      [{ id: "queued-1" }],
    )).toBe(false);
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      "queued-1",
      [],
      true,
    )).toBe(false);
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      "queued-new",
      [{ id: "identical-old-prompt" }],
    )).toBe(true);
  });

  it("rejects delayed HTTP materialization after SSE add, dispatch, and user message", () => {
    const view = new ThreadViewModel();
    const queueRevision = view.trackQueueRevision();
    const prompt = {
      id: "queued-race",
      thread_id: "th_1",
      position: 1,
      content: "Run this next",
      created_at: "2026-08-01T12:00:00Z",
      attachments: [],
    };
    const event = (
      cursor: number,
      value: Record<string, unknown>,
    ): ProtocolEventEnvelope => ({
      cursor,
      scope: { thread: "th_1" },
      ts: `2026-08-01T12:00:0${cursor}Z`,
      ...value,
    }) as ProtocolEventEnvelope;

    view.apply(event(1, { type: "thread.queue_updated", prompts: [prompt] }));
    view.apply(event(2, { type: "thread.queue_updated", prompts: [] }));

    // The delayed sendMessage response arrives after the durable row was
    // observed and dispatched. Its in-flight tracker prevents a ghost append.
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      prompt.id,
      view.queue,
      queueRevision.queueChanged(),
    )).toBe(false);

    view.apply(event(3, {
      type: "user.message",
      turn: 4,
      content: prompt.content,
      attachments: [],
    }));
    expect(view.queue).toEqual([]);
    queueRevision.close();
  });

  it("rejects delayed materialization after prolonged queue churn in constant space", () => {
    const view = new ThreadViewModel();
    const queueRevision = view.trackQueueRevision();
    const prompt = (id: string, position: number) => ({
      id,
      thread_id: "th_1",
      position,
      content: id,
      created_at: "2026-08-01T12:00:00Z",
      attachments: [],
    });

    view.replaceQueue([prompt("target", 1)]);
    view.replaceQueue([]);
    for (let index = 0; index < 300; index += 1) {
      view.replaceQueue([prompt(`churn-${index}`, index + 2)]);
      view.replaceQueue([]);
    }

    expect(queueRevision.queueChanged()).toBe(true);
    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      "target",
      view.queue,
      queueRevision.queueChanged(),
    )).toBe(false);

    queueRevision.close();
    queueRevision.close();
    expect(queueRevision.queueChanged()).toBe(false);
  });

  it("materializes a legitimate response when its queue projection has not changed", () => {
    const view = new ThreadViewModel();
    const queueRevision = view.trackQueueRevision();

    expect(shouldMaterializeAcceptedQueuedPrompt(
      "submission-1",
      "submission-1",
      "queued-new",
      view.queue,
      queueRevision.queueChanged(),
    )).toBe(true);
    queueRevision.close();
  });

  it("separates queue mutations from idle-only dispatch", () => {
    expect(queueControlState({
      threadAvailable: true,
      queueLength: 3,
      turnRunning: true,
      busy: false,
      connectivityBlocked: false,
    })).toEqual({
      mutationsDisabled: false,
      dispatchDisabled: true,
      sendNowDisabled: false,
    });
    expect(queueControlState({
      threadAvailable: true,
      queueLength: 3,
      turnRunning: false,
      busy: false,
      connectivityBlocked: false,
    })).toEqual({
      mutationsDisabled: false,
      dispatchDisabled: false,
      sendNowDisabled: false,
    });
    expect(queueControlState({
      threadAvailable: true,
      queueLength: 3,
      turnRunning: false,
      busy: true,
      connectivityBlocked: false,
    })).toEqual({
      mutationsDisabled: true,
      dispatchDisabled: true,
      sendNowDisabled: true,
    });
    expect(queueControlState({
      threadAvailable: true,
      queueLength: 3,
      turnRunning: false,
      busy: false,
      connectivityBlocked: true,
    })).toEqual({
      mutationsDisabled: true,
      dispatchDisabled: true,
      sendNowDisabled: true,
    });
  });

  it("builds complete reorder orders without mutating the input", () => {
    expect(reorderedQueueIds(queue, 1, -1)).toEqual(["two", "one", "three"]);
    expect(reorderedQueueIds(queue, 1, 1)).toEqual(["one", "three", "two"]);
    expect(reorderedQueueIds(queue, 0, -1)).toBeUndefined();
    expect(queue.map(({ id }) => id)).toEqual(["one", "two", "three"]);
  });

  it("builds remove-and-insert orders for pointer drag and drop", () => {
    expect(droppedQueueIds(queue, "one", "three", "after")).toEqual([
      "two",
      "three",
      "one",
    ]);
    expect(droppedQueueIds(queue, "three", "one", "before")).toEqual([
      "three",
      "one",
      "two",
    ]);
    expect(droppedQueueIds(queue, "one", "two", "before")).toBeUndefined();
    expect(droppedQueueIds(queue, "missing", "two", "before")).toBeUndefined();
    expect(droppedQueueIds(queue, "two", "two", "after")).toBeUndefined();
    expect(queue.map(({ id }) => id)).toEqual(["one", "two", "three"]);
  });

  it("turns an adjacent no-op edge into a meaningful row drop", () => {
    expect(effectiveQueueDropPlacement(queue, "two", "one", "after")).toBe("before");
    expect(effectiveQueueDropPlacement(queue, "two", "three", "before")).toBe("after");
    expect(effectiveQueueDropPlacement(queue, "one", "three", "before")).toBe("before");
    expect(effectiveQueueDropPlacement(queue, "missing", "one", "before"))
      .toBeUndefined();
  });

  it("recovers focus to the next row, previous row, or composer", () => {
    expect(queueFocusAfterDelete(queue, "two")).toEqual({
      kind: "prompt",
      promptId: "three",
    });
    expect(queueFocusAfterDelete(queue, "three")).toEqual({
      kind: "prompt",
      promptId: "two",
    });
    expect(queueFocusAfterDelete([{ id: "one" }], "one")).toEqual({ kind: "composer" });
  });

  it("uses only the first meaningful line for compact queue rows", () => {
    expect(queuePreview("\n  First line  \nsecond line")).toBe("First line");
    expect(queuePreview("  \n\t")).toBe("");
  });
});
