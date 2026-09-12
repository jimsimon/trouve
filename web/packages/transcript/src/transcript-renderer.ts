import { html, nothing } from "lit";

import {
  DEFAULT_CHAT_PREFERENCES,
  effectiveChatCollapsePreferences,
  sameChatCollapsePreferences,
  type ChatPreferences,
} from "./chat-preferences.js";
import type { ProtocolModelInfo } from "@trouve-ai/protocol/client";
import type {
  CompactionState,
  ThreadChatItem,
  TurnState,
} from "./thread-view-model.js";
import { TOOL_OUTPUT_OMITTED_MESSAGE } from "./tool-output.js";
import type {
  AgentActivityPresentation,
  RunningAgentActivityInput,
} from "./agent-activity-model.js";
import "./agent-activity.js";
import {
  activityRunItems,
  hasNativeCompactionMarker,
  planAgentBody,
  type AgentBodySpan,
  type TurnSegment,
} from "./agent-body-plan.js";
import {
  activityGroupSummary,
  isContextCompactionTool,
  type AgentActivityItem,
  type AgentChatItem,
  type ChatRenderUnit,
} from "./chat-layout.js";
import {
  assistantCopyText,
  collapsedChatPreview,
  copyActionLabel,
  copyChatText,
  formatAttachmentBytes,
  isImageAttachment,
  isVideoAttachment,
  protocolAttachmentPath,
  turnResponseItemId,
  type ChatCopyResult,
  type ChatPresentationIndex,
} from "./chat-presentation.js";
import { composerContextUsage } from "./composer-usage.js";
import {
  fontAwesomeIcon,
  type FontAwesomeIconName,
} from "@trouve-ai/ui-foundation/font-awesome-icon";
import "@trouve-ai/content-rendering/image-preview";
import { modelOptionLabel } from "./model-option-label.js";
import {
  QUESTION_SKIPPED_MESSAGE,
  QUESTION_SKIPPED_STATUS,
  resolvedQuestionSummary,
} from "./question-wizard.js";
import {
  presentToolCall,
  toolDetailText,
  toolExecutionMetadata,
  type ToolPresentation,
} from "./tool-presentation.js";
import {
  checkpointBoundaryAfterTurn,
  checkpointBoundaryBeforeTurn,
  type TurnCheckpointBoundary,
} from "./turn-checkpoint-actions.js";
import "./turn-metadata.js";

export type TranscriptToolItem = Extract<ThreadChatItem, { readonly kind: "tool" }>;
export type TranscriptQuestionsItem = Extract<ThreadChatItem, { readonly kind: "questions" }>;
export type TranscriptSubagentItem = Extract<AgentChatItem, { readonly kind: "subagent" }>;

interface AgentBodyPlanEntry {
  readonly items: readonly AgentChatItem[];
  readonly collapse: ChatPreferences;
  readonly responseId: string | undefined;
  readonly spans: readonly AgentBodySpan[];
}

/**
 * Optional right-click handling for assistant text blocks. The desktop thread
 * screen supplies its custom copy menu; read-only hosts leave the browser's
 * native menu in place.
 */
export interface TranscriptMarkdownContextMenu {
  readonly capture: (event: MouseEvent) => void;
  readonly open: (event: MouseEvent, markdown: string) => void;
}

/**
 * Interactive affordances that only a host with write access to the thread
 * can honour. Read-only hosts omit them and the renderer falls back to plain
 * separators, approval status text, and a static question card.
 */
export interface TranscriptInteractiveControls {
  renderCheckpointRule(boundary: TurnCheckpointBoundary, turnRunning: boolean): unknown;
  renderToolApprovalActions(item: TranscriptToolItem): unknown;
  toolKeydown(event: KeyboardEvent, callId: string): void;
  renderPendingQuestions(item: TranscriptQuestionsItem): unknown;
}

/**
 * What a component must provide to render a thread transcript. Everything
 * else the renderer needs (disclosure, copy feedback, deferred tool detail
 * loading, lazy viewer imports) is owned by the renderer itself.
 */
export interface TranscriptRendererHost {
  /** Element owning the transcript DOM; typed file/inspection events bubble from it. */
  readonly element: HTMLElement;
  readonly threadId: string;
  requestUpdate(): void;
  /** Re-render after a disclosure change. Virtualized hosts also re-pin the tail. */
  requestDisclosureUpdate(): void;
  chatPreferences(): ChatPreferences | undefined;
  availableModels(): readonly ProtocolModelInfo[];
  /** Monotonic counter that changes whenever in-flight thread requests become stale. */
  interactionGeneration(): number;
  isCurrentInteraction(threadId: string, generation: number): boolean;
  findTool(callId: string): TranscriptToolItem | undefined;
  /**
   * Fetch deferred tool details and merge them into the folded thread view.
   * Resolves without effect once the interaction is stale; rejects when the
   * merge no longer belongs to this thread view.
   */
  loadToolDetails(threadId: string, callId: string, generation: number): Promise<void>;
  openSubagent(item: TranscriptSubagentItem): void;
  readonly markdownContextMenu?: TranscriptMarkdownContextMenu;
  readonly interactive?: TranscriptInteractiveControls;
}

const boundedJson = (value: unknown): string => {
  let text: string;
  try {
    text = JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    text = "[unavailable result]";
  }
  return text.length <= 32_000 ? text : `${text.slice(0, 32_000)}\n… output truncated`;
};

export const toolStatusLabel = (status: TranscriptToolItem["status"]): string =>
  ({
    "awaiting-approval": "Approval needed",
    running: "Running",
    ok: "Completed",
    error: "Failed",
    denied: "Denied",
    aborted: "Aborted",
  } as const)[status];

const toolStatusIcon = (
  status: TranscriptToolItem["status"],
): FontAwesomeIconName =>
  ({
    "awaiting-approval": "pause",
    running: "spinner",
    ok: "check",
    error: "xmark",
    denied: "ban",
    aborted: "xmark",
  } as const)[status];

type ActivityGroupStatus = "awaiting-approval" | "running" | "ok" | "mixed" | "error";

const activityGroupStatusLabel = (status: ActivityGroupStatus): string =>
  ({
    "awaiting-approval": "Approval needed",
    running: "Running",
    ok: "Completed",
    mixed: "Mixed results",
    error: "Failed",
  } as const)[status];

export const agentTurnLabels = (
  models: ReadonlyMap<number, string> | undefined,
  thinkingLevels: ReadonlyMap<number, string> | undefined,
): ReadonlyMap<number, string> => {
  const labels = new Map<number, string>();
  for (const [turn, model] of models ?? []) {
    const thinkingLevel = thinkingLevels?.get(turn);
    labels.set(
      turn,
      thinkingLevel === undefined
        ? model
        : `${model} · ${modelOptionLabel(thinkingLevel)}`,
    );
  }
  return labels;
};

/** Activity label for a turn phase that has no explicit turn-control label. */
export const turnPhaseLabel = (phase: string | undefined): string | undefined => {
  switch (phase) {
    case "connecting_tools":
      return "Connecting tools…";
    case "waiting_for_subagents":
      return "Waiting for subagents…";
    default:
      return undefined;
  }
};

/** The item drawn as a unit's answer, which shapes its body plan. */
export const unitResponseItemId = (
  unit: ChatRenderUnit,
  presentation: ChatPresentationIndex,
): string | undefined =>
  turnResponseItemId(unit.items, unit.status?.state ?? presentation.turnStates.get(unit.turn));

/** The `turn-*` class every segment of a turn card shares. A cancelled turn
 * drops its `turn-status` item, so without a recorded state the body's live
 * activity decides whether the card still reads as running. */
