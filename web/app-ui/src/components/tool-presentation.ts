import { utf8Length, utf8Prefix } from "../services/utf8-text.js";
import type { FontAwesomeIconName } from "./font-awesome-icon.js";
import { todoStatusIcon } from "./todo-plan-model.js";

export const TRANSCRIPT_TRUNCATION_NOTICE = "Additional transcript results were omitted.";

export type ToolDiffLineKind = "separator" | "context" | "add" | "delete";

export interface ToolDiffLine {
  readonly kind: ToolDiffLineKind;
  readonly oldNumber: number;
  readonly newNumber: number;
  readonly text: string;
}

export interface ToolTodoRow {
  readonly status: string;
  readonly icon: FontAwesomeIconName;
  readonly content: string;
}

export interface ToolPresentation {
  readonly title: string;
  readonly subject: string;
  readonly filePath: string;
  readonly lineFrom: number;
  readonly lineTo: number;
  readonly meta: string;
  readonly additions: number;
  readonly deletions: number;
  readonly diff: readonly ToolDiffLine[];
  readonly todos: readonly ToolTodoRow[];
}

export interface ToolDetailField {
  readonly label: string;
  readonly value: string;
  readonly code?: boolean;
}

export interface ToolSourceDetail {
  readonly path: string;
  readonly content: string;
  readonly startLine: number;
  readonly totalLines?: number;
  readonly truncated: boolean;
}

export interface ToolSearchResultDetail {
  readonly path: string;
  readonly startLine: number;
  readonly endLine: number;
  readonly score?: number;
  readonly content: string;
}

export interface ToolMatchDetail {
  readonly path: string;
  readonly line: number;
  readonly text: string;
}

export interface ToolTranscriptMatchDetail {
  readonly threadId: string;
  readonly turn: number;
  readonly role: string;
  readonly timestamp: string;
  readonly snippet: string;
}

export type ToolDetailPresentation =
  | {
      readonly kind: "source";
      readonly inputs: readonly ToolDetailField[];
      readonly source: ToolSourceDetail;
    }
  | {
      readonly kind: "search";
      readonly inputs: readonly ToolDetailField[];
      readonly results: readonly ToolSearchResultDetail[];
      readonly truncated: boolean;
    }
  | {
      readonly kind: "matches";
      readonly inputs: readonly ToolDetailField[];
      readonly matches: readonly ToolMatchDetail[];
      readonly truncated: boolean;
    }
  | {
      readonly kind: "paths";
      readonly inputs: readonly ToolDetailField[];
      readonly paths: readonly string[];
      readonly truncated: boolean;
    }
  | {
      readonly kind: "command";
      readonly inputs: readonly ToolDetailField[];
      readonly stdout: string;
      readonly stderr: string;
      readonly truncated: boolean;
    }
  | {
      readonly kind: "document";
      readonly inputs: readonly ToolDetailField[];
      readonly content: string;
      readonly language: string;
      readonly truncated: boolean;
    }
  | {
      readonly kind: "diff";
      readonly inputs: readonly ToolDetailField[];
      readonly diff: string;
      readonly truncated: boolean;
      readonly nextOffset?: number;
      readonly totalBytes?: number;
    }
  | {
      readonly kind: "transcript";
      readonly inputs: readonly ToolDetailField[];
      readonly matches: readonly ToolTranscriptMatchDetail[];
      readonly messages: readonly ToolDetailField[];
      readonly truncated: boolean;
    }
  | {
      readonly kind: "structured";
      readonly inputs: readonly ToolDetailField[];
      readonly resultText: string;
      readonly error: boolean;
    };

/** Compact execution duration shown beside the tool title. A positive
 * provider duration is authoritative; zero is commonly a provider
 * placeholder, so the server's executor-only measurement (or its compatible
 * durable-timestamp fallback) supplies the value. The status glyph
 * communicates success or failure without repeating an exit code. */
export const toolExecutionMetadata = (
  resultValue: unknown,
  measuredDurationMs?: number,
): string => {
  const result = record(resultValue);
  const metadata = record(result?.metadata);
  const firstNumber = (keys: readonly string[]): number | undefined => {
    for (const source of [result, metadata]) {
      if (source === undefined) continue;
      for (const key of keys) {
        const value = numberValue(source[key]);
        if (value !== undefined) return value;
      }
    }
    return undefined;
  };
  const reportedDurationMs = firstNumber([
    "duration_ms",
    "durationMs",
    "elapsed_ms",
    "elapsedMs",
  ]);
  const validMeasuredDurationMs = measuredDurationMs !== undefined
    && Number.isFinite(measuredDurationMs)
    && measuredDurationMs >= 0
    ? measuredDurationMs
    : undefined;
  const durationMs = reportedDurationMs !== undefined && reportedDurationMs > 0
    ? reportedDurationMs
    : validMeasuredDurationMs;
  const parts: string[] = [];
  if (durationMs !== undefined && Number.isFinite(durationMs)) {
    const milliseconds = Math.max(0, Math.floor(durationMs));
    if (milliseconds === 0) {
      parts.push("<1ms");
    } else if (milliseconds < 1_000) {
      parts.push(`${milliseconds}ms`);
    } else {
      const seconds = Math.floor(milliseconds / 1_000);
      if (seconds < 60) {
        parts.push(`${seconds}s`);
      } else {
        const minutes = Math.floor(seconds / 60);
        parts.push(`${minutes}m ${(seconds % 60).toString().padStart(2, "0")}s`);
      }
    }
  }
  return parts.join(" · ");
};

