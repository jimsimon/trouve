export interface TextRange {
  readonly from: number;
  readonly to: number;
}

/** Convert a one-based inclusive line range into CodeMirror UTF-16 offsets.
 * Zero/invalid ranges deliberately reveal the start without selecting text. */
export const lineRangeOffsets = (
  content: string,
  requestedFrom: number,
  requestedTo = requestedFrom,
): TextRange => {
  if (!Number.isSafeInteger(requestedFrom) || requestedFrom <= 0) {
    return Object.freeze({ from: 0, to: 0 });
  }
  const lines = content.split("\n");
  const fromLine = Math.min(requestedFrom, Math.max(1, lines.length));
  const toLine = Number.isSafeInteger(requestedTo) && requestedTo >= fromLine
    ? Math.min(requestedTo, lines.length)
    : fromLine;
  let from = 0;
  for (let index = 1; index < fromLine; index += 1) {
    from += (lines[index - 1]?.length ?? 0) + 1;
  }
  let to = from;
  for (let index = fromLine; index <= toLine; index += 1) {
    to += lines[index - 1]?.length ?? 0;
    if (index < toLine) to += 1;
  }
  return Object.freeze({ from, to });
};

export const parentDirectories = (path: string): readonly string[] => {
  const normalized = path.replace(/^\.\//u, "").replace(/\/+$/u, "");
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length <= 1) return Object.freeze([]);
  return Object.freeze(parts.slice(0, -1).map((_, index) => parts.slice(0, index + 1).join("/")));
};