const turnStateKind = (
  unit: ChatRenderUnit,
  turnState: TurnState | undefined,
): TurnState["kind"] => {
  if (turnState !== undefined) return turnState.kind;
  const activityRunning = unit.items.some((item) =>
    (item.kind === "assistant" || item.kind === "progress" || item.kind === "thinking")
      && !item.complete
    || item.kind === "compaction" && item.state.kind === "running"
    || item.kind === "tool" && (
      item.status === "running" || item.status === "awaiting-approval"
    )
    || item.kind === "questions" && item.answers === undefined
  );
  return activityRunning || unit.items.length === 0 ? "running" : "completed";
};

export const renderContextUsage = (
  contextUsage: ReturnType<typeof composerContextUsage>,
  additionalClass = "",
) =>
  html`<span
    class=${[
      "composer-context-usage",
      additionalClass,
      contextUsage.compacting ? "compacting" : "",
    ].filter(Boolean).join(" ")}
    role="img"
    aria-label=${contextUsage.label}
    title=${contextUsage.label}
  >
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <circle class="context-dial-track" cx="12" cy="12" r="9"></circle>
      <circle
        class="context-dial-value"
        cx="12"
        cy="12"
        r="9"
        pathLength="100"
        stroke-dasharray=${`${contextUsage.percent} 100`}
      ></circle>
    </svg>
    ${contextUsage.unavailable && !contextUsage.compacting
      ? fontAwesomeIcon("triangle-exclamation", {
          className: "context-dial-glyph",
        })
      : nothing}
  </span>`;

export const renderQuestionSummary = (
  summary: readonly { readonly prompt: string; readonly answer: string }[],
) =>
  html`
    <dl class="question-summary">
      ${summary.map((entry) => html`
        <div>
          <dt>${entry.prompt}</dt>
          <dd>${entry.answer}</dd>
        </div>
      `)}
    </dl>
  `;

/**
 * Renders folded thread items (turn cards, prompts, reasoning, progress,
 * tool calls, activity groups, attachments) into the shared transcript
 * markup, independent of how the host sources or scrolls the thread.
 */
export class TranscriptRenderer {
  readonly #host: TranscriptRendererHost;
  #copyFeedbackGeneration = 0;
  readonly #copyFeedback = new Map<string, ChatCopyResult>();
  readonly #messageDisclosure = new Map<string, boolean>();
  readonly #rawToolCalls = new Set<string>();
  readonly #toolDisclosure = new Map<string, boolean>();
  readonly #toolDetailLoading = new Set<string>();
  readonly #toolDetailErrors = new Map<string, string>();
  #markdownRequested = false;
  #toolDetailRequested = false;
  #toolDetailLoadFailed = false;
  /** Body spans per unit id, validated against the unit's items, response
   * item, and collapse preferences on every read. Hosts plan rows from the
   * same entries the body renderer draws, so a turn is planned once per
   * transcript revision rather than once per mounted segment. */
  readonly #bodyPlans = new Map<string, AgentBodyPlanEntry>();

  constructor(host: TranscriptRendererHost) {
    this.#host = host;
  }

  /** Forget per-thread presentation state when the host switches threads. */
  reset(): void {
    this.#copyFeedbackGeneration += 1;
    this.#copyFeedback.clear();
    this.#messageDisclosure.clear();
    this.#rawToolCalls.clear();
    this.#toolDisclosure.clear();
    this.#toolDetailLoading.clear();
    this.#toolDetailErrors.clear();
    this.#bodyPlans.clear();
  }

  /** Drop pending copy feedback timers' effects, e.g. when the host disconnects. */
  invalidateCopyFeedback(): void {
    this.#copyFeedbackGeneration += 1;
  }

  isUnitOpen(unitId: string): boolean {
    return this.#messageDisclosure.get(unitId) ?? true;
  }

  /** Whether a turn card shows its body: expanded, or pinned open while its
   * context is compacting. */
  isTurnCardOpen(unit: ChatRenderUnit | undefined): boolean {
    if (unit === undefined) return false;
    if (this.isUnitOpen(unit.id)) return true;
    return unit.items.some(
      (item) => item.kind === "compaction" && item.state.kind === "running",
    );
  }

  /** Effective collapse preferences for the current host settings. */
  collapsePreferences(): ChatPreferences {
    return effectiveChatCollapsePreferences(
      this.#host.chatPreferences() ?? DEFAULT_CHAT_PREFERENCES,
    );
  }

  /** Body spans for one unit, computed once per items identity, response
   * item, and collapse-preference set. Virtualized hosts segment these spans
   * into rows; the body renderer walks the same spans when drawing. */
  planTurnBody(
    unit: ChatRenderUnit,
    presentation: ChatPresentationIndex,
    collapse: ChatPreferences = this.collapsePreferences(),
  ): readonly AgentBodySpan[] {
    const cached = this.#bodyPlans.get(unit.id);
    const responseId = unitResponseItemId(unit, presentation);
    if (
      cached !== undefined
      && cached.items === unit.items
      && cached.responseId === responseId
      && sameChatCollapsePreferences(cached.collapse, collapse)
    ) {
      return cached.spans;
    }
    const spans = planAgentBody(unit.items, collapse, responseId);
    this.#bodyPlans.set(unit.id, { items: unit.items, collapse, responseId, spans });
    return spans;
  }

  /**
   * Render one unit, or one virtual-row segment of it. Hosts that mount a
   * long turn as several rows pass the segment; a whole-turn host passes
   * `undefined` and gets the header, body, and tail in one card.
   */
  renderUnit(
    unit: ChatRenderUnit,
    segment: TurnSegment | undefined,
    turnLabels: ReadonlyMap<number, string>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityPresentation: AgentActivityPresentation | undefined,
    activityInput: RunningAgentActivityInput | undefined,
    checkpointRestoreDisabled: boolean,
    finalUnit: boolean,
  ) {
    // A collapsed turn mounts only its first segment, so the checkpoint
    // boundary after the turn has to render on whichever segment is last
    // on screen rather than the last one planned.
    const lastMounted = segment === undefined || segment.last || !this.isTurnCardOpen(unit);
    const trailingBoundary = lastMounted
      ? checkpointBoundaryAfterTurn(unit.turn, presentation.turnStates)
      : undefined;
    return html`
      ${unit.divider && (segment === undefined || segment.first)
        ? this.#renderTurnRule(
            unit.turn,
            presentation,
            checkpointRestoreDisabled,
          )
        : nothing}
      ${this.#renderTurnCard(
        unit,
        segment,
        turnLabels,
        turnModels,
        turnDurationMs,
        presentation,
        activityPresentation,
        activityInput,
      )}
      ${finalUnit && trailingBoundary !== undefined
        ? this.#renderCheckpointRule(trailingBoundary, checkpointRestoreDisabled)
        : nothing}
    `;
  }

