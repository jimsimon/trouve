import type { ThreadChatItem } from "../state/thread-view-model.js";
import { buildChatLayout } from "./chat-layout.js";

export interface ChatFindResult {
  readonly unitIds: readonly string[];
  readonly activeIndex: number;
}

const SEARCH_TEXT_LIMIT = 512 * 1024;
const SEARCH_NODE_LIMIT = 20_000;
const SEARCH_OPERATION_NODE_LIMIT = 20_000;

interface SearchBudget {
  remainingNodes: number;
  remainingItems: number;
}

interface SearchableTextResult {
  readonly text: string;
  readonly complete: boolean;
}

export interface ChatFindMatches {
  readonly unitIds: readonly string[];
  readonly incomplete: boolean;
}

interface CachedSearchableItem {
  readonly revision: readonly unknown[];
  readonly text: string;
}

interface SearchableItemState {
  readonly revision: readonly unknown[];
  readonly cachedText: string | undefined;
}

const searchableItemCache = new WeakMap<object, CachedSearchableItem>();

const searchableText = (
  value: unknown,
  budget: Pick<SearchBudget, "remainingNodes">,
): SearchableTextResult => {
  const parts: string[] = [];
  const seen = new Set<object>();
  let length = 0;
  let visited = 0;
  let complete = true;
  const append = (value: string): void => {
    const remaining = SEARCH_TEXT_LIMIT - length;
    if (value.length > remaining) complete = false;
    const text = value.slice(0, remaining);
    parts.push(text);
    length += text.length + 1;
  };
  const visit = (candidate: unknown, depth: number): void => {
    if (length >= SEARCH_TEXT_LIMIT || depth > 12) {
      complete = false;
      return;
    }
    if (candidate == null) return;
    if (visited >= SEARCH_NODE_LIMIT || budget.remainingNodes <= 0) {
      complete = false;
      return;
    }
    visited += 1;
    budget.remainingNodes -= 1;
    if (typeof candidate === "string") {
      append(candidate);
      return;
    }
    if (typeof candidate === "boolean") {
      append(String(candidate));
      return;
    }
    if (typeof candidate === "number" && Number.isFinite(candidate)) {
      append(String(candidate));
      return;
    }
    if (typeof candidate !== "object" || seen.has(candidate)) return;
    seen.add(candidate);
    if (Array.isArray(candidate)) {
      for (const item of candidate) {
        visit(item, depth + 1);
        if (
          length >= SEARCH_TEXT_LIMIT
          || visited >= SEARCH_NODE_LIMIT
          || budget.remainingNodes <= 0
        ) {
          complete = false;
          break;
        }
      }
      return;
    }
    for (const key in candidate) {
      if (!Object.hasOwn(candidate, key)) continue;
      visit(key, depth + 1);
      visit((candidate as Record<string, unknown>)[key], depth + 1);
      if (
        length >= SEARCH_TEXT_LIMIT
        || visited >= SEARCH_NODE_LIMIT
        || budget.remainingNodes <= 0
      ) {
        complete = false;
        break;
      }
    }
  };
  visit(value, 0);
  return { text: parts.join("\n"), complete };
};

const searchableItemContent = (item: ThreadChatItem): unknown => {
  switch (item.kind) {
    case "user":
    case "steered":
      return [
        item.content,
        item.attachments.map((attachment) => [
          attachment.name,
          attachment.mime,
          attachment.size_bytes,
        ]),
      ];
    case "assistant":
    case "progress":
    case "thinking":
      return item.content;
    case "subagent":
      return [item.prompt, item.model];
    case "compaction":
      return item.state.kind === "completed"
        ? ["context compaction completed", item.state.messagesCompacted]
        : [`context compaction ${item.state.kind}`];
    case "todo":
      return [item.content, item.state];
    case "tool":
      return [
        item.tool,
        item.args,
        item.result,
        item.output.text,
        item.status,
        item.durationMs,
      ];
    case "turn-status": {
      const state = item.state;
      if (state.kind === "failed") return ["turn failed", state.error];
      if (state.kind === "completed" || state.kind === "running") {
        const usage = state.usage;
        return usage === undefined
          ? [`turn ${state.kind}`]
          : [
              `turn ${state.kind}`,
              `input tokens ${usage.input_tokens}`,
              `output tokens ${usage.output_tokens}`,
              usage.cached_input_tokens == null
                ? undefined
                : `cached input tokens ${usage.cached_input_tokens}`,
              usage.cost_usd == null ? undefined : `cost ${usage.cost_usd}`,
            ];
      }
      return [`turn ${state.kind}`];
    }
    case "questions":
      return [
        item.title,
        item.questions.map((question) => [
          question.prompt,
          question.options.map((option) => option.label),
        ]),
        item.answers?.map((answer) => answer.other_text) ?? [],
      ];
  }
};

const searchableItemRevision = (item: ThreadChatItem): readonly unknown[] => {
  switch (item.kind) {
    case "user":
    case "steered":
      return [item.content, item.attachments];
    case "assistant":
    case "progress":
    case "thinking":
      return [item.content];
    case "subagent":
      return [item.prompt, item.model];
    case "compaction":
    case "turn-status":
      return [item.state];
    case "todo":
      return [item.content, item.state];
    case "tool":
      return [item.tool, item.args, item.result, item.output.text, item.status, item.durationMs];
    case "questions":
      return [item.title, item.questions, item.answers];
  }
};

