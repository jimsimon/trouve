import type { ThreadChatItem } from "../state/thread-view-model.js";
import { buildChatLayout } from "./chat-layout.js";

export interface ChatFindResult {
  readonly unitIds: readonly string[];
  readonly activeIndex: number;
}

const SEARCH_TEXT_LIMIT = 512 * 1024;

const searchableText = (value: unknown): string => {
  const parts: string[] = [];
  const seen = new Set<object>();
  let length = 0;
  const visit = (candidate: unknown, depth: number): void => {
    if (length >= SEARCH_TEXT_LIMIT || depth > 12 || candidate == null) return;
    if (typeof candidate === "string") {
      const remaining = SEARCH_TEXT_LIMIT - length;
      const text = candidate.slice(0, remaining);
      parts.push(text);
      length += text.length + 1;
      return;
    }
    if (typeof candidate !== "object" || seen.has(candidate)) return;
    seen.add(candidate);
    if (Array.isArray(candidate)) {
      for (const item of candidate) visit(item, depth + 1);
      return;
    }
    for (const [key, item] of Object.entries(candidate)) {
      visit(key, depth + 1);
      visit(item, depth + 1);
    }
  };
  visit(value, 0);
  return parts.join("\n");
};

/** Literal, per-turn transcript matches in display order. */
export const chatFindUnitIds = (
  items: readonly ThreadChatItem[],
  query: string,
  caseSensitive: boolean,
): readonly string[] => {
  const needle = query.trim();
  if (needle === "") return Object.freeze([]);
  const expected = caseSensitive ? needle : needle.toLowerCase();
  return Object.freeze(
    buildChatLayout(items).units
      .filter((unit) => {
        const text = searchableText(unit);
        return (caseSensitive ? text : text.toLowerCase()).includes(expected);
      })
      .map((unit) => unit.id),
  );
};

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