  #renderTurnRule(
    nextTurn: number,
    presentation: ChatPresentationIndex,
    turnRunning: boolean,
  ) {
    const boundary = checkpointBoundaryBeforeTurn(nextTurn, presentation.turnStates);
    if (boundary === undefined) {
      return html`<div class="turn-rule" role="separator"></div>`;
    }
    return this.#renderCheckpointRule(boundary, turnRunning);
  }

  #renderCheckpointRule(
    boundary: TurnCheckpointBoundary,
    turnRunning: boolean,
  ) {
    const interactive = this.#host.interactive;
    if (interactive === undefined) {
      return html`<div class="turn-rule" role="separator"></div>`;
    }
    return interactive.renderCheckpointRule(boundary, turnRunning);
  }

  #renderTurnCard(
    unit: ChatRenderUnit,
    segment: TurnSegment | undefined,
    turnLabels: ReadonlyMap<number, string>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityPresentation: AgentActivityPresentation | undefined,
    activityInput: RunningAgentActivityInput | undefined,
  ) {
    this.ensureMarkdown();
    if (segment !== undefined && !segment.first) {
      return this.#renderTurnCardContinuation(
        unit,
        segment,
        presentation,
        activityPresentation,
        activityInput,
      );
    }
    const assistantItems = unit.items.filter(
      (item): item is Extract<AgentChatItem, { readonly kind: "assistant" }> =>
        item.kind === "assistant",
    );
    const joined = assistantItems
      .map((item) => item.content)
      .filter((content) => content !== "")
      .join("\n\n");
    const compactionRunning = unit.items.some(
      (item) => item.kind === "compaction" && item.state.kind === "running",
    );
    const open = compactionRunning || (this.#messageDisclosure.get(unit.id) ?? true);
    const turnState = unit.status?.state ?? presentation.turnStates.get(unit.turn);
    const promptPreview = unit.prompt === undefined
      ? ""
      : collapsedChatPreview(assistantCopyText(unit.prompt.content))
        || `${unit.prompt.attachments.length} attachment${unit.prompt.attachments.length === 1 ? "" : "s"}`;
    const preview = promptPreview || collapsedChatPreview(joined) || `Turn ${unit.turn}`;
    const stateKind = turnStateKind(unit, turnState);
    const modelLabel = turnLabels.get(unit.turn);
    const modelId = turnModels.get(unit.turn);
    const model = this.#host.availableModels().find((candidate) => candidate.id === modelId);
    const usage = turnState?.kind === "running" || turnState?.kind === "completed"
      ? turnState.usage
      : undefined;
    const turnContextUsage = composerContextUsage(
      usage,
      model?.context_window,
      compactionRunning,
      modelId?.startsWith("codex/") ?? false,
    );
    const continues = open && segment !== undefined && !segment.last;
    return html`
      <article
        class=${`message turn-card assistant-message agent-turn-card conversation-turn turn-${stateKind}${
          continues ? " turn-segment-continues" : ""
        }`}
        aria-labelledby=${`turn-heading-${unit.id}`}
      >
        <header class="message-header agent-header turn-header ${open ? "" : "collapsed"}">
          <button
            class="message-disclosure"
            type="button"
            aria-expanded=${open ? "true" : "false"}
            aria-disabled=${compactionRunning ? "true" : "false"}
            aria-label=${open ? `Collapse turn ${unit.turn}` : `Expand turn ${unit.turn}`}
            title=${compactionRunning ? "The turn stays open while context is compacting" : ""}
            @click=${() => this.#toggleMessageDisclosure(unit.id, true, compactionRunning)}
          >
            ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
              className: "disclosure-icon",
            })}
            <strong id=${`turn-heading-${unit.id}`}>Turn ${unit.turn}</strong>
            <small class="agent-model-label">${modelLabel === undefined
              ? "Agent"
              : `Agent: ${modelLabel}`}</small>
            ${open
              ? html`<span class="agent-header-spacer"></span>`
              : html`<small class="agent-collapsed-preview">${preview}</small>`}
            <span class="turn-header-metadata-slot">
              ${this.#renderAgentTurnMetadata(
                turnState,
                turnDurationMs.get(unit.turn),
              )}
            </span>
            ${modelId === undefined
              ? nothing
              : renderContextUsage(turnContextUsage, "turn-context-usage")}
          </button>
        </header>
        ${open
          ? html`<div
              class="message-body turn-body-stream agent-body-stream turn-timeline"
            >
              ${unit.prompt === undefined ? nothing : this.#renderUserNode(unit.prompt)}
              ${this.#renderAgentBody(
                unit,
                presentation,
                segment,
              )}
              ${this.#renderTurnTail(unit, segment, activityPresentation, activityInput)}
            </div>`
          : nothing}
      </article>
    `;
  }

  /** Rows of an open turn after its first segment. They share the first
   * segment's card frame visually (see `.turn-segment-continuation`) but are
   * mounted and measured as independent virtual rows. */
  #renderTurnCardContinuation(
    unit: ChatRenderUnit,
    segment: TurnSegment,
    presentation: ChatPresentationIndex,
    activityPresentation: AgentActivityPresentation | undefined,
    activityInput: RunningAgentActivityInput | undefined,
  ) {
    const turnState = unit.status?.state ?? presentation.turnStates.get(unit.turn);
    const stateKind = turnStateKind(unit, turnState);
    return html`
      <article
        class=${`message turn-card assistant-message agent-turn-card conversation-turn turn-${stateKind} turn-segment-continuation${
          segment.last ? "" : " turn-segment-continues"
        }`}
        aria-label=${`Turn ${unit.turn} (continued)`}
      >
        <div class="message-body turn-body-stream agent-body-stream turn-timeline">
          ${this.#renderAgentBody(unit, presentation, segment)}
          ${this.#renderTurnTail(unit, segment, activityPresentation, activityInput)}
        </div>
      </article>
    `;
  }

  #renderTurnTail(
    unit: ChatRenderUnit,
    segment: TurnSegment | undefined,
    activityPresentation: AgentActivityPresentation | undefined,
    activityInput: RunningAgentActivityInput | undefined,
  ) {
    if (segment !== undefined && !segment.last) return nothing;
    return html`
      ${activityPresentation === undefined
        ? nothing
        : this.#renderTransientActivityNode(activityPresentation, activityInput)}
      ${unit.status === undefined
        ? nothing
        : this.#renderTerminalTurnState(unit.status)}
    `;
  }

  #renderUserNode(
    item: Extract<ThreadChatItem, { readonly kind: "user" | "steered" }>,
  ) {
    this.ensureMarkdown();
    const steered = item.kind === "steered";
    const background = item.kind === "user" && item.background;
    const label = steered ? "Steered" : background ? "Background activity" : "Prompt";
    return html`
      <section
        class=${`turn-rail-node turn-${steered ? "steered" : "prompt"}-node user-message`}
        data-chat-anchor-id=${`item:${item.id}`}
        aria-label=${label}
      >
        <span class=${`turn-rail-marker ${steered ? "steered" : "prompt"}`} aria-hidden="true">
          ${fontAwesomeIcon(steered ? "route" : background ? "gear" : "user")}
        </span>
        <header class="turn-node-header"><strong>${label}</strong></header>
        <div class="turn-node-body user-body-stream">
          ${item.content === "" || background
            ? nothing
            : html`<trouve-markdown-view
                .content=${item.content}
              ></trouve-markdown-view>`}
          ${this.#renderAttachments(item.attachments)}
        </div>
      </section>
    `;
  }

  #renderTerminalTurnState(
    item: Extract<ThreadChatItem, { readonly kind: "turn-status" }>,
  ) {
    if (item.state.kind !== "failed" && item.state.kind !== "cancelled") return nothing;
    const failed = item.state.kind === "failed";
    const detail = item.state.kind === "failed"
      ? item.state.error
      : "The active response was interrupted.";
    return html`
      <section
        class=${`turn-rail-node turn-state-node ${item.state.kind}`}
        role=${failed ? "alert" : "status"}
        aria-label=${failed ? "Turn failed" : "Turn cancelled"}
      >
        <span class=${`turn-rail-marker ${item.state.kind}`} aria-hidden="true">
          ${fontAwesomeIcon(failed ? "xmark" : "ban")}
        </span>
        <header class="turn-node-header">
          <strong>${failed ? "Turn failed" : "Turn cancelled"}</strong>
        </header>
        <p>${detail}</p>
      </section>
    `;
  }

  #renderAgentTurnMetadata(
    turnState: TurnState | undefined,
    completedDurationMs: number | undefined,
  ) {
    if (
      turnState?.kind !== "waiting-for-capacity"
      && turnState?.kind !== "running"
      && turnState?.kind !== "completed"
    ) {
      return nothing;
    }
    if (
      turnState.kind !== "completed" &&
      turnState.startedAt === undefined &&
      (turnState.kind !== "running" || turnState.usage === undefined)
    ) return nothing;
    const active = turnState.kind !== "completed";
    const usage = turnState.kind === "running" || turnState.kind === "completed"
      ? turnState.usage
      : undefined;
    return html`<small class="turn-metadata">
      <trouve-turn-metadata
        .usage=${usage}
        .running=${active}
        .startedAt=${active ? turnState.startedAt ?? "" : ""}
        .durationMs=${active ? undefined : completedDurationMs}
      ></trouve-turn-metadata>
    </small>`;
  }

  renderActivityRow(
    activity: AgentActivityPresentation,
    activityInput: RunningAgentActivityInput | undefined,
  ) {
    return html`<div class="activity-row agent-activity">
      <span class="activity-dots" aria-hidden="true"><i></i><i></i><i></i></span>
      <trouve-agent-activity
        .presentation=${activity}
        .input=${activityInput}
        variant="row"
      ></trouve-agent-activity>
    </div>`;
  }

  #renderTransientActivityNode(
    activity: AgentActivityPresentation,
    activityInput: RunningAgentActivityInput | undefined,
  ) {
    return html`
      <section class="turn-rail-node turn-transient-activity">
        <span class="turn-rail-marker transient" aria-hidden="true">
          ${fontAwesomeIcon("spinner", {
            className: "turn-transient-spinner",
            spin: true,
          })}
        </span>
        <trouve-agent-activity
          .presentation=${activity}
          .input=${activityInput}
          variant="transient"
        ></trouve-agent-activity>
      </section>
    `;
  }

  renderCompactionMarker(
    state: CompactionState,
    anchorId?: string,
    timelineConnections: {
      readonly before: boolean;
      readonly after: boolean;
      readonly nested?: boolean;
    } = { before: false, after: false },
  ) {
    const running = state.kind === "running";
    const completed = state.kind === "completed";
    const label = running
      ? "Compacting context"
      : completed
        ? "Context compacted"
        : "Context compaction stopped";
    const detail = running
      ? "Summarizing earlier messages to make room for this turn…"
      : completed
        ? state.messagesCompacted === 0
          ? "Earlier context summarized by the model harness"
          : `${state.messagesCompacted} earlier transcript ${state.messagesCompacted === 1 ? "message" : "messages"} summarized`
        : "Compaction did not report completion; the turn continued.";
    return html`
      <section
        class=${`context-compaction-marker ${state.kind} ${
          timelineConnections.before ? "timeline-connect-before" : ""
        } ${timelineConnections.after ? "timeline-connect-after" : ""} ${
          timelineConnections.nested ? "nested-timeline-marker" : ""
        }`}
        data-chat-anchor-id=${anchorId === undefined ? nothing : `item:${anchorId}`}
        role="status"
        aria-live="polite"
        aria-label=${`${label}. ${detail}`}
      >
        <span class="context-compaction-symbol">
          ${running
            ? html`<span class="context-compaction-spinner" aria-hidden="true"></span>`
            : fontAwesomeIcon(completed ? "check" : "triangle-exclamation", {
                className: "context-compaction-glyph",
              })}
        </span>
        <span class="context-compaction-copy">
          <strong>${label}</strong>
          <small>${detail}</small>
        </span>
      </section>
    `;
  }

  #renderSubagentNode(item: TranscriptSubagentItem) {
    const prompt = collapsedChatPreview(item.prompt) || "Subagent transcript";
    return html`
      <button
        class="turn-rail-node subagent-rail-item"
        type="button"
        data-chat-anchor-id=${`item:${item.id}`}
        aria-label=${`Open subagent transcript: ${prompt}`}
        title="Open subagent transcript"
        @click=${() => this.#host.openSubagent(item)}
      >
        <span class="turn-rail-marker subagent" aria-hidden="true">
          ${fontAwesomeIcon("users")}
        </span>
        <span class="subagent-rail-content">
          <span class="subagent-rail-heading">
            <strong>Subagent</strong>
            <small>${item.model}</small>
          </span>
          <span class="subagent-rail-prompt">${prompt}</span>
        </span>
        <span class="subagent-rail-open" aria-hidden="true">
          Open ${fontAwesomeIcon("arrow-up-right-from-square")}
        </span>
      </button>
    `;
  }

  #renderAgentBody(
    unit: ChatRenderUnit,
    presentation: ChatPresentationIndex,
    segment: TurnSegment | undefined,
  ) {
    const collapse = this.collapsePreferences();
    const collapseThinkingWithTools = collapse.collapseThinkingWithTools;
    const collapseCompactionWithTools = collapse.collapseCompactionWithTools;
    const collapseTodoUpdatesWithTools = collapse.collapseTodoUpdatesWithTools;
    const spans = this.planTurnBody(unit, presentation, collapse);
    const spanStart = segment?.spanStart ?? 0;
    const spanEnd = segment?.spanEnd ?? spans.length;
    const rows: unknown[] = [];
    let activityConnectedFromCompaction = false;
    let activityRows: Array<{
      readonly content: unknown;
      readonly expandedGroup: boolean;
      readonly endsWithExpandedToolGroup: boolean;
    }> = [];
    const flushActivityRows = (activityConnectedToCompaction = false): void => {
      if (activityRows.length === 0) return;
      const compactionConnected = activityConnectedFromCompaction
        || activityConnectedToCompaction;
      const timelineClass = `agent-activity-timeline ${
        activityRows.length === 1 ? "single-activity" : ""
      } ${activityRows.some(({ expandedGroup }) => expandedGroup)
        ? "has-expanded-group"
        : ""} ${!activityConnectedToCompaction
          && activityRows.at(-1)?.endsWithExpandedToolGroup === true
        ? "ends-with-expanded-tool-group"
        : ""} ${compactionConnected ? "compaction-connected-timeline" : ""}`;
      rows.push(html`<div class=${timelineClass}>${
        activityRows.map(({ content }) => content)
      }</div>`);
      activityRows = [];
      activityConnectedFromCompaction = false;
    };
    const turnState = unit.status?.state ?? presentation.turnStates.get(unit.turn);
    const responseId = turnResponseItemId(unit.items, turnState);
    const pushActivity = (
      content: unknown,
      expandedGroup = false,
      endsWithExpandedToolGroup = false,
    ): void => {
      activityRows.push({ content, expandedGroup, endsWithExpandedToolGroup });
    };
    const hasNativeCompaction = hasNativeCompactionMarker(unit.items);
    for (let spanIndex = spanStart; spanIndex < spanEnd; spanIndex += 1) {
      const span = spans[spanIndex];
      const item = span === undefined ? undefined : unit.items[span.start];
      if (span === undefined || item === undefined) continue;
      if (span.kind === "skip") {
        if (span.flush) flushActivityRows();
        continue;
      }
      if (span.kind === "node") {
        flushActivityRows();
        if (item.kind === "steered") {
          rows.push(this.#renderUserNode(item));
        } else if (item.kind === "subagent") {
          rows.push(this.#renderSubagentNode(item));
        } else if (item.kind === "artifacts") {
          rows.push(html`<section
            class="turn-rail-node turn-response-node agent-artifacts"
            data-chat-anchor-id=${`item:${item.id}`}
            aria-label="Agent attachments"
          >
            <span class="turn-rail-marker response complete" aria-hidden="true">
              ${fontAwesomeIcon("paperclip")}
            </span>
            <header class="turn-node-header"><strong>Attachments</strong></header>
            ${this.#renderAttachments(item.attachments, "Agent attachments")}
          </section>`);
        } else if (item.kind === "questions") {
          rows.push(this.#renderItem(item, presentation));
        }
        continue;
      }
      if (span.kind === "assistant") {
        flushActivityRows();
        const stretch = unit.items.slice(span.start, span.end) as Extract<
          AgentChatItem,
          { readonly kind: "assistant" }
        >[];
        const content = stretch.map((part) => part.content).filter(Boolean).join("\n\n");
        if (content !== "") {
          rows.push(this.#renderAgentTextNode(unit, {
            content,
            anchor: stretch.at(-1)?.id ?? stretch[0]?.id ?? unit.id,
            response: stretch.some((part) => part.id === responseId),
            streaming: stretch.some((part) => !part.complete),
            turnState,
          }));
        }
        continue;
      }
      if (span.kind === "progress-response") {
        if (item.kind !== "progress") continue;
        // The turn ended on harness-authored progress with no answer text
        // after it, so that progress is the answer the user received.
        flushActivityRows();
        rows.push(this.#renderAgentTextNode(unit, {
          content: item.content,
          anchor: item.id,
          response: true,
          streaming: !item.complete,
          turnState,
        }));
        continue;
      }
      if (span.kind === "compaction") {
        const connectBefore = activityRows.length > 0;
        flushActivityRows(connectBefore);
        const state = span.legacy && item.kind === "tool"
          ? this.#legacyCompactionState(item)
          : item.kind === "compaction"
            ? item.state
            : undefined;
        if (state !== undefined) {
          rows.push(this.renderCompactionMarker(state, item.id, {
            before: connectBefore,
            after: span.connectAfter,
          }));
        }
        activityConnectedFromCompaction = span.connectAfter;
        continue;
      }
      // Activity rows. Approval controls must remain directly reachable.
      // Running calls can join the same collapsed activity run as soon as
      // they are requested; the transient tail describes the current action
      // without adding a shifting top-level tool node for each parallel call.
      switch (span.activity) {
        case "progress":
          if (item.kind === "progress") pushActivity(this.#renderVisibleProgress(item));
          continue;
        case "thinking":
          if (item.kind === "thinking") pushActivity(this.#renderVisibleThinking(item));
          continue;
        case "todo":
          if (item.kind === "todo") pushActivity(this.#renderTodoUpdate(item));
          continue;
        case "todo-group": {
          const repeatedTodos = unit.items.slice(span.start, span.end) as Extract<
            AgentActivityItem,
            { readonly kind: "todo" }
          >[];
          pushActivity(
            this.#renderActivityGroup(unit, repeatedTodos, presentation),
            this.#activityGroupOpen(unit, repeatedTodos),
          );
          continue;
        }
        case "approval":
        case "tool":
          if (item.kind === "tool") pushActivity(this.#renderItem(item, presentation));
          continue;
        case "run":
          break;
      }
      const run = activityRunItems(unit.items, span, hasNativeCompaction);
      const only = run[0];
      const groupSinglePreferenceBoundary = run.length === 1 && (
        (collapseThinkingWithTools && only?.kind === "thinking")
        || (collapseTodoUpdatesWithTools && only?.kind === "todo")
        || (collapseCompactionWithTools && (
          only?.kind === "compaction"
          || (only?.kind === "tool" && isContextCompactionTool(only))
        ))
      );
      const groupSingleActiveTurnTool = run.length === 1
        && only?.kind === "tool"
        && unit.status?.state.kind === "running";
      if (
        run.length < 2
        && !groupSinglePreferenceBoundary
        && !groupSingleActiveTurnTool
      ) {
        if (only !== undefined) pushActivity(this.#renderItem(only, presentation));
        continue;
      }
      const expandedGroup = this.#activityGroupOpen(unit, run);
      const finalGroupedItem = run.at(-1);
      const endsWithCollapsedTool = finalGroupedItem?.kind === "tool"
        && !isContextCompactionTool(finalGroupedItem)
        && finalGroupedItem.status !== "awaiting-approval"
        && !(this.#toolDisclosure.get(finalGroupedItem.callId) ?? false);
      pushActivity(
        this.#renderActivityGroup(unit, run, presentation),
        expandedGroup,
        expandedGroup && endsWithCollapsedTool,
      );
    }
    flushActivityRows();
    return rows;
  }

  /** Top-level agent text node: the turn's response, or an interim update
   * when more answer text follows (or may still follow) it. */
  #renderAgentTextNode(
    unit: ChatRenderUnit,
    node: {
      readonly content: string;
      readonly anchor: string;
      readonly response: boolean;
      readonly streaming: boolean;
      readonly turnState: TurnState | undefined;
    },
  ) {
    const { content, anchor, response, streaming, turnState } = node;
    const tone = turnState?.kind === "failed"
      ? "failed"
      : turnState?.kind === "cancelled"
        ? "cancelled"
        : response && (streaming || turnState?.kind === "running")
          ? "running"
          : response
            ? "complete"
            : "update";
    const contextMenu = this.#host.markdownContextMenu;
    return html`<section
      class=${`turn-rail-node turn-response-node agent-text-block ${tone}`}
      data-chat-anchor-id=${`assistant:${anchor}`}
      aria-label=${response ? "Response" : "Agent progress"}
      @pointerdown=${contextMenu?.capture}
      @mousedown=${contextMenu?.capture}
      @contextmenu=${contextMenu === undefined
        ? undefined
        : (event: MouseEvent) => contextMenu.open(event, content)}
    >
      <span class=${`turn-rail-marker response ${tone}`} aria-hidden="true">
        ${fontAwesomeIcon("message")}
      </span>
      <header class="turn-node-header">
        <strong>${response ? "Response" : "Progress"}</strong>
        <span class="thinking-header-spacer"></span>
        <span class="agent-copy-action">
          ${this.#renderCopyButton(
            `agent:${unit.id}:${anchor}`,
            assistantCopyText(content),
            response ? "Copy assistant response" : "Copy assistant progress",
          )}
        </span>
      </header>
      <trouve-markdown-view
        .content=${content}
        .streaming=${streaming}
      ></trouve-markdown-view>
    </section>`;
  }

  #renderVisibleThinking(
    item: Extract<ThreadChatItem, { readonly kind: "thinking" }>,
  ) {
    this.ensureMarkdown();
    return html`
      <article
        class=${`message thinking-output ${item.complete ? "complete" : "running"}`}
        data-chat-anchor-id=${`item:${item.id}`}
      >
        <span class="thinking-rail-icon" aria-hidden="true">
          ${fontAwesomeIcon("brain")}
        </span>
        <header class="thinking-header">
          <strong>Reasoning</strong>
          <span class="thinking-header-spacer"></span>
          ${this.#renderCopyButton(
            `message:${item.id}`,
            item.content,
            "Copy reasoning",
          )}
        </header>
        <div class="thinking-body">
          <trouve-markdown-view
            .content=${item.content}
            .streaming=${!item.complete}
          ></trouve-markdown-view>
        </div>
      </article>
    `;
  }

  #renderVisibleProgress(
    item: Extract<ThreadChatItem, { readonly kind: "progress" }>,
  ) {
    this.ensureMarkdown();
    return html`
      <article
        class=${`message thinking-output progress-output ${item.complete ? "complete" : "running"}`}
        data-chat-anchor-id=${`item:${item.id}`}
      >
        <span class="thinking-rail-icon progress-rail-icon" aria-hidden="true">
          ${fontAwesomeIcon("message")}
        </span>
        <header class="thinking-header progress-header">
          <strong>Progress</strong>
          <span class="thinking-header-spacer"></span>
          ${this.#renderCopyButton(
            `message:${item.id}`,
            item.content,
            "Copy progress",
          )}
        </header>
        <div class="thinking-body progress-body">
          <trouve-markdown-view
            .content=${item.content}
            .streaming=${!item.complete}
          ></trouve-markdown-view>
        </div>
      </article>
    `;
  }

  #renderTodoUpdate(
    item: Extract<ThreadChatItem, { readonly kind: "todo" }>,
  ) {
    const presentation = {
      started: { icon: "play", label: "Started TODO" },
      completed: { icon: "check", label: "Completed TODO" },
      cancelled: { icon: "xmark", label: "Cancelled TODO" },
      skipped: { icon: "arrow-right", label: "Skipped TODO" },
    } as const;
    const { icon, label } = presentation[item.state];
    return html`
      <button
        class=${`message todo-rail-item ${item.state}`}
        type="button"
        data-chat-anchor-id=${`item:${item.id}`}
        aria-label=${`${label}: ${item.content}. Open in Details pane.`}
        title="Open in Details"
        @click=${() => this.#host.element.dispatchEvent(new CustomEvent(
          "trouve-open-inspection",
          {
            detail: { panel: "info" },
            bubbles: true,
            composed: true,
          },
        ))}
      >
        <span class="todo-rail-icon" aria-hidden="true">
          ${fontAwesomeIcon(icon)}
        </span>
        <span class="todo-rail-copy">
          <strong>${label}</strong>
          <span>${item.content}</span>
        </span>
      </button>
    `;
  }

  #renderActivityGroup(
    unit: ChatRenderUnit,
    items: readonly AgentActivityItem[],
    presentation: ChatPresentationIndex,
  ) {
    const first = items[0];
    if (first === undefined) return nothing;
    const key = `activity:${unit.id}:${first.id}`;
    const needsApproval = items.some(
      (item) => item.kind === "tool" && item.status === "awaiting-approval",
    );
    const latestTodoStates = new Map<
      string,
      Extract<AgentActivityItem, { readonly kind: "todo" }>["state"]
    >();
    for (const item of items) {
      if (item.kind === "todo") latestTodoStates.set(item.todoId, item.state);
    }
    const active = items.some((item) =>
      item.kind === "thinking"
        ? !item.complete
        : item.kind === "compaction"
          ? item.state.kind === "running"
          : item.kind === "todo"
            ? latestTodoStates.get(item.todoId) === "started"
            : item.status === "running" || item.status === "awaiting-approval"
    );
    const failed = items.some((item) =>
      item.kind === "compaction"
        ? item.state.kind === "failed"
        : item.kind === "todo"
          ? latestTodoStates.get(item.todoId) === "cancelled"
          : item.kind === "tool"
            && (item.status === "error" || item.status === "denied" || item.status === "aborted")
    );
    const succeeded = items.some((item) =>
      item.kind === "compaction"
        ? item.state.kind === "completed"
        : item.kind === "todo"
          ? latestTodoStates.get(item.todoId) === "completed"
          : item.kind === "tool" && item.status === "ok"
    );
    const skipped = items.some(
      (item) => item.kind === "todo" && latestTodoStates.get(item.todoId) === "skipped",
    );
    const mixed = failed && succeeded;
    const tone = mixed
      ? "warning"
      : failed
      ? "error"
      : skipped
        ? "warning"
      : needsApproval
        ? "warning"
        : active
          ? "active"
          : "complete";
    const status: ActivityGroupStatus = mixed
      ? "mixed"
      : failed
      ? "error"
      : skipped
        ? "mixed"
      : needsApproval
        ? "awaiting-approval"
        : active
          ? "running"
          : "ok";
    const open = this.#activityGroupOpen(unit, items);
    return html`
      <details
        class=${`activity-group ${tone}`}
        data-chat-anchor-id=${`activity:${items.at(-1)?.id ?? first.id}`}
        .open=${open}
      >
        <summary
          @click=${(event: Event) =>
            this.#toggleActivityGroup(event, key, open, needsApproval)}
        >
          <span class="activity-rail-disclosure" aria-hidden="true">
            ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
              className: "activity-rail-disclosure-icon",
            })}
          </span>
          <strong>${activityGroupSummary(items)}</strong>
          <small class="visually-hidden">Group status: ${activityGroupStatusLabel(status)}</small>
        </summary>
        ${open
          ? html`<div class="activity-group-body">
              <div class=${`agent-activity-timeline activity-group-timeline ${
                items.length === 1 ? "single-activity" : ""
              }`}>
                ${items.map((item) => this.#renderGroupedActivityItem(
                  item,
                  presentation,
                ))}
              </div>
            </div>`
          : nothing}
      </details>
    `;
  }

  #legacyCompactionState(
    item: Extract<AgentActivityItem, { readonly kind: "tool" }>,
  ): CompactionState {
    return item.status === "ok"
      ? { kind: "completed", messagesCompacted: 0 }
      : item.status === "running" || item.status === "awaiting-approval"
        ? { kind: "running" }
        : { kind: "failed" };
  }

  #renderGroupedActivityItem(
    item: AgentActivityItem,
    presentation: ChatPresentationIndex,
  ) {
    if (item.kind === "thinking") return this.#renderVisibleThinking(item);
    if (item.kind === "todo") return this.#renderTodoUpdate(item);
    if (item.kind === "compaction") {
      return this.renderCompactionMarker(item.state, item.id, {
        before: false,
        after: false,
        nested: true,
      });
    }
    if (isContextCompactionTool(item)) {
      return this.renderCompactionMarker(this.#legacyCompactionState(item), item.id, {
        before: false,
        after: false,
        nested: true,
      });
    }
    return this.#renderItem(item, presentation);
  }

  #activityGroupOpen(
    unit: ChatRenderUnit,
    items: readonly AgentActivityItem[],
  ): boolean {
    const first = items[0];
    if (first === undefined) return false;
    const needsApproval = items.some(
      (item) => item.kind === "tool" && item.status === "awaiting-approval",
    );
    const key = `activity:${unit.id}:${first.id}`;
    return needsApproval || (this.#messageDisclosure.get(key) ?? false);
  }

  #toggleActivityGroup(
    event: Event,
    key: string,
    open: boolean,
    forcedOpen: boolean,
  ): void {
    event.preventDefault();
    if (forcedOpen) return;
    this.#messageDisclosure.set(key, !open);
    this.#host.requestDisclosureUpdate();
  }

  #renderItem(
    item: Exclude<AgentChatItem, { readonly kind: "assistant" }>,
    presentation: ChatPresentationIndex,
  ) {
    switch (item.kind) {
      case "subagent":
        return this.#renderSubagentNode(item);
      case "progress":
        return this.#renderVisibleProgress(item);
      case "thinking": {
        this.ensureMarkdown();
        const defaultOpen = item.turn === presentation.latestTurn;
        const open = this.#messageDisclosure.get(item.id) ?? defaultOpen;
        const preview = collapsedChatPreview(item.content);
        return html`
          <article
            class=${`message thinking-card ${item.complete ? "complete" : "running"}`}
            data-chat-anchor-id=${`item:${item.id}`}
          >
            <span class="thinking-rail-icon" aria-hidden="true">
              ${fontAwesomeIcon("brain")}
            </span>
            <header class="thinking-header">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse reasoning" : "Expand reasoning"}
                @click=${() => this.#toggleMessageDisclosure(item.id, defaultOpen)}
              >
                ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
                  className: "disclosure-icon",
                })}
                <strong>Reasoning</strong>
                ${open
                  ? nothing
                  : html`<small class="message-collapsed-preview">${preview}</small>`}
              </button>
              ${this.#renderCopyButton(
                `message:${item.id}`,
                item.content,
                "Copy reasoning",
              )}
            </header>
            ${open
              ? html`
                  <div class="thinking-body">
                    <trouve-markdown-view .content=${item.content}></trouve-markdown-view>
                  </div>
                `
              : nothing}
          </article>
        `;
      }
      case "compaction":
        return this.renderCompactionMarker(item.state, item.id);
      case "todo":
        return this.#renderTodoUpdate(item);
      case "tool": {
        const interactive = this.#host.interactive;
        const approvalRequired = item.status === "awaiting-approval";
        const raw = this.#rawToolCalls.has(item.callId);
        const detailLoading = this.#toolDetailLoading.has(item.callId);
        const detailError = this.#toolDetailErrors.get(item.callId) ?? "";
        const toolPresentation = presentToolCall(item.tool, item.args, item.result);
        const toolOpen = approvalRequired
          || (this.#toolDisclosure.get(item.callId) ?? false);
        const toolDuration = toolExecutionMetadata(item.result, item.durationMs);
        const toolTargetMeta = [
          toolPresentation.meta,
          toolDuration,
        ].filter((part) => part !== "").join(" · ");
        if (
          toolOpen
          && !raw
          && toolPresentation.diff.length === 0
          && toolPresentation.todos.length === 0
        ) this.#ensureToolDetail();
        // An approval card is forced open and its summary cannot be toggled, so
        // nothing else would ever request its deferred arguments; fetch them as
        // soon as it renders (outside the render pass, since the fetch mutates
        // loading state). A failed fetch keeps its error rather than retrying
        // on every render.
        if (
          approvalRequired
          && item.detailsDeferred
          && !detailLoading
          && detailError === ""
        ) {
          const callId = item.callId;
          queueMicrotask(() => void this.#ensureToolDetails(callId));
        }
        return html`
          <details
            class=${`message tool-card tool-${item.status} ${approvalRequired ? "approval-required" : ""}`}
            data-chat-anchor-id=${`item:${item.id}`}
            data-call-id=${item.callId}
            ?open=${toolOpen}
            @keydown=${interactive === undefined
              ? undefined
              : (event: KeyboardEvent) => interactive.toolKeydown(event, item.callId)}
          >
            <summary
              @click=${(event: Event) =>
                this.#toggleToolDisclosure(event, item.callId, approvalRequired)}
            >
              <span class=${`activity-rail-disclosure ${item.status}`} aria-hidden="true">
                ${fontAwesomeIcon(toolOpen ? "caret-down" : "caret-right", {
                  className: "activity-rail-disclosure-icon",
                })}
              </span>
              <strong>${toolPresentation.title}${toolPresentation.subject === "" ? "" : ":"}</strong>
              ${toolPresentation.subject === ""
                ? nothing
                : toolPresentation.filePath === ""
                  ? html`<span class="tool-subject">${toolPresentation.subject}</span>`
                  : html`<button
                      class="tool-file-target"
                      type="button"
                      title=${`Open ${toolPresentation.filePath}${toolTargetMeta === "" ? "" : ` ${toolTargetMeta}`}`}
                      @click=${(event: MouseEvent) => this.#openToolFile(event, toolPresentation)}
                    >${toolPresentation.subject}</button>`}
              ${toolPresentation.additions === 0
                ? nothing
                : html`<span class="tool-change-count add">+${toolPresentation.additions}</span>`}
              ${toolPresentation.deletions === 0
                ? nothing
                : html`<span class="tool-change-count delete">−${toolPresentation.deletions}</span>`}
              ${toolPresentation.meta === ""
                ? nothing
                : html`<small class="tool-meta tool-detail-meta">${toolPresentation.meta}</small>`}
              <span class="tool-inline-status ${item.status}" aria-hidden="true">
                ${fontAwesomeIcon(toolStatusIcon(item.status), {
                  className: "tool-status-icon",
                  spin: item.status === "running",
                })}
              </span>
              <small class="tool-state visually-hidden">${toolStatusLabel(item.status)}</small>
              ${toolDuration === ""
                ? nothing
                : html`<small class="tool-meta tool-duration">· ${toolDuration}</small>`}
              <span class="tool-raw-action" @click=${(event: Event) => event.stopPropagation()}>
                <button
                  type="button"
                  aria-pressed=${raw ? "true" : "false"}
                  aria-label=${raw ? "Show formatted tool output" : "Show raw tool output"}
                  title=${raw ? "Show formatted output" : "Show raw data"}
                  @click=${() => this.#toggleRawTool(item.callId)}
                >${fontAwesomeIcon(raw ? "code" : "list")}</button>
              </span>
              <span class="tool-copy-action" @click=${(event: Event) => event.stopPropagation()}>
                ${item.detailsDeferred
                  ? nothing
                  : this.#renderCopyButton(
                      `tool:${item.callId}`,
                      raw ? this.#rawToolText(item) : this.#toolCopyText(item),
                      `Copy ${item.tool} details`,
                    )}
              </span>
              ${approvalRequired && interactive !== undefined
                ? interactive.renderToolApprovalActions(item)
                : nothing}
            </summary>
            ${toolOpen
              ? html`
                  ${item.detailsDeferred
                    ? html`<div class="tool-detail-loading" role="status">
                        ${detailLoading
                          ? "Loading tool details…"
                          : detailError === ""
                            ? "Tool details are loading…"
                            : detailError}
                      </div>`
                    : raw
                    ? html`<pre aria-label="Raw tool data">${this.#rawToolText(item)}</pre>`
                    : html`
                        ${toolPresentation.diff.length === 0
                          ? nothing
                          : html`<div class="tool-inline-diff" role="table" aria-label=${`${toolPresentation.title} ${toolPresentation.subject} line changes`}>
                              ${toolPresentation.diff.map((line) => html`
                                <div class=${`tool-diff-line ${line.kind}`} role="row">
                                  <span class="tool-diff-gutter" role="cell">${line.oldNumber > 0 ? line.oldNumber : ""}</span>
                                  <span class="tool-diff-gutter" role="cell">${line.newNumber > 0 ? line.newNumber : ""}</span>
                                  <span class="tool-diff-mark" aria-hidden="true">${line.kind === "add" ? "+" : line.kind === "delete" ? "−" : " "}</span>
                                  <code role="cell">${line.text}</code>
                                </div>
                              `)}
                            </div>`}
                        ${toolPresentation.todos.length === 0
                          ? nothing
                          : html`<ul class="tool-todo-list" aria-label="TODO state">
                              ${toolPresentation.todos.map((todo) => html`<li class=${`todo-${todo.status}`}>
                                ${fontAwesomeIcon(todo.icon)}<span>${todo.content}</span>
                              </li>`)}
                            </ul>`}
                        ${toolPresentation.diff.length > 0 || toolPresentation.todos.length > 0
                          ? nothing
                          : this.#toolDetailLoadFailed
                          ? html`<div class="tool-detail-loading" role="alert">
                              Tool detail viewer could not be loaded.
                              <button type="button" @click=${this.#retryToolDetailImport}>Retry</button>
                            </div>`
                          : html`<trouve-tool-detail-view
                              .tool=${item.tool}
                              .args=${item.args}
                              .result=${item.result}
                              .output=${item.output.text}
                              .outputOmitted=${item.output.omitted}
                            ></trouve-tool-detail-view>`}
                      `}
                  ${toolPresentation.diff.length === 0 && toolPresentation.todos.length === 0
                    ? nothing
                    : item.output.text === "" && !item.output.omitted
                    ? nothing
                    : html`<pre aria-label="Live tool output">Output\n${item.output.omitted
                        ? TOOL_OUTPUT_OMITTED_MESSAGE
                        : ""}${item.output.text}</pre>`}
                `
              : nothing}
          </details>
        `;
      }
      case "questions":
        if (item.answers !== undefined) {
          const summary = item.answers === null
            ? []
            : resolvedQuestionSummary(item.questions, item.answers);
          return html`
            <section
              class="message question-card question-resolved"
              data-chat-anchor-id=${`item:${item.id}`}
            >
              <header>
                <strong>${item.title ?? "Questions"}</strong>
                <span>${item.answers === null ? QUESTION_SKIPPED_STATUS : "Answered"}</span>
              </header>
              ${item.answers === null
                ? html`<p class="resolved-label">${QUESTION_SKIPPED_MESSAGE}</p>`
                : renderQuestionSummary(summary)}
            </section>
          `;
        }
        return this.#host.interactive === undefined
          ? this.#renderReadOnlyPendingQuestions(item)
          : this.#host.interactive.renderPendingQuestions(item);
    }
  }

  /** Pending questions shown by a host that cannot answer them. */
  #renderReadOnlyPendingQuestions(item: TranscriptQuestionsItem) {
    return html`
      <section
        class="message question-card question-pending"
        data-chat-anchor-id=${`item:${item.id}`}
        data-question-request-id=${item.requestId}
      >
        <header>
          <strong>${item.title ?? "Questions"}</strong>
          <span>Awaiting answers</span>
        </header>
        ${renderQuestionSummary(item.questions.map((question) => ({
          prompt: question.prompt,
          answer: "Unanswered",
        })))}
      </section>
    `;
  }

  #renderAttachments(
    attachments: Extract<ThreadChatItem, { kind: "user" }>["attachments"],
    label = "Message attachments",
  ) {
    if (attachments.length === 0) return nothing;
    return html`
      <ul class="attachment-list" aria-label=${label}>
        ${attachments.map((attachment) => {
          const path = protocolAttachmentPath(attachment);
          const image = isImageAttachment(attachment);
          const video = isVideoAttachment(attachment);
          const copyKey = `attachment:${attachment.id}`;
          return html`
            <li class=${image || video ? "image-attachment" : "file-attachment"}>
              ${path !== undefined && (image || video)
                ? html`
                    <trouve-image-preview
                      .source=${path}
                      .name=${attachment.name}
                      .mime=${attachment.mime}
                      .video=${video}
                      lazy
                    ></trouve-image-preview>
                  `
                : html`<span class="attachment-icon">${fontAwesomeIcon(
                    image ? "file-image" : "file",
                  )}</span>`}
              <div class="attachment-details">
                <strong title=${attachment.name}>${attachment.name}</strong>
                <small>${attachment.mime} · ${formatAttachmentBytes(attachment.size_bytes)}</small>
                <div class="attachment-actions">
                  ${path === undefined
                    ? html`<span>Unavailable</span>`
                    : html`
                        <a href=${path} download=${attachment.name}>Download</a>
                        ${this.#renderCopyButton(
                          copyKey,
                          this.#absoluteAttachmentUrl(path),
                          `Copy link to ${attachment.name}`,
                        )}
                      `}
                </div>
              </div>
            </li>
          `;
        })}
      </ul>
    `;
  }

  #renderCopyButton(key: string, text: string, accessibleLabel: string) {
    const result = this.#copyFeedback.get(key);
    const icon = result === "copied"
      ? "check"
      : result === undefined ? "copy" : "circle-exclamation";
    return html`
      <button
        class="copy-action"
        type="button"
        aria-label=${result === undefined
          ? accessibleLabel
          : `${accessibleLabel}: ${copyActionLabel(result)}`}
        aria-live="polite"
        ?disabled=${text === ""}
        @click=${() => void this.#copyText(key, text)}
      >${fontAwesomeIcon(icon)}</button>
    `;
  }

  #toggleMessageDisclosure(
    itemId: string,
    defaultOpen: boolean,
    forcedOpen = false,
  ): void {
    if (forcedOpen) return;
    const open = this.#messageDisclosure.get(itemId) ?? defaultOpen;
    this.#messageDisclosure.set(itemId, !open);
    this.#host.requestDisclosureUpdate();
  }

  #toggleToolDisclosure(
    event: Event,
    callId: string,
    approvalRequired: boolean,
  ): void {
    // Own the disclosure state instead of letting <details> mutate first.
    // That lets live-tail convergence start before the row changes height.
    event.preventDefault();
    if (approvalRequired) return;
    const open = this.#toolDisclosure.get(callId) ?? false;
    this.#toolDisclosure.set(callId, !open);
    if (!open) void this.#ensureToolDetails(callId);
    this.#host.requestDisclosureUpdate();
  }

  #toggleRawTool(callId: string): void {
    if (this.#rawToolCalls.has(callId)) this.#rawToolCalls.delete(callId);
    else this.#rawToolCalls.add(callId);
    void this.#ensureToolDetails(callId);
    this.#host.requestDisclosureUpdate();
  }

  async #ensureToolDetails(callId: string): Promise<void> {
    const threadId = this.#host.threadId;
    if (threadId === "" || this.#toolDetailLoading.has(callId)) return;
    const tool = this.#host.findTool(callId);
    if (tool?.detailsDeferred !== true) return;
    const generation = this.#host.interactionGeneration();
    this.#toolDetailLoading.add(callId);
    this.#toolDetailErrors.delete(callId);
    this.#host.requestUpdate();
    try {
      await this.#host.loadToolDetails(threadId, callId, generation);
    } catch {
      if (this.#host.isCurrentInteraction(threadId, generation)) {
        this.#toolDetailErrors.set(callId, "Tool details could not be loaded.");
      }
    } finally {
      if (this.#host.isCurrentInteraction(threadId, generation)) {
        this.#toolDetailLoading.delete(callId);
        this.#host.requestUpdate();
      }
    }
  }

  async #copyText(key: string, value: string): Promise<void> {
    const generation = this.#copyFeedbackGeneration;
    const result = await copyChatText(value, globalThis.navigator?.clipboard);
    if (generation !== this.#copyFeedbackGeneration) return;
    this.#copyFeedback.set(key, result);
    this.#host.requestUpdate();
    globalThis.setTimeout(() => {
      if (
        generation === this.#copyFeedbackGeneration
        && this.#copyFeedback.get(key) === result
      ) {
        this.#copyFeedback.delete(key);
        this.#host.requestUpdate();
      }
    }, 1_800);
  }

  #absoluteAttachmentUrl(path: string): string {
    try {
      return new URL(path, globalThis.location.href).href;
    } catch {
      return path;
    }
  }

  #toolCopyText(item: TranscriptToolItem): string {
    const sections = [
      `${presentToolCall(item.tool, item.args, item.result).title} — ${toolStatusLabel(item.status)}`,
      toolDetailText(item.args, item.result),
    ].filter((section) => section !== "");
    if (item.output.text !== "" || item.output.omitted) {
      sections.push(
        `Output\n${item.output.omitted ? TOOL_OUTPUT_OMITTED_MESSAGE : ""}${item.output.text}`,
      );
    }
    return sections.join("\n\n");
  }

  #rawToolText(item: TranscriptToolItem): string {
    const data = {
      call_id: item.callId,
      tool: item.tool,
      status: item.status,
      arguments: item.args,
      ...(item.result === undefined ? {} : { result: item.result }),
    };
    return boundedJson(data);
  }

  #openToolFile(event: MouseEvent, presentation: ToolPresentation): void {
    event.preventDefault();
    event.stopPropagation();
    if (presentation.filePath === "") return;
    this.#host.element.dispatchEvent(new CustomEvent("trouve-open-file", {
      detail: {
        path: presentation.filePath,
        from: presentation.lineFrom,
        to: presentation.lineTo,
      },
      bubbles: true,
      composed: true,
    }));
  }

  /** Lazily load the Markdown viewer the first time transcript text renders. */
  ensureMarkdown(): void {
    if (this.#markdownRequested) return;
    this.#markdownRequested = true;
    void import("@trouve-ai/content-rendering/markdown-view");
  }

  #ensureToolDetail(): void {
    if (this.#toolDetailRequested || this.#toolDetailLoadFailed) return;
    this.#toolDetailRequested = true;
    void import("./tool-detail-view.js").catch(() => {
      this.#toolDetailRequested = false;
      this.#toolDetailLoadFailed = true;
      this.#host.requestUpdate();
    });
  }

  readonly #retryToolDetailImport = (): void => {
    this.#toolDetailLoadFailed = false;
    this.#toolDetailRequested = false;
    this.#host.requestUpdate();
  };
}