const sameRevision = (left: readonly unknown[], right: readonly unknown[]): boolean =>
  left.length === right.length && left.every((value, index) => Object.is(value, right[index]));

const searchableItemState = (item: ThreadChatItem): SearchableItemState => {
  const revision = searchableItemRevision(item);
  const cached = searchableItemCache.get(item);
  return {
    revision,
    cachedText: cached !== undefined && sameRevision(cached.revision, revision)
      ? cached.text
      : undefined,
  };
};

const searchableItemText = (
  item: ThreadChatItem,
  budget: SearchBudget,
  state: SearchableItemState,
): SearchableTextResult => {
  if (state.cachedText !== undefined) return { text: state.cachedText, complete: true };
  const remainingItems = Math.max(1, budget.remainingItems);
  budget.remainingItems = Math.max(0, budget.remainingItems - 1);
  const allowance = Math.min(
    SEARCH_NODE_LIMIT,
    Math.floor(budget.remainingNodes / remainingItems),
  );
  if (allowance <= 0) return { text: "", complete: false };
  const itemBudget = { remainingNodes: allowance };
  const { text, complete } = searchableText(searchableItemContent(item), itemBudget);
  budget.remainingNodes -= allowance - itemBudget.remainingNodes;
  if (complete) searchableItemCache.set(item, { revision: state.revision, text });
  return { text, complete };
};

const searchableUnitText = (
  prompt: ThreadChatItem | undefined,
  items: readonly ThreadChatItem[],
  status: ThreadChatItem | undefined,
  budget: SearchBudget,
  states: ReadonlyMap<ThreadChatItem, SearchableItemState>,
): SearchableTextResult => {
  const parts: string[] = [];
  let length = 0;
  let complete = true;
  const append = (item: ThreadChatItem | undefined): void => {
    if (item === undefined) return;
    if (length >= SEARCH_TEXT_LIMIT) {
      complete = false;
      if (states.get(item)?.cachedText === undefined) {
        budget.remainingItems = Math.max(0, budget.remainingItems - 1);
      }
      return;
    }
    const state = states.get(item);
    if (state === undefined) throw new Error("missing chat find item state");
    const result = searchableItemText(item, budget, state);
    if (!result.complete) complete = false;
    const text = result.text.slice(0, SEARCH_TEXT_LIMIT - length);
    if (text.length < result.text.length) complete = false;
    parts.push(text);
    length += text.length + 1;
  };
  append(prompt);
  for (const item of items) append(item);
  append(status);
  return { text: parts.join("\n"), complete };
};

/** Literal, per-turn transcript matches in display order. */
export const chatFindMatches = (
  items: readonly ThreadChatItem[],
  query: string,
  caseSensitive: boolean,
): ChatFindMatches => {
  const needle = query.trim();
  if (needle === "") {
    return Object.freeze({ unitIds: Object.freeze([]), incomplete: false });
  }
  const expected = caseSensitive ? needle : needle.toLowerCase();
  const units = buildChatLayout(items).units;
  const states = new Map<ThreadChatItem, SearchableItemState>();
  let cacheMissCount = 0;
  const recordItem = (item: ThreadChatItem | undefined): void => {
    if (item === undefined || states.has(item)) return;
    const state = searchableItemState(item);
    states.set(item, state);
    if (state.cachedText === undefined) cacheMissCount += 1;
  };
  for (const unit of units) {
    recordItem(unit.prompt);
    for (const item of unit.items) recordItem(item);
    recordItem(unit.status);
  }
  const budget = {
    remainingNodes: SEARCH_OPERATION_NODE_LIMIT,
    remainingItems: cacheMissCount,
  };
  const unitIds: string[] = [];
  let incomplete = false;
  for (const unit of units) {
    const result = searchableUnitText(unit.prompt, unit.items, unit.status, budget, states);
    if (!result.complete) incomplete = true;
    const text = caseSensitive ? result.text : result.text.toLowerCase();
    if (text.includes(expected)) unitIds.push(unit.id);
  }
  return Object.freeze({ unitIds: Object.freeze(unitIds), incomplete });
};

export const chatFindUnitIds = (
  items: readonly ThreadChatItem[],
  query: string,
  caseSensitive: boolean,
): readonly string[] => chatFindMatches(items, query, caseSensitive).unitIds;

/** Preserve the active turn across streaming recomputes when it still matches. */
export const reconcileChatFind = (
  unitIds: readonly string[],
  activeUnitId: string | undefined,
  resetActive = false,
): ChatFindResult => {
  const preserved = resetActive || activeUnitId === undefined
    ? -1
    : unitIds.indexOf(activeUnitId);
  return Object.freeze({
    unitIds: Object.freeze([...unitIds]),
    activeIndex: preserved >= 0 ? preserved : unitIds.length === 0 ? -1 : 0,
  });
};

export const stepChatFindIndex = (
  matchCount: number,
  activeIndex: number,
  delta: number,
): number => {
  if (matchCount <= 0) return -1;
  const current = activeIndex >= 0 && activeIndex < matchCount ? activeIndex : 0;
  return ((current + delta) % matchCount + matchCount) % matchCount;
};
