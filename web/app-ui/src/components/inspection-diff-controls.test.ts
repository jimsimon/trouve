import { describe, expect, it, vi } from "vitest";

import {
  checkpointAvailabilityDescription,
  checkpointHintsAfterRestore,
  changedFileIndexForKey,
  copyRawDiffToClipboard,
  diffFileActionForKey,
  initialCheckpointHints,
} from "./inspection-diff-controls.js";

describe("changedFileIndexForKey", () => {
  it("moves with arrows and jumps with Home and End", () => {
    expect(changedFileIndexForKey("ArrowUp", 2, 4)).toBe(1);
    expect(changedFileIndexForKey("ArrowDown", 2, 4)).toBe(3);
    expect(changedFileIndexForKey("Home", 2, 4)).toBe(0);
    expect(changedFileIndexForKey("End", 1, 4)).toBe(3);
  });

  it("clamps at list boundaries and ignores unrelated keys", () => {
    expect(changedFileIndexForKey("ArrowUp", 0, 3)).toBe(0);
    expect(changedFileIndexForKey("ArrowDown", 2, 3)).toBe(2);
    expect(changedFileIndexForKey("ArrowDown", -4, 3)).toBe(1);
    expect(changedFileIndexForKey("Enter", 1, 3)).toBeUndefined();
    expect(changedFileIndexForKey("Home", 0, 0)).toBeUndefined();
  });
});

describe("diffFileActionForKey", () => {
  it("combines roving selection with disclosure keyboard controls", () => {
    expect(diffFileActionForKey("ArrowDown", 0, 3, true)).toEqual({
      kind: "select",
      index: 1,
    });
    expect(diffFileActionForKey("ArrowLeft", 1, 3, true)).toEqual({
      kind: "collapse",
    });
    expect(diffFileActionForKey("ArrowRight", 1, 3, false)).toEqual({
      kind: "expand",
    });
    expect(diffFileActionForKey("Enter", 1, 3, true)).toEqual({ kind: "toggle" });
    expect(diffFileActionForKey(" ", 1, 3, false)).toEqual({ kind: "toggle" });
  });

  it("leaves already-satisfied disclosure keys and unrelated keys alone", () => {
    expect(diffFileActionForKey("ArrowLeft", 1, 3, false)).toBeUndefined();
    expect(diffFileActionForKey("ArrowRight", 1, 3, true)).toBeUndefined();
    expect(diffFileActionForKey("Tab", 1, 3, true)).toBeUndefined();
  });
});

describe("checkpoint availability hints", () => {
  it("only confirms the inverse direction after a successful restore", () => {
    const initial = initialCheckpointHints();
    expect(checkpointHintsAfterRestore(initial, "undo")).toEqual({
      undo: "unknown",
      redo: "available",
    });
    expect(checkpointHintsAfterRestore(initial, "redo")).toEqual({
      undo: "available",
      redo: "unknown",
    });
  });

  it("describes unknown state without claiming a checkpoint boundary", () => {
    expect(checkpointAvailabilityDescription("undo", "unknown")).toBe(
      "Undo availability will be checked when used.",
    );
    expect(checkpointAvailabilityDescription("redo", "available")).toContain(
      "confirmed",
    );
  });
});

describe("copyRawDiffToClipboard", () => {
  it("starts the Clipboard API write synchronously and reports success", async () => {
    let finish: (() => void) | undefined;
    const pendingWrite = new Promise<void>((resolve) => {
      finish = resolve;
    });
    const writeText = vi.fn(() => pendingWrite);

    const result = copyRawDiffToClipboard("diff --git a/a b/a\n", { writeText });

    expect(writeText).toHaveBeenCalledWith("diff --git a/a b/a\n");
    finish?.();
    await expect(result).resolves.toBe("copied");
  });

  it("distinguishes unavailable access and contains write failures", async () => {
    await expect(copyRawDiffToClipboard("patch", undefined)).resolves.toBe("unavailable");
    await expect(copyRawDiffToClipboard("patch", {
      writeText: async () => {
        throw new Error("sensitive platform detail");
      },
    })).resolves.toBe("failed");
  });
});
