import type { ThreadChatItem } from "../state/thread-view-model.js";

export type AgentChatItem = Extract<
  ThreadChatItem,
  { readonly kind: "assistant" | "thinking" | "compaction" | "tool" | "questions" }
>;

export type AgentActivityItem = Extract<
  AgentChatItem,
  { readonly kind: "thinking" | "tool" }
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

export type ChatRenderUnit =
  | {
      readonly id: string;
      readonly kind: "user";
      readonly divider: boolean;
      readonly item: Extract<ThreadChatItem, { readonly kind: "user" }>;
    }
  | {
      readonly id: string;
      readonly kind: "agent";
      readonly turn: number;
      readonly items: readonly AgentChatItem[];
    }
  | {
      readonly id: string;
      readonly kind: "status";
      readonly item: Extract<ThreadChatItem, { readonly kind: "turn-status" }>;
    };

export interface ChatLayout {
  readonly units: readonly ChatRenderUnit[];
  readonly unitIdForItem: ReadonlyMap<string, string>;
}

const isAgentItem = (item: ThreadChatItem): item is AgentChatItem =>
  item.kind === "assistant"
  || item.kind === "thinking"
  || item.kind === "compaction"
  || item.kind === "tool"
  || item.kind === "questions";

/**
 * Preserve the retained Slint transcript hierarchy: prompts are individual
 * cards, while each uninterrupted assistant/work run is one Agent card.
 * Running/completed status rows are represented by the Agent header/activity
 * treatment and therefore do not become empty virtual rows.
 */
export const buildChatLayout = (items: readonly ThreadChatItem[]): ChatLayout => {
  const units: ChatRenderUnit[] = [];
  const unitIdForItem = new Map<string, string>();
  let currentTurn = 0;
  let pendingAgentItems: AgentChatItem[] = [];
  let pendingStatus: Extract<ThreadChatItem, { readonly kind: "turn-status" }> | undefined;
  let lastAgentTurn: number | undefined;
  let lastAgentUnitId: string | undefined;

  const flushAgent = (): void => {
    const first = pendingAgentItems[0];
    if (first === undefined) return;
    const turn = pendingAgentItems.find(
      (item): item is Extract<AgentChatItem, {
        readonly kind: "assistant" | "thinking" | "compaction";
      }> =>
        item.kind === "assistant"
        || item.kind === "thinking"
        || item.kind === "compaction",
    )?.turn ?? currentTurn;
    const id = `agent:${first.id}`;
    const agentItems = Object.freeze([...pendingAgentItems]);
    units.push(Object.freeze({ id, kind: "agent", turn, items: agentItems }));
    for (const item of agentItems) unitIdForItem.set(item.id, id);
    lastAgentTurn = turn;
    lastAgentUnitId = id;
    pendingAgentItems = [];
  };

  const flushStatus = (): void => {
    const item = pendingStatus;
    if (item === undefined) return;
    pendingStatus = undefined;
    if (
      item.state.kind === "running"
      || (item.state.kind === "completed" && lastAgentTurn === item.turn)
    ) {
      if (lastAgentUnitId !== undefined && lastAgentTurn === item.turn) {
        unitIdForItem.set(item.id, lastAgentUnitId);
      }
      return;
    }
    const id = `status:${item.id}`;
    units.push(Object.freeze({ id, kind: "status", item }));
    unitIdForItem.set(item.id, id);
  };

  for (const item of items) {
    if (item.kind === "user") {
      flushAgent();
      if (pendingStatus !== undefined && pendingStatus.turn !== item.turn) {
        flushStatus();
      }
      currentTurn = item.turn;
      const id = `user:${item.id}`;
      units.push(Object.freeze({
        id,
        kind: "user",
        divider: units.length > 0,
        item,
      }));
      unitIdForItem.set(item.id, id);
      continue;
    }
    if (isAgentItem(item)) {
      pendingAgentItems.push(item);
      if (
        item.kind === "assistant"
        || item.kind === "thinking"
        || item.kind === "compaction"
      ) currentTurn = item.turn;
      continue;
    }

    flushAgent();
    flushStatus();
    currentTurn = item.turn;
    pendingStatus = item;
  }
  flushAgent();
  flushStatus();

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

const argumentPath = (args: unknown): string | undefined => {
  if (args === null || typeof args !== "object") return undefined;
  const object = args as Record<string, unknown>;
  const value = object["file_path"] ?? object["path"];
  return typeof value === "string" && value !== "" ? value : undefined;
};

const plural = (count: number, one: string, many: string): string =>
  `${count} ${count === 1 ? one : many}`;

/** Match the Slint activity-group sentence used for consecutive work items. */
export const activityGroupSummary = (items: readonly AgentActivityItem[]): string => {
  const edited = new Set<string>();
  const read = new Set<string>();
  let editsWithoutPath = 0;
  let readsWithoutPath = 0;
  let commands = 0;
  let tools = 0;
  let thoughts = 0;

  for (const item of items) {
    if (item.kind === "thinking") {
      thoughts += 1;
      continue;
    }
    if (item.kind !== "tool") continue;
    const effective = nestedMcpTool(item);
    const base = effective.tool.split("__").at(-1) ?? effective.tool;
    const path = argumentPath(effective.args);
    if ([
      "edit", "Edit", "MultiEdit", "NotebookEdit", "Write", "write",
      "edit_file", "write_file", "create_file", "apply_patch", "delete", "delete_file",
    ].includes(base)) {
      if (path === undefined) editsWithoutPath += 1;
      else edited.add(path);
    } else if (["read", "Read", "read_file"].includes(base)) {
      if (path === undefined) readsWithoutPath += 1;
      else read.add(path);
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
  if (commands > 0) parts.push(`ran ${plural(commands, "command", "commands")}`);
  if (tools > 0) parts.push(`called ${plural(tools, "tool", "tools")}`);
  if (thoughts > 0) parts.push(`thought ${plural(thoughts, "time", "times")}`);
  const summary = parts.join(", ");
  return summary === "" ? "Worked" : `${summary[0]?.toUpperCase() ?? ""}${summary.slice(1)}`;
};
