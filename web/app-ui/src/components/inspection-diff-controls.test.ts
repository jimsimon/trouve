import { describe, expect, it, vi } from "vitest";

import {
  checkpointAvailabilityDescription,
  checkpointHintsAfterRestore,
  copyRawDiffToClipboard,
  initialCheckpointHints,
} from "./inspection-diff-controls.js";

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