type JsonRecord = Readonly<Record<string, unknown>>;

const record = (value: unknown): JsonRecord | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as JsonRecord
    : undefined;

const stringValue = (value: unknown): string | undefined =>
  typeof value === "string" ? value : undefined;

const numberValue = (value: unknown): number | undefined =>
  typeof value === "number" && Number.isFinite(value) ? Math.trunc(value) : undefined;

const firstString = (source: JsonRecord, keys: readonly string[]): string | undefined => {
  for (const key of keys) {
    const value = stringValue(source[key]);
    if (value !== undefined) return value;
  }
  return undefined;
};

const baseToolName = (tool: string): string => tool.split("__").at(-1) ?? tool;

const normalizedToolIdentifier = (tool: string): string =>
  tool.replaceAll(/[^a-z0-9]/giu, "").toLowerCase();

const isGenericToolWrapper = (tool: string): boolean => {
  const normalized = normalizedToolIdentifier(tool);
  return normalized === "mcptoolcall" || normalized === "dynamictoolcall";
};

/** Resolve provider wrapper calls without dropping their MCP server identity.
 * A qualified identifier is the common input for labels, rich presentation,
 * suppression, and activity classification. */
export const effectiveToolCall = (
  tool: string,
  argsValue: unknown,
): { readonly tool: string; readonly args: JsonRecord } => {
  const args = record(argsValue) ?? {};
  if (!isGenericToolWrapper(tool)) {
    return { tool, args };
  }
  const nestedTool = firstString(args, ["tool", "toolName", "name"]);
  const nestedArgs = record(args.arguments) ?? args;
  const server = firstString(args, ["server", "serverName", "mcpServer", "mcpServerName"])
    ?.trim();
  if (nestedTool === undefined) return { tool, args: nestedArgs };
  if (server === undefined || server === "") return { tool: nestedTool, args: nestedArgs };
  const nestedName = nestedTool.startsWith("mcp__")
    ? nestedTool.slice(5).split("__").slice(1).join("__")
    : nestedTool;
  return {
    tool: `mcp__${server}__${nestedName}`,
    args: nestedArgs,
  };
};

/** Whether a tool identifier belongs to the built-in/native catalog. MCP
 * basenames are not globally unique, so only the trouve MCP namespace may
 * opt into built-in presentation and suppression behavior. */
export const isFirstPartyToolCall = (tool: string, argsValue: unknown): boolean => {
  const effective = effectiveToolCall(tool, record(argsValue) ?? {});
  if (effective.tool.startsWith("mcp__")) {
    return effective.tool.startsWith("mcp__trouve__");
  }
  // An unqualified native identifier is first-party. An unqualified name
  // extracted from a generic provider wrapper is ambiguous and must not opt
  // into built-in rendering or suppression.
  return !isGenericToolWrapper(tool);
};

/** `spawn_output` is model-side collection/polling plumbing. The durable
 * subagent node and thread own its user-visible status and response, so a
 * second low-level tool row would duplicate the same work. */
export const isSpawnOutputToolCall = (tool: string, argsValue: unknown): boolean => {
  if (!isFirstPartyToolCall(tool, argsValue)) return false;
  const effective = effectiveToolCall(tool, record(argsValue) ?? {});
  const normalized = baseToolName(effective.tool)
    .replaceAll(/[^a-z0-9]/giu, "")
    .toLowerCase();
  return normalized === "spawnoutput";
};

/** Match the established tool naming contract across native and vendor
 * harness identifiers. */
export const toolDisplayName = (tool: string): string => {
  const mcp = tool.startsWith("mcp__") ? tool.slice(5).split("__", 2) : [];
  if (mcp.length === 2) {
    const [server, name] = mcp as [string, string];
    return server === "trouve"
      ? toolDisplayName(name)
      : `${server}: ${toolDisplayName(name)}`;
  }
  if (tool === "search") return "Code Search";
  if (tool === "find_related") return "Find Related";
  if (tool === "execute") return "Shell";
  return tool
    .replaceAll("_", " ")
    .replaceAll(/([a-z0-9])([A-Z])/gu, "$1 $2")
    .split(/\s+/u)
    .filter(Boolean)
    .map((word) => `${word[0]?.toUpperCase() ?? ""}${word.slice(1)}`)
    .join(" ");
};

