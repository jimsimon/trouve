export type RawDiffCopyResult = "copied" | "unavailable" | "failed";

export interface ClipboardTextWriter {
  writeText(text: string): Promise<void>;
}

export type DiffFileAction =
  | { readonly kind: "select"; readonly index: number }
  | { readonly kind: "expand" }
  | { readonly kind: "collapse" }
  | { readonly kind: "toggle" };

export type CheckpointAvailability = "unknown" | "available";

export interface CheckpointHints {
  readonly undo: CheckpointAvailability;
  readonly redo: CheckpointAvailability;
}

export const initialCheckpointHints = (): CheckpointHints => ({
  undo: "unknown",
  redo: "unknown",
});

/** A successful restore only establishes the inverse operation. The protocol
 * does not expose authoritative boundary state, so unknown directions remain
 * enabled and are checked when invoked. */
export const checkpointHintsAfterRestore = (
  hints: CheckpointHints,
  direction: "undo" | "redo",
): CheckpointHints => direction === "undo"
  ? { ...hints, redo: "available" }
  : { ...hints, undo: "available" };

export const checkpointAvailabilityDescription = (
  direction: "undo" | "redo",
  availability: CheckpointAvailability,
): string => {
  const label = direction === "undo" ? "Undo" : "Redo";
  return availability === "available"
    ? `${label} availability was confirmed by the last successful restore.`
    : `${label} availability will be checked when used.`;
};

/**
 * Copy from the click call stack without a legacy DOM fallback. Calling
 * `writeText` before the first await preserves the browser's transient user
 * activation; failures stay data so the component can announce generic text.
 */
export const copyRawDiffToClipboard = async (
  rawDiff: string,
  clipboard: ClipboardTextWriter | undefined,
): Promise<RawDiffCopyResult> => {
  if (clipboard === undefined || typeof clipboard.writeText !== "function") {
    return "unavailable";
  }
  try {
    const write = clipboard.writeText(rawDiff);
    await write;
    return "copied";
  } catch {
    return "failed";
  }
};

/** Return the selected/focusable changed-file index for listbox navigation. */
export const changedFileIndexForKey = (
  key: string,
  currentIndex: number,
  fileCount: number,
): number | undefined => {
  if (fileCount <= 0) return undefined;
  const current = Math.min(Math.max(currentIndex, 0), fileCount - 1);
  if (key === "ArrowUp") return Math.max(0, current - 1);
  if (key === "ArrowDown") return Math.min(fileCount - 1, current + 1);
  if (key === "Home") return 0;
  if (key === "End") return fileCount - 1;
  return undefined;
};

/** Keyboard behavior for the expandable changed-file headers. */
export const diffFileActionForKey = (
  key: string,
  currentIndex: number,
  fileCount: number,
  expanded: boolean,
): DiffFileAction | undefined => {
  const nextIndex = changedFileIndexForKey(key, currentIndex, fileCount);
  if (nextIndex !== undefined) return { kind: "select", index: nextIndex };
  if (key === "ArrowLeft" && expanded) return { kind: "collapse" };
  if (key === "ArrowRight" && !expanded) return { kind: "expand" };
  if (key === "Enter" || key === " " || key === "Spacebar") {
    return { kind: "toggle" };
  }
  return undefined;
};
