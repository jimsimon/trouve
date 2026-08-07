import type { DiffLineNumber } from "./diff-line-numbers.js";

export interface ParsedDiffFile {
  readonly path: string;
  readonly raw: string;
  readonly original: string;
  readonly modified: string;
  readonly originalLineNumbers: readonly DiffLineNumber[];
  readonly modifiedLineNumbers: readonly DiffLineNumber[];
  readonly additions: number;
  readonly deletions: number;
  readonly binary: boolean;
  readonly rows: readonly ParsedUnifiedDiffRow[];
}

export type ParsedUnifiedDiffRowKind = "hunk" | "context" | "add" | "delete";

/** One non-file-header row in the same continuous unified presentation used
 * by the retained Slint DiffView. File headers are rendered from
 * ParsedDiffFile so collapse and per-file copy remain separate controls. */
export interface ParsedUnifiedDiffRow {
  readonly kind: ParsedUnifiedDiffRowKind;
  readonly oldNumber: number | null;
  readonly newNumber: number | null;
  readonly text: string;
}

/** Keep the user's file selection stable while a live diff is reparsed. */
export const selectedDiffIndexAfterRefresh = (
  currentFiles: readonly Pick<ParsedDiffFile, "path">[],
  currentIndex: number,
  nextFiles: readonly Pick<ParsedDiffFile, "path">[],
): number => {
  if (nextFiles.length === 0) return 0;
  const selectedPath = currentFiles[currentIndex]?.path;
  if (selectedPath !== undefined) {
    const matchingIndex = nextFiles.findIndex((file) => file.path === selectedPath);
    if (matchingIndex >= 0) return matchingIndex;
  }
  return Math.min(Math.max(currentIndex, 0), nextFiles.length - 1);
};

interface MutableDiffFile {
  path: string;
  raw: string;
  original: string[];
  modified: string[];
  originalLineNumbers: DiffLineNumber[];
  modifiedLineNumbers: DiffLineNumber[];
  additions: number;
  deletions: number;
  binary: boolean;
  rows: ParsedUnifiedDiffRow[];
  hunks: number;
  inHunk: boolean;
  oldLine: number;
  newLine: number;
}

const pathFromGitHeader = (line: string): string => {
  const marker = " b/";
  const index = line.lastIndexOf(marker);
  return index < 0 ? "Changed file" : line.slice(index + marker.length).replace(/^"|"$/g, "");
};

/** Parse bounded, display-oriented before/after excerpts from git unified
 * diff text. It never applies patches or resolves paths; the server remains
 * authoritative for file content and effects. */
export const parseUnifiedDiff = (diff: string): readonly ParsedDiffFile[] => {
  const fileStarts = [...diff.matchAll(/^diff --git /gm)].map(
    (match) => match.index,
  );
  const rawFiles = fileStarts.map((start, index) =>
    diff.slice(start, fileStarts[index + 1] ?? diff.length),
  );
  const files: MutableDiffFile[] = [];
  let current: MutableDiffFile | undefined;
  for (const line of diff.split("\n")) {
    if (line.startsWith("diff --git ")) {
      current = {
        path: pathFromGitHeader(line),
        raw: rawFiles[files.length] ?? diff,
        original: [],
        modified: [],
        originalLineNumbers: [],
        modifiedLineNumbers: [],
        additions: 0,
        deletions: 0,
        binary: false,
        rows: [],
        hunks: 0,
        inHunk: false,
        oldLine: 0,
        newLine: 0,
      };
      files.push(current);
      continue;
    }
    if (current === undefined && line.startsWith("@@")) {
      current = {
        path: "Changed file",
        raw: diff,
        original: [],
        modified: [],
        originalLineNumbers: [],
        modifiedLineNumbers: [],
        additions: 0,
        deletions: 0,
        binary: false,
        rows: [],
        hunks: 0,
        inHunk: false,
        oldLine: 0,
        newLine: 0,
      };
      files.push(current);
    }
    if (current === undefined) continue;
    if (!current.inHunk && line.startsWith("+++ ") && line !== "+++ /dev/null") {
      current.path = line.slice(4).replace(/^b\//, "").replace(/^"|"$/g, "");
      continue;
    }
    if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) {
      current.binary = true;
      current.rows.push({
        kind: "context",
        oldNumber: null,
        newNumber: null,
        text: "Binary file changed",
      });
      continue;
    }
    if (line.startsWith("@@")) {
      if (current.hunks > 0) {
        current.original.push("⋯");
        current.modified.push("⋯");
        current.originalLineNumbers.push(null);
        current.modifiedLineNumbers.push(null);
      }
      const header = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
      current.oldLine = Number(header?.[1] ?? 0);
      current.newLine = Number(header?.[2] ?? 0);
      current.hunks += 1;
      current.inHunk = true;
      current.rows.push({
        kind: "hunk",
        oldNumber: null,
        newNumber: null,
        text: line,
      });
      continue;
    }
    if (!current.inHunk || line === "\\ No newline at end of file") continue;
    if (line.startsWith("-")) {
      const oldNumber = current.oldLine;
      current.original.push(line.slice(1));
      current.originalLineNumbers.push(oldNumber);
      current.rows.push({
        kind: "delete",
        oldNumber,
        newNumber: null,
        text: line.slice(1),
      });
      current.oldLine += 1;
      current.deletions += 1;
    } else if (line.startsWith("+")) {
      const newNumber = current.newLine;
      current.modified.push(line.slice(1));
      current.modifiedLineNumbers.push(newNumber);
      current.rows.push({
        kind: "add",
        oldNumber: null,
        newNumber,
        text: line.slice(1),
      });
      current.newLine += 1;
      current.additions += 1;
    } else if (line.startsWith(" ")) {
      const oldNumber = current.oldLine;
      const newNumber = current.newLine;
      current.original.push(line.slice(1));
      current.modified.push(line.slice(1));
      current.originalLineNumbers.push(oldNumber);
      current.modifiedLineNumbers.push(newNumber);
      current.rows.push({
        kind: "context",
        oldNumber,
        newNumber,
        text: line.slice(1),
      });
      current.oldLine += 1;
      current.newLine += 1;
    }
  }
  return files.map((file) => ({
    path: file.path,
    raw: file.raw,
    original: file.binary ? "[Binary file changed]" : file.original.join("\n"),
    modified: file.binary ? "[Binary file changed]" : file.modified.join("\n"),
    originalLineNumbers: file.binary ? [null] : file.originalLineNumbers,
    modifiedLineNumbers: file.binary ? [null] : file.modifiedLineNumbers,
    additions: file.additions,
    deletions: file.deletions,
    binary: file.binary,
    rows: file.rows,
  }));
};
