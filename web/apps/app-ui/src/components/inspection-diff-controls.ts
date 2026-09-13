export type RawDiffCopyResult = "copied" | "unavailable" | "failed";

export interface ClipboardTextWriter {
  writeText(text: string): Promise<void>;
}

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
