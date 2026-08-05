import { ContextConsumer, ContextProvider } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";
import { live } from "lit/directives/live.js";
import { repeat } from "lit/directives/repeat.js";

import {
  appServicesContext,
  appStoreContext,
  hostCapabilitiesContext,
  sessionContext,
  threadContext,
  workspaceContext,
} from "../contexts/app-contexts.js";
import {
  AttachmentEncodingError,
  encodeAttachment,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "../services/attachments.js";
import type {
  ProtocolAgentMode,
  ProtocolModelInfo,
  ProtocolResolveApprovalRequest,
  ProtocolResolveQuestionRequest,
  ProtocolSubscriptionHealth,
  ProtocolUpdateThreadRequest,
  ProtocolUsageSummary,
} from "../services/protocol-client.js";
import type { ChatScrollBookmark } from "../services/resume-preferences.js";
import { rankComposerCompletionsOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import type { QueuedPrompt, ThreadChatItem } from "../state/thread-view-model.js";
import { TOOL_OUTPUT_OMITTED_MESSAGE } from "../state/tool-output.js";
import {
  approvalDecisionForShortcut,
  ApprovalSubmissionTracker,
} from "./approval-controls.js";
import {
  assistantCopyText,
  collapsedChatPreview,
  copyActionLabel,
  copyChatText,
  formatAttachmentBytes,
  formatTurnMetadata,
  indexChatPresentation,
  isImageAttachment,
  protocolAttachmentPath,
  type ChatCopyResult,
  type ChatPresentationIndex,
} from "./chat-presentation.js";
import { chatTurnControlState } from "./chat-turn-controls.js";
import {
  activityGroupSummary,
  buildChatLayout,
  type AgentChatItem,
  type ChatRenderUnit,
} from "./chat-layout.js";
import {
  presentToolCall,
  runningActivityLabel,
  toolDetailText,
  toolExecutionMetadata,
  type ToolPresentation,
} from "./tool-presentation.js";
import {
  applyComposerCompletion,
  composerCompletionToken,
  isComposerCompletionTokenCurrent,
  MAX_COMPOSER_COMPLETION_SOURCES,
  MAX_COMPOSER_COMPLETIONS,
  rankComposerCompletions,
  type ComposerCompletionCandidate,
  type ComposerCompletionToken,
  type RankedComposerCompletion,
} from "./composer-completion.js";
import {
  composerTextareaLayout,
  isComposerCompositionKey,
} from "./composer-input-model.js";
import {
  composerContextUsage,
  formatSessionUsage,
} from "./composer-usage.js";
import {
  modelOptionControls,
  modelOptionLabel,
} from "./model-option-controls.js";
import { modelHealthPresentations } from "./model-health.js";
import {
  droppedQueueIds,
  prioritizedQueueIds,
  queueControlState,
  queueFocusAfterDelete,
  queuePreview,
  reorderedQueueIds,
  type QueueDropPlacement,
} from "./queue-controls.js";
import {
  advanceQuestionWizard,
  canAdvanceQuestionWizard,
  createQuestionWizard,
  editQuestionOther,
  normalizeQuestionWizard,
  OTHER_OPTION_ID,
  pendingQuestionSummary,
  questionWizardAnswers,
  resolvedQuestionSummary,
  retreatQuestionWizard,
  toggleQuestionOption,
  type QuestionWizardState,
} from "./question-wizard.js";
import {
  nextHorizontalTabIndex,
  rovingTabIndex,
} from "./tab-navigation.js";
import type {
  NewThreadSetupCancelEvent,
  NewThreadSetupSubmitEvent,
} from "./new-thread-setup.js";
import {
  Virtualizer,
  type VirtualItem,
  type VirtualWindow,
} from "./virtualization/virtualizer.js";
import "./new-thread-setup.js";
import "./model-picker.js";

type VirtualChatItem = VirtualItem & (
  | { readonly kind: "unit"; readonly unitIndex: number }
  | { readonly kind: "compacting" }
  | { readonly kind: "activity"; readonly label: string }
  | { readonly kind: "history" }
  | { readonly kind: "edge-spacer"; readonly edge: "start" }
);

const CHAT_START_SPACER_ID = "ephemeral:chat-start-spacer";
const CHAT_HISTORY_LOADER_ID = "ephemeral:chat-history-loader";
const CHAT_TAIL_EPSILON_PX = 2;
const CHAT_POSITION_SETTLE_MS = 140;
const CHAT_SCROLL_CORRECTION_SETTLE_MS = 240;
const CHAT_TAIL_CONVERGENCE_FRAMES = 3;

const sameVirtualRenderWindow = (
  left: VirtualWindow<VirtualChatItem>,
  right: VirtualWindow<VirtualChatItem>,
): boolean =>
  left.followingTail === right.followingTail
  && left.totalHeight === right.totalHeight
  && left.paddingBefore === right.paddingBefore
  && left.paddingAfter === right.paddingAfter
  && left.items.length === right.items.length
  && left.items.every(
    ({ item, start, height }, index) => {
      const candidate = right.items[index];
      return item.id === candidate?.item.id
        && start === candidate.start
        && height === candidate.height;
    },
  );

interface ActiveComposerCompletion {
  readonly token: ComposerCompletionToken;
  readonly matches: readonly RankedComposerCompletion[];
  readonly loading: boolean;
  readonly searching: boolean;
  readonly unavailable: boolean;
  readonly emptyMessage: string;
}

const PATH_REFRESH_INTERVAL_MS = 5_000;
const WORKER_COMPLETION_THRESHOLD = 200;

const PWA_QUICK_REPLIES = Object.freeze([
  { label: "Continue", prompt: "Continue." },
  { label: "Explain", prompt: "Explain what you just did." },
  { label: "Undo", prompt: "Undo the last change." },
] as const);

const shortModelName = (model: string): string => {
  const segments = model.split("/").filter((segment) => segment !== "");
  return segments.at(-1) ?? model;
};

const threadTabLabel = (
  thread: {
    readonly mode: string;
    readonly model: string;
    readonly spawned?: boolean;
  },
  modeDisplayName?: string,
): string => {
  const mode = modeDisplayName?.trim() || thread.mode;
  return `${thread.spawned === true ? "⑂ " : ""}${mode} · ${shortModelName(thread.model)}`;
};

const threadTodoProgress = (
  todos: readonly { readonly status: string }[] | undefined,
): string => {
  if (todos === undefined || todos.length === 0) return "";
  const completed = todos.filter((todo) => todo.status === "completed").length;
  return `${completed}/${todos.length}`;
};

const boundedJson = (value: unknown): string => {
  let text: string;
  try {
    text = JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    text = "[unavailable result]";
  }
  return text.length <= 32_000 ? text : `${text.slice(0, 32_000)}\n… output truncated`;
};

const toolStatusLabel = (status: Extract<ThreadChatItem, { kind: "tool" }>["status"]): string =>
  ({
    "awaiting-approval": "Approval needed",
    running: "Running",
    ok: "Completed",
    error: "Failed",
    denied: "Denied",
    aborted: "Aborted",
  })[status];

const toolStatusGlyph = (status: Extract<ThreadChatItem, { kind: "tool" }>["status"]): string =>
  ({
    "awaiting-approval": "⏸",
    running: "◌",
    ok: "✓",
    error: "✗",
    denied: "⊘",
    aborted: "✗",
  })[status];

export class TrouveThreadScreen extends withSignalTracking(LitElement) {
  static override properties = {
    workspaceId: { type: String, attribute: "workspace-id" },
    sessionId: { type: String, attribute: "session-id" },
    threadId: { type: String, attribute: "thread-id" },
    scrollBookmark: { attribute: false },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  workspaceId = "";
  sessionId = "";
  threadId = "";
  scrollBookmark: ChatScrollBookmark | undefined;
  #requestPending = false;
  #turnRequestGeneration = 0;
  #threadInteractionGeneration = 0;
  #requestError = "";
  #pendingStartTurn: number | undefined;
  #cancelRequestedTurn: number | undefined;
  #messageRequest: "start" | "queue" | undefined;
  #newThreadSetupOpen = false;
  #newThreadBusy = false;
  #newThreadError = "";
  #accessibleHistory = false;
  #historyLoading = false;
  #historyError = "";
  #historyGeneration = 0;
  #pendingHistoryPrepend:
    | { readonly scrollTop: number; readonly totalHeight: number }
    | undefined;
  #virtualizer = new Virtualizer<VirtualChatItem>({
    estimatedHeight: 120,
    overscanPx: 1_200,
    tailTolerancePx: 32,
  });
  #resizeObserver: ResizeObserver | undefined;
  readonly #observedVirtualRows = new Set<HTMLElement>();
  #viewportHeight = 0;
  #programmaticScrollFrame: number | undefined;
  #tailConvergenceFrame: number | undefined;
  #scrollRenderFrame: number | undefined;
  #chatPositionTimer: ReturnType<typeof setTimeout> | undefined;
  #scrollCorrectionResumeAt = 0;
  #followTailControlHeight = 0;
  #chatScrollIntent = false;
  #restoredScrollThreadId: string | undefined;
  #invalidScrollBookmarkThreadId: string | undefined;
  #markdownRequested = false;
  #queueEditId = "";
  #queueEditDraft = "";
  #queueBusy = "";
  #queueError = "";
  #queueDragId = "";
  #queueDropId = "";
  #queueDropPlacement: QueueDropPlacement = "before";
  #pendingAttachments: PendingAttachment[] = [];
  #attachmentPending = false;
  #attachmentGeneration = 0;
  #modes: readonly ProtocolAgentMode[] = [];
  #models: readonly ProtocolModelInfo[] = [];
  #subscriptionHealth: readonly ProtocolSubscriptionHealth[] = [];
  #observedSubscriptionUsageCursor = 0;
  #optionCatalogKey = "";
  #threadSettingsPending = false;
  #composerDraft = "";
  #composerCursor = 0;
  #composerComposing = false;
  #completionSelected = 0;
  #completionDismissed = false;
  #completionWorkerKey = "";
  #completionWorkerRequestedKey = "";
  #completionWorkerMatches: readonly RankedComposerCompletion[] = [];
  #completionWorkerGeneration = 0;
  #completionWorkerPending = false;
  #sessionPaths: readonly string[] = [];
  #pathsSessionId = "";
  #pathsLoadingSessionId = "";
  #pathsUnavailableSessionId = "";
  #pathsLoadedAt = 0;
  #pathsRetryAfter = 0;
  #pathsGeneration = 0;
  #sessionUsage: ProtocolUsageSummary | undefined;
  #usageRequestKey = "";
  #usageResolvedKey = "";
  #usagePending = false;
  #usageGeneration = 0;
  #copyFeedbackGeneration = 0;
  readonly #approvalSubmissions = new ApprovalSubmissionTracker();
  readonly #copyFeedback = new Map<string, ChatCopyResult>();
  readonly #messageDisclosure = new Map<string, boolean>();
  readonly #rawAssistantTurns = new Set<number>();
  readonly #rawToolCalls = new Set<string>();
  readonly #toolDisclosure = new Map<string, boolean>();
  readonly #questionWizards = new Map<string, QuestionWizardState>();
  readonly #questionSubmissions = new Set<string>();

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  readonly #capabilities = new ContextConsumer(this, {
    context: hostCapabilitiesContext,
    subscribe: true,
  });
  readonly #workspaceProvider = new ContextProvider(this, {
    context: workspaceContext,
    initialValue: { workspaceId: "" },
  });
  readonly #sessionProvider = new ContextProvider(this, {
    context: sessionContext,
    initialValue: { sessionId: "" },
  });
  readonly #threadProvider = new ContextProvider(this, {
    context: threadContext,
    initialValue: { threadId: "" },
  });

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has("workspaceId")) {
      this.#workspaceProvider.setValue({ workspaceId: this.workspaceId });
      this.#optionCatalogKey = "";
      this.#modes = [];
      this.#models = [];
      this.#subscriptionHealth = [];
      this.#observedSubscriptionUsageCursor = 0;
    }
    if (changed.has("sessionId")) {
      this.#sessionProvider.setValue({ sessionId: this.sessionId });
      this.#newThreadSetupOpen = false;
      this.#newThreadBusy = false;
      this.#newThreadError = "";
      this.#pathsGeneration += 1;
      this.#sessionPaths = [];
      this.#pathsSessionId = "";
      this.#pathsLoadingSessionId = "";
      this.#pathsUnavailableSessionId = "";
      this.#pathsLoadedAt = 0;
      this.#pathsRetryAfter = 0;
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#queueEditId = "";
      this.#queueEditDraft = "";
      this.#queueBusy = "";
      this.#queueError = "";
      this.#queueDragId = "";
      this.#queueDropId = "";
      this.#queueDropPlacement = "before";
      this.#usageGeneration += 1;
      this.#sessionUsage = undefined;
      this.#usageRequestKey = "";
      this.#usageResolvedKey = "";
      this.#usagePending = false;
    }
    if (changed.has("threadId")) {
      this.#observedSubscriptionUsageCursor = 0;
      this.#turnRequestGeneration += 1;
      this.#attachmentGeneration += 1;
      this.#threadInteractionGeneration += 1;
      this.#historyGeneration += 1;
      this.#historyLoading = false;
      this.#historyError = "";
      this.#pendingHistoryPrepend = undefined;
      this.#requestPending = false;
      this.#requestError = "";
      this.#threadSettingsPending = false;
      this.#newThreadSetupOpen = false;
      this.#newThreadBusy = false;
      this.#newThreadError = "";
      this.#resizeObserver?.disconnect();
      this.#observedVirtualRows.clear();
      this.#cancelProgrammaticScrollWindow();
      this.#cancelTailConvergence();
      this.#cancelScheduledScrollRender();
      this.#cancelScheduledChatPosition();
      this.#virtualizer = new Virtualizer<VirtualChatItem>({
        estimatedHeight: 120,
        overscanPx: 1_200,
        tailTolerancePx: 32,
        mode: this.#accessibleHistory ? "accessible" : "virtual",
      });
      this.#viewportHeight = 0;
      this.#followTailControlHeight = 0;
      this.#scrollCorrectionResumeAt = 0;
      this.#chatScrollIntent = false;
      this.#restoredScrollThreadId = undefined;
      this.#invalidScrollBookmarkThreadId = undefined;
      this.#threadProvider.setValue({ threadId: this.threadId });
      this.#pendingAttachments = [];
      this.#attachmentPending = false;
      this.#copyFeedbackGeneration += 1;
      this.#copyFeedback.clear();
      this.#messageDisclosure.clear();
      this.#rawAssistantTurns.clear();
      this.#rawToolCalls.clear();
      this.#toolDisclosure.clear();
      this.#questionWizards.clear();
      this.#questionSubmissions.clear();
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#composerComposing = false;
      this.#queueEditId = "";
      this.#queueEditDraft = "";
      this.#queueBusy = "";
      this.#queueError = "";
      this.#queueDragId = "";
      this.#queueDropId = "";
      this.#queueDropPlacement = "before";
      this.#pendingStartTurn = undefined;
      this.#cancelRequestedTurn = undefined;
      this.#messageRequest = undefined;
    }
    if (
      changed.has("scrollBookmark")
      && this.scrollBookmark === undefined
      && changed.get("scrollBookmark") !== undefined
    ) {
      // A running turn or persisted queue superseded a parked bookmark after
      // replay completed. Move the already-mounted virtualizer to the live
      // tail as well as clearing the persisted preference in the shell.
      this.#virtualizer.enableFollowTail();
      this.#restoredScrollThreadId = this.threadId;
    }
  }

  protected override updated(): void {
    this.#cancelScheduledScrollRender();
    void this.#ensureThreadOptions();
    this.#refreshSubscriptionHealthAfterTurn();
    void this.#ensureSessionUsage();
    this.#resizeComposer();
    if (this.#invalidScrollBookmarkThreadId === this.threadId) {
      this.#invalidScrollBookmarkThreadId = undefined;
      this.#emitChatPosition();
    }
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport === null) {
      this.#resizeObserver?.disconnect();
      this.#observedVirtualRows.clear();
      this.#followTailControlHeight = 0;
      return;
    }
    this.#followTailControlHeight = viewport
      .querySelector<HTMLElement>(".follow-tail")
      ?.offsetHeight ?? 0;
    if (viewport.clientHeight !== this.#viewportHeight) {
      this.#viewportHeight = viewport.clientHeight;
      const correction = this.#virtualizer.resizeViewport(viewport.clientHeight);
      const expected = this.#virtualizer.window().followingTail
        ? this.#transcriptTailScrollTop(viewport)
        : correction.scrollTop;
      this.#setChatScrollTop(viewport, expected);
      this.requestUpdate();
      return;
    }
    const virtualWindow = this.#virtualizer.window();
    const expected = virtualWindow.followingTail
      ? this.#transcriptTailScrollTop(viewport)
      : virtualWindow.scrollTop;
    this.#setChatScrollTop(viewport, expected);
    if (typeof ResizeObserver === "undefined") return;
    this.#resizeObserver ??= new ResizeObserver((entries) => {
      const activeViewport = this.querySelector<HTMLElement>(".chat-stream");
      if (activeViewport === null) return;
      const before = this.#virtualizer.window();
      const followingTail = before.followingTail;
      let measured = false;
      let scrollCorrected = false;
      let scrollTop = activeViewport.scrollTop;
      for (const entry of entries) {
        const element = entry.target as HTMLElement;
        const id = element.dataset["virtualId"];
        if (id === undefined || entry.contentRect.height <= 0) continue;
        measured = true;
        try {
          const correction = this.#virtualizer.measure(id, entry.contentRect.height);
          if (correction.delta !== 0) {
            scrollCorrected = true;
            // A ResizeObserver delivery can lag behind another native scroll
            // frame. Apply parked-history corrections relative to the live DOM
            // position instead of writing the virtualizer's now-stale absolute
            // position back over the user's momentum.
            scrollTop = followingTail
              ? correction.scrollTop
              : scrollTop + correction.delta;
          }
        } catch {
          // A row may have unmounted between delivery and measurement.
        }
      }
      let expectedScrollTop: number | undefined;
      if (!followingTail && scrollCorrected) {
        if (Date.now() < this.#scrollCorrectionResumeAt) {
          // Keep the native viewport authoritative while wheel/touch momentum
          // is active. Measurements still improve future windows, but must
          // never push against the direction of the user's current gesture.
          this.#virtualizer.setViewport(
            activeViewport.scrollTop,
            activeViewport.clientHeight,
            { userInitiated: true, atTail: false },
          );
        } else {
          this.#virtualizer.setViewport(scrollTop, activeViewport.clientHeight);
          expectedScrollTop = this.#virtualizer.window().scrollTop;
        }
      }
      const after = this.#virtualizer.window();
      if (!sameVirtualRenderWindow(before, after)) {
        // A measured row can change height without changing scrollTop when it
        // is the parked anchor or sits below it. Reposition the already-
        // mounted absolute rows before paint; waiting for the next virtual
        // render would briefly draw the resized row over its successor.
        this.#syncMountedVirtualGeometry(activeViewport, after);
        this.#scheduleScrollRender();
      }
      if (followingTail && measured) {
        // A remounted cached child can expand back to its already-known
        // measurement. The virtual model then reports no delta even though
        // the DOM's scroll range grew, so live-tail pinning must follow every
        // observed layout delivery rather than only new measurements.
        this.#setChatScrollTop(
          activeViewport,
          this.#transcriptTailScrollTop(activeViewport),
        );
      } else if (expectedScrollTop !== undefined) {
        this.#setChatScrollTop(activeViewport, expectedScrollTop);
      }
    });
    const mountedRows = new Set(
      this.querySelectorAll<HTMLElement>("[data-virtual-id]"),
    );
    for (const row of this.#observedVirtualRows) {
      if (mountedRows.has(row)) continue;
      this.#resizeObserver.unobserve(row);
      this.#observedVirtualRows.delete(row);
    }
    for (const row of mountedRows) {
      if (this.#observedVirtualRows.has(row)) continue;
      this.#resizeObserver.observe(row);
      this.#observedVirtualRows.add(row);
    }
  }

  override disconnectedCallback(): void {
    this.#resizeObserver?.disconnect();
    this.#resizeObserver = undefined;
    this.#observedVirtualRows.clear();
    this.#cancelProgrammaticScrollWindow();
    this.#cancelTailConvergence();
    this.#cancelScheduledScrollRender();
    this.#cancelScheduledChatPosition();
    this.#scrollCorrectionResumeAt = 0;
    this.#copyFeedbackGeneration += 1;
    this.#usageGeneration += 1;
    this.#turnRequestGeneration += 1;
    this.#attachmentGeneration += 1;
    this.#threadInteractionGeneration += 1;
    this.#requestPending = false;
    this.#attachmentPending = false;
    this.#messageRequest = undefined;
    this.#chatScrollIntent = false;
    super.disconnectedCallback();
  }

  #selectThreadWithKeyboard(
    event: KeyboardEvent,
    currentIndex: number,
    threads: readonly { readonly id: string }[],
  ): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const tabCount = threads.length + (this.#newThreadSetupOpen ? 1 : 0);
    const nextIndex = nextHorizontalTabIndex(event.key, currentIndex, tabCount);
    const nextThread = nextIndex === undefined ? undefined : threads[nextIndex];
    const services = this.#services.value;
    if (nextIndex === undefined || services === undefined) return;
    event.preventDefault();
    const tablist = (event.currentTarget as HTMLElement).closest('[role="tablist"]');
    tablist?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus();
    if (nextThread === undefined && nextIndex === threads.length) {
      this.openNewThreadSetup();
    } else if (nextThread !== undefined) {
      this.#selectThread(nextThread.id);
    }
  }

  override render() {
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) {
      return html`<div class="screen-empty" role="status">Loading thread…</div>`;
    }
    const threads = store.threadsForSession(this.sessionId);
    const thread = this.threadId === "" ? undefined : store.thread(this.threadId);
    const view = this.threadId === "" ? undefined : store.threadView(this.threadId);
    this.#reconcileTurnAcknowledgements(view?.items ?? [], view?.turnRunning ?? false);
    const selectedThreadIndex = threads.findIndex((candidate) => candidate.id === this.threadId);
    const selectedTabIndex = this.#newThreadSetupOpen
      ? threads.length
      : selectedThreadIndex;
    const threadTabCount = threads.length + (this.#newThreadSetupOpen ? 1 : 0);
    const session = readSignal(store.sessions).find(
      (session) => session.id === this.sessionId,
    );
    const sessionTitle = session?.title ?? "";
    const serverOnline = readSignal(store.serverInfo)?.online;
    const connectivityBlocked = serverOnline === false && this.#models.length === 0;
    const hasComposerContent = this.#composerDraft.trim() !== ""
      || this.#pendingAttachments.length > 0;
    const turnControls = chatTurnControlState({
      threadAvailable: thread !== undefined,
      durableTurnRunning: view?.turnRunning ?? false,
      startPending: this.#pendingStartTurn !== undefined,
      cancellationRequested: this.#cancelRequestedTurn !== undefined,
      messageRequest: this.#messageRequest,
      requestPending: this.#requestPending,
      attachmentPending: this.#attachmentPending,
      hasContent: hasComposerContent,
      connectivityBlocked,
    });
    const attachmentDisabled = thread === undefined
      || this.#requestPending
      || this.#attachmentPending
      || connectivityBlocked;
    const completion = connectivityBlocked
      ? undefined
      : this.#activeComposerCompletion(view?.commands ?? []);
    const selectedModel = thread === undefined
      ? undefined
      : this.#models.find((model) => model.id === thread.model);
    const modelControls = modelOptionControls(selectedModel, thread?.model_options);
    const modelHealth = modelHealthPresentations(this.#models, this.#subscriptionHealth);
    const selectedModelHealth = modelHealth[
      this.#models.findIndex((model) => model.id === thread?.model)
    ];
    const contextUsage = composerContextUsage(
      view?.lastUsage,
      selectedModel?.context_window,
      view?.compacting ?? false,
    );
    const sessionUsageText = formatSessionUsage(this.#sessionUsage);
    return html`
      <header class="thread-header">
        <div class="thread-tabs" role="tablist" aria-label="Threads">
          ${repeat(
            threads,
            (candidate) => candidate.id,
            (candidate, index) => html`
              <button
                type="button"
                role="tab"
                aria-selected=${!this.#newThreadSetupOpen && candidate.id === this.threadId ? "true" : "false"}
                tabindex=${rovingTabIndex(index, selectedTabIndex, threadTabCount)}
                @keydown=${(event: KeyboardEvent) =>
                  this.#selectThreadWithKeyboard(event, index, threads)}
                @click=${() => this.#selectThread(candidate.id)}
              >
                <span class="thread-tab-label">${threadTabLabel(
                  candidate,
                  this.#modes.find((mode) => mode.id === candidate.mode)?.display_name,
                )}</span>
                ${threadTodoProgress(candidate.todos) === ""
                  ? nothing
                  : html`<span class="thread-todo-progress">${threadTodoProgress(candidate.todos)}</span>`}
              </button>
            `,
          )}
          ${this.#newThreadSetupOpen
            ? html`
                <button
                  type="button"
                  role="tab"
                  class="provisional-thread-tab"
                  aria-selected="true"
                  tabindex=${rovingTabIndex(threads.length, selectedTabIndex, threadTabCount)}
                  @keydown=${(event: KeyboardEvent) =>
                    this.#selectThreadWithKeyboard(event, threads.length, threads)}
                >
                  <span class="thread-tab-label">New Thread</span>
                </button>
              `
            : nothing}
          <button
            type="button"
            aria-label="New thread"
            title="New thread"
            ?disabled=${this.sessionId === "" || this.#newThreadSetupOpen || this.#newThreadBusy}
            @click=${this.openNewThreadSetup}
          >+</button>
        </div>
      </header>

      ${this.#newThreadSetupOpen
        ? html`
            <trouve-new-thread-setup
              session-title=${sessionTitle}
              .busy=${this.#newThreadBusy}
              .errorMessage=${this.#newThreadError}
              @trouve-new-thread-submit=${this.#submitNewThread}
              @trouve-new-thread-cancel=${this.#cancelNewThread}
            ></trouve-new-thread-setup>
          `
        : html`
      ${this.#renderChat(
        view?.items ?? [],
        view?.turnRunning ?? false,
        turnControls.effectiveTurnRunning,
        view?.thinking ?? false,
        view?.compacting ?? false,
        view?.turnModels ?? new Map<number, string>(),
        view?.turnDurationMs ?? new Map<number, number>(),
        turnControls.activityLabel,
        view?.hasOlder ?? false,
      )}

      ${this.#renderQueue(
        view?.queue ?? [],
        turnControls.effectiveTurnRunning,
        connectivityBlocked,
      )}

      <form class="composer" @submit=${this.#sendMessage}>
        ${this.#pendingAttachments.length === 0
          ? nothing
          : html`
              <ul class="pending-attachments" aria-label="Pending attachments">
                ${this.#pendingAttachments.map(
                  (attachment, index) => html`
                    <li>
                      <span>${attachment.upload.name}</span>
                      <small>${this.#formatBytes(attachment.size)}</small>
                      <button type="button" aria-label=${`Remove ${attachment.upload.name}`} ?disabled=${this.#requestPending} @click=${() => this.#removeAttachment(index)}>×</button>
                    </li>
                  `,
                )}
              </ul>
            `}
        ${completion === undefined ? nothing : this.#renderComposerCompletion(completion)}
        ${services.deployment !== "pwa" || thread === undefined
          ? nothing
          : html`<div class="composer-quick-replies" aria-label="Quick replies">
              ${PWA_QUICK_REPLIES.map(({ label, prompt }) => html`<button
                type="button"
                ?disabled=${this.#requestPending || connectivityBlocked}
                @click=${() => this.#applyQuickReply(prompt)}
              >${label}</button>`)}
            </div>`}
        <div class="composer-entry">
          <textarea
            name="message"
            aria-label="Message"
            role=${completion === undefined ? nothing : "combobox"}
            aria-autocomplete=${completion === undefined ? nothing : "list"}
            aria-controls=${completion === undefined ? nothing : "composer-completions"}
            aria-expanded=${completion === undefined ? nothing : "true"}
            aria-activedescendant=${completion === undefined || completion.matches.length === 0
              ? nothing
              : `composer-completion-${Math.min(this.#completionSelected, completion.matches.length - 1)}`}
            placeholder=${thread === undefined
              ? "Select or create a thread first"
              : connectivityBlocked
                ? "Offline — prompts are disabled"
                : "Message the agent…  (Shift+Enter for a new line)"}
            rows="1"
            .value=${live(this.#composerDraft)}
            ?disabled=${thread === undefined || this.#requestPending || connectivityBlocked}
            @input=${this.#composerChanged}
            @select=${this.#composerCursorMoved}
            @click=${this.#composerCursorMoved}
            @keydown=${this.#composerKeydown}
            @compositionstart=${this.#composerCompositionStarted}
            @compositionend=${this.#composerCompositionEnded}
            @paste=${this.#composerPaste}
          ></textarea>
          ${turnControls.action === "cancel"
            ? html`<wa-button
                class="composer-submit"
                type="button"
                title=${turnControls.accessibleLabel}
                @click=${this.#cancelTurn}
                ?disabled=${turnControls.disabled}
              >${turnControls.label}</wa-button>`
            : turnControls.submit
              ? html`<wa-button
                class="composer-submit"
                type="submit"
                variant="brand"
                title=${turnControls.accessibleLabel}
                ?disabled=${turnControls.disabled}
              >${turnControls.label}</wa-button>`
              : html`<wa-button
                  class="composer-submit"
                  type="button"
                  title=${turnControls.accessibleLabel}
                  ?disabled=${turnControls.disabled}
                >${turnControls.label}</wa-button>`}
        </div>
        <div class="composer-controls" aria-label="Composer options">
          ${thread === undefined
            ? nothing
            : html`
                <label class="composer-option mode-option">
                  <span>Mode</span>
                  <select
                    aria-label="Mode"
                    .value=${thread.mode}
                    ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                    @change=${(event: Event) => this.#updateThreadSetting(
                      { mode: (event.currentTarget as HTMLSelectElement).value },
                      "Mode could not be changed.",
                    )}
                  >
                    ${this.#modes.some((mode) => mode.id === thread.mode)
                      ? nothing
                      : html`<option value=${thread.mode}>${thread.mode}</option>`}
                    ${this.#modes.map(
                      (mode) => html`<option value=${mode.id}>${mode.display_name}</option>`,
                    )}
                  </select>
                </label>
                <div class="composer-option model-option">
                  <span>Model</span>
                  <trouve-model-picker
                    accessible-label="Model"
                    .value=${thread.model}
                    .models=${this.#models}
                    .health=${modelHealth}
                    .disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || this.#models.length === 0 || connectivityBlocked}
                    @trouve-model-picked=${(event: CustomEvent<{ readonly modelId: string }>) => this.#updateThreadSetting(
                      { model: event.detail.modelId, model_options: {} },
                      "Model could not be changed.",
                    )}
                  ></trouve-model-picker>
                </div>
                ${selectedModelHealth === undefined
                  ? nothing
                  : html`<details class=${`model-health-pill tone-${selectedModelHealth.tone}`}>
                      <summary title=${selectedModelHealth.detail}>
                        <span class=${`model-health-dot tone-${selectedModelHealth.tone}`} aria-hidden="true"></span>
                        <span>${selectedModelHealth.summary}</span>
                      </summary>
                      <pre class="model-health-detail">${selectedModelHealth.detail}</pre>
                    </details>`}
                ${modelControls.thinking === undefined
                  ? nothing
                  : html`
                      <label class="composer-option thinking-option">
                        <span>Thinking</span>
                        <select
                          aria-label="Thinking level"
                          .value=${modelControls.thinking.selected}
                          ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                          @change=${(event: Event) => this.#updateThreadModelOption(
                            modelControls.thinking!.key,
                            (event.currentTarget as HTMLSelectElement).value,
                            "Thinking level could not be changed.",
                          )}
                        >
                          ${modelControls.thinking.selected === ""
                            ? html`<option value="" disabled>Select…</option>`
                            : nothing}
                          ${modelControls.thinking.values.map(
                            (value) => html`<option value=${value}>${modelOptionLabel(value)}</option>`,
                          )}
                        </select>
                      </label>
                    `}
                <label class="composer-option permission-option">
                  <span class=${thread.permission_mode === "yolo" ? "permission-yolo" : ""}>Permissions</span>
                  <span class="permission-control-row">
                    <select
                      class=${thread.permission_mode === "yolo" ? "permission-yolo" : ""}
                      aria-label="Permission mode"
                      .value=${thread.permission_mode}
                      ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                      @change=${(event: Event) => this.#updateThreadSetting(
                        { permission_mode: (event.currentTarget as HTMLSelectElement).value as "ask" | "allow_list" | "yolo" },
                        "Permission mode could not be changed.",
                      )}
                    >
                      <option value="ask">Ask</option>
                      <option value="allow_list">Allow list</option>
                      <option value="yolo">Yolo</option>
                    </select>
                    ${thread.permission_mode === "yolo"
                      ? html`<span class="permission-warning" role="img" aria-label="Warning: YOLO changes run without approval" title="YOLO: changes run without approval">⚠</span>`
                      : nothing}
                  </span>
                </label>
                ${modelControls.context === undefined
                  ? nothing
                  : html`
                      <label class="composer-option context-option">
                        <span>Context</span>
                        <select
                          aria-label="Context size"
                          .value=${modelControls.context.selected}
                          ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                          @change=${(event: Event) => this.#updateThreadModelOption(
                            "context",
                            (event.currentTarget as HTMLSelectElement).value,
                            "Context size could not be changed.",
                          )}
                        >
                          ${modelControls.context.selected === ""
                            ? html`<option value="" disabled>Select…</option>`
                            : nothing}
                          ${modelControls.context.values.map(
                            (value) => html`<option value=${value}>${value.toUpperCase()}</option>`,
                          )}
                        </select>
                      </label>
                    `}
                ${modelControls.fast === undefined
                  ? nothing
                  : html`
                      <div class="composer-option composer-fast-option">
                        <span>Fast</span>
                        <button
                          type="button"
                          aria-pressed=${modelControls.fast.selected ? "true" : "false"}
                          ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                          @click=${() => this.#updateThreadModelOption(
                            "fast",
                            !modelControls.fast!.selected,
                            "Fast mode could not be changed.",
                          )}
                        >${modelControls.fast.selected ? "On" : "Off"}</button>
                      </div>
                    `}
              `}
          <span class="composer-controls-spacer" aria-hidden="true"></span>
          <label
            class=${`attachment-button ${attachmentDisabled ? "disabled" : ""}`}
            aria-disabled=${attachmentDisabled ? "true" : "false"}
            title="Attach files"
          >
            <span aria-hidden="true">📎</span><span class="visually-hidden">Attach files</span>
            <input
              type="file"
              multiple
              ?disabled=${attachmentDisabled}
              @click=${this.#attachmentPickerClicked}
              @change=${this.#filesSelected}
            />
          </label>
          <button
            class="history-mode"
            type="button"
            aria-pressed=${this.#accessibleHistory}
            title="Render the complete transcript for assistive technology"
            @click=${this.#toggleAccessibleHistory}
          >${this.#accessibleHistory ? "Use windowed history" : "Use full history"}</button>
          ${thread === undefined
            ? nothing
            : html`
                <span
                  class="composer-context-usage"
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
                  ${contextUsage.unavailable
                    ? html`<span class="context-dial-glyph" aria-hidden="true">!</span>`
                    : contextUsage.compacting
                      ? html`<span class="context-dial-glyph compacting" aria-hidden="true">↻</span>`
                      : nothing}
                </span>
                ${sessionUsageText === "" && !this.#usagePending
                  ? nothing
                  : html`<span class="composer-session-usage" aria-live="polite">
                      ${sessionUsageText === "" ? "Loading usage…" : sessionUsageText}
                    </span>`}
              `}
          ${this.#requestError === ""
            ? nothing
            : html`<span class="inline-error" role="alert">${this.#requestError}</span>`}
          ${connectivityBlocked
            ? html`<span class="inline-warning" role="status">Offline · no local models available</span>`
            : nothing}
        </div>
      </form>
      `}
    `;
  }

  #activeComposerCompletion(
    commands: readonly { readonly name: string; readonly description?: string }[],
  ): ActiveComposerCompletion | undefined {
    if (this.#completionDismissed || this.#composerComposing) {
      this.#completionWorkerGeneration += 1;
      this.#completionWorkerPending = false;
      this.#completionWorkerRequestedKey = "";
      return undefined;
    }
    const token = composerCompletionToken(this.#composerDraft, this.#composerCursor);
    if (token === undefined) {
      this.#completionWorkerGeneration += 1;
      this.#completionWorkerPending = false;
      this.#completionWorkerRequestedKey = "";
      return undefined;
    }
    const candidates: readonly ComposerCompletionCandidate[] = token.kind === "command"
      ? commands.map((command) => ({
          value: command.name.replace(/^\/+/, ""),
          detail: command.description ?? "",
        }))
      : this.#sessionPaths.map((path) => ({ value: path }));
    let matches: readonly RankedComposerCompletion[];
    let searching = false;
    if (candidates.length < WORKER_COMPLETION_THRESHOLD) {
      this.#completionWorkerGeneration += 1;
      this.#completionWorkerPending = false;
      this.#completionWorkerRequestedKey = "";
      matches = rankComposerCompletions(candidates, token.query);
    } else {
      const key = `${token.kind}\u0000${token.query}\u0000${candidates
        .map((candidate) => `${candidate.value}\u0001${candidate.detail ?? ""}`)
        .join("\u0000")}`;
      if (key !== this.#completionWorkerRequestedKey) {
        this.#completionWorkerRequestedKey = key;
        this.#completionWorkerPending = true;
        const generation = ++this.#completionWorkerGeneration;
        void rankComposerCompletionsOffThread(
          candidates,
          token.query,
          MAX_COMPOSER_COMPLETIONS,
        ).then(
          (nextMatches) => {
            if (generation !== this.#completionWorkerGeneration || !this.isConnected) return;
            this.#completionWorkerKey = key;
            this.#completionWorkerMatches = nextMatches;
            this.#completionWorkerPending = false;
            this.#completionSelected = 0;
            this.requestUpdate();
          },
          () => {
            if (generation !== this.#completionWorkerGeneration || !this.isConnected) return;
            this.#completionWorkerRequestedKey = "";
            this.#completionWorkerPending = false;
            this.requestUpdate();
          },
        );
      }
      matches = this.#completionWorkerKey === key ? this.#completionWorkerMatches : [];
      searching = this.#completionWorkerPending;
    }
    const loading = token.kind === "file" && this.#pathsLoadingSessionId === this.sessionId;
    const unavailable =
      token.kind === "file" && this.#pathsUnavailableSessionId === this.sessionId;
    const emptyMessage = token.kind === "command"
      ? commands.length === 0
        ? "Slash commands are unavailable for this thread."
        : "No matching slash commands."
      : this.#pathsSessionId === this.sessionId && this.#sessionPaths.length === 0
        ? "No workspace files are available."
        : "No matching workspace files.";
    return { token, matches, loading, searching, unavailable, emptyMessage };
  }

  #renderComposerCompletion(completion: ActiveComposerCompletion) {
    const selected = Math.min(
      this.#completionSelected,
      Math.max(0, completion.matches.length - 1),
    );
    return html`
      <div
        id="composer-completions"
        class="composer-completions"
        role="listbox"
        aria-label=${completion.token.kind === "command" ? "Slash commands" : "Workspace files"}
      >
        ${completion.matches.map(
          (match, index) => html`
            <button
              id=${`composer-completion-${index}`}
              class="composer-completion-option"
              type="button"
              role="option"
              aria-selected=${index === selected ? "true" : "false"}
              @mousedown=${(event: MouseEvent) => event.preventDefault()}
              @click=${() => this.#applyComposerCompletion(completion.token, match.value)}
            >
              <span>${completion.token.kind === "command" ? `/${match.value}` : match.value}</span>
              ${match.detail === "" ? nothing : html`<small>${match.detail}</small>`}
            </button>
          `,
        )}
        ${completion.loading
          ? html`<p class="composer-completion-status" role="status">Loading workspace files…</p>`
          : completion.searching
            ? html`<p class="composer-completion-status" role="status">Searching suggestions…</p>`
          : completion.unavailable && completion.matches.length === 0
            ? html`
                <div class="composer-completion-status completion-error" role="status">
                  <span>File suggestions are unavailable.</span>
                  <button type="button" @click=${this.#retryMentionPaths}>Retry</button>
                </div>
              `
            : completion.matches.length === 0
              ? html`<p class="composer-completion-status" role="status">${completion.emptyMessage}</p>`
              : nothing}
      </div>
    `;
  }

  #renderChat(
    items: readonly ThreadChatItem[],
    turnRunning: boolean,
    effectiveTurnRunning: boolean,
    thinking: boolean,
    compacting: boolean,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    activityOverride: string | undefined,
    hasOlder: boolean,
  ) {
    this.#syncQuestionWizards(items);
    const presentation = indexChatPresentation(items);
    const layout = buildChatLayout(items);
    let runningTurn: number | undefined;
    for (const [turn, state] of presentation.turnStates) {
      if (state.kind === "running" && (runningTurn === undefined || turn > runningTurn)) {
        runningTurn = turn;
      }
    }
    const activityLabel = activityOverride
      ?? (turnRunning ? runningActivityLabel(items, thinking) : undefined);
    let nestedActivityUnitId: string | undefined;
    if (activityLabel !== undefined && runningTurn !== undefined) {
      for (let index = layout.units.length - 1; index >= 0; index -= 1) {
        const unit = layout.units[index];
        if (
          unit?.kind === "agent"
          && unit.turn === runningTurn
          && (this.#messageDisclosure.get(unit.id) ?? true)
        ) {
          nestedActivityUnitId = unit.id;
          break;
        }
      }
    }
    const virtualItems: VirtualChatItem[] = layout.units.map((unit, unitIndex) => ({
      id: unit.id,
      kind: "unit",
      unitIndex,
      estimatedHeight:
        unit.kind === "agent"
          ? Math.max(150, Math.min(620, unit.items.length * 90))
          : unit.kind === "user"
            ? 80
            : 70,
      heavyweight: unit.kind === "agent" && unit.items.some(
        (item) => item.kind === "tool" || item.kind === "questions",
      ),
    }));
    if (compacting) {
      virtualItems.push({
        id: "ephemeral:compacting",
        kind: "compacting",
        estimatedHeight: 32,
      });
    }
    if (activityLabel !== undefined && nestedActivityUnitId === undefined) {
      virtualItems.push({
        id: "ephemeral:activity",
        kind: "activity",
        label: activityLabel,
        estimatedHeight: 32,
      });
    }
    if (hasOlder || this.#historyLoading || this.#historyError !== "") {
      virtualItems.unshift({
        id: CHAT_HISTORY_LOADER_ID,
        kind: "history",
        estimatedHeight: this.#historyError === "" ? 38 : 58,
      });
    }
    if (virtualItems.length > 0) {
      // Keep the transcript tail flush with the virtual canvas. The queue and
      // composer own Slint's 8px separation outside the scrollport; a virtual
      // end spacer would duplicate that gap and move every tail-pin target.
      virtualItems.unshift({
        id: CHAT_START_SPACER_ID,
        kind: "edge-spacer",
        edge: "start",
        estimatedHeight: 10,
      });
    }
    this.#virtualizer.setMode(this.#accessibleHistory ? "accessible" : "virtual");
    this.#virtualizer.setItems(virtualItems);
    if (this.#pendingHistoryPrepend !== undefined) {
      const before = this.#pendingHistoryPrepend;
      this.#pendingHistoryPrepend = undefined;
      const addedHeight = Math.max(
        0,
        this.#virtualizer.window().totalHeight - before.totalHeight,
      );
      this.#virtualizer.setViewport(
        before.scrollTop + addedHeight,
        this.#viewportHeight,
      );
    }
    if (this.#restoredScrollThreadId !== this.threadId) {
      const bookmark = this.scrollBookmark;
      if (bookmark === undefined) {
        this.#restoredScrollThreadId = this.threadId;
      } else {
        const unitId = layout.unitIdForItem.get(bookmark.itemId) ?? bookmark.itemId;
        if (virtualItems.some((item) => item.id === unitId)) {
          this.#virtualizer.restoreBookmark({
            id: unitId,
            offset: bookmark.offset,
          });
        } else {
          this.#invalidScrollBookmarkThreadId = this.threadId;
        }
        this.#restoredScrollThreadId = this.threadId;
      }
    }
    const window = this.#virtualizer.window();
    return html`
      <div
        class="chat-stream"
        data-thread-id=${this.threadId}
        role="log"
        aria-label="Conversation"
        aria-live=${window.followingTail ? "polite" : "off"}
        aria-relevant="additions text"
        aria-busy=${effectiveTurnRunning || compacting}
        @wheel=${this.#chatScrollIntended}
        @pointerdown=${this.#chatScrollIntended}
        @touchstart=${this.#chatScrollIntended}
        @scroll=${this.#chatScrolled}
        @scrollend=${this.#chatScrollEnded}
      >
        ${virtualItems.length === 0
          ? nothing
          : html`<div
              class="chat-virtual-canvas"
              style=${`height:${window.totalHeight}px`}
            >${repeat(window.items, ({ item }) => item.id, ({ item, start }) => {
              const style = `inset-block-start:${start}px`;
              if (item.kind === "edge-spacer") {
                return html`<div
                  class=${`chat-edge-spacer ${item.edge}`}
                  data-virtual-id=${item.id}
                  style=${style}
                  aria-hidden="true"
                ></div>`;
              }
              if (item.kind === "compacting") {
                return html`<div data-virtual-id=${item.id} style=${style}>
                  <p class="activity-row" role="status">Compacting context…</p>
                </div>`;
              }
              if (item.kind === "activity") {
                return html`<div data-virtual-id=${item.id} style=${style}>
                  ${this.#renderActivityRow(item.label)}
                </div>`;
              }
              if (item.kind === "history") {
                return html`<div
                  class="chat-history-loader"
                  data-virtual-id=${item.id}
                  style=${style}
                  role="status"
                >
                  ${this.#historyLoading
                    ? html`<span>Loading earlier messages…</span>`
                    : html`
                        <button type="button" @click=${() => this.#loadOlderHistory(false)}>
                          Load earlier messages
                        </button>
                        ${this.#historyError === ""
                          ? nothing
                          : html`<span class="error-text">${this.#historyError}</span>`}
                      `}
                </div>`;
              }
              const unit = layout.units[item.unitIndex];
              return unit === undefined
                ? nothing
                : html`<div data-virtual-id=${item.id} style=${style}>${this.#renderUnit(
                    unit,
                    turnModels,
                    turnDurationMs,
                    presentation,
                    unit.id === nestedActivityUnitId ? activityLabel : undefined,
                  )}</div>`;
            })}</div>`}
        ${!window.followingTail && virtualItems.length > 0
          ? html`<button class="follow-tail" type="button" @click=${this.#followTail}>Jump to latest</button>`
          : nothing}
      </div>
    `;
  }

  #renderUnit(
    unit: ChatRenderUnit,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityLabel: string | undefined,
  ) {
    if (unit.kind === "user") {
      return html`
        ${unit.divider ? html`<div class="turn-rule" aria-hidden="true"></div>` : nothing}
        ${this.#renderItem(unit.item, turnModels, turnDurationMs, presentation)}
      `;
    }
    if (unit.kind === "status") {
      return this.#renderItem(unit.item, turnModels, turnDurationMs, presentation);
    }
    return this.#renderAgentCard(
      unit,
      turnModels,
      turnDurationMs,
      presentation,
      activityLabel,
    );
  }

  #renderAgentCard(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityLabel: string | undefined,
  ) {
    this.#ensureMarkdown();
    const assistantItems = unit.items.filter(
      (item): item is Extract<AgentChatItem, { readonly kind: "assistant" }> =>
        item.kind === "assistant",
    );
    const joined = assistantItems
      .map((item) => item.content)
      .filter((content) => content !== "")
      .join("\n\n");
    const open = this.#messageDisclosure.get(unit.id) ?? true;
    const raw = this.#rawAssistantTurns.has(unit.turn);
    const turnState = presentation.turnStates.get(unit.turn);
    const metadata = turnState?.kind === "completed"
      ? formatTurnMetadata(turnState.usage, turnDurationMs.get(unit.turn))
      : "";
    const preview = collapsedChatPreview(joined);
    return html`
      <article class="message turn-card assistant-message agent-turn-card">
        <header class="message-header agent-header ${open ? "" : "collapsed"}">
          <button
            class="message-disclosure"
            type="button"
            aria-expanded=${open ? "true" : "false"}
            aria-label=${open ? "Collapse agent message" : "Expand agent message"}
            @click=${() => this.#toggleMessageDisclosure(unit.id, true)}
          >
            <span class="disclosure-icon" aria-hidden="true">${open ? "▾" : "▸"}</span>
            <strong>Agent</strong>
            ${turnModels.get(unit.turn) === undefined
              ? nothing
              : html`<small class="agent-model-label">(${turnModels.get(unit.turn)})</small>`}
            ${open
              ? html`<span class="agent-header-spacer"></span>`
              : html`<small class="agent-collapsed-preview">${preview}</small>`}
            ${metadata === ""
              ? nothing
              : html`<small class="turn-metadata">${metadata}</small>`}
          </button>
          <div class="message-actions">
            <button
              type="button"
              aria-pressed=${raw ? "true" : "false"}
              aria-label=${raw ? "Show formatted assistant output" : "Show raw assistant output"}
              title=${raw ? "Show styled view" : "Show raw Markdown"}
              @click=${() => this.#toggleRawAssistant(unit.turn)}
            ><span aria-hidden="true">👁</span></button>
            ${this.#renderCopyButton(
              `agent:${unit.id}`,
              assistantCopyText(joined, raw),
              "Copy assistant output",
            )}
          </div>
        </header>
        ${open
          ? html`<div class="message-body turn-body-stream agent-body-stream">
              ${this.#renderAgentBody(unit, raw, turnModels, turnDurationMs, presentation)}
              ${activityLabel === undefined
                ? nothing
                : this.#renderActivityRow(activityLabel)}
            </div>`
          : nothing}
      </article>
    `;
  }

  #renderActivityRow(label: string) {
    return html`<p class="activity-row agent-activity" role="status">
      <span class="activity-dots" aria-hidden="true"><i></i><i></i><i></i></span>
      <span>${label}</span>
    </p>`;
  }

  #renderAgentBody(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
    raw: boolean,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
  ) {
    const rows: unknown[] = [];
    let index = 0;
    while (index < unit.items.length) {
      const item = unit.items[index];
      if (item === undefined) break;
      if (item.kind === "assistant") {
        const stretch: Extract<AgentChatItem, { readonly kind: "assistant" }>[] = [];
        while (index < unit.items.length && unit.items[index]?.kind === "assistant") {
          stretch.push(unit.items[index] as Extract<AgentChatItem, { readonly kind: "assistant" }>);
          index += 1;
        }
        const content = stretch.map((part) => part.content).filter(Boolean).join("\n\n");
        if (content !== "") {
          rows.push(raw
            ? html`<pre class="assistant-raw agent-text-block">${content}</pre>`
            : html`<div class="agent-text-block"><trouve-markdown-view
                class="turn-markdown"
                .content=${content}
                .streaming=${stretch.some((part) => !part.complete)}
              ></trouve-markdown-view></div>`);
        }
        continue;
      }
      if (item.kind === "questions") {
        rows.push(this.#renderItem(item, turnModels, turnDurationMs, presentation));
        index += 1;
        continue;
      }

      const run: AgentChatItem[] = [];
      while (index < unit.items.length) {
        const candidate = unit.items[index];
        if (candidate === undefined || candidate.kind === "assistant" || candidate.kind === "questions") break;
        run.push(candidate);
        index += 1;
      }
      if (run.length < 2) {
        const only = run[0];
        if (only !== undefined) {
          rows.push(this.#renderItem(only, turnModels, turnDurationMs, presentation));
        }
        continue;
      }
      rows.push(this.#renderActivityGroup(
        unit,
        run,
        turnModels,
        turnDurationMs,
        presentation,
      ));
    }
    return rows;
  }

  #renderActivityGroup(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
    items: readonly AgentChatItem[],
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
  ) {
    const first = items[0];
    if (first === undefined) return nothing;
    const key = `activity:${unit.id}:${first.id}`;
    const needsApproval = items.some(
      (item) => item.kind === "tool" && item.status === "awaiting-approval",
    );
    const open = needsApproval || (this.#messageDisclosure.get(key) ?? false);
    return html`
      <details
        class="activity-group"
        .open=${open}
      >
        <summary
          @click=${(event: Event) =>
            this.#toggleActivityGroup(event, key, open, needsApproval)}
        >
          <span class="disclosure-icon" aria-hidden="true">${open ? "▾" : "▸"}</span>
          <strong>${activityGroupSummary(items)}</strong>
        </summary>
        ${open
          ? html`<div class="activity-group-body">
              ${items.map((item) => this.#renderItem(
                item,
                turnModels,
                turnDurationMs,
                presentation,
              ))}
            </div>`
          : nothing}
      </details>
    `;
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
    this.#requestDisclosureUpdate();
  }

  #renderQueue(
    queue: readonly QueuedPrompt[],
    turnRunning: boolean,
    connectivityBlocked: boolean,
  ) {
    if (queue.length === 0) return nothing;
    const controls = queueControlState({
      threadAvailable: this.threadId !== "",
      queueLength: queue.length,
      turnRunning,
      busy: this.#queueBusy !== "",
      connectivityBlocked,
    });
    return html`
      <section class="queue-panel" aria-busy=${this.#queueBusy !== "" ? "true" : "false"}>
        <header>
          <span role="status" aria-live="polite">${queue.length} queued prompt${queue.length === 1 ? "" : "s"}</span>
          ${turnRunning
            ? nothing
            : html`<button
                class="primary"
                type="button"
                data-queue-action="dispatch"
                ?disabled=${controls.dispatchDisabled}
                @click=${this.#dispatchQueue}
              >Send now</button>`}
        </header>
        <ol>
          ${repeat(
            queue,
            (prompt) => prompt.id,
            (prompt, index) => html`
              <li
                data-queue-id=${prompt.id}
                data-queue-drop=${this.#queueDropId === prompt.id ? this.#queueDropPlacement : ""}
                @dragover=${(event: DragEvent) => this.#dragQueueOver(event, prompt.id)}
                @drop=${(event: DragEvent) => void this.#dropQueued(event, queue, prompt.id)}
              >
                <div class="queue-row">
                  <span
                    class="queue-grip"
                    draggable=${!controls.mutationsDisabled && queue.length > 1 ? "true" : "false"}
                    aria-label="Drag to reorder queued prompt"
                    title="Drag to reorder"
                    @dragstart=${(event: DragEvent) => this.#startQueueDrag(
                      event,
                      prompt.id,
                      controls.mutationsDisabled || queue.length < 2,
                    )}
                    @dragend=${this.#endQueueDrag}
                  >⠿</span>
                  <span class="queue-index" aria-hidden="true">${index + 1}.</span>
                  <p title=${prompt.content}>${queuePreview(prompt.content)}</p>
                  ${prompt.attachments === undefined || prompt.attachments.length === 0
                    ? nothing
                    : html`<span
                        class="queue-attachment-badge"
                        role="img"
                        aria-label=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                        title=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                      >📎${prompt.attachments.length}</span>`}
                  <div class="queue-actions" aria-label=${`Actions for queued prompt ${index + 1}`}>
                    ${turnRunning
                      ? nothing
                      : html`<button type="button" data-queue-action="send-now" aria-label="Send this queued prompt now" title="Send now" ?disabled=${controls.dispatchDisabled} @click=${() => this.#sendQueuedNow(queue, index)}>▶</button>`}
                    <button type="button" data-queue-action="earlier" aria-label="Run earlier" title="Run earlier" ?disabled=${index === 0 || controls.mutationsDisabled} @click=${() => this.#moveQueued(queue, index, -1)}>↑</button>
                    <button type="button" data-queue-action="later" aria-label="Run later" title="Run later" ?disabled=${index === queue.length - 1 || controls.mutationsDisabled} @click=${() => this.#moveQueued(queue, index, 1)}>↓</button>
                    <button type="button" data-queue-action="edit" aria-label="Edit queued prompt" title="Edit" ?disabled=${controls.mutationsDisabled} @click=${() => this.#startQueueEdit(prompt)}>✎</button>
                    <button class="danger" type="button" data-queue-action="delete" aria-label="Remove from queue" title="Remove from queue" ?disabled=${controls.mutationsDisabled} @click=${() => this.#deleteQueued(queue, prompt.id)}>✕</button>
                  </div>
                </div>
                ${this.#queueEditId === prompt.id
                  ? html`
                      <form class="queue-edit" @submit=${(event: SubmitEvent) => this.#saveQueued(event, prompt)}>
                        <label class="visually-hidden" for=${`queue-${prompt.id}`}>Queued prompt</label>
                        <textarea
                          id=${`queue-${prompt.id}`}
                          data-queue-action="edit-input"
                          name="content"
                          rows="3"
                          required
                          .value=${live(this.#queueEditDraft)}
                          ?disabled=${controls.mutationsDisabled}
                          @input=${(event: InputEvent) => {
                            this.#queueEditDraft = (event.currentTarget as HTMLTextAreaElement).value;
                            this.requestUpdate();
                          }}
                        ></textarea>
                        <div class="queue-actions">
                          <button type="button" data-queue-action="cancel-edit" ?disabled=${controls.mutationsDisabled} @click=${this.#cancelQueueEdit}>Cancel</button>
                          <button class="primary" type="submit" data-queue-action="save" ?disabled=${controls.mutationsDisabled || this.#queueEditDraft.trim() === ""}>Save</button>
                        </div>
                      </form>`
                  : nothing}
              </li>
            `,
          )}
        </ol>
        ${this.#queueError === ""
          ? nothing
          : html`<p class="queue-error" role="alert">${this.#queueError}</p>`}
      </section>
    `;
  }

  readonly #chatScrolled = (event: Event): void => {
    const viewport = event.currentTarget as HTMLElement;
    if (
      viewport.dataset["threadId"] !== this.threadId ||
      this.#restoredScrollThreadId !== this.threadId
    ) return;
    const before = this.#virtualizer.window();
    // The sticky jump control is an overlay visually, but WebKit retains its
    // border-box in normal flow and includes it in scrollHeight. Its height is
    // cached after rendering so this hot path never forces synchronous layout.
    const tailGap = Math.max(
      0,
      viewport.scrollHeight
        - this.#followTailControlHeight
        - viewport.clientHeight
        - viewport.scrollTop,
    );
    const atTail = tailGap <= CHAT_TAIL_EPSILON_PX;
    const userInitiated = this.#chatScrollIntent;
    this.#chatScrollIntent = false;
    if (
      (
        this.#programmaticScrollFrame !== undefined
        || this.#tailConvergenceFrame !== undefined
      )
      && !userInitiated
    ) {
      // Row measurement and tail corrections already updated the virtualizer.
      // Ignore their resulting DOM events instead of treating them as another
      // user scroll and starting a render/persistence loop.
      return;
    }
    if (before.followingTail && atTail) return;
    this.#scrollCorrectionResumeAt = Date.now() + CHAT_SCROLL_CORRECTION_SETTLE_MS;
    if (userInitiated) {
      this.#cancelProgrammaticScrollWindow();
      this.#cancelTailConvergence();
    }
    this.#virtualizer.setViewport(
      viewport.scrollTop,
      viewport.clientHeight,
      { userInitiated: true, atTail },
    );
    const after = this.#virtualizer.window();
    if (viewport.scrollTop <= Math.max(320, viewport.clientHeight)) {
      void this.#loadOlderHistory(false);
    }
    if (!before.followingTail && after.followingTail) {
      this.#cancelScheduledChatPosition();
      this.#scheduleTailConvergence();
      this.#emitChatPosition();
    } else {
      this.#scheduleChatPosition();
    }
    if (!sameVirtualRenderWindow(before, after)) this.#scheduleScrollRender();
  };

  readonly #chatScrollEnded = (): void => {
    this.#flushScheduledChatPosition();
  };

  readonly #chatScrollIntended = (event: Event): void => {
    if (event.type === "wheel") {
      const wheel = event as WheelEvent;
      if (wheel.deltaX === 0 && wheel.deltaY === 0) return;
    }
    if (event.type === "pointerdown") {
      const target = event.target;
      if (
        target instanceof Element
        && target.closest("button, a, input, textarea, select, summary") !== null
      ) return;
    }
    this.#chatScrollIntent = true;
  };

  #setChatScrollTop(viewport: HTMLElement, scrollTop: number): void {
    if (Math.abs(viewport.scrollTop - scrollTop) <= 0.5) return;
    if (this.#programmaticScrollFrame !== undefined) {
      globalThis.cancelAnimationFrame(this.#programmaticScrollFrame);
    }
    this.#programmaticScrollFrame = globalThis.requestAnimationFrame(() => {
      this.#programmaticScrollFrame = globalThis.requestAnimationFrame(() => {
        this.#programmaticScrollFrame = undefined;
      });
    });
    viewport.scrollTop = scrollTop;
  }

  #syncMountedVirtualGeometry(
    viewport: HTMLElement,
    virtualWindow: VirtualWindow<VirtualChatItem>,
  ): void {
    const canvas = viewport.querySelector<HTMLElement>(".chat-virtual-canvas");
    if (canvas !== null) canvas.style.height = `${virtualWindow.totalHeight}px`;
    const starts = new Map(
      virtualWindow.items.map(({ item, start }) => [item.id, start] as const),
    );
    for (const row of viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")) {
      const id = row.dataset["virtualId"];
      const start = id === undefined ? undefined : starts.get(id);
      if (start !== undefined) row.style.setProperty("inset-block-start", `${start}px`);
    }
  }

  #transcriptTailScrollTop(viewport: HTMLElement): number {
    return Math.max(
      0,
      viewport.scrollHeight
        - this.#followTailControlHeight
        - viewport.clientHeight,
    );
  }

  #cancelProgrammaticScrollWindow(): void {
    if (this.#programmaticScrollFrame !== undefined) {
      globalThis.cancelAnimationFrame(this.#programmaticScrollFrame);
      this.#programmaticScrollFrame = undefined;
    }
  }

  #scheduleTailConvergence(): void {
    this.#cancelTailConvergence();
    let remaining = CHAT_TAIL_CONVERGENCE_FRAMES;
    const converge = (): void => {
      this.#tailConvergenceFrame = undefined;
      if (!this.isConnected || !this.#virtualizer.window().followingTail) return;
      const viewport = this.querySelector<HTMLElement>(".chat-stream");
      if (viewport === null) return;
      this.#setChatScrollTop(viewport, this.#transcriptTailScrollTop(viewport));
      remaining -= 1;
      if (remaining > 0) {
        this.#tailConvergenceFrame = globalThis.requestAnimationFrame(converge);
      }
    };
    this.#tailConvergenceFrame = globalThis.requestAnimationFrame(converge);
  }

  #cancelTailConvergence(): void {
    if (this.#tailConvergenceFrame === undefined) return;
    globalThis.cancelAnimationFrame(this.#tailConvergenceFrame);
    this.#tailConvergenceFrame = undefined;
  }

  #scheduleScrollRender(): void {
    this.#scrollRenderFrame ??= globalThis.requestAnimationFrame(() => {
      this.#scrollRenderFrame = undefined;
      if (this.isConnected) this.requestUpdate();
    });
  }

  #cancelScheduledScrollRender(): void {
    if (this.#scrollRenderFrame === undefined) return;
    globalThis.cancelAnimationFrame(this.#scrollRenderFrame);
    this.#scrollRenderFrame = undefined;
  }

  #scheduleChatPosition(): void {
    if (this.#chatPositionTimer !== undefined) {
      clearTimeout(this.#chatPositionTimer);
    }
    this.#chatPositionTimer = setTimeout(() => {
      this.#chatPositionTimer = undefined;
      if (this.isConnected) this.#emitChatPosition();
    }, CHAT_POSITION_SETTLE_MS);
  }

  #flushScheduledChatPosition(): void {
    if (this.#chatPositionTimer === undefined) return;
    clearTimeout(this.#chatPositionTimer);
    this.#chatPositionTimer = undefined;
    this.#emitChatPosition();
  }

  #cancelScheduledChatPosition(): void {
    if (this.#chatPositionTimer === undefined) return;
    clearTimeout(this.#chatPositionTimer);
    this.#chatPositionTimer = undefined;
  }

  readonly #followTail = (): void => {
    this.#cancelScheduledChatPosition();
    this.#scrollCorrectionResumeAt = 0;
    this.#virtualizer.enableFollowTail();
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport !== null) {
      this.#setChatScrollTop(viewport, this.#transcriptTailScrollTop(viewport));
    }
    this.#scheduleTailConvergence();
    this.#emitChatPosition();
    this.requestUpdate();
  };

  #emitChatPosition(): void {
    if (this.threadId === "") return;
    const anchor = this.#virtualizer.bookmark();
    const bookmark = anchor === undefined
      ? undefined
      : Object.freeze({ itemId: anchor.id, offset: anchor.offset });
    this.dispatchEvent(new CustomEvent("trouve-chat-position", {
      bubbles: true,
      composed: true,
      detail: Object.freeze({ threadId: this.threadId, bookmark }),
    }));
  }

  async #loadOlderHistory(loadAll: boolean): Promise<void> {
    if (this.#historyLoading || this.threadId === "") return;
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) return;
    const threadId = this.threadId;
    const generation = this.#historyGeneration;
    const initialView = store.threadView(threadId);
    if (!initialView.hasOlder || initialView.itemOffset === 0) return;

    this.#historyLoading = true;
    this.#historyError = "";
    this.requestUpdate();
    try {
      do {
        const view = store.threadView(threadId);
        if (!view.hasOlder || view.itemOffset === 0) break;
        const page = await services.protocol.threadView(threadId, view.itemOffset);
        if (generation !== this.#historyGeneration || threadId !== this.threadId) return;
        const virtualWindow = this.#virtualizer.window();
        this.#pendingHistoryPrepend = {
          scrollTop: virtualWindow.scrollTop,
          totalHeight: virtualWindow.totalHeight,
        };
        if (!store.prependThreadViewSnapshot(threadId, page.value)) {
          this.#pendingHistoryPrepend = undefined;
          throw new Error("non-contiguous thread history page");
        }
      } while (loadAll);
    } catch {
      if (generation === this.#historyGeneration && threadId === this.threadId) {
        this.#historyError = "Earlier messages could not be loaded.";
      }
    } finally {
      if (generation === this.#historyGeneration && threadId === this.threadId) {
        this.#historyLoading = false;
        this.requestUpdate();
      }
    }
  }

  readonly #toggleAccessibleHistory = (): void => {
    this.#accessibleHistory = !this.#accessibleHistory;
    this.#virtualizer.setMode(this.#accessibleHistory ? "accessible" : "virtual");
    if (this.#accessibleHistory) void this.#loadOlderHistory(true);
    this.requestUpdate();
  };

  #renderItem(
    item: ThreadChatItem,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
  ) {
    switch (item.kind) {
      case "user": {
        this.#ensureMarkdown();
        const open = this.#messageDisclosure.get(item.id) ?? true;
        const preview = collapsedChatPreview(item.content)
          || `${item.attachments.length} attachment${item.attachments.length === 1 ? "" : "s"}`;
        return html`
          <article class="message turn-card user-message">
            <header class="message-header user-header ${open ? "" : "collapsed"}">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse your message" : "Expand your message"}
                @click=${() => this.#toggleMessageDisclosure(item.id, true)}
              >
                <span class="disclosure-icon" aria-hidden="true">${open ? "▾" : "▸"}</span>
                <strong>You</strong>
                ${open
                  ? html`<span class="agent-header-spacer"></span>`
                  : html`<small class="message-collapsed-preview">${preview}</small>`}
              </button>
              <div class="message-actions">
                ${this.#renderCopyButton(
                  `message:${item.id}`,
                  item.content,
                  "Copy your message",
                )}
              </div>
            </header>
            ${open
              ? html`
                  <div class="message-body turn-body-stream user-body-stream">
                    ${item.content === ""
                      ? nothing
                      : html`<trouve-markdown-view
                          class="turn-markdown"
                          .content=${item.content}
                        ></trouve-markdown-view>`}
                    ${this.#renderAttachments(item.attachments)}
                  </div>
                `
              : nothing}
          </article>
        `;
      }
      case "assistant": {
        this.#ensureMarkdown();
        const open = this.#messageDisclosure.get(item.id) ?? true;
        const raw = this.#rawAssistantTurns.has(item.turn);
        const turnState = presentation.turnStates.get(item.turn);
        const metadata = presentation.lastAssistantIds.has(item.id)
          && turnState?.kind === "completed"
          ? formatTurnMetadata(turnState.usage, turnDurationMs.get(item.turn))
          : "";
        return html`
          <article class="message turn-card assistant-message">
            <header class="message-header agent-header ${open ? "" : "collapsed"}">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse agent message" : "Expand agent message"}
                @click=${() => this.#toggleMessageDisclosure(item.id, true)}
              >
                <span class="disclosure-icon" aria-hidden="true">${open ? "▾" : "▸"}</span>
                <strong>Agent</strong>
                ${turnModels.get(item.turn) === undefined
                  ? nothing
                  : html`<small class="agent-model-label">(${turnModels.get(item.turn)})</small>`}
                ${metadata === ""
                  ? nothing
                  : html`<small class="turn-metadata">${metadata}</small>`}
              </button>
              <div class="message-actions">
                <button
                  type="button"
                  aria-pressed=${raw ? "true" : "false"}
                  aria-label=${raw ? "Show formatted assistant output" : "Show raw assistant output"}
                  title=${raw ? "Show formatted output" : "Show raw Markdown"}
                  @click=${() => this.#toggleRawAssistant(item.turn)}
                ><span aria-hidden="true">👁</span></button>
                ${this.#renderCopyButton(
                  `message:${item.id}`,
                  assistantCopyText(item.content, raw),
                  "Copy assistant output",
                )}
              </div>
            </header>
            ${open
              ? html`
                  <div class="message-body turn-body-stream">
                    ${raw
                      ? html`<pre class="assistant-raw">${item.content}</pre>`
                      : html`
                          <trouve-markdown-view
                            class="turn-markdown"
                            .content=${item.content}
                            .streaming=${!item.complete}
                          ></trouve-markdown-view>
                        `}
                  </div>
                `
              : nothing}
          </article>
        `;
      }
      case "thinking": {
        this.#ensureMarkdown();
        const defaultOpen = item.turn === presentation.latestTurn;
        const open = this.#messageDisclosure.get(item.id) ?? defaultOpen;
        const preview = collapsedChatPreview(item.content);
        return html`
          <article class="message thinking-card">
            <header class="thinking-header">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse thought process" : "Expand thought process"}
                @click=${() => this.#toggleMessageDisclosure(item.id, defaultOpen)}
              >
                <span class="disclosure-icon" aria-hidden="true">${open ? "▾" : "▸"}</span>
                <strong>${item.complete ? "Thought" : "Thinking"}</strong>
                ${open
                  ? nothing
                  : html`<small class="message-collapsed-preview">${preview}</small>`}
              </button>
              ${this.#renderCopyButton(
                `message:${item.id}`,
                item.content,
                "Copy thought process",
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
      case "tool": {
        const approvalPending = this.#approvalSubmissions.has(item.callId);
        const approvalRequired = item.status === "awaiting-approval";
        const raw = this.#rawToolCalls.has(item.callId);
        const toolPresentation = presentToolCall(item.tool, item.args, item.result);
        const toolOpen = approvalRequired
          || (this.#toolDisclosure.get(item.callId) ?? false);
        const toolDetail = toolOpen && !raw
          ? toolDetailText(item.args, item.result)
          : "";
        const toolMeta = [
          toolPresentation.meta,
          toolExecutionMetadata(item.result, item.durationMs),
        ].filter((part) => part !== "").join(" · ");
        return html`
          <details
            class=${`message tool-card ${approvalRequired ? "approval-required" : ""}`}
            data-call-id=${item.callId}
            ?open=${toolOpen}
            @keydown=${(event: KeyboardEvent) =>
              this.#approvalShortcut(event, item.callId)}
          >
            <summary
              @click=${(event: Event) =>
                this.#toggleToolDisclosure(event, item.callId, approvalRequired)}
            >
              <span class="tool-disclosure" aria-hidden="true">${toolOpen ? "▾" : "▸"}</span>
              <span class="tool-status ${item.status}" aria-hidden="true">
                ${item.status === "running"
                  ? html`
                      <svg class="tool-running-spinner" viewBox="0 0 24 24">
                        <path d="M12 3a9 9 0 1 1-9 9"></path>
                      </svg>
                      <span class="tool-running-static">◌</span>
                    `
                  : toolStatusGlyph(item.status)}
              </span>
              <strong>${toolPresentation.title}</strong>
              ${toolPresentation.subject === ""
                ? nothing
                : toolPresentation.filePath === ""
                  ? html`<span class="tool-subject">${toolPresentation.subject}</span>`
                  : html`<button
                      class="tool-file-target"
                      type="button"
                      title=${`Open ${toolPresentation.filePath}${toolMeta === "" ? "" : ` ${toolMeta}`}`}
                      @click=${(event: MouseEvent) => this.#openToolFile(event, toolPresentation)}
                    >${toolPresentation.subject}</button>`}
              ${toolPresentation.additions === 0
                ? nothing
                : html`<span class="tool-change-count add">+${toolPresentation.additions}</span>`}
              ${toolPresentation.deletions === 0
                ? nothing
                : html`<span class="tool-change-count delete">−${toolPresentation.deletions}</span>`}
              ${toolMeta === ""
                ? nothing
                : html`<small class="tool-meta">${toolMeta}</small>`}
              <small class="tool-state visually-hidden">${toolStatusLabel(item.status)}</small>
              <span class="tool-raw-action" @click=${(event: Event) => event.stopPropagation()}>
                <button
                  type="button"
                  aria-pressed=${raw ? "true" : "false"}
                  aria-label=${raw ? "Show formatted tool output" : "Show raw tool output"}
                  title=${raw ? "Show formatted output" : "Show raw data"}
                  @click=${() => this.#toggleRawTool(item.callId)}
                ><span aria-hidden="true">${raw ? "{}" : "≡"}</span></button>
              </span>
              <span class="tool-copy-action" @click=${(event: Event) => event.stopPropagation()}>
                ${this.#renderCopyButton(
                  `tool:${item.callId}`,
                  raw ? this.#rawToolText(item) : this.#toolCopyText(item),
                  `Copy ${item.tool} details`,
                )}
              </span>
              ${approvalRequired
                ? html`
                    <span
                      class="tool-approval-actions"
                      role="group"
                      aria-label="Tool approval"
                      aria-busy=${approvalPending ? "true" : "false"}
                      @click=${(event: Event) => event.stopPropagation()}
                    >
                      <button
                        class="primary"
                        type="button"
                        aria-keyshortcuts="Y"
                        ?disabled=${approvalPending}
                        @click=${() => void this.#resolveApproval(item.callId, "approve")}
                      >Approve</button>
                      <button
                        class="approval-additive-action"
                        type="button"
                        aria-keyshortcuts="A"
                        ?disabled=${approvalPending}
                        @click=${() => void this.#resolveApproval(item.callId, "always_approve")}
                      >Always allow</button>
                      <button
                        type="button"
                        aria-keyshortcuts="N"
                        ?disabled=${approvalPending}
                        @click=${() => void this.#resolveApproval(item.callId, "deny")}
                      >Deny</button>
                      ${approvalPending
                        ? html`<span class="visually-hidden" role="status">Submitting approval decision…</span>`
                        : nothing}
                    </span>
                  `
                : nothing}
            </summary>
            ${toolOpen
              ? html`
                  ${raw
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
                          : html`<ul class="tool-todo-list" aria-label="Todo state">
                              ${toolPresentation.todos.map((todo) => html`<li class=${`todo-${todo.status}`}>
                                <span aria-hidden="true">${todo.glyph}</span><span>${todo.content}</span>
                              </li>`)}
                            </ul>`}
                        ${toolPresentation.diff.length > 0 || toolPresentation.todos.length > 0 || toolDetail === ""
                          ? nothing
                          : html`<pre aria-label="Tool details">${toolDetail}</pre>`}
                      `}
                  ${item.output.text === "" && !item.output.omitted
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
            <section class="message question-card question-resolved">
              <header>
                <strong>${item.title ?? "Questions"}</strong>
                <span>${item.answers === null ? "Skipped" : "Answered"}</span>
              </header>
              ${item.answers === null
                ? html`<p class="resolved-label">The questions were skipped.</p>`
                : this.#renderQuestionSummary(summary)}
            </section>
          `;
        }
        {
          const state = normalizeQuestionWizard(
            this.#questionWizards.get(item.requestId),
            item.questions.length,
          );
          const review = state.step === item.questions.length;
          const question = item.questions[state.step];
          const selected = state.selections[state.step] ?? [];
          const multiple = question?.allow_multiple === true;
          const submitting = this.#questionSubmissions.has(item.requestId);
          return html`
            <section
              class="message question-card question-pending"
              data-question-request-id=${item.requestId}
              aria-busy=${submitting ? "true" : "false"}
            >
              <header>
                <strong>${item.title ?? "Questions"}</strong>
                <span>${review
                  ? "Review your answers"
                  : `Question ${state.step + 1} of ${item.questions.length}`}</span>
                <span class="question-header-spacer"></span>
                <button
                  class="question-skip"
                  type="button"
                  ?disabled=${submitting}
                  @click=${() => this.#skipQuestions(item.requestId)}
                >Skip</button>
              </header>
              ${review
                ? html`
                    <div class="question-step question-review question-step-focus" tabindex="-1">
                      ${this.#renderQuestionSummary(pendingQuestionSummary(item.questions, state))}
                    </div>
                  `
                : question === undefined
                  ? nothing
                  : html`
                      <div class="question-step">
                        <h3 class="question-step-focus" tabindex="-1">${question.prompt}</h3>
                        <div
                          class="question-options"
                          role=${multiple ? "group" : "radiogroup"}
                          aria-label=${question.prompt}
                        >
                          ${question.options.map((option) => {
                            const checked = selected.includes(option.id);
                            return html`
                              <button
                                class="question-option ${checked ? "selected" : ""}"
                                type="button"
                                role=${multiple ? "checkbox" : "radio"}
                                aria-checked=${checked ? "true" : "false"}
                                ?disabled=${submitting}
                                @click=${() => this.#toggleQuestionOption(item, option.id)}
                              >
                                <span class="question-option-mark" aria-hidden="true">${multiple
                                  ? checked ? "☑" : "☐"
                                  : checked ? "◉" : "○"}</span>
                                <span>${option.label}</span>
                              </button>
                            `;
                          })}
                          ${(() => {
                            const checked = selected.includes(OTHER_OPTION_ID);
                            return html`
                              <button
                                class="question-option question-other-option ${checked ? "selected" : ""}"
                                type="button"
                                role=${multiple ? "checkbox" : "radio"}
                                aria-checked=${checked ? "true" : "false"}
                                ?disabled=${submitting}
                                @click=${() => this.#toggleQuestionOption(item, OTHER_OPTION_ID)}
                              >
                                <span class="question-option-mark" aria-hidden="true">${multiple
                                  ? checked ? "☑" : "☐"
                                  : checked ? "◉" : "○"}</span>
                                <em>Other</em>
                              </button>
                              ${checked
                                ? html`
                                    <textarea
                                      class="question-other-input"
                                      rows="2"
                                      aria-label=${`Other answer for ${question.prompt}`}
                                      ?disabled=${submitting}
                                      .value=${live(state.otherTexts[state.step] ?? "")}
                                      @input=${(event: InputEvent) => this.#editQuestionOther(
                                        item,
                                        (event.currentTarget as HTMLTextAreaElement).value,
                                      )}
                                    ></textarea>
                                  `
                                : nothing}
                            `;
                          })()}
                        </div>
                      </div>
                    `}
              <div class="card-actions question-navigation">
                ${state.step > 0
                  ? html`
                      <button
                        type="button"
                        ?disabled=${submitting}
                        @click=${() => this.#previousQuestion(item)}
                      >Back</button>
                    `
                  : nothing}
                <button
                  class="primary"
                  type="button"
                  ?disabled=${submitting || !canAdvanceQuestionWizard(state, item.questions.length)}
                  @click=${() => this.#nextQuestion(item)}
                >${review ? "Submit" : state.step + 1 === item.questions.length ? "Review" : "Next"}</button>
              </div>
            </section>
          `;
        }
      case "turn-status":
        if (item.state.kind === "failed") {
          return html`<p class="message turn-error" role="alert">Turn failed: ${item.state.error}</p>`;
        }
        if (item.state.kind === "cancelled") {
          return html`
            <p class="message turn-cancelled" role="status">
              <strong>Turn cancelled</strong> — the active response was interrupted.
            </p>
          `;
        }
        if (
          item.state.kind === "completed"
          && !presentation.turnsWithAssistant.has(item.turn)
        ) {
          return html`
            <p class="message turn-metadata-standalone" role="status">
              ${formatTurnMetadata(item.state.usage, turnDurationMs.get(item.turn))}
            </p>
          `;
        }
        return nothing;
    }
  }

  #renderAttachments(
    attachments: Extract<ThreadChatItem, { kind: "user" }>["attachments"],
  ) {
    if (attachments.length === 0) return nothing;
    return html`
      <ul class="attachment-list" aria-label="Message attachments">
        ${attachments.map((attachment) => {
          const path = protocolAttachmentPath(attachment);
          const copyKey = `attachment:${attachment.id}`;
          return html`
            <li class=${isImageAttachment(attachment) ? "image-attachment" : "file-attachment"}>
              ${path !== undefined && isImageAttachment(attachment)
                ? html`
                    <img
                      src=${path}
                      alt=${`Preview of ${attachment.name}`}
                      loading="lazy"
                      decoding="async"
                    />
                  `
                : html`<span class="attachment-icon" aria-hidden="true">▧</span>`}
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
    const glyph = result === "copied" ? "✓" : result === undefined ? "⧉" : "!";
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
      ><span aria-hidden="true">${glyph}</span></button>
    `;
  }

  #toggleMessageDisclosure(itemId: string, defaultOpen: boolean): void {
    const open = this.#messageDisclosure.get(itemId) ?? defaultOpen;
    this.#messageDisclosure.set(itemId, !open);
    this.#requestDisclosureUpdate();
  }

  #toggleRawAssistant(turn: number): void {
    if (this.#rawAssistantTurns.has(turn)) this.#rawAssistantTurns.delete(turn);
    else this.#rawAssistantTurns.add(turn);
    this.#requestDisclosureUpdate();
  }

  #toggleRawTool(callId: string): void {
    if (this.#rawToolCalls.has(callId)) this.#rawToolCalls.delete(callId);
    else this.#rawToolCalls.add(callId);
    this.#requestDisclosureUpdate();
  }

  #requestDisclosureUpdate(): void {
    const preserveTail = this.#virtualizer.window().followingTail;
    this.requestUpdate();
    if (!preserveTail) return;
    // A disclosure changes its box before ResizeObserver reports the new
    // virtual-row height. Guard layout-generated scroll events and converge
    // on the real tail while Lit and the virtual canvas apply the measurement.
    this.#scrollCorrectionResumeAt = 0;
    this.#scheduleTailConvergence();
  }

  async #copyText(key: string, value: string): Promise<void> {
    const generation = this.#copyFeedbackGeneration;
    const result = await copyChatText(value, globalThis.navigator?.clipboard);
    if (generation !== this.#copyFeedbackGeneration) return;
    this.#copyFeedback.set(key, result);
    this.requestUpdate();
    globalThis.setTimeout(() => {
      if (
        generation === this.#copyFeedbackGeneration
        && this.#copyFeedback.get(key) === result
      ) {
        this.#copyFeedback.delete(key);
        this.requestUpdate();
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

  #toolCopyText(item: Extract<ThreadChatItem, { kind: "tool" }>): string {
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

  #rawToolText(item: Extract<ThreadChatItem, { kind: "tool" }>): string {
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
    this.dispatchEvent(new CustomEvent("trouve-open-file", {
      detail: {
        path: presentation.filePath,
        from: presentation.lineFrom,
        to: presentation.lineTo,
      },
      bubbles: true,
      composed: true,
    }));
  }

  #ensureMarkdown(): void {
    if (this.#markdownRequested) return;
    this.#markdownRequested = true;
    void import("./markdown-view.js");
  }

  #currentSessionUsageKey(): string {
    const store = this.#store.value;
    if (store === undefined || this.sessionId === "") return "";
    const sessionUpdatedAt = store.session(this.sessionId)?.updatedAt ?? "";
    const lastUsageCursor = this.threadId === ""
      ? 0
      : store.threadView(this.threadId).lastUsageCursor;
    return `${this.sessionId}:${sessionUpdatedAt}:${lastUsageCursor}`;
  }

  async #ensureSessionUsage(): Promise<void> {
    const services = this.#services.value;
    const key = this.#currentSessionUsageKey();
    if (
      services === undefined
      || key === ""
      || key === this.#usageResolvedKey
      || (key === this.#usageRequestKey && this.#usagePending)
    ) return;

    const generation = ++this.#usageGeneration;
    const sessionId = this.sessionId;
    this.#usageRequestKey = key;
    this.#usagePending = true;
    this.requestUpdate();
    try {
      const usage = await services.protocol.sessionUsage(sessionId);
      if (
        generation !== this.#usageGeneration
        || sessionId !== this.sessionId
        || key !== this.#currentSessionUsageKey()
      ) return;
      this.#sessionUsage = usage;
      this.#usageResolvedKey = key;
    } catch {
      if (
        generation === this.#usageGeneration
        && sessionId === this.sessionId
        && key === this.#currentSessionUsageKey()
      ) {
        // The native app also treats aggregate usage as optional status text.
        // Resolve this key silently so an unavailable endpoint cannot create a
        // request loop; the next durable session update retries automatically.
        this.#usageResolvedKey = key;
      }
    } finally {
      if (generation === this.#usageGeneration) {
        this.#usagePending = false;
        this.requestUpdate();
      }
    }
  }

  #threadOptionCatalogKey(workspaceId: string): string {
    const offline = this.#store.value !== undefined
      && readSignal(this.#store.value.serverInfo)?.online === false;
    return `${workspaceId}:${offline ? "offline" : "online"}`;
  }

  async #ensureThreadOptions(): Promise<void> {
    const services = this.#services.value;
    const catalogKey = this.#threadOptionCatalogKey(this.workspaceId);
    if (
      services === undefined ||
      this.workspaceId === "" ||
      this.#optionCatalogKey === catalogKey
    ) return;
    const workspaceId = this.workspaceId;
    this.#optionCatalogKey = catalogKey;
    if (this.#models.length > 0) {
      this.#models = [];
      this.#subscriptionHealth = [];
      this.requestUpdate();
    }
    try {
      const [modes, models, subscriptionHealth] = await Promise.all([
        services.protocol.modes(workspaceId),
        services.protocol.models(),
        services.subscriptionHealth.refresh("if-stale").catch(() => []),
      ]);
      const currentCatalogKey = this.#threadOptionCatalogKey(this.workspaceId);
      if (this.workspaceId !== workspaceId || currentCatalogKey !== catalogKey) return;
      this.#modes = modes;
      this.#models = models;
      this.#subscriptionHealth = subscriptionHealth;
      this.requestUpdate();
    } catch {
      if (
        this.workspaceId === workspaceId
        && this.#threadOptionCatalogKey(this.workspaceId) === catalogKey
      ) {
        this.#requestError = "Mode and model options could not be loaded.";
        this.requestUpdate();
      }
    }
  }

  #refreshSubscriptionHealthAfterTurn(): void {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined || this.threadId === "") return;
    const usageCursor = store.threadView(this.threadId).lastUsageCursor;
    if (usageCursor <= this.#observedSubscriptionUsageCursor) return;
    this.#observedSubscriptionUsageCursor = usageCursor;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    void services.subscriptionHealth.refresh("if-stale").then((health) => {
      if (
        this.isConnected &&
        threadId === this.threadId &&
        generation === this.#threadInteractionGeneration
      ) {
        this.#subscriptionHealth = health;
        this.requestUpdate();
      }
    }).catch(() => undefined);
  }

  async #updateThreadSetting(
    request: ProtocolUpdateThreadRequest,
    errorMessage: string,
  ): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined ||
      store === undefined ||
      this.threadId === "" ||
      this.#threadSettingsPending ||
      this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#threadSettingsPending = true;
    this.#requestError = "";
    this.requestUpdate();
    try {
      const updated = await services.protocol.updateThread(threadId, request);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      store.upsertThread(updated);
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#requestError = errorMessage;
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#threadSettingsPending = false;
        this.requestUpdate();
      }
    }
  }

  #isCurrentThreadInteraction(threadId: string, generation: number): boolean {
    return this.isConnected
      && this.threadId === threadId
      && this.#threadInteractionGeneration === generation;
  }

  async #updateThreadModelOption(
    key: string,
    value: string | boolean,
    errorMessage: string,
  ): Promise<void> {
    const thread = this.#store.value?.thread(this.threadId);
    if (thread === undefined) return;
    await this.#updateThreadSetting({
      model_options: {
        ...(thread.model_options ?? {}),
        [key]: value,
      },
    }, errorMessage);
  }

  /** Open the provisional setup tab without creating durable server state. */
  openNewThreadSetup = (): void => {
    if (this.sessionId === "" || this.#newThreadBusy) return;
    this.#newThreadSetupOpen = true;
    this.#newThreadError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.querySelector<HTMLButtonElement>('.provisional-thread-tab')?.focus();
    });
  };

  #selectThread(threadId: string): void {
    const services = this.#services.value;
    if (services === undefined) return;
    this.#newThreadSetupOpen = false;
    this.#newThreadError = "";
    services.router.navigate({
      kind: "session",
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
      threadId,
    });
  }

  readonly #submitNewThread = async (
    event: NewThreadSetupSubmitEvent,
  ): Promise<void> => {
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined
      || store === undefined
      || this.#newThreadBusy
      || event.detail.workspaceId !== this.workspaceId
      || event.detail.sessionId !== this.sessionId
    ) return;
    event.preventDefault();
    this.#newThreadBusy = true;
    this.#newThreadError = "";
    this.requestUpdate();
    let createdThreadId: string | undefined;
    try {
      const thread = await services.protocol.createThread(event.detail.request);
      createdThreadId = thread.id;
      store.upsertThread(thread);
      this.#newThreadSetupOpen = false;
      services.router.navigate({
        kind: "session",
        workspaceId: this.workspaceId,
        sessionId: this.sessionId,
        threadId: thread.id,
      });
      this.requestUpdate();
      if (event.detail.initialMessage !== undefined) {
        await services.protocol.sendMessage(thread.id, event.detail.initialMessage);
      }
    } catch {
      if (createdThreadId === undefined) {
        this.#newThreadError = "Thread could not be created. Review the setup and try again.";
      } else {
        this.#requestError =
          "Thread was created, but its first message could not be sent. The message was not queued.";
      }
    } finally {
      this.#newThreadBusy = false;
      this.requestUpdate();
    }
  };

  readonly #cancelNewThread = (event: NewThreadSetupCancelEvent): void => {
    if (
      this.#newThreadBusy
      || event.detail.workspaceId !== this.workspaceId
      || event.detail.sessionId !== this.sessionId
    ) return;
    event.preventDefault();
    this.#newThreadSetupOpen = false;
    this.#newThreadError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.querySelector<HTMLButtonElement>('[aria-label="New thread"]')?.focus();
    });
  };

  #startQueueEdit(prompt: QueuedPrompt): void {
    if (this.#queueBusy !== "" || this.#connectivityBlocked()) return;
    this.#queueEditId = prompt.id;
    this.#queueEditDraft = prompt.content;
    this.#queueError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.#focusQueueControlNow(prompt.id, "edit-input");
    });
  }

  readonly #cancelQueueEdit = (): void => {
    if (this.#queueBusy !== "") return;
    const promptId = this.#queueEditId;
    this.#queueEditId = "";
    this.#queueEditDraft = "";
    this.#queueError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.#focusQueueControlNow(promptId, "edit");
    });
  };

  async #saveQueued(event: SubmitEvent, prompt: QueuedPrompt): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const store = this.#store.value;
    const content = this.#queueEditDraft.trim();
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || content === ""
      || this.#queueBusy !== ""
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = prompt.id;
    this.#queueError = "";
    this.requestUpdate();
    let saved = false;
    try {
      await services.protocol.updateQueuedPrompt(prompt.id, content);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      const view = store.threadView(threadId);
      store.replaceThreadQueue(
        threadId,
        view.queue.map((candidate) =>
          candidate.id === prompt.id ? { ...candidate, content } : candidate,
        ),
      );
      this.#queueEditId = "";
      this.#queueEditDraft = "";
      saved = true;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queued prompt could not be updated. Your edit is still available.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        this.#focusQueueControlNow(prompt.id, saved ? "edit" : "edit-input");
      }
    }
  }

  async #deleteQueued(queue: readonly QueuedPrompt[], promptId: string): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || this.#queueBusy !== ""
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    const focusAfterDelete = queueFocusAfterDelete(queue, promptId);
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    let deleted = false;
    try {
      await services.protocol.deleteQueuedPrompt(promptId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      const latest = store.threadView(threadId).queue;
      store.replaceThreadQueue(
        threadId,
        latest.filter((prompt) => prompt.id !== promptId),
      );
      if (this.#queueEditId === promptId) {
        this.#queueEditId = "";
        this.#queueEditDraft = "";
      }
      deleted = true;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queued prompt could not be deleted.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        if (!deleted) {
          this.#focusQueueControlNow(promptId, "delete");
        } else if (focusAfterDelete.kind === "prompt") {
          this.#focusQueueControlNow(focusAfterDelete.promptId, "edit");
        } else {
          this.#focusComposerNow();
        }
      }
    }
  }

  async #moveQueued(
    queue: readonly QueuedPrompt[],
    index: number,
    delta: -1 | 1,
  ): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const ids = reorderedQueueIds(queue, index, delta);
    const promptId = queue[index]?.id;
    if (
      services === undefined ||
      store === undefined ||
      this.threadId === "" ||
      ids === undefined ||
      promptId === undefined ||
      this.#queueBusy !== "" ||
      this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    try {
      const reordered = await services.protocol.reorderQueue(threadId, ids);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      store.replaceThreadQueue(
        threadId,
        reordered,
      );
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queue order could not be changed.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        this.#focusQueueControlNow(promptId, delta < 0 ? "earlier" : "later");
      }
    }
  }

  #startQueueDrag(event: DragEvent, promptId: string, disabled: boolean): void {
    if (disabled) {
      event.preventDefault();
      return;
    }
    this.#queueDragId = promptId;
    this.#queueDropId = "";
    if (event.dataTransfer !== null) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", promptId);
    }
  }

  #dragQueueOver(event: DragEvent, targetId: string): void {
    if (this.#queueDragId === "" || this.#queueDragId === targetId || this.#queueBusy !== "") {
      return;
    }
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
    const row = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const placement: QueueDropPlacement = event.clientY >= row.top + row.height / 2
      ? "after"
      : "before";
    if (this.#queueDropId === targetId && this.#queueDropPlacement === placement) return;
    this.#queueDropId = targetId;
    this.#queueDropPlacement = placement;
    this.requestUpdate();
  }

  async #dropQueued(
    event: DragEvent,
    queue: readonly QueuedPrompt[],
    targetId: string,
  ): Promise<void> {
    event.preventDefault();
    const promptId = this.#queueDragId;
    const ids = droppedQueueIds(queue, promptId, targetId, this.#queueDropPlacement);
    this.#endQueueDrag();
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || ids === undefined
      || this.#queueBusy !== ""
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    try {
      const reordered = await services.protocol.reorderQueue(threadId, ids);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      store.replaceThreadQueue(
        threadId,
        reordered,
      );
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queue order could not be changed.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        this.#focusQueueControlNow(promptId, "edit");
      }
    }
  }

  readonly #endQueueDrag = (): void => {
    const changed = this.#queueDragId !== "" || this.#queueDropId !== "";
    this.#queueDragId = "";
    this.#queueDropId = "";
    this.#queueDropPlacement = "before";
    if (changed) this.requestUpdate();
  };

  readonly #dispatchQueue = async (): Promise<void> => {
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || this.#queueBusy !== ""
      || store.threadView(this.threadId).turnRunning
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = "dispatch";
    this.#queueError = "";
    this.requestUpdate();
    let dispatched = false;
    try {
      await services.protocol.dispatchQueue(threadId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      dispatched = true;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queue could not be started. The prompts remain queued.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        if (dispatched) this.#focusComposerNow();
        else this.#focusQueueControlNow("", "dispatch");
      }
    }
  };

  async #sendQueuedNow(queue: readonly QueuedPrompt[], index: number): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const ids = prioritizedQueueIds(queue, index);
    const promptId = queue[index]?.id;
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || ids === undefined
      || promptId === undefined
      || this.#queueBusy !== ""
      || store.threadView(this.threadId).turnRunning
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    let reordered = false;
    let dispatched = false;
    try {
      if (index > 0) {
        const reorderedQueue = await services.protocol.reorderQueue(threadId, ids);
        if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
        store.replaceThreadQueue(
          threadId,
          reorderedQueue,
        );
        reordered = true;
      }
      await services.protocol.dispatchQueue(threadId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      dispatched = true;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = reordered
          ? "The prompt was moved first, but the queue could not be started."
          : "This prompt could not be sent now. It remains queued.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        if (dispatched) this.#focusComposerNow();
        else this.#focusQueueControlNow(promptId, "send-now");
      }
    }
  }

  #focusQueueControlNow(promptId: string, action: string): void {
    const roots = promptId === ""
      ? [this.querySelector<HTMLElement>(".queue-panel")].filter(
          (element): element is HTMLElement => element !== null,
        )
      : [...this.querySelectorAll<HTMLElement>("[data-queue-id]")].filter(
          (element) => element.dataset["queueId"] === promptId,
        );
    const control = roots
      .flatMap((root) => [...root.querySelectorAll<HTMLElement>("[data-queue-action]")])
      .find((element) => element.dataset["queueAction"] === action);
    if (control !== undefined && !(control instanceof HTMLButtonElement && control.disabled)) {
      control.focus();
      return;
    }
    roots[0]?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }

  #focusComposerNow(): void {
    this.querySelector<HTMLTextAreaElement>('textarea[name="message"]')?.focus();
  }

  #connectivityBlocked(): boolean {
    const store = this.#store.value;
    return store !== undefined
      && readSignal(store.serverInfo)?.online === false
      && this.#models.length === 0;
  }

  #reconcileTurnAcknowledgements(
    items: readonly ThreadChatItem[],
    durableTurnRunning: boolean,
  ): void {
    if (this.#pendingStartTurn !== undefined) {
      const pendingTurn = this.#pendingStartTurn;
      if (items.some(
        (item) => item.kind === "turn-status" && item.turn === pendingTurn,
      )) {
        this.#pendingStartTurn = undefined;
      }
    }

    if (this.#cancelRequestedTurn !== undefined) {
      const cancelledTurn = this.#cancelRequestedTurn;
      const state = items.find(
        (item) => item.kind === "turn-status" && item.turn === cancelledTurn,
      );
      if (
        (state?.kind === "turn-status" && state.state.kind !== "running")
        || (
          !durableTurnRunning
          && this.#pendingStartTurn === undefined
          && state === undefined
        )
      ) {
        this.#cancelRequestedTurn = undefined;
      }
    }
  }

  #latestRunningTurn(items: readonly ThreadChatItem[]): number | undefined {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const item = items[index];
      if (item?.kind === "turn-status" && item.state.kind === "running") {
        return item.turn;
      }
    }
    return undefined;
  }

  readonly #sendMessage = async (event: SubmitEvent): Promise<void> => {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    const textarea = form.elements.namedItem("message") as HTMLTextAreaElement | null;
    const content = textarea?.value.trim() ?? "";
    if (
      services === undefined ||
      this.threadId === "" ||
      this.#connectivityBlocked() ||
      (content === "" && this.#pendingAttachments.length === 0)
    ) return;
    const threadId = this.threadId;
    const requestGeneration = ++this.#turnRequestGeneration;
    this.#requestPending = true;
    const view = this.#store.value?.threadView(this.threadId);
    this.#messageRequest = view?.turnRunning === true
      || this.#pendingStartTurn !== undefined
      || this.#cancelRequestedTurn !== undefined
      ? "queue"
      : "start";
    this.#requestError = "";
    this.requestUpdate();
    try {
      const accepted = await services.protocol.sendMessage(threadId, {
        content,
        ...(this.#pendingAttachments.length === 0
          ? {}
          : { attachments: this.#pendingAttachments.map(({ upload }) => upload) }),
      });
      if (
        requestGeneration !== this.#turnRequestGeneration
        || threadId !== this.threadId
      ) return;
      if (accepted.queued !== true && accepted.turn > 0) {
        this.#pendingStartTurn = accepted.turn;
      }
      if (textarea !== null) {
        textarea.value = "";
        this.#resizeComposer(textarea);
      }
      this.#composerDraft = "";
      this.#composerCursor = 0;
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#pendingAttachments = [];
      const input = form.querySelector<HTMLInputElement>('input[type="file"]');
      if (input !== null) input.value = "";
    } catch {
      if (
        requestGeneration === this.#turnRequestGeneration
        && threadId === this.threadId
      ) {
        this.#requestError = "Message could not be sent.";
      }
    } finally {
      if (
        requestGeneration === this.#turnRequestGeneration
        && threadId === this.threadId
      ) {
        this.#messageRequest = undefined;
        this.#requestPending = false;
        this.requestUpdate();
        await this.updateComplete;
        this.#focusComposerNow();
      }
    }
  };

  readonly #filesSelected = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const files = input.files === null ? [] : [...input.files];
    input.value = "";
    void this.#addAttachments(files);
  };

  readonly #attachmentPickerClicked = (event: MouseEvent): void => {
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (
      services?.nativeHost === undefined ||
      capabilities === undefined ||
      !readSignal(capabilities.current).filePicker
    ) {
      return;
    }
    event.preventDefault();
    void this.#pickNativeAttachments();
  };

  readonly #composerPaste = (event: ClipboardEvent): void => {
    // Prefer the textual representation of rich clipboard content. Otherwise
    // copying formatted text from a browser can unexpectedly stage its image
    // representation as an attachment.
    if (event.clipboardData?.types.includes("text/plain") === true) return;
    const files = event.clipboardData?.files;
    if (files !== undefined && files.length > 0) {
      event.preventDefault();
      void this.#addAttachments([...files]);
      return;
    }
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (
      services?.nativeHost === undefined ||
      capabilities === undefined ||
      !readSignal(capabilities.current).clipboardImage
    ) {
      return;
    }
    event.preventDefault();
    void this.#readNativeClipboardImage();
  };

  async #pickNativeAttachments(): Promise<void> {
    const nativeHost = this.#services.value?.nativeHost;
    if (nativeHost === undefined || this.#attachmentPending) return;
    const threadId = this.threadId;
    const generation = ++this.#attachmentGeneration;
    this.#attachmentPending = true;
    this.#requestError = "";
    this.requestUpdate();
    try {
      const attachments = await nativeHost.pickFiles();
      if (generation !== this.#attachmentGeneration || threadId !== this.threadId) return;
      for (const attachment of attachments) {
        if (!this.#stageNativeAttachment(attachment)) break;
      }
    } catch {
      if (generation === this.#attachmentGeneration && threadId === this.threadId) {
        this.#requestError = "Files could not be read from the desktop picker.";
      }
    } finally {
      if (generation === this.#attachmentGeneration && threadId === this.threadId) {
        this.#attachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  async #readNativeClipboardImage(): Promise<void> {
    const nativeHost = this.#services.value?.nativeHost;
    if (nativeHost === undefined || this.#attachmentPending) return;
    const threadId = this.threadId;
    const generation = ++this.#attachmentGeneration;
    this.#attachmentPending = true;
    this.#requestError = "";
    this.requestUpdate();
    try {
      const attachment = await nativeHost.readClipboardImage();
      if (generation !== this.#attachmentGeneration || threadId !== this.threadId) return;
      if (attachment !== undefined) this.#stageNativeAttachment(attachment);
    } catch {
      if (generation === this.#attachmentGeneration && threadId === this.threadId) {
        this.#requestError = "The desktop clipboard image could not be read.";
      }
    } finally {
      if (generation === this.#attachmentGeneration && threadId === this.threadId) {
        this.#attachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  #stageNativeAttachment(attachment: PendingAttachment): boolean {
    if (this.#pendingAttachments.length >= MAX_PENDING_ATTACHMENTS) {
      this.#requestError = `Attach at most ${MAX_PENDING_ATTACHMENTS} files at once.`;
      return false;
    }
    const total = this.#pendingAttachments.reduce(
      (bytes, pending) => bytes + pending.size,
      attachment.size,
    );
    if (total > MAX_PENDING_ATTACHMENT_BYTES) {
      this.#requestError = "Pending attachments exceed the 20 MB mobile memory budget.";
      return false;
    }
    this.#pendingAttachments = [...this.#pendingAttachments, attachment];
    return true;
  }

  async #addAttachments(files: readonly File[]): Promise<void> {
    if (files.length === 0 || this.#attachmentPending) return;
    const threadId = this.threadId;
    const generation = ++this.#attachmentGeneration;
    this.#attachmentPending = true;
    this.#requestError = "";
    this.requestUpdate();
    try {
      for (const [index, file] of files.entries()) {
        if (this.#pendingAttachments.length >= MAX_PENDING_ATTACHMENTS) {
          this.#requestError = `Attach at most ${MAX_PENDING_ATTACHMENTS} files at once.`;
          break;
        }
        let attachment: PendingAttachment;
        try {
          attachment = await encodeAttachment(
            file,
            `pasted-${Date.now()}-${index + 1}.bin`,
          );
          if (generation !== this.#attachmentGeneration || threadId !== this.threadId) return;
        } catch (error) {
          if (generation !== this.#attachmentGeneration || threadId !== this.threadId) return;
          const kind =
            error instanceof AttachmentEncodingError ? error.kind : "read-failed";
          this.#requestError =
            kind === "too-large"
              ? `${file.name || "Attachment"} is larger than the 10 MB limit.`
              : kind === "empty"
                ? `${file.name || "Attachment"} is empty.`
                : `${file.name || "Attachment"} could not be read.`;
          continue;
        }
        const total = this.#pendingAttachments.reduce(
          (bytes, pending) => bytes + pending.size,
          attachment.size,
        );
        if (total > MAX_PENDING_ATTACHMENT_BYTES) {
          this.#requestError = "Pending attachments exceed the 20 MB mobile memory budget.";
          break;
        }
        this.#pendingAttachments = [...this.#pendingAttachments, attachment];
      }
    } finally {
      if (generation === this.#attachmentGeneration && threadId === this.threadId) {
        this.#attachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  #removeAttachment(index: number): void {
    this.#pendingAttachments = this.#pendingAttachments.filter(
      (_, candidate) => candidate !== index,
    );
    this.requestUpdate();
  }

  #formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  readonly #composerChanged = (event: InputEvent): void => {
    const textarea = event.currentTarget as HTMLTextAreaElement;
    this.#resizeComposer(textarea);
    this.#composerDraft = textarea.value;
    this.#composerCursor = textarea.selectionStart ?? textarea.value.length;
    this.#completionSelected = 0;
    const composing = this.#composerComposing || event.isComposing;
    if (!composing) {
      this.#completionDismissed = false;
      this.#loadMentionPathsIfNeeded();
    }
    if (!composing) this.requestUpdate();
  };

  #applyQuickReply(prompt: string): void {
    if (this.#requestPending || prompt === "") return;
    const current = this.#composerDraft.trimEnd();
    this.#composerDraft = current === "" ? prompt : `${current}\n${prompt}`;
    this.#composerCursor = this.#composerDraft.length;
    this.#completionSelected = 0;
    this.#completionDismissed = false;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      const textarea = this.querySelector<HTMLTextAreaElement>('textarea[name="message"]');
      if (textarea === null) return;
      textarea.focus();
      textarea.setSelectionRange(this.#composerCursor, this.#composerCursor);
      this.#resizeComposer(textarea);
    });
  }

  #resizeComposer(
    textarea = this.querySelector<HTMLTextAreaElement>('textarea[name="message"]'),
  ): void {
    if (textarea === null) return;
    textarea.style.height = "auto";
    const layout = composerTextareaLayout(textarea.scrollHeight);
    textarea.style.height = `${layout.height}px`;
    textarea.style.overflowY = layout.overflowY;
  }

  readonly #composerCursorMoved = (event: Event): void => {
    const textarea = event.currentTarget as HTMLTextAreaElement;
    const cursor = textarea.selectionStart ?? textarea.value.length;
    if (cursor === this.#composerCursor && textarea.value === this.#composerDraft) return;
    this.#composerDraft = textarea.value;
    this.#composerCursor = cursor;
    this.#completionSelected = 0;
    if (this.#composerComposing) return;
    this.#loadMentionPathsIfNeeded();
    this.requestUpdate();
  };

  readonly #composerCompositionStarted = (): void => {
    this.#composerComposing = true;
    this.requestUpdate();
  };

  readonly #composerCompositionEnded = (event: CompositionEvent): void => {
    const textarea = event.currentTarget as HTMLTextAreaElement;
    this.#composerComposing = false;
    this.#composerDraft = textarea.value;
    this.#composerCursor = textarea.selectionStart ?? textarea.value.length;
    this.#completionSelected = 0;
    this.#completionDismissed = false;
    this.#resizeComposer(textarea);
    this.#loadMentionPathsIfNeeded();
    this.requestUpdate();
  };

  #loadMentionPathsIfNeeded(): void {
    if (
      !this.#composerComposing &&
      composerCompletionToken(this.#composerDraft, this.#composerCursor)?.kind === "file"
    ) void this.#ensureSessionPaths();
  }

  readonly #retryMentionPaths = (): void => {
    if (this.sessionId === "" || this.#pathsLoadingSessionId === this.sessionId) return;
    this.#pathsUnavailableSessionId = "";
    this.#pathsRetryAfter = 0;
    this.#pathsLoadedAt = 0;
    void this.#ensureSessionPaths();
  };

  async #ensureSessionPaths(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    const sessionId = this.sessionId;
    const now = Date.now();
    if (
      protocol === undefined ||
      sessionId === "" ||
      this.#pathsLoadingSessionId === sessionId ||
      (this.#pathsSessionId === sessionId && now - this.#pathsLoadedAt < PATH_REFRESH_INTERVAL_MS) ||
      (this.#pathsUnavailableSessionId === sessionId && now < this.#pathsRetryAfter)
    ) return;

    const generation = ++this.#pathsGeneration;
    this.#pathsLoadingSessionId = sessionId;
    this.#pathsUnavailableSessionId = "";
    this.requestUpdate();
    try {
      const paths = await protocol.sessionPaths(sessionId);
      if (generation !== this.#pathsGeneration || this.sessionId !== sessionId) return;
      this.#sessionPaths = paths.slice(0, MAX_COMPOSER_COMPLETION_SOURCES);
      this.#pathsSessionId = sessionId;
      this.#pathsLoadedAt = Date.now();
      this.#pathsRetryAfter = 0;
    } catch {
      if (generation !== this.#pathsGeneration || this.sessionId !== sessionId) return;
      this.#pathsUnavailableSessionId = sessionId;
      this.#pathsRetryAfter = Date.now() + PATH_REFRESH_INTERVAL_MS;
    } finally {
      if (generation === this.#pathsGeneration) {
        this.#pathsLoadingSessionId = "";
        this.requestUpdate();
      }
    }
  }

  #applyComposerCompletion(token: ComposerCompletionToken, value: string): void {
    if (!isComposerCompletionTokenCurrent(
      this.#composerDraft,
      this.#composerCursor,
      token,
    )) {
      this.#completionSelected = 0;
      this.requestUpdate();
      return;
    }
    const sourceStillContainsValue = token.kind === "command"
      ? (this.#store.value?.threadView(this.threadId).commands ?? [])
          .some((command) => command.name.replace(/^\/+/, "") === value.replace(/^\/+/, ""))
      : this.#sessionPaths.includes(value);
    if (!sourceStillContainsValue) {
      this.#completionSelected = 0;
      this.requestUpdate();
      return;
    }
    const applied = applyComposerCompletion(this.#composerDraft, token, value);
    if (applied === undefined) {
      this.#completionSelected = 0;
      this.requestUpdate();
      return;
    }
    this.#composerDraft = applied.draft;
    this.#composerCursor = applied.cursor;
    this.#completionSelected = 0;
    this.#completionDismissed = false;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      const textarea = this.querySelector<HTMLTextAreaElement>('textarea[name="message"]');
      if (textarea === null) return;
      textarea.focus();
      textarea.setSelectionRange(applied.cursor, applied.cursor);
    });
  }

  readonly #composerKeydown = (event: KeyboardEvent): void => {
    if (isComposerCompositionKey({
      key: event.key,
      keyCode: event.keyCode,
      isComposing: event.isComposing,
      compositionActive: this.#composerComposing,
    })) return;
    const commands = this.threadId === ""
      ? []
      : (this.#store.value?.threadView(this.threadId).commands ?? []);
    const completion = this.#activeComposerCompletion(commands);
    if (completion !== undefined) {
      if (event.key === "Escape") {
        event.preventDefault();
        this.#completionDismissed = true;
        this.requestUpdate();
        return;
      }
      if (completion.matches.length > 0 && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
        event.preventDefault();
        const delta = event.key === "ArrowUp" ? -1 : 1;
        this.#completionSelected = Math.max(
          0,
          Math.min(
            completion.matches.length - 1,
            this.#completionSelected + delta,
          ),
        );
        this.requestUpdate();
        return;
      }
      if (
        completion.matches.length > 0 &&
        (event.key === "Tab" || event.key === "Enter") &&
        !event.shiftKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.altKey
      ) {
        event.preventDefault();
        const selected = completion.matches[
          Math.min(this.#completionSelected, completion.matches.length - 1)
        ];
        if (selected !== undefined) {
          this.#applyComposerCompletion(completion.token, selected.value);
        }
        return;
      }
    }
    if (event.key !== "Enter" || event.shiftKey) return;
    event.preventDefault();
    (event.currentTarget as HTMLTextAreaElement).form?.requestSubmit();
  };

  readonly #cancelTurn = async (): Promise<void> => {
    const services = this.#services.value;
    if (services === undefined || this.threadId === "" || this.#connectivityBlocked()) return;
    const threadId = this.threadId;
    const requestGeneration = ++this.#turnRequestGeneration;
    const requestedTurn = this.#latestRunningTurn(
      this.#store.value?.threadView(this.threadId).items ?? [],
    ) ?? this.#pendingStartTurn ?? -1;
    this.#cancelRequestedTurn = requestedTurn;
    this.#requestPending = true;
    this.#requestError = "";
    this.requestUpdate();
    try {
      await services.protocol.cancelTurn(threadId);
    } catch {
      if (
        requestGeneration === this.#turnRequestGeneration
        && threadId === this.threadId
        && this.#cancelRequestedTurn === requestedTurn
      ) {
        this.#cancelRequestedTurn = undefined;
        this.#requestError = "Turn could not be stopped.";
      }
    } finally {
      if (
        requestGeneration === this.#turnRequestGeneration
        && threadId === this.threadId
      ) {
        this.#requestPending = false;
        this.requestUpdate();
        await this.updateComplete;
        this.#focusComposerNow();
      }
    }
  };

  #approvalShortcut(event: KeyboardEvent, callId: string): void {
    const target = event.target;
    const editable = target instanceof HTMLElement && (
      target.isContentEditable ||
      target.matches("input, textarea, select")
    );
    const decision = approvalDecisionForShortcut({
      key: event.key,
      altKey: event.altKey,
      ctrlKey: event.ctrlKey,
      metaKey: event.metaKey,
      repeat: event.repeat,
      isComposing: event.isComposing,
      editable,
    });
    if (decision === undefined || this.#approvalSubmissions.has(callId)) return;
    event.preventDefault();
    event.stopPropagation();
    void this.#resolveApproval(callId, decision);
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
    this.#requestDisclosureUpdate();
  }

  async #resolveApproval(
    callId: string,
    decision: ProtocolResolveApprovalRequest["decision"],
  ): Promise<void> {
    const services = this.#services.value;
    if (
      services === undefined
      || this.threadId === ""
      || !this.#approvalSubmissions.begin(callId)
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#requestError = "";
    this.requestUpdate();
    let submitted = false;
    try {
      await services.protocol.resolveApproval({ call_id: callId, decision });
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      submitted = true;
      // The HTTP response can win the race with its durable SSE event. Remove
      // the controls immediately after a successful mutation so a second
      // click/key cannot submit the already-consumed approval in that gap.
      const tool = this.#store.value?.threadView(threadId).findTool(callId);
      if (tool?.status === "awaiting-approval") {
        tool.status = decision === "deny" ? "denied" : "running";
      }
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#requestError = "Approval could not be submitted.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#approvalSubmissions.finish(callId);
        this.requestUpdate();
        await this.updateComplete;
        if (submitted) this.#focusToolSummary(callId);
      }
    }
  }

  #focusToolSummary(callId: string): void {
    const card = [...this.querySelectorAll<HTMLElement>(".tool-card")].find(
      (candidate) => candidate.dataset["callId"] === callId,
    );
    card?.querySelector<HTMLElement>("summary")?.focus();
  }

  #syncQuestionWizards(items: readonly ThreadChatItem[]): void {
    const pending = new Set<string>();
    for (const item of items) {
      if (item.kind !== "questions" || item.answers !== undefined) continue;
      pending.add(item.requestId);
      this.#questionWizards.set(
        item.requestId,
        normalizeQuestionWizard(this.#questionWizards.get(item.requestId), item.questions.length),
      );
    }
    for (const requestId of this.#questionWizards.keys()) {
      if (!pending.has(requestId)) this.#questionWizards.delete(requestId);
    }
    for (const requestId of this.#questionSubmissions) {
      if (!pending.has(requestId)) this.#questionSubmissions.delete(requestId);
    }
  }

  #renderQuestionSummary(
    summary: readonly { readonly prompt: string; readonly answer: string }[],
  ) {
    return html`
      <dl class="question-summary">
        ${summary.map((entry) => html`
          <div>
            <dt>${entry.prompt}</dt>
            <dd>${entry.answer}</dd>
          </div>
        `)}
      </dl>
    `;
  }

  #toggleQuestionOption(
    item: Extract<ThreadChatItem, { kind: "questions" }>,
    optionId: string,
  ): void {
    const state = normalizeQuestionWizard(
      this.#questionWizards.get(item.requestId),
      item.questions.length,
    );
    this.#questionWizards.set(
      item.requestId,
      toggleQuestionOption(state, item.questions, optionId),
    );
    this.requestUpdate();
  }

  #editQuestionOther(
    item: Extract<ThreadChatItem, { kind: "questions" }>,
    text: string,
  ): void {
    const state = normalizeQuestionWizard(
      this.#questionWizards.get(item.requestId),
      item.questions.length,
    );
    this.#questionWizards.set(
      item.requestId,
      editQuestionOther(state, item.questions.length, text),
    );
    this.requestUpdate();
  }

  #previousQuestion(item: Extract<ThreadChatItem, { kind: "questions" }>): void {
    const state = normalizeQuestionWizard(
      this.#questionWizards.get(item.requestId),
      item.questions.length,
    );
    this.#questionWizards.set(
      item.requestId,
      retreatQuestionWizard(state, item.questions.length),
    );
    this.requestUpdate();
    this.#focusQuestionStep(item.requestId);
  }

  #nextQuestion(item: Extract<ThreadChatItem, { kind: "questions" }>): void {
    const state = normalizeQuestionWizard(
      this.#questionWizards.get(item.requestId),
      item.questions.length,
    );
    if (!canAdvanceQuestionWizard(state, item.questions.length)) return;
    if (state.step === item.questions.length) {
      void this.#submitQuestion({
        request_id: item.requestId,
        answers: questionWizardAnswers(item.questions, state),
      });
      return;
    }
    this.#questionWizards.set(
      item.requestId,
      advanceQuestionWizard(state, item.questions.length),
    );
    this.requestUpdate();
    this.#focusQuestionStep(item.requestId);
  }

  #focusQuestionStep(requestId: string): void {
    void this.updateComplete.then(() => {
      const card = [...this.querySelectorAll<HTMLElement>(".question-card")].find(
        (candidate) => candidate.dataset["questionRequestId"] === requestId,
      );
      card?.querySelector<HTMLElement>(".question-step-focus")?.focus({ preventScroll: true });
    });
  }

  readonly #skipQuestions = (requestId: string): void => {
    void this.#submitQuestion({ request_id: requestId, answers: null });
  };

  async #submitQuestion(request: ProtocolResolveQuestionRequest): Promise<void> {
    const services = this.#services.value;
    if (
      services === undefined
      || this.threadId === ""
      || this.#questionSubmissions.has(request.request_id)
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#questionSubmissions.add(request.request_id);
    this.#requestError = "";
    this.requestUpdate();
    try {
      await services.protocol.resolveQuestion(request);
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#requestError = "Answers could not be submitted.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#questionSubmissions.delete(request.request_id);
        this.requestUpdate();
      }
    }
  }
}

customElements.define("trouve-thread-screen", TrouveThreadScreen);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-thread-screen": TrouveThreadScreen;
  }
}