const titleArgument = (text: string): string => {
  const oneLine = text.trim().split(/\s+/u).join(" ");
  return utf8Length(oneLine) <= 60
    ? oneLine
    : `${utf8Prefix(oneLine, 59)}…`;
};

export const toolLabel = (tool: string, argsValue: unknown): string => {
  const rawArgs = record(argsValue) ?? {};
  const effective = effectiveToolCall(tool, rawArgs);
  const command = ["shell", "Bash", "bash", "execute"].includes(baseToolName(effective.tool))
    ? stringValue(effective.args.command)
    : undefined;
  const display = toolDisplayName(effective.tool);
  if (command !== undefined && command.trim() !== "") {
    return `${display}: ${titleArgument(command)}`;
  }
  const query = firstString(
    effective.args,
    ["query", "pattern", "url", "path", "file_path", "title"],
  );
  return query !== undefined && query.trim() !== "" && query !== display
    ? `${display}: ${titleArgument(query)}`
    : display;
};

const diffLine = (
  kind: ToolDiffLineKind,
  oldNumber: number,
  newNumber: number,
  text: string,
): ToolDiffLine => ({ kind, oldNumber, newNumber, text });

const sourceLines = (source: string): readonly string[] => {
  if (source === "") return [];
  const lines = source.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines;
};

const snippetDiff = (oldText: string, newText: string, start: number): ToolDiffLine[] => {
  const before = sourceLines(oldText);
  const after = sourceLines(newText);
  let oldNumber = start > 0 ? start : 0;
  let newNumber = start > 0 ? start : 0;
  const tick = start > 0 ? 1 : 0;
  const output: ToolDiffLine[] = [];
  const deleted = (line: string): void => {
    output.push(diffLine("delete", oldNumber, 0, line));
    oldNumber += tick;
  };
  const added = (line: string): void => {
    output.push(diffLine("add", 0, newNumber, line));
    newNumber += tick;
  };
  const context = (line: string): void => {
    output.push(diffLine("context", oldNumber, newNumber, line));
    oldNumber += tick;
    newNumber += tick;
  };

  if (before.length * after.length > 1_000_000) {
    before.forEach(deleted);
    after.forEach(added);
    return output;
  }

  const columns = after.length + 1;
  const table = new Uint32Array((before.length + 1) * columns);
  const at = (i: number, j: number): number => i * columns + j;
  for (let i = before.length - 1; i >= 0; i -= 1) {
    for (let j = after.length - 1; j >= 0; j -= 1) {
      table[at(i, j)] = before[i] === after[j]
        ? (table[at(i + 1, j + 1)] ?? 0) + 1
        : Math.max(table[at(i + 1, j)] ?? 0, table[at(i, j + 1)] ?? 0);
    }
  }

  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      context(before[i] ?? "");
      i += 1;
      j += 1;
    } else if ((table[at(i + 1, j)] ?? 0) >= (table[at(i, j + 1)] ?? 0)) {
      deleted(before[i] ?? "");
      i += 1;
    } else {
      added(after[j] ?? "");
      j += 1;
    }
  }
  before.slice(i).forEach(deleted);
  after.slice(j).forEach(added);
  return output;
};

const hunkStarts = (line: string): readonly [number, number] | undefined => {
  const match = /^@@\s+-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s+@@/u.exec(line);
  if (match === null) return undefined;
  const oldNumber = Number(match[1]);
  const newNumber = Number(match[2]);
  return oldNumber > 0 && newNumber > 0 ? [oldNumber, newNumber] : undefined;
};

const patchDiff = (patch: string): ToolDiffLine[] => {
  const output: ToolDiffLine[] = [];
  let changed = false;
  let oldNumber = 0;
  let newNumber = 0;
  for (const line of sourceLines(patch)) {
    if (
      line.startsWith("+++") ||
      line.startsWith("---") ||
      line.startsWith("diff --git") ||
      line.startsWith("index ")
    ) continue;
    if (line.startsWith("@@") || line.startsWith("*** ")) {
      [oldNumber, newNumber] = hunkStarts(line) ?? [0, 0];
      output.push(diffLine("separator", 0, 0, line));
    } else if (line.startsWith("+")) {
      output.push(diffLine("add", 0, newNumber, line.slice(1)));
      if (newNumber > 0) newNumber += 1;
      changed = true;
    } else if (line.startsWith("-")) {
      output.push(diffLine("delete", oldNumber, 0, line.slice(1)));
      if (oldNumber > 0) oldNumber += 1;
      changed = true;
    } else {
      output.push(diffLine("context", oldNumber, newNumber, line.startsWith(" ") ? line.slice(1) : line));
      if (oldNumber > 0) oldNumber += 1;
      if (newNumber > 0) newNumber += 1;
    }
  }
  return changed ? output : [];
};

