import { describe, expect, it } from "vitest";

import {
  droppedQueueIds,
  effectiveQueueDropPlacement,
  queueControlState,
  queueFocusAfterDelete,
  queuePreview,
  reorderedQueueIds,
} from "./queue-controls.js";

const queue = [{ id: "one" }, { id: "two" }, { id: "three" }];

describe("queue controls", () => {
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
