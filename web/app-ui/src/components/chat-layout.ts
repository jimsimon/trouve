import type { ThreadChatItem } from "../state/thread-view-model.js";
import { isTodoToolCall } from "./tool-presentation.js";

export type AgentChatItem = Extract<
  ThreadChatItem,
  { readonly kind: "assistant" | "steered" | "subagent" | "thinking" | "compaction" | "todo" | "tool" | "questions" }
>;

export type AgentActivityItem = Extract<
  AgentChatItem,
  { readonly kind: "thinking" | "compaction" | "todo" | "tool" }
>;

/** Older Codex transcripts represented native context compaction as a tool
 * call. Keep recognizing those rows so the renderer can promote them to the
 * same durable top-level boundary used by the current protocol lifecycle. */
export const isContextCompactionTool = (item: AgentActivityItem): boolean => {
  if (item.kind !== "tool") return false;
  const normalized = (item.tool.split("__").at(-1) ?? item.tool)
    .replaceAll(/[^a-z0-9]/giu, "")
    .toLowerCase();
  return normalized === "contextcompaction" || normalized === "compactcontext";
};

export type ChatRenderUnit = {
  readonly id: string;
  readonly kind: "turn";
  readonly turn: number;
  readonly divider: boolean;
  readonly prompt: Extract<ThreadChatItem, { readonly kind: "user" }> | undefined;
  readonly items: readonly AgentChatItem[];
  readonly status: Extract<ThreadChatItem, { readonly kind: "turn-status" }> | undefined;
};

export interface ChatLayout {
  readonly units: readonly ChatRenderUnit[];
  readonly unitIdForItem: ReadonlyMap<string, string>;
}

const isAgentItem = (item: ThreadChatItem): item is AgentChatItem =>
  item.kind === "assistant"
  || item.kind === "steered"
  || item.kind === "subagent"
  || item.kind === "thinking"
  || item.kind === "compaction"
  || item.kind === "todo"
  || item.kind === "tool"
  || item.kind === "questions";

interface MutableTurnUnit {
  turn: number | undefined;
  readonly firstId: string;
  prompt: Extract<ThreadChatItem, { readonly kind: "user" }> | undefined;
  readonly items: AgentChatItem[];
  status: Extract<ThreadChatItem, { readonly kind: "turn-status" }> | undefined;
}

/** Build one stable virtual row per conversational turn. A bounded history
 * page can begin with a tool that carries no turn number; keep that provisional
 * row open until the next explicit prompt/assistant/status event claims it. */
export const buildChatLayout = (items: readonly ThreadChatItem[]): ChatLayout => {
  const units: ChatRenderUnit[] = [];
  const unitIdForItem = new Map<string, string>();
  let current: MutableTurnUnit | undefined;
  let lastExplicitTurn: number | undefined;

  const flush = (): void => {
    if (current === undefined) return;
    const turn = current.turn ?? lastExplicitTurn ?? 0;
    const id = turn === 0 ? `turn:0:${current.firstId}` : `turn:${turn}`;
    const linkedSpawnCalls = new Set(
      current.items.flatMap((item) =>
        item.kind === "subagent" && item.callId !== undefined ? [item.callId] : []),
    );
    const hasTodoLifecycle = current.items.some((item) => item.kind === "todo");
    const visibleItems = current.items.filter((item) =>
      item.kind !== "tool"
      || (
        !linkedSpawnCalls.has(item.callId)
        && (!hasTodoLifecycle || !isTodoToolCall(item.tool, item.args))
      ));
    const unit: ChatRenderUnit = Object.freeze({
      id,
      kind: "turn",
      turn,
      divider: units.length > 0,
      prompt: current.prompt,
      items: Object.freeze(visibleItems),
      status: current.status,
    });
    units.push(unit);
    if (current.prompt !== undefined) unitIdForItem.set(current.prompt.id, id);
    for (const item of visibleItems) unitIdForItem.set(item.id, id);
    if (current.status !== undefined) unitIdForItem.set(current.status.id, id);
    current = undefined;
  };

  const claim = (turn: number | undefined, firstId: string): MutableTurnUnit => {
    if (
      turn !== undefined
      && current?.turn !== undefined
      && current.turn !== turn
    ) flush();
    current ??= {
      turn,
      firstId,
      prompt: undefined,
      items: [],
      status: undefined,
    };
    if (turn !== undefined) {
      current.turn = turn;
      lastExplicitTurn = turn;
    }
    return current;
  };

  for (const item of items) {
    if (item.kind === "user") {
      claim(item.turn, item.id).prompt = item;
      continue;
    }
    if (isAgentItem(item)) {
      const explicitTurn =
        item.kind === "assistant"
        || item.kind === "steered"
        || item.kind === "subagent"
        || item.kind === "thinking"
        || item.kind === "compaction"
        || item.kind === "todo"
          ? item.turn
          : undefined;
      claim(explicitTurn, item.id).items.push(item);
      continue;
    }
    claim(item.turn, item.id).status = item;
  }
  flush();

  return Object.freeze({ units: Object.freeze(units), unitIdForItem });
};

const nestedMcpTool = (item: Extract<AgentChatItem, { readonly kind: "tool" }>) => {
  if (item.tool !== "mcpToolCall" || item.args === null || typeof item.args !== "object") {
    return { tool: item.tool, args: item.args };
  }
  const wrapped = item.args as Record<string, unknown>;
  return {
    tool: typeof wrapped["tool"] === "string" ? wrapped["tool"] : item.tool,
    args: wrapped["arguments"] ?? item.args,
  };
};