interface EditPresentation {
  readonly verb: "Edit" | "Write";
  readonly paths: readonly string[];
  readonly lines: readonly ToolDiffLine[];
}

const patchPaths = (base: string, patch: string): readonly string[] => {
  const paths: string[] = [];
  const add = (path: string): void => {
    const normalized = path.trim();
    if (normalized !== "" && !paths.includes(normalized)) paths.push(normalized);
  };
  if (base === "hashline_edit") {
    for (const match of patch.matchAll(/^\[([^\]\n]+)#[0-9a-f]{12}\]$/gimu)) {
      if (match[1] !== undefined) add(match[1]);
    }
  }
  for (const match of patch.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gmu)) {
    if (match[1] !== undefined) add(match[1]);
  }
  for (const match of patch.matchAll(/^diff --git a\/(.+) b\/(.+)$/gmu)) {
    if (match[2] !== undefined) add(match[2]);
  }
  return paths;
};

const editPresentation = (tool: string, args: JsonRecord): EditPresentation | undefined => {
  const base = baseToolName(tool);
  const verb = [
    "edit", "Edit", "MultiEdit", "NotebookEdit", "edit_file", "hashline_edit", "apply_patch", "apply_patch_fallback", "fileChange",
  ].includes(base)
    ? "Edit"
    : ["write", "Write", "write_file", "create_file"].includes(base)
      ? "Write"
      : undefined;
  if (verb === undefined) return undefined;
  const patch = firstString(args, ["diff", "patch", "unified_diff", "unifiedDiff", "input"]);
  const explicitPath = firstString(args, ["file_path", "path", "abs_path", "target_file", "filePath"]);
  const paths = explicitPath === undefined
    ? patch === undefined ? [] : patchPaths(base, patch)
    : [explicitPath];
  if (patch !== undefined) {
    const lines = patchDiff(patch);
    if (lines.length > 0) return { verb, paths, lines };
  }

  const pair = (value: unknown): readonly [string, string, number] | undefined => {
    const source = record(value);
    if (source === undefined) return undefined;
    const oldText = firstString(source, ["old_string", "oldText", "old_text", "old_str"]);
    const newText = firstString(source, ["new_string", "newText", "new_text", "new_str"]);
    if (oldText === undefined && newText === undefined) return undefined;
    return [oldText ?? "", newText ?? "", Math.max(0, numberValue(source._line) ?? 0)];
  };
  const edits = Array.isArray(args.edits)
    ? args.edits.map(pair).filter((value): value is readonly [string, string, number] => value !== undefined)
    : [];
  const directPair = pair(args);
  const content = firstString(args, ["content", "contents", "file_text", "fileText"]);
  const pairs = edits.length > 0
    ? edits
    : directPair !== undefined
      ? [directPair]
      : content !== undefined
        ? [["", content, 1] as const]
        : [];
  if (pairs.length === 0) return undefined;
  const lines: ToolDiffLine[] = [];
  for (const [index, [oldText, newText, start]] of pairs.entries()) {
    if (index > 0) lines.push(diffLine("separator", 0, 0, "···"));
    lines.push(...snippetDiff(oldText, newText, start));
  }
  return { verb, paths, lines };
};

const readRange = (args: JsonRecord): readonly [number, number] => {
  const positive = (keys: readonly string[]): number | undefined => {
    for (const key of keys) {
      const value = numberValue(args[key]);
      if (value !== undefined && value > 0) return value;
    }
    return undefined;
  };
  const start = positive(["offset", "start_line", "startLine", "start"]);
  const end = positive(["end_line", "endLine", "end"]);
  const limit = positive(["limit"]);
  if (start !== undefined && end !== undefined) return [start, Math.max(start, end)];
  if (start !== undefined && limit !== undefined) return [start, start + limit - 1];
  if (start !== undefined) return [start, start];
  if (end !== undefined) return [1, end];
  return [0, 0];
};

const todoRows = (args: JsonRecord, resultValue: unknown): readonly ToolTodoRow[] => {
  const result = record(resultValue);
  const values = Array.isArray(result?.todos)
    ? result.todos
    : Array.isArray(args.todos) ? args.todos : [];
  return values.flatMap((value): readonly ToolTodoRow[] => {
    const item = record(value);
    if (item === undefined) return [];
    const status = stringValue(item.status) ?? "pending";
    const content = stringValue(item.content) ?? "";
    const icon = todoStatusIcon(status);
    return [{ status, content, icon }];
  });
};

const emptyPresentation = (title: string): ToolPresentation => ({
  title,
  subject: "",
  filePath: "",
  lineFrom: 0,
  lineTo: 0,
  meta: "",
  additions: 0,
  deletions: 0,
  diff: [],
  todos: [],
});

