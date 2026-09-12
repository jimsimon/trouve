import type { ChatPreferences } from "../services/chat-preferences.js";
import {
  isContextCompactionTool,
  type AgentActivityItem,
  type AgentChatItem,
} from "./chat-layout.js";

/** One rendered row of a turn body, as a half-open range over the turn's
 * agent items. The renderer walks these spans instead of the raw items so
 * a turn can be split into several virtual rows without changing how any
 * individual row is grouped or drawn. */
export type AgentBodySpan =
  /** A standalone rail node (steered prompt, subagent, artifacts, questions).
   * Ends any pending activity timeline. */
  | { readonly kind: "node"; readonly start: number; readonly end: number }
  /** Consecutive assistant parts drawn as one response/progress block. */
  | { readonly kind: "assistant"; readonly start: number; readonly end: number }
  /** Harness-authored progress that is the turn's answer, drawn as a text
   * block rather than an activity row. */
  | { readonly kind: "progress-response"; readonly start: number; readonly end: number }
  /** A top-level compaction boundary (native or legacy tool form). */
  | {
      readonly kind: "compaction";
      readonly start: number;
      readonly end: number;
      readonly legacy: boolean;
      /** Whether activity rows follow, so the marker connects downward. */
      readonly connectAfter: boolean;
    }
  /** A legacy compaction tool shadowed by a native marker; renders nothing. */
  | {
      readonly kind: "skip";
      readonly start: number;
      readonly end: number;
      readonly flush: boolean;
    }
  /** One row inside an activity timeline. */
  | {
      readonly kind: "activity";
      readonly start: number;
      readonly end: number;
      readonly activity:
        | "progress"
        | "thinking"
        | "todo"
        | "todo-group"
        | "approval"
        | "tool"
        | "run";
    };

const toolCallNeedsApproval = (item: AgentActivityItem): boolean =>
  item.kind === "tool" && item.status === "awaiting-approval";

/** Legacy clients represented the same boundary as a synthetic tool directly
 * before the native lifecycle marker. Only that adjacent pair is redundant;
 * another legacy boundary in the turn remains independently visible. */
const legacyCompactionIsShadowed = (
  items: readonly AgentChatItem[],
  index: number,
): boolean =>
  items[index]?.kind === "tool"
  && isContextCompactionTool(items[index] as AgentActivityItem)
  && items[index + 1]?.kind === "compaction";

const activityFollows = (
  items: readonly AgentChatItem[],
  start: number,
  responseId: string | undefined,
): boolean => {
  for (let cursor = start; cursor < items.length; cursor += 1) {
    const candidate = items[cursor];
    if (candidate === undefined) return false;
    if (candidate.kind === "tool" && isContextCompactionTool(candidate)) {
      if (legacyCompactionIsShadowed(items, cursor)) continue;
      return false;
    }
    return (candidate.kind === "progress" && candidate.id !== responseId)
      || candidate.kind === "thinking"
      || candidate.kind === "todo"
      || candidate.kind === "tool";
  }
  return false;
};

/** Items that belong to a `run` span, excluding legacy compaction tools that a
 * native marker already represents. */
export const activityRunItems = (
  items: readonly AgentChatItem[],
  span: { readonly start: number; readonly end: number },
): AgentActivityItem[] => {
  const run: AgentActivityItem[] = [];
  for (let index = span.start; index < span.end; index += 1) {
    const candidate = items[index];
    if (candidate === undefined) continue;
    if (
      candidate.kind === "tool"
      && isContextCompactionTool(candidate)
      && legacyCompactionIsShadowed(items, index)
    ) continue;
    run.push(candidate as AgentActivityItem);
  }
  return run;
};

/** Rendered-row budget per virtual row. Each item in a span costs one row
 * because expanded groups mount one card per item. */
export const TURN_SEGMENT_TARGET_ITEMS = 40;

/** Longest consecutive activity run kept as one collapsible group. Bounded by
 * the segment budget so no single span can exceed one virtual row. */
