export type RawDiffCopyResult = "copied" | "unavailable" | "failed";

export interface ClipboardTextWriter {
  writeText(text: string): Promise<void>;
}

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