const argumentPaths = (args: unknown, tool = ""): string[] => {
  if (args === null || typeof args !== "object") return [];
  const object = args as Record<string, unknown>;
  const value = object["file_path"] ?? object["path"];
  if (typeof value === "string" && value !== "") return [value];
  if ((tool.split("__").at(-1) ?? tool) !== "hashline_edit") return [];
  const input = object["input"];
  if (typeof input !== "string") return [];
  return input.split("\n").flatMap((line) => {
    const header = line.trim();
    if (!header.startsWith("[") || !header.endsWith("]")) return [];
    const inner = header.slice(1, -1);
    const separator = inner.lastIndexOf("#");
    return separator > 0 ? [inner.slice(0, separator)] : [];
  });
};

const plural = (count: number, one: string, many: string): string =>
  `${count} ${count === 1 ? one : many}`;

/** Build the activity-group sentence used for consecutive work items. */
export const activityGroupSummary = (items: readonly AgentActivityItem[]): string => {
  const todoItems = items.filter((item) => item.kind === "todo");
  const repeatedTodoState = todoItems.length > 1
    && todoItems.length === items.length
    && todoItems.every((item) => item.state === todoItems[0]?.state)
      ? todoItems[0]?.state
      : undefined;
  if (repeatedTodoState !== undefined) {
    const action = {
      started: "Started",
      completed: "Completed",
      cancelled: "Cancelled",
      skipped: "Skipped",
    }[repeatedTodoState];
    const count = new Set(todoItems.map((item) => item.todoId)).size;
    return `${action} ${plural(count, "TODO", "TODOs")}`;
  }

  const edited = new Set<string>();
  const read = new Set<string>();
  let editsWithoutPath = 0;
  let readsWithoutPath = 0;
  let codeSearches = 0;
  let transcriptSearches = 0;
  let commands = 0;
  let tools = 0;
  let thoughts = 0;
  let compactions = 0;
  const todos = new Set<string>();

  for (const item of items) {
    if (item.kind === "thinking") {
      thoughts += 1;
      continue;
    }
    if (item.kind === "todo") {
      todos.add(item.todoId);
      continue;
    }
    if (item.kind === "compaction" || isContextCompactionTool(item)) {
      compactions += 1;
      continue;
    }
    if (item.kind !== "tool") continue;
    const effective = nestedMcpTool(item);
    const base = effective.tool.split("__").at(-1) ?? effective.tool;
    const paths = argumentPaths(effective.args, effective.tool);
    const path = paths[0];
    if ([
      "edit", "Edit", "MultiEdit", "NotebookEdit", "Write", "write",
      "edit_file", "hashline_edit", "write_file", "create_file", "apply_patch", "apply_patch_fallback", "delete", "delete_file",
    ].includes(base)) {
      if (paths.length === 0) editsWithoutPath += 1;
      else for (const editedPath of paths) edited.add(editedPath);
    } else if (["read", "Read", "read_file"].includes(base)) {
      if (path === undefined) readsWithoutPath += 1;
      else read.add(path);
    } else if (["search", "find_related"].includes(base)) {
      codeSearches += 1;
    } else if (base === "search_transcript") {
      transcriptSearches += 1;
    } else if (["shell", "bash", "Bash", "execute", "commandExecution"].includes(base)) {
      commands += 1;
    } else if (base === "fileChange") {
      let found = false;
      if (effective.args !== null && typeof effective.args === "object") {
        const changes = (effective.args as Record<string, unknown>)["changes"];
        if (Array.isArray(changes)) {
          for (const change of changes) {
            if (change === null || typeof change !== "object") continue;
            const changedPath = (change as Record<string, unknown>)["path"];
            if (typeof changedPath === "string" && changedPath !== "") {
              edited.add(changedPath);
              found = true;
            }
          }
        }
      }
      if (!found) editsWithoutPath += 1;
    } else {
      tools += 1;
    }
  }

  const parts: string[] = [];
  const editCount = edited.size + editsWithoutPath;
  const readCount = read.size + readsWithoutPath;
  if (editCount > 0) parts.push(`edited ${plural(editCount, "file", "files")}`);
  if (readCount > 0) parts.push(`read ${plural(readCount, "file", "files")}`);
  if (codeSearches > 0) parts.push(`ran ${plural(codeSearches, "code search", "code searches")}`);
  if (transcriptSearches > 0) {
    parts.push(`ran ${plural(transcriptSearches, "transcript search", "transcript searches")}`);
  }
  if (commands > 0) parts.push(`ran ${plural(commands, "command", "commands")}`);
  if (tools > 0) parts.push(`called ${plural(tools, "tool", "tools")}`);
  if (thoughts > 0) parts.push(`thought ${plural(thoughts, "time", "times")}`);
  if (compactions > 0) {
    parts.push(compactions === 1 ? "compacted context" : `compacted context ${compactions} times`);
  }
  if (todos.size > 0) parts.push(`updated ${plural(todos.size, "TODO", "TODOs")}`);
  const summary = parts.join(", ");
  return summary === "" ? "Worked" : `${summary[0]?.toUpperCase() ?? ""}${summary.slice(1)}`;
};