export const MAX_ACTIVITY_RUN_ITEMS = TURN_SEGMENT_TARGET_ITEMS;

/** Segment a turn's agent items into render spans using the same grouping
 * rules as the transcript renderer. Pure over items, collapse preferences,
 * and the turn's response item (see `turnResponseItemId`); disclosure state
 * only affects how a span is drawn, never where it starts. */
export const planAgentBody = (
  items: readonly AgentChatItem[],
  collapse: ChatPreferences,
  responseId?: string,
): readonly AgentBodySpan[] => {
  const spans: AgentBodySpan[] = [];
  let index = 0;
  while (index < items.length) {
    const item = items[index];
    if (item === undefined) break;
    if (
      item.kind === "steered"
      || item.kind === "subagent"
      || item.kind === "artifacts"
      || item.kind === "questions"
    ) {
      spans.push({ kind: "node", start: index, end: index + 1 });
      index += 1;
      continue;
    }
    if (item.kind === "assistant") {
      const start = index;
      while (index < items.length && items[index]?.kind === "assistant") index += 1;
      spans.push({ kind: "assistant", start, end: index });
      continue;
    }
    if (item.kind === "progress") {
      // The turn ended on harness-authored progress with no answer text
      // after it, so that progress is the answer the user received.
      const response = item.id === responseId && item.content !== "";
      spans.push(response
        ? { kind: "progress-response", start: index, end: index + 1 }
        : { kind: "activity", activity: "progress", start: index, end: index + 1 });
      index += 1;
      continue;
    }
    if (item.kind === "compaction" && !collapse.collapseCompactionWithTools) {
      spans.push({
        kind: "compaction",
        legacy: false,
        start: index,
        end: index + 1,
        connectAfter: activityFollows(items, index + 1, responseId),
      });
      index += 1;
      continue;
    }
    if (item.kind === "tool" && isContextCompactionTool(item)) {
      if (legacyCompactionIsShadowed(items, index)) {
        spans.push({
          kind: "skip",
          start: index,
          end: index + 1,
          flush: !collapse.collapseCompactionWithTools,
        });
        index += 1;
        continue;
      }
      if (!collapse.collapseCompactionWithTools) {
        spans.push({
          kind: "compaction",
          legacy: true,
          start: index,
          end: index + 1,
          connectAfter: activityFollows(items, index + 1, responseId),
        });
        index += 1;
        continue;
      }
    }
    if (item.kind === "thinking" && !collapse.collapseThinkingWithTools) {
      spans.push({ kind: "activity", activity: "thinking", start: index, end: index + 1 });
      index += 1;
      continue;
    }
    if (item.kind === "todo" && !collapse.collapseTodoUpdatesWithTools) {
      let nextIndex = index + 1;
      while (nextIndex < items.length) {
        const candidate = items[nextIndex];
        if (candidate?.kind !== "todo" || candidate.state !== item.state) break;
        nextIndex += 1;
      }
      spans.push({
        kind: "activity",
        activity: nextIndex - index === 1 ? "todo" : "todo-group",
        start: index,
        end: nextIndex,
      });
      index = nextIndex;
      continue;
    }
    if (toolCallNeedsApproval(item)) {
      spans.push({ kind: "activity", activity: "approval", start: index, end: index + 1 });
      index += 1;
      continue;
    }
    if (item.kind === "tool" && !collapse.collapseSequentialToolCalls) {
      spans.push({ kind: "activity", activity: "tool", start: index, end: index + 1 });
      index += 1;
      continue;
    }
    const start = index;
    while (index < items.length) {
      const candidate = items[index];
      if (
        candidate === undefined
        || candidate.kind === "assistant"
        || candidate.kind === "artifacts"
        || candidate.kind === "steered"
        || candidate.kind === "subagent"
        || candidate.kind === "questions"
        || candidate.kind === "progress"
        || (!collapse.collapseCompactionWithTools && candidate.kind === "compaction")
        || (!collapse.collapseCompactionWithTools
          && candidate.kind === "tool"
          && isContextCompactionTool(candidate))
        || (!collapse.collapseThinkingWithTools && candidate.kind === "thinking")
        || (!collapse.collapseTodoUpdatesWithTools && candidate.kind === "todo")
        || (candidate.kind === "tool" && toolCallNeedsApproval(candidate))
      ) break;
      index += 1;
    }
    if (index === start) {
      // Defensive: every kind above either advanced or matched a branch.
      index += 1;
      continue;
    }
    // An expanded group mounts one card per item, so an unbounded run would
    // put a whole tool stream into a single virtual row. Chunk from the
    // start so existing groups keep their identity while a turn streams.
    for (let chunk = start; chunk < index; chunk += MAX_ACTIVITY_RUN_ITEMS) {
      spans.push({
        kind: "activity",
        activity: "run",
        start: chunk,
        end: Math.min(index, chunk + MAX_ACTIVITY_RUN_ITEMS),
      });
    }
  }
  return spans;
};

