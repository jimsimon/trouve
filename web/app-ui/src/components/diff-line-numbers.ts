export type DiffLineNumber = number | null;

/** Map CodeMirror's one-based display row to the source line represented by
 * the reconstructed diff excerpt. Synthetic gap rows intentionally stay
 * blank instead of pretending to be source lines. */
export const formatMappedLineNumber = (
  displayLineNumber: number,
  lineNumbers: readonly DiffLineNumber[],
): string => {
  const sourceLineNumber = lineNumbers[displayLineNumber - 1];
  return sourceLineNumber === null || sourceLineNumber === undefined
    ? ""
    : String(sourceLineNumber);
};

const describedRange = (
  label: string,
  lineNumbers: readonly DiffLineNumber[],
): string => {
  let first: number | undefined;
  let last: number | undefined;
  for (const lineNumber of lineNumbers) {
    if (lineNumber === null) continue;
    first = first === undefined ? lineNumber : Math.min(first, lineNumber);
    last = last === undefined ? lineNumber : Math.max(last, lineNumber);
  }
  if (first === undefined || last === undefined) {
    return `${label} line positions unavailable`;
  }
  return first === last
    ? `${label} excerpt line ${first}`
    : `${label} excerpt lines ${first}–${last}`;
};

/** Concise screen-reader context for excerpts whose visual gutters may skip
 * between disjoint hunks. */
export const diffLineRangeDescription = (
  originalLineNumbers: readonly DiffLineNumber[],
  modifiedLineNumbers: readonly DiffLineNumber[],
): string =>
  `${describedRange("Original", originalLineNumbers)}; ${describedRange(
    "modified",
    modifiedLineNumbers,
  )}.`;
