import { utf8Length, utf8Prefix } from "../services/utf8-text.js";
import type { FontAwesomeIconName } from "./font-awesome-icon.js";
import { todoStatusIcon } from "./todo-plan-model.js";

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

/** Compact execution duration shown beside the tool title. A positive
 * provider duration is authoritative; zero is commonly a provider
 * placeholder, so a server/event measurement supplies the fallback. The
 * status glyph communicates success or failure without repeating an exit
 * code in the collapsed row. */
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

const effectiveToolCall = (
  tool: string,
  args: JsonRecord,
): { readonly tool: string; readonly args: JsonRecord } => {
  const normalized = tool.replaceAll(/[^a-z0-9]/giu, "").toLowerCase();
  if (normalized !== "mcptoolcall" && normalized !== "dynamictoolcall") {
    return { tool, args };
  }
  const nestedTool = firstString(args, ["tool", "toolName", "name"]);
  const nestedArgs = record(args.arguments) ?? args;
  return { tool: nestedTool ?? tool, args: nestedArgs };
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
  const command = ["shell", "Bash", "bash", "execute"].includes(effective.tool)
    ? stringValue(effective.args.command)
    : undefined;
  const display = toolDisplayName(effective.tool);
  if (command !== undefined && command.trim() !== "") {
    return `${display} (${titleArgument(command)})`;
  }
  const query = firstString(effective.args, ["query", "pattern", "url", "path", "title"]);
  return query !== undefined && query.trim() !== "" && query !== display
    ? `${display} ${titleArgument(query)}`
    : display;
};

const toolActivityLabel = (tool: string, argsValue: unknown): string => {
  const rawArgs = record(argsValue) ?? {};
  const wrapper = tool.replaceAll(/[^a-z0-9]/giu, "").toLowerCase();
  if (wrapper === "mcptoolcall" || wrapper === "dynamictoolcall") {
    const server = firstString(rawArgs, ["serverName", "server"]);
    if (server !== undefined && server !== "" && server !== "trouve") {
      return `Using ${server}…`;
    }
  }
  const effective = effectiveToolCall(tool, rawArgs);
  const mcp = effective.tool.startsWith("mcp__")
    ? effective.tool.slice(5).split("__", 2)
    : [];
  if (mcp.length === 2 && mcp[0] !== "trouve") return `Using ${mcp[0]}…`;
  const normalized = baseToolName(effective.tool)
    .replaceAll(/[^a-z0-9]/giu, "")
    .toLowerCase();
  const title = stringValue(effective.args.title)?.toLowerCase() ?? "";
  if (title.includes("web search")) return "Searching the web…";
  if (title.includes("code search") || title.includes("find related")) {
    return "Searching through code…";
  }
  if ([
    "edit", "multiedit", "notebookedit", "write", "editfile", "writefile",
    "createfile", "applypatch", "delete", "deletefile", "filechange",
  ].includes(normalized)) return "Editing files…";
  if (["read", "readfile", "listdir"].includes(normalized)) return "Reading files…";
  if ([
    "shell", "bash", "execute", "commandexecution", "shelloutput", "shellkill",
  ].includes(normalized)) return "Running commands…";
  if (["search", "findrelated", "grep", "glob"].includes(normalized)) {
    return "Searching through code…";
  }
  if (normalized === "websearch") return "Searching the web…";
  if (normalized === "webfetch") return "Fetching web content…";
  if (["todowrite", "createplan", "updateplan"].includes(normalized)) return "Updating the plan…";
  if (["task", "agent", "spawnagent", "collabagenttoolcall"].includes(normalized)) {
    return "Delegating work…";
  }
  return `Using ${toolDisplayName(effective.tool)}…`;
};

/** Describe the active work at the transcript tail, using only running tools
 * after the current turn marker so stale interrupted cards cannot leak into a
 * newer turn. */
export const runningActivityLabel = (
  items: readonly unknown[],
  thinking: boolean,
): string => {
  if (thinking) return "Thinking…";
  let start = 0;
  for (let index = items.length - 1; index >= 0; index -= 1) {
    const item = record(items[index]);
    if (item?.kind === "turn-status" && record(item.state)?.kind === "running") {
      start = index + 1;
      break;
    }
  }
  for (let index = items.length - 1; index >= start; index -= 1) {
    const item = record(items[index]);
    if (
      item?.kind === "tool" &&
      item.status === "running" &&
      typeof item.tool === "string"
    ) return toolActivityLabel(item.tool, item.args);
  }
  return "Processing…";
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
  readonly path: string;
  readonly lines: readonly ToolDiffLine[];
}

const editPresentation = (tool: string, args: JsonRecord): EditPresentation | undefined => {
  const base = baseToolName(tool);
  const verb = [
    "edit", "Edit", "MultiEdit", "NotebookEdit", "edit_file", "apply_patch", "fileChange",
  ].includes(base)
    ? "Edit"
    : ["write", "Write", "write_file", "create_file"].includes(base)
      ? "Write"
      : undefined;
  if (verb === undefined) return undefined;
  const path = firstString(args, ["file_path", "path", "abs_path", "target_file", "filePath"]) ?? "";
  const patch = firstString(args, ["diff", "patch", "unified_diff", "unifiedDiff", "input"]);
  if (patch !== undefined) {
    const lines = patchDiff(patch);
    if (lines.length > 0) return { verb, path, lines };
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
  return { verb, path, lines };
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
    : object !== undefined && Object.keys(object).length === 1 && Array.isArray(object.content)
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

export const presentToolCall = (
  tool: string,
  argsValue: unknown,
  resultValue?: unknown,
): ToolPresentation => {
  const rawArgs = record(argsValue) ?? {};
  const effective = effectiveToolCall(tool, rawArgs);
  const base = baseToolName(effective.tool);
  if (base === "todo_write" || base === "TodoWrite") {
    const todos = todoRows(effective.args, resultValue);
    if (todos.length > 0) {
      const done = todos.filter((todo) => todo.status === "completed" || todo.status === "cancelled").length;
      const current = todos.find((todo) => todo.status === "in_progress")?.content;
      return {
        ...emptyPresentation("Todos"),
        subject: current ?? `${done}/${todos.length} done`,
        meta: `${done}/${todos.length}`,
        todos,
      };
    }
  }

  const edit = editPresentation(effective.tool, effective.args);
  if (edit !== undefined) {
    const fullLines = edit.lines;
    const additions = fullLines.filter((line) => line.kind === "add").length;
    const deletions = fullLines.filter((line) => line.kind === "delete").length;
    const diff = fullLines.length <= 300
      ? fullLines
      : [...fullLines.slice(0, 300), diffLine("separator", 0, 0, `… ${fullLines.length - 300} more lines`)];
    return {
      ...emptyPresentation(edit.verb),
      subject: edit.path.split(/[\\/]/u).at(-1) ?? "",
      filePath: edit.path,
      additions,
      deletions,
      diff,
    };
  }

  const isRead = ["Read", "read", "read_file"].includes(effective.tool)
    || ["Read", "read", "read_file"].includes(base);
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