/** A contiguous slice of a turn's body spans rendered as one virtual row. */
export interface TurnSegment {
  readonly index: number;
  /** Half-open range over the turn's `planAgentBody` spans. */
  readonly spanStart: number;
  readonly spanEnd: number;
  /** Half-open range over the turn's agent items covered by those spans. */
  readonly itemStart: number;
  readonly itemEnd: number;
  readonly first: boolean;
  readonly last: boolean;
}

const cutAllowedBetween = (before: AgentBodySpan, after: AgentBodySpan): boolean =>
  before.kind !== "compaction"
  && before.kind !== "skip"
  && after.kind !== "compaction"
  && after.kind !== "skip";

/** Split a turn into virtual-row segments. Cuts fall only between spans, never
 * adjacent to a compaction boundary (whose rail connections read across
 * neighbours), so each segment renders exactly the rows it would have
 * rendered as part of the whole turn. */
export const segmentTurnSpans = (
  spans: readonly AgentBodySpan[],
  itemCount: number,
  targetItems = TURN_SEGMENT_TARGET_ITEMS,
): readonly TurnSegment[] => {
  const ranges: Array<{ spanStart: number; spanEnd: number }> = [];
  let spanStart = 0;
  let weight = 0;
  for (let index = 0; index < spans.length; index += 1) {
    const span = spans[index];
    if (span === undefined) continue;
    const spanWeight = span.kind === "skip" ? 0 : span.end - span.start;
    const next = spans[index + 1];
    weight += spanWeight;
    if (
      next !== undefined
      && weight >= targetItems
      && cutAllowedBetween(span, next)
    ) {
      ranges.push({ spanStart, spanEnd: index + 1 });
      spanStart = index + 1;
      weight = 0;
    }
  }
  ranges.push({ spanStart, spanEnd: spans.length });
  return ranges.map((range, index) => {
    const firstSpan = spans[range.spanStart];
    const lastSpan = spans[range.spanEnd - 1];
    const first = index === 0;
    const last = index === ranges.length - 1;
    return {
      index,
      spanStart: range.spanStart,
      spanEnd: range.spanEnd,
      itemStart: first ? 0 : firstSpan?.start ?? 0,
      itemEnd: last ? itemCount : lastSpan?.end ?? itemCount,
      first,
      last,
    };
  });
};

export const turnSegmentId = (unitId: string, segmentIndex: number): string =>
  segmentIndex === 0 ? unitId : `${unitId}:seg:${segmentIndex}`;

/** Recover the owning unit id from a segment row id (identity for the first
 * segment, which keeps the unit id so bookmarks and find targets stay valid). */
export const turnSegmentUnitId = (rowId: string): string => {
  const marker = rowId.lastIndexOf(":seg:");
  return marker === -1 ? rowId : rowId.slice(0, marker);
};