const isNoise = (value: unknown): boolean =>
  value === null ||
  value === undefined ||
  value === "" ||
  (Array.isArray(value) && value.length === 0) ||
  (record(value) !== undefined && Object.keys(record(value)!).length === 0);

const humanizeJson = (value: unknown, indent = 0): string => {
  const padding = "  ".repeat(indent);
  const object = record(value);
  if (object !== undefined) {
    return Object.entries(object).flatMap(([key, entry]): readonly string[] => {
      if (isNoise(entry)) return [];
      if (typeof entry === "string" && entry.includes("\n")) {
        return [`${padding}${key}:\n${entry.split("\n").filter((_, index, lines) =>
          index + 1 < lines.length || lines[index] !== ""
        ).map((line) => `${padding}  ${line}`).join("\n")}`];
      }
      if (typeof entry === "string") return [`${padding}${key}: ${entry}`];
      if (record(entry) !== undefined || Array.isArray(entry)) {
        return [`${padding}${key}:\n${humanizeJson(entry, indent + 1)}`];
      }
      return [`${padding}${key}: ${String(entry)}`];
    }).join("\n");
  }
  if (Array.isArray(value)) {
    return value.map((entry) => {
      if (record(entry) !== undefined || Array.isArray(entry)) {
        return `${padding}-\n${humanizeJson(entry, indent + 1)}`;
      }
      return `${padding}- ${String(entry)}`;
    }).join("\n");
  }
  if (typeof value === "string") {
    return value.split("\n").filter((_, index, lines) =>
      index + 1 < lines.length || lines[index] !== ""
    ).map((line) => `${padding}${line}`).join("\n");
  }
  return `${padding}${String(value)}`;
};

const textBlocks = (value: unknown): string | undefined => {
  if (typeof value === "string") return value;
  const object = record(value);
  const blocks = Array.isArray(value)
    ? value
    : object !== undefined && Array.isArray(object.content)
      ? object.content
      : undefined;
  if (blocks === undefined || blocks.length === 0) return undefined;
  const texts: string[] = [];
  for (const block of blocks) {
    const item = record(block);
    if (item?.type !== "text" || typeof item.text !== "string") return undefined;
    texts.push(item.text);
  }
  return texts.join("\n");
};

/** The expanded non-edit detail contract: readable
 * key/value blocks without JSON brace/quote noise, with vendor text result
 * wrappers flattened and a strict historical-card bound. */
export const toolDetailText = (args: unknown, result?: unknown): string => {
  const parts: string[] = [];
  const argumentText = humanizeJson(args).trimEnd();
  if (argumentText !== "") parts.push(argumentText);
  if (result !== undefined) {
    const resultText = textBlocks(result) ?? humanizeJson(result).trimEnd();
    parts.push(`── result ──\n${resultText}`);
  }
  const detail = parts.join("\n").trimEnd();
  return utf8Length(detail) <= 4_000
    ? detail
    : `${utf8Prefix(detail, 4_000)}…`;
};

const compactDetailText = (value: unknown, limit = 2_000): string => {
  const text = humanizeJson(value).trimEnd();
  return utf8Length(text) <= limit ? text : `${utf8Prefix(text, limit)}…`;
};

const detailField = (
  label: string,
  value: unknown,
  options: { readonly code?: boolean } = {},
): ToolDetailField | undefined => {
  if (isNoise(value)) return undefined;
  const text = typeof value === "string" ? value : compactDetailText(value, 800);
  if (text.trim() === "") return undefined;
  return { label, value: text, ...(options.code === true ? { code: true } : {}) };
};

const detailFields = (
  source: JsonRecord,
  omitted: ReadonlySet<string> = new Set(),
): readonly ToolDetailField[] => Object.entries(source).flatMap(([key, value]) => {
  if (omitted.has(key)) return [];
  const label = key
    .replaceAll("_", " ")
    .replaceAll(/([a-z0-9])([A-Z])/gu, "$1 $2")
    .replace(/^./u, (character) => character.toUpperCase());
  const field = detailField(label, value, {
    code: typeof value === "string" && (key.includes("path") || key === "command" || key === "url"),
  });
  return field === undefined ? [] : [field];
});

const parsedJson = (source: string): unknown | undefined => {
  const trimmed = source.trim();
  if (!(trimmed.startsWith("{") && trimmed.endsWith("}"))
    && !(trimmed.startsWith("[") && trimmed.endsWith("]"))) return undefined;
  try {
    return JSON.parse(trimmed) as unknown;
  } catch {
    return undefined;
  }
};

/** Normalize native results, trouve's JSON-string search result, and MCP
 * text-content wrappers into the same value before presentation. */
const resolvedToolResult = (value: unknown): unknown => {
  let current = value;
  for (let depth = 0; depth < 4; depth += 1) {
    if (typeof current === "string") {
      const parsed = parsedJson(current);
      if (parsed === undefined) return current;
      current = parsed;
      continue;
    }
    const flattened = textBlocks(current);
    if (flattened === undefined) return current;
    const parsed = parsedJson(flattened);
    if (parsed === undefined) return flattened;
    current = parsed;
  }
  return current;
};

const normalizedToolName = (tool: string): string => baseToolName(tool)
  .replaceAll(/[^a-z0-9]/giu, "")
  .toLowerCase();

const booleanValue = (value: unknown): boolean => value === true;

const finiteNumber = (value: unknown): number | undefined =>
  typeof value === "number" && Number.isFinite(value) ? value : undefined;

const requestedReadStart = (args: JsonRecord): number => {
  const offset = numberValue(args.offset)
    ?? numberValue(args.start_line)
    ?? numberValue(args.startLine);
  return offset !== undefined && offset > 0 ? offset : 1;
};

const decodedReadSource = (
  args: JsonRecord,
  result: JsonRecord,
): { readonly content: string; readonly startLine: number } | undefined => {
  const raw = stringValue(result.content);
  if (raw === undefined) return undefined;
  if (result.format !== "hashline") {
    return { content: raw.endsWith("\n") ? raw.slice(0, -1) : raw, startLine: requestedReadStart(args) };
  }
  const rows = raw.split("\n");
  if (rows.at(-1) === "") rows.pop();
  if (/^\[[^\n]+#[A-Za-z0-9]+\]$/u.test(rows[0] ?? "")) rows.shift();
  const decoded = rows.map((row) => /^(\d+):(.*)$/u.exec(row));
  if (decoded.some((match) => match === null)) {
    return { content: raw.endsWith("\n") ? raw.slice(0, -1) : raw, startLine: requestedReadStart(args) };
  }
  const firstLine = Number(decoded[0]?.[1] ?? requestedReadStart(args));
  return {
    content: decoded.map((match) => match?.[2] ?? "").join("\n"),
    startLine: Number.isFinite(firstLine) ? firstLine : requestedReadStart(args),
  };
};

const searchInputs = (tool: string, args: JsonRecord): readonly ToolDetailField[] => {
  const related = normalizedToolName(tool) === "findrelated";
  const fields = related
    ? [
        detailField("Source", `${firstString(args, ["file_path", "path"]) ?? ""}:${numberValue(args.line) ?? 1}`, { code: true }),
      ]
    : [detailField("Query", firstString(args, ["query"]) ?? "")];
  fields.push(
    detailField("Repository", firstString(args, ["repo"]) ?? ".", { code: true }),
    detailField("Results", numberValue(args.top_k) ?? 5),
    detailField("Snippet lines", numberValue(args.max_snippet_lines) ?? 10),
  );
  return fields.filter((field): field is ToolDetailField => field !== undefined);
};

const searchResults = (result: JsonRecord): readonly ToolSearchResultDetail[] => {
  if (!Array.isArray(result.results)) return [];
  return result.results.slice(0, 100).flatMap((value): readonly ToolSearchResultDetail[] => {
    const row = record(value);
    if (row === undefined) return [];
    const path = firstString(row, ["file_path", "path"]);
    if (path === undefined) return [];
    const startLine = numberValue(row.start_line) ?? numberValue(row.line) ?? 1;
    const score = finiteNumber(row.score);
    return [{
      path,
      startLine,
      endLine: Math.max(startLine, numberValue(row.end_line) ?? startLine),
      ...(score === undefined ? {} : { score }),
      content: firstString(row, ["content", "snippet", "text"]) ?? "",
    }];
  });
};

const transcriptPresentation = (
  args: JsonRecord,
  result: JsonRecord,
): ToolDetailPresentation => {
  const inputs = [
    detailField("Query", args.query),
    detailField("Scope", args.scope ?? "thread"),
    detailField("Thread", args.thread_id, { code: true }),
    detailField("Turn", args.turn),
  ].filter((field): field is ToolDetailField => field !== undefined);
  const resultMatches = Array.isArray(result.matches) ? result.matches : [];
  const matches = resultMatches
    .slice(0, 100).flatMap((value): readonly ToolTranscriptMatchDetail[] => {
        const row = record(value);
        if (row === undefined) return [];
        return [{
          threadId: stringValue(row.thread_id) ?? "",
          turn: numberValue(row.turn) ?? 0,
          role: stringValue(row.role) ?? "message",
          timestamp: stringValue(row.ts) ?? "",
          snippet: stringValue(row.snippet) ?? "",
        }];
      });
  const resultMessages = Array.isArray(result.messages) ? result.messages : [];
  const messages = resultMessages
    .slice(0, 100)
    .flatMap((value): readonly ToolDetailField[] => {
        const row = record(value);
        if (row === undefined) return [];
        const role = stringValue(row.role) ?? "message";
        const body = firstString(row, ["content", "args"]) ?? compactDetailText(row, 2_000);
        return [{ label: role.replace(/^./u, (character) => character.toUpperCase()), value: body }];
      });
  return {
    kind: "transcript",
    inputs,
    matches,
    messages,
    truncated: booleanValue(result.truncated)
      || resultMatches.length > 100
      || resultMessages.length > 100,
  };
};

/** Purpose-built expanded content for built-in tools, with a readable
 * structured fallback for provider-native and third-party MCP tools. */
export const presentToolDetail = (
  tool: string,
  argsValue: unknown,
  resultValue?: unknown,
): ToolDetailPresentation => {
  const rawArgs = record(argsValue) ?? {};
  const effective = effectiveToolCall(tool, rawArgs);
  const normalized = normalizedToolName(effective.tool);
  const firstParty = isFirstPartyToolCall(tool, argsValue);
  const result = resolvedToolResult(resultValue);
  const resultRecord = record(result);

  if (firstParty && ["read", "readfile"].includes(normalized)) {
    const source = resultRecord === undefined
      ? typeof result === "string"
        ? {
            content: result.endsWith("\n") ? result.slice(0, -1) : result,
            startLine: requestedReadStart(effective.args),
          }
        : undefined
      : decodedReadSource(effective.args, resultRecord);
    const path = firstString(effective.args, ["file_path", "path"]) ?? "";
    if (source !== undefined) {
      const totalLines = resultRecord === undefined
        ? undefined
        : numberValue(resultRecord.total_lines);
      return {
        kind: "source",
        inputs: [],
        source: {
          path,
          content: source.content,
          startLine: source.startLine,
          ...(totalLines === undefined ? {} : { totalLines }),
          truncated: booleanValue(resultRecord?.truncated),
        },
      };
    }
  }

  if (firstParty && ["search", "findrelated"].includes(normalized) && resultRecord !== undefined) {
    return {
      kind: "search",
      inputs: searchInputs(effective.tool, effective.args),
      results: searchResults(resultRecord),
      truncated: booleanValue(resultRecord.truncated),
    };
  }

  if (firstParty && normalized === "grep" && resultRecord !== undefined && Array.isArray(resultRecord.matches)) {
    const matches = resultRecord.matches.slice(0, 200).flatMap((value): readonly ToolMatchDetail[] => {
      const row = record(value);
      const path = row === undefined ? undefined : firstString(row, ["path", "file_path"]);
      if (row === undefined || path === undefined) return [];
      return [{ path, line: numberValue(row.line) ?? 1, text: stringValue(row.text) ?? "" }];
    });
    return {
      kind: "matches",
      inputs: [
        detailField("Pattern", effective.args.pattern),
        detailField("Path", effective.args.path ?? ".", { code: true }),
        detailField("Case insensitive", effective.args.case_insensitive ?? false),
      ].filter((field): field is ToolDetailField => field !== undefined),
      matches,
      truncated: booleanValue(resultRecord.truncated),
    };
  }

  if (firstParty && ["glob", "listdir"].includes(normalized) && resultRecord !== undefined) {
    const files = Array.isArray(resultRecord.files)
      ? resultRecord.files.filter((value): value is string => typeof value === "string")
      : Array.isArray(resultRecord.entries)
        ? resultRecord.entries.flatMap((value): readonly string[] => {
            const row = record(value);
            const name = row === undefined ? undefined : stringValue(row.name);
            if (name === undefined) return [];
            return [`${name}${row?.kind === "dir" ? "/" : ""}`];
          })
        : [];
    return {
      kind: "paths",
      inputs: [
        detailField("Pattern", effective.args.pattern),
        detailField("Path", effective.args.path ?? ".", { code: true }),
      ].filter((field): field is ToolDetailField => field !== undefined),
      paths: files.slice(0, 500),
      truncated: booleanValue(resultRecord.truncated) || files.length > 500,
    };
  }

  if (
    firstParty && (
      ["shell", "bash", "execute", "commandexecution", "shelloutput", "shellkill"].includes(normalized)
      || resultRecord !== undefined
        && (typeof resultRecord.stdout === "string" || typeof resultRecord.stderr === "string")
    )
  ) {
    const output = resultRecord ?? {};
    return {
      kind: "command",
      inputs: [
        detailField("Command", effective.args.command, { code: true }),
        detailField("Job", effective.args.job_id ?? output.job_id, { code: true }),
        detailField("Process", output.pid),
        detailField("Timeout", effective.args.timeout_secs === undefined ? undefined : `${String(effective.args.timeout_secs)}s`),
        detailField("Background", effective.args.run_in_background),
        detailField("Exit code", output.exit_code),
        detailField("Running", output.running),
        detailField("Killed", output.killed),
        detailField("Already finished", output.already_finished),
        detailField("Note", output.note),
      ].filter((field): field is ToolDetailField => field !== undefined),
      stdout: firstString(output, ["stdout", "new_output", "output"]) ?? "",
      stderr: stringValue(output.stderr) ?? "",
      truncated: booleanValue(output.truncated),
    };
  }

  if (firstParty && normalized === "webfetch" && resultRecord !== undefined && typeof resultRecord.content === "string") {
    const content = resultRecord.content;
    return {
      kind: "document",
      inputs: [
        detailField("URL", effective.args.url, { code: true }),
        detailField("Resolved URL", resultRecord.url === effective.args.url ? undefined : resultRecord.url, { code: true }),
        detailField("Offset", effective.args.offset),
        detailField("Characters", resultRecord.total_chars),
      ].filter((field): field is ToolDetailField => field !== undefined),
      content,
      language: parsedJson(content) === undefined ? "markdown" : "json",
      truncated: booleanValue(resultRecord.truncated),
    };
  }

  if (firstParty && normalized === "gitdiff" && resultRecord !== undefined && typeof resultRecord.diff === "string") {
    const nextOffset = numberValue(resultRecord.next_offset);
    const totalBytes = numberValue(resultRecord.total_bytes);
    return {
      kind: "diff",
      inputs: [
        detailField("Base", effective.args.base, { code: true }),
        detailField("Path", effective.args.path, { code: true }),
        detailField("Byte offset", effective.args.offset ?? resultRecord.offset),
        detailField("Byte limit", effective.args.limit),
      ].filter((field): field is ToolDetailField => field !== undefined),
      diff: resultRecord.diff,
      truncated: booleanValue(resultRecord.truncated),
      ...(nextOffset === undefined ? {} : { nextOffset }),
      ...(totalBytes === undefined ? {} : { totalBytes }),
    };
  }

  if (firstParty && normalized === "searchtranscript" && resultRecord !== undefined) {
    return transcriptPresentation(effective.args, resultRecord);
  }

  const error = resultRecord?.isError === true
    || typeof resultRecord?.error === "string"
    || record(resultValue)?.isError === true;
  return {
    kind: "structured",
    inputs: detailFields(effective.args),
    resultText: resultValue === undefined ? "" : compactDetailText(result, 8_000),
    error,
  };
};

export const presentToolCall = (
  tool: string,
  argsValue: unknown,
  resultValue?: unknown,
): ToolPresentation => {
  const rawArgs = record(argsValue) ?? {};
  const effective = effectiveToolCall(tool, rawArgs);
  const base = baseToolName(effective.tool);
  const firstParty = isFirstPartyToolCall(tool, argsValue);
  if (firstParty && (base === "todo_write" || base === "TodoWrite")) {
    const todos = todoRows(effective.args, resultValue);
    if (todos.length > 0) {
      const done = todos.filter((todo) => todo.status === "completed" || todo.status === "cancelled").length;
      const current = todos.find((todo) => todo.status === "in_progress")?.content;
      return {
        ...emptyPresentation("TODOs"),
        subject: current ?? `${done}/${todos.length} done`,
        meta: `${done}/${todos.length}`,
        todos,
      };
    }
  }

  const edit = firstParty ? editPresentation(effective.tool, effective.args) : undefined;
  if (edit !== undefined) {
    const fullLines = edit.lines;
    const path = edit.paths.length === 1 ? edit.paths[0] ?? "" : "";
    const additions = fullLines.filter((line) => line.kind === "add").length;
    const deletions = fullLines.filter((line) => line.kind === "delete").length;
    const diff = fullLines.length <= 300
      ? fullLines
      : [...fullLines.slice(0, 300), diffLine("separator", 0, 0, `… ${fullLines.length - 300} more lines`)];
    return {
      ...emptyPresentation(edit.verb),
      subject: edit.paths.length > 1
        ? `${edit.paths.length} files`
        : path.split(/[\\/]/u).at(-1) ?? "",
      filePath: path,
      additions,
      deletions,
      diff,
    };
  }

  const isRead = firstParty && (
    ["Read", "read", "read_file"].includes(effective.tool)
    || ["Read", "read", "read_file"].includes(base)
  );
  const path = isRead
    ? firstString(effective.args, ["file_path", "path"]) ?? ""
    : "";
  if (path !== "") {
    const [lineFrom, lineTo] = readRange(effective.args);
    return {
      ...emptyPresentation("Read"),
      subject: path.split(/[\\/]/u).at(-1) ?? "",
      filePath: path,
      lineFrom,
      lineTo,
      meta: lineFrom === 0 ? "" : lineTo > lineFrom ? `L${lineFrom}-${lineTo}` : `L${lineFrom}`,
    };
  }

  return emptyPresentation(toolLabel(tool, argsValue));
};
