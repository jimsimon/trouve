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
  pendingAttachmentPreviewUrl,
  type PendingAttachment,
} from "../services/attachments.js";
import type {
  ProtocolAgentMode,
  ProtocolAttachment,
  ProtocolModelInfo,
  ProtocolResolveApprovalRequest,
  ProtocolResolveQuestionRequest,
  ProtocolSubscriptionHealth,
  ProtocolUpdateThreadRequest,
  ProtocolUsageSummary,
} from "../services/protocol-client.js";
import type { ComposerDraft } from "../services/composer-drafts.js";
import type { ChatScrollBookmark } from "../services/resume-preferences.js";
import { rankComposerCompletionsOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import type {
  CompactionState,
  QueuedPrompt,
  ThreadChatItem,
} from "../state/thread-view-model.js";
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
  isContextCompactionTool,
  type AgentActivityItem,
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
  fontAwesomeIcon,
  type FontAwesomeIconName,
} from "./font-awesome-icon.js";
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
import "./image-preview.js";
import "./model-picker.js";
import "./new-thread-setup.js";

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
const CHAT_SCROLL_INDICATOR_INSET_PX = 3;
const CHAT_SCROLL_INDICATOR_MIN_HEIGHT_PX = 32;
const CHAT_HISTORY_PREFETCH_VIEWPORTS = 5;
const CHAT_HISTORY_PREFETCH_MIN_PX = 2_400;
const CHAT_HISTORY_ANCHOR_SETTLE_MS = 500;

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

interface MarkdownContextMenu {
  readonly markdown: string;
  readonly selection: string;
  readonly x: number;
  readonly y: number;
}

interface ChatDomAnchor {
  readonly id: string;
  readonly offset: number;
}

interface ActiveChatDomAnchor extends ChatDomAnchor {
  readonly scrollTop: number;
}

interface PendingHistoryPrepend {
  readonly scrollTop: number;
  readonly totalHeight: number;
  readonly followingTail: boolean;
  readonly anchor: ChatDomAnchor | undefined;
}

const PATH_REFRESH_INTERVAL_MS = 5_000;
const WORKER_COMPLETION_THRESHOLD = 200;
const COMPOSER_DRAFT_PERSIST_DELAY_MS = 200;

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
  return `${mode} · ${shortModelName(thread.model)}`;
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
  } as const)[status];

const toolStatusIcon = (
  status: Extract<ThreadChatItem, { kind: "tool" }>["status"],
): FontAwesomeIconName =>
  ({
    "awaiting-approval": "pause",
    running: "spinner",
    ok: "check",
    error: "xmark",
    denied: "ban",
    aborted: "xmark",
  } as const)[status];

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
  #historyLoadRequested = false;
  #historyError = "";
  #historyGeneration = 0;
  readonly #historyWarmThreads = new Set<string>();
  #pendingHistoryPrepend: PendingHistoryPrepend | undefined;
  #historyAnchorToRestore: ChatDomAnchor | undefined;
  #historyAnchorStabilizer: ActiveChatDomAnchor | undefined;
  #historyAnchorSettleTimer: ReturnType<typeof setTimeout> | undefined;
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
  #scrollIndicatorFrame: number | undefined;
  #scrollIndicatorMetrics:
    | { readonly maxScrollTop: number; readonly thumbTravel: number }
    | undefined;
  #chatPositionTimer: ReturnType<typeof setTimeout> | undefined;
  #scrollCorrectionResumeAt = 0;
  #followTailControlHeight = 0;
  #chatScrollIntent = false;
  #restoredScrollThreadId: string | undefined;
  #invalidScrollBookmarkThreadId: string | undefined;
  #markdownRequested = false;
  #queueEditId = "";
  #queueEditRetainedAttachments: ProtocolAttachment[] = [];
  #queueEditReturnDraft: ComposerDraft | undefined;
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
  #composerDraftThreadId = "";
  #composerDraftRestoreGeneration = 0;
  #composerDraftPersistTimer: ReturnType<typeof setTimeout> | undefined;
  #restoreComposerSelection = false;
  #composerComposing = false;
  #completionSelected = 0;
  #completionDismissed = false;
  #completionWorkerKey = "";
  #completionWorkerRequestedKey = "";
  #completionWorkerMatches: readonly RankedComposerCompletion[] = [];
  #completionWorkerGeneration = 0;
  #completionWorkerPending = false;
  #completionCommandSource: readonly {
    readonly name: string;
    readonly description?: string;
  }[] = [];
  #completionCommandSourceRevision = 0;
  #sessionPaths: readonly string[] = [];
  #sessionPathsRevision = 0;
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
  #markdownContextMenu: MarkdownContextMenu | undefined;
  #markdownContextMenuStatus = "";
  #markdownContextMenuReturnFocus: HTMLElement | undefined;
  readonly #approvalSubmissions = new ApprovalSubmissionTracker();
  readonly #copyFeedback = new Map<string, ChatCopyResult>();
  readonly #messageDisclosure = new Map<string, boolean>();
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
    const composerScopeChanged = changed.has("sessionId") || changed.has("threadId");
    if (composerScopeChanged) this.#persistComposerDraftNow();
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
      this.#sessionPathsRevision += 1;
      this.#pathsSessionId = "";
      this.#pathsLoadingSessionId = "";
      this.#pathsUnavailableSessionId = "";
      this.#pathsLoadedAt = 0;
      this.#pathsRetryAfter = 0;
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#queueEditId = "";
      this.#queueEditRetainedAttachments = [];
      this.#queueEditReturnDraft = undefined;
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
      this.#historyLoadRequested = false;
      this.#historyError = "";
      this.#pendingHistoryPrepend = undefined;
      this.#historyAnchorToRestore = undefined;
      this.#clearHistoryAnchorStabilizer();
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
      this.#cancelScheduledScrollIndicator();
      this.#cancelScheduledChatPosition();
      this.#scrollIndicatorMetrics = undefined;
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
      this.#attachmentPending = false;
      this.#copyFeedbackGeneration += 1;
      this.#copyFeedback.clear();
      this.#markdownContextMenu = undefined;
      this.#markdownContextMenuStatus = "";
      this.#markdownContextMenuReturnFocus = undefined;
      this.#messageDisclosure.clear();
      this.#rawToolCalls.clear();
      this.#toolDisclosure.clear();
      this.#questionWizards.clear();
      this.#questionSubmissions.clear();
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#composerComposing = false;
      this.#queueEditId = "";
      this.#queueEditRetainedAttachments = [];
      this.#queueEditReturnDraft = undefined;
      this.#queueBusy = "";
      this.#queueError = "";
      this.#queueDragId = "";
      this.#queueDropId = "";
      this.#queueDropPlacement = "before";
      this.#pendingStartTurn = undefined;
      this.#cancelRequestedTurn = undefined;
      this.#messageRequest = undefined;
    }
    if (composerScopeChanged) {
      this.#restoreComposerDraft(this.#draftThreadIdForCurrentScope());
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
    if (!changed.has("threadId") && this.#pendingHistoryPrepend !== undefined) {
      const viewport = this.querySelector<HTMLElement>(".chat-stream");
      if (viewport !== null) {
        const virtualWindow = this.#virtualizer.window();
        this.#pendingHistoryPrepend = {
          scrollTop: viewport.scrollTop,
          totalHeight: virtualWindow.totalHeight,
          followingTail: virtualWindow.followingTail,
          anchor: virtualWindow.followingTail
            ? undefined
            : this.#captureChatDomAnchor(viewport),
        };
      }
    }
  }

  protected override updated(): void {
    this.#cancelScheduledScrollRender();
    void this.#ensureThreadOptions();
    this.#refreshSubscriptionHealthAfterTurn();
    void this.#ensureSessionUsage();
    this.#syncComposerCompletionEffect(this.#currentComposerCommands());
    const draftThreadId = this.#draftThreadIdForCurrentScope();
    if (this.#composerDraftThreadId !== draftThreadId) {
      this.#restoreComposerDraft(draftThreadId);
    }
    this.#resizeComposer();
    if (this.#restoreComposerSelection) {
      const textarea = this.querySelector<HTMLTextAreaElement>('textarea[name="message"]');
      if (textarea !== null) {
        textarea.setSelectionRange(this.#composerCursor, this.#composerCursor);
        this.#restoreComposerSelection = false;
      }
    }
    if (this.#invalidScrollBookmarkThreadId === this.threadId) {
      this.#invalidScrollBookmarkThreadId = undefined;
      this.#emitChatPosition();
    }
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport === null) {
      this.#resizeObserver?.disconnect();
      this.#observedVirtualRows.clear();
      this.#followTailControlHeight = 0;
      this.#cancelScheduledScrollIndicator();
      this.#scrollIndicatorMetrics = undefined;
      return;
    }
    this.#followTailControlHeight = viewport
      .querySelector<HTMLElement>(".follow-tail")
      ?.offsetHeight ?? 0;
    this.#restoreHistoryPrependAnchor(viewport);
    if (viewport.clientHeight !== this.#viewportHeight) {
      this.#viewportHeight = viewport.clientHeight;
      const correction = this.#virtualizer.resizeViewport(viewport.clientHeight);
      const expected = this.#virtualizer.window().followingTail
        ? this.#transcriptTailScrollTop(viewport)
        : correction.scrollTop;
      this.#setChatScrollTop(viewport, expected);
      this.#refreshChatScrollIndicator(viewport);
      this.requestUpdate();
      return;
    }
    const virtualWindow = this.#virtualizer.window();
    const expected = virtualWindow.followingTail
      ? this.#transcriptTailScrollTop(viewport)
      : virtualWindow.scrollTop;
    this.#setChatScrollTop(viewport, expected);
    this.#refreshChatScrollIndicator(viewport);
    this.#warmOlderHistory();
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
      if (!followingTail && this.#historyAnchorStabilizer !== undefined) {
        // A single paged Agent turn can re-render many nested markdown blocks
        // after the parent update. Keep the exact visible thought/tool fixed
        // through those pre-paint ResizeObserver deliveries.
        this.#correctHistoryAnchor(activeViewport);
        this.#scheduleHistoryAnchorRelease();
      }
      this.#refreshChatScrollIndicator(activeViewport);
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

  override connectedCallback(): void {
    super.connectedCallback();
    document.addEventListener("pointerdown", this.#dismissMarkdownContextMenuFromPointer, true);
    document.addEventListener("scroll", this.#dismissMarkdownContextMenu, true);
    globalThis.addEventListener("resize", this.#dismissMarkdownContextMenu);
    globalThis.addEventListener("pagehide", this.#persistComposerDraftFromPageHide);
  }

  override disconnectedCallback(): void {
    this.#persistComposerDraftNow();
    document.removeEventListener("pointerdown", this.#dismissMarkdownContextMenuFromPointer, true);
    document.removeEventListener("scroll", this.#dismissMarkdownContextMenu, true);
    globalThis.removeEventListener("resize", this.#dismissMarkdownContextMenu);
    globalThis.removeEventListener("pagehide", this.#persistComposerDraftFromPageHide);
    this.#resizeObserver?.disconnect();
    this.#resizeObserver = undefined;
    this.#observedVirtualRows.clear();
    this.#cancelProgrammaticScrollWindow();
    this.#cancelTailConvergence();
    this.#cancelScheduledScrollRender();
    this.#cancelScheduledScrollIndicator();
    this.#cancelScheduledChatPosition();
    this.#clearHistoryAnchorStabilizer();
    this.#scrollIndicatorMetrics = undefined;
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
    this.#markdownContextMenu = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
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
      || this.#pendingAttachments.length > 0
      || this.#queueEditRetainedAttachments.length > 0;
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
    const queueEditing = this.#queueEditId !== "";
    const queueEditPending = queueEditing && this.#queueBusy === this.#queueEditId;
    const attachmentDisabled = thread === undefined
      || this.#requestPending
      || this.#attachmentPending
      || queueEditPending
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
      thread?.model.startsWith("codex/") ?? false,
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
                <span class="thread-tab-label">${candidate.spawned === true
                  ? fontAwesomeIcon("code-branch")
                  : nothing}${threadTabLabel(
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
          >${fontAwesomeIcon("plus")}</button>
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
        ${queueEditing
          ? html`
              <div class="queue-edit-indicator" role="status">
                <span>${fontAwesomeIcon("pen")} Editing queued prompt</span>
                <button
                  type="button"
                  aria-label="Cancel queued prompt edit"
                  title="Cancel editing"
                  ?disabled=${queueEditPending || this.#attachmentPending}
                  @click=${this.#cancelQueueEdit}
                >${fontAwesomeIcon("xmark")}</button>
              </div>
            `
          : nothing}
        ${this.#queueEditRetainedAttachments.length === 0
          && this.#pendingAttachments.length === 0
          ? nothing
          : html`
              <ul
                class="attachment-list pending-attachments"
                aria-label=${queueEditing ? "Queued prompt attachments" : "Pending attachments"}
              >
                ${this.#queueEditRetainedAttachments.map(
                  (attachment, index) => {
                    const preview = isImageAttachment(attachment)
                      ? protocolAttachmentPath(attachment)
                      : undefined;
                    return html`
                      <li class=${preview === undefined ? "file-attachment" : "image-attachment"}>
                        ${preview === undefined
                          ? html`<span class="attachment-icon">${fontAwesomeIcon(
                              isImageAttachment(attachment) ? "file-image" : "file",
                            )}</span>`
                          : html`<trouve-image-preview
                              .source=${preview}
                              .name=${attachment.name}
                            ></trouve-image-preview>`}
                        <div class="attachment-details">
                          <strong title=${attachment.name}>${attachment.name}</strong>
                          <small>${attachment.mime} · ${formatAttachmentBytes(attachment.size_bytes)}</small>
                        </div>
                        <button
                          class="attachment-remove"
                          type="button"
                          aria-label=${`Remove ${attachment.name}`}
                          ?disabled=${queueEditPending || this.#attachmentPending}
                          @click=${() => this.#removeRetainedQueueAttachment(index)}
                        >${fontAwesomeIcon("xmark")}</button>
                      </li>
                    `;
                  },
                )}
                ${this.#pendingAttachments.map(
                  (attachment, index) => {
                    const preview = pendingAttachmentPreviewUrl(attachment);
                    return html`
                      <li class=${preview === undefined ? "file-attachment" : "image-attachment"}>
                        ${preview === undefined
                          ? html`<span class="attachment-icon">${fontAwesomeIcon("file")}</span>`
                          : html`<trouve-image-preview
                              .source=${preview}
                              .name=${attachment.upload.name}
                            ></trouve-image-preview>`}
                        <div class="attachment-details">
                          <strong title=${attachment.upload.name}>${attachment.upload.name}</strong>
                          <small>${attachment.upload.mime} · ${formatAttachmentBytes(attachment.size)}</small>
                        </div>
                        <button
                          class="attachment-remove"
                          type="button"
                          aria-label=${`Remove ${attachment.upload.name}`}
                          ?disabled=${this.#requestPending
                            || queueEditPending
                            || this.#attachmentPending}
                          @click=${() => this.#removeAttachment(index)}
                        >${fontAwesomeIcon("xmark")}</button>
                      </li>
                    `;
                  },
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
                : queueEditing
                  ? "Edit queued prompt…  (Shift+Enter for a new line)"
                  : "Message the agent…  (Shift+Enter for a new line)"}
            rows="1"
            .value=${live(this.#composerDraft)}
            ?disabled=${thread === undefined
              || this.#requestPending
              || queueEditPending
              || connectivityBlocked}
            @input=${this.#composerChanged}
            @select=${this.#composerCursorMoved}
            @click=${this.#composerCursorMoved}
            @keydown=${this.#composerKeydown}
            @compositionstart=${this.#composerCompositionStarted}
            @compositionend=${this.#composerCompositionEnded}
            @paste=${this.#composerPaste}
          ></textarea>
          ${queueEditing
            ? html`<wa-button
                class="composer-submit"
                type="submit"
                variant="brand"
                title="Update queued prompt"
                ?disabled=${queueEditPending || this.#attachmentPending || !hasComposerContent || connectivityBlocked}
              >${queueEditPending ? "Updating…" : "Update"}</wa-button>`
            : turnControls.action === "cancel"
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
                      : html`<option value=${thread.mode} .selected=${true}>${thread.mode}</option>`}
                    ${this.#modes.map(
                      (mode) => html`<option
                        value=${mode.id}
                        .selected=${mode.id === thread.mode}
                      >${mode.display_name}</option>`,
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
                            ? html`<option value="" disabled .selected=${true}>Select…</option>`
                            : nothing}
                          ${modelControls.thinking.values.map(
                            (value) => html`<option
                              value=${value}
                              .selected=${value === modelControls.thinking!.selected}
                            >${modelOptionLabel(value)}</option>`,
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
                      ? html`<span
                          class="permission-warning"
                          role="img"
                          aria-label="Warning: YOLO changes run without approval"
                          title="YOLO: changes run without approval"
                        >${fontAwesomeIcon("triangle-exclamation")}</span>`
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
                            ? html`<option value="" disabled .selected=${true}>Select…</option>`
                            : nothing}
                          ${modelControls.context.values.map(
                            (value) => html`<option
                              value=${value}
                              .selected=${value === modelControls.context!.selected}
                            >${value.toUpperCase()}</option>`,
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
            ${fontAwesomeIcon("paperclip")}<span class="visually-hidden">Attach files</span>
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
                    ? fontAwesomeIcon("triangle-exclamation", {
                        className: "context-dial-glyph",
                      })
                    : contextUsage.compacting
                      ? fontAwesomeIcon("arrows-rotate", {
                          className: "context-dial-glyph compacting",
                          spin: true,
                        })
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
      ${this.#renderMarkdownContextMenu()}
      <span class="visually-hidden" role="status" aria-live="polite">
        ${this.#markdownContextMenuStatus}
      </span>
    `;
  }

  #activeComposerCompletion(
    commands: readonly { readonly name: string; readonly description?: string }[],
  ): ActiveComposerCompletion | undefined {
    if (this.#completionDismissed || this.#composerComposing) return undefined;
    const token = composerCompletionToken(this.#composerDraft, this.#composerCursor);
    if (token === undefined) return undefined;
    const candidates: readonly ComposerCompletionCandidate[] = token.kind === "command"
      ? commands.map((command) => ({
          value: command.name.replace(/^\/+/, ""),
          detail: command.description ?? "",
        }))
      : this.#sessionPaths.map((path) => ({ value: path }));
    let matches: readonly RankedComposerCompletion[];
    let searching = false;
    if (candidates.length < WORKER_COMPLETION_THRESHOLD) {
      matches = rankComposerCompletions(candidates, token.query);
    } else {
      const key = this.#completionWorkerIdentity(token.kind, token.query);
      matches = this.#completionWorkerKey === key ? this.#completionWorkerMatches : [];
      searching = this.#completionWorkerPending
        && this.#completionWorkerRequestedKey === key;
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

  #currentComposerCommands(): readonly {
    readonly name: string;
    readonly description?: string;
  }[] {
    return this.threadId === ""
      ? []
      : (this.#store.value?.threadView(this.threadId).commands ?? []);
  }

  #completionWorkerIdentity(kind: "command" | "file", query: string): string {
    const sourceRevision = kind === "file"
      ? this.#sessionPathsRevision
      : this.#completionCommandSourceRevision;
    return `${kind}\u0000${query}\u0000${sourceRevision}`;
  }

  #cancelCompletionWorker(): void {
    if (
      !this.#completionWorkerPending
      && this.#completionWorkerRequestedKey === ""
    ) return;
    this.#completionWorkerGeneration += 1;
    this.#completionWorkerPending = false;
    this.#completionWorkerRequestedKey = "";
  }

  #syncComposerCompletionEffect(
    commands: readonly { readonly name: string; readonly description?: string }[],
  ): void {
    if (commands !== this.#completionCommandSource) {
      this.#completionCommandSource = commands;
      this.#completionCommandSourceRevision += 1;
    }
    if (this.#completionDismissed || this.#composerComposing) {
      this.#cancelCompletionWorker();
      return;
    }
    const token = composerCompletionToken(this.#composerDraft, this.#composerCursor);
    if (token === undefined) {
      this.#cancelCompletionWorker();
      return;
    }
    const candidates: readonly ComposerCompletionCandidate[] = token.kind === "command"
      ? commands.map((command) => ({
          value: command.name.replace(/^\/+/, ""),
          detail: command.description ?? "",
        }))
      : this.#sessionPaths.map((path) => ({ value: path }));
    if (candidates.length < WORKER_COMPLETION_THRESHOLD) {
      this.#cancelCompletionWorker();
      return;
    }
    const key = this.#completionWorkerIdentity(token.kind, token.query);
    if (key === this.#completionWorkerRequestedKey) return;
    this.#completionWorkerRequestedKey = key;
    this.#completionWorkerPending = true;
    const generation = ++this.#completionWorkerGeneration;
    this.requestUpdate();
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
          && (
            unit.items.some(
              (item) => item.kind === "compaction" && item.state.kind === "running",
            )
            || (this.#messageDisclosure.get(unit.id) ?? true)
          )
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
    const hasRunningCompaction = items.some(
      (item) => item.kind === "compaction" && item.state.kind === "running",
    );
    if (compacting && !hasRunningCompaction) {
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
      // composer own the established 8px separation outside the scrollport; a virtual
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
      this.#historyAnchorToRestore = before.anchor;
      if (!before.followingTail) {
        const addedHeight = Math.max(
          0,
          this.#virtualizer.window().totalHeight - before.totalHeight,
        );
        this.#virtualizer.setViewport(
          before.scrollTop + addedHeight,
          this.#viewportHeight,
          { userInitiated: true, atTail: false },
        );
      }
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
      <div class="chat-scroll-shell">
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
                  ${this.#renderCompactionMarker({ kind: "running" })}
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
        <span class="chat-scroll-indicator" aria-hidden="true"></span>
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
    const compactionRunning = unit.items.some(
      (item) => item.kind === "compaction" && item.state.kind === "running",
    );
    const open = compactionRunning || (this.#messageDisclosure.get(unit.id) ?? true);
    const turnState = presentation.turnStates.get(unit.turn);
    const metadata = turnState?.kind === "completed"
      ? formatTurnMetadata(turnState.usage, turnDurationMs.get(unit.turn))
      : "";
    const preview = collapsedChatPreview(joined);
    return html`
      <article
        class="message turn-card assistant-message agent-turn-card"
        @contextmenu=${(event: MouseEvent) =>
          this.#openMarkdownContextMenu(event, joined)}
      >
        <header class="message-header agent-header ${open ? "" : "collapsed"}">
          <button
            class="message-disclosure"
            type="button"
            aria-expanded=${open ? "true" : "false"}
            aria-disabled=${compactionRunning ? "true" : "false"}
            aria-label=${open ? "Collapse agent message" : "Expand agent message"}
            title=${compactionRunning ? "Agent output stays open while context is compacting" : ""}
            @click=${() => this.#toggleMessageDisclosure(unit.id, true, compactionRunning)}
          >
            ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
              className: "disclosure-icon",
            })}
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
          ${joined === ""
            ? nothing
            : html`<span class="agent-copy-action">
                ${this.#renderCopyButton(
                  `agent:${unit.id}`,
                  assistantCopyText(joined),
                  "Copy assistant output",
                )}
              </span>`}
        </header>
        ${open
          ? html`<div class="message-body turn-body-stream agent-body-stream">
              ${this.#renderAgentBody(
                unit,
                turnModels,
                turnDurationMs,
                presentation,
              )}
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

  #renderCompactionMarker(
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
            ? fontAwesomeIcon("arrows-rotate", {
                className: "context-compaction-glyph",
                spin: true,
              })
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

  #renderAgentBody(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
  ) {
    const chatPreferences = this.#services.value === undefined
      ? undefined
      : readSignal(this.#services.value.chatPreferences);
    const collapseThinkingWithTools = chatPreferences?.collapseThinkingWithTools ?? false;
    const collapseCompactionWithTools = chatPreferences?.collapseCompactionWithTools ?? false;
    const rows: unknown[] = [];
    let activityConnectedFromCompaction = false;
    let activityRows: Array<{
      readonly content: unknown;
      readonly expandedGroup: boolean;
    }> = [];
    const flushActivityRows = (activityConnectedToCompaction = false): void => {
      if (activityRows.length === 0) return;
      const compactionConnected = activityConnectedFromCompaction
        || activityConnectedToCompaction;
      const timelineClass = `agent-activity-timeline ${
        activityRows.length === 1 ? "single-activity" : ""
      } ${activityRows.some(({ expandedGroup }) => expandedGroup)
        ? "has-expanded-group"
        : ""} ${compactionConnected ? "compaction-connected-timeline" : ""}`;
      rows.push(html`<div class=${timelineClass}>${
        activityRows.map(({ content }) => content)
      }</div>`);
      activityRows = [];
      activityConnectedFromCompaction = false;
    };
    const activityFollows = (start: number): boolean => {
      for (let cursor = start; cursor < unit.items.length; cursor += 1) {
        const candidate = unit.items[cursor];
        if (candidate === undefined) return false;
        if (candidate.kind === "tool" && isContextCompactionTool(candidate)) continue;
        return candidate.kind === "thinking" || candidate.kind === "tool";
      }
      return false;
    };
    const hasNativeCompaction = unit.items.some((item) => item.kind === "compaction");
    let index = 0;
    while (index < unit.items.length) {
      const item = unit.items[index];
      if (item === undefined) break;
      if (item.kind === "assistant") {
        flushActivityRows();
        const stretch: Extract<AgentChatItem, { readonly kind: "assistant" }>[] = [];
        while (index < unit.items.length && unit.items[index]?.kind === "assistant") {
          stretch.push(unit.items[index] as Extract<AgentChatItem, { readonly kind: "assistant" }>);
          index += 1;
        }
        const content = stretch.map((part) => part.content).filter(Boolean).join("\n\n");
        if (content !== "") {
          rows.push(html`<div
            class="agent-text-block"
            data-chat-anchor-id=${`assistant:${stretch.at(-1)?.id ?? stretch[0]?.id}`}
          ><trouve-markdown-view
                class="turn-markdown"
                .content=${content}
                .streaming=${stretch.some((part) => !part.complete)}
              ></trouve-markdown-view></div>`);
        }
        continue;
      }
      if (item.kind === "questions") {
        flushActivityRows();
        rows.push(this.#renderItem(item, turnModels, turnDurationMs, presentation));
        index += 1;
        continue;
      }
      if (item.kind === "compaction" && !collapseCompactionWithTools) {
        const connectBefore = activityRows.length > 0;
        const connectAfter = activityFollows(index + 1);
        flushActivityRows(connectBefore);
        rows.push(this.#renderCompactionMarker(item.state, item.id, {
          before: connectBefore,
          after: connectAfter,
        }));
        activityConnectedFromCompaction = connectAfter;
        index += 1;
        continue;
      }
      if (item.kind === "tool" && isContextCompactionTool(item)) {
        if (hasNativeCompaction) {
          if (!collapseCompactionWithTools) flushActivityRows();
          index += 1;
          continue;
        }
        if (!collapseCompactionWithTools) {
          const connectBefore = activityRows.length > 0;
          const connectAfter = activityFollows(index + 1);
          flushActivityRows(connectBefore);
          rows.push(this.#renderCompactionMarker(this.#legacyCompactionState(item), item.id, {
            before: connectBefore,
            after: connectAfter,
          }));
          activityConnectedFromCompaction = connectAfter;
          index += 1;
          continue;
        }
      }
      if (item.kind === "thinking" && !collapseThinkingWithTools) {
        activityRows.push({
          content: this.#renderVisibleThinking(item),
          expandedGroup: false,
        });
        index += 1;
        continue;
      }

      const run: AgentActivityItem[] = [];
      while (index < unit.items.length) {
        const candidate = unit.items[index];
        if (
          candidate === undefined
          || candidate.kind === "assistant"
          || candidate.kind === "questions"
          || (!collapseCompactionWithTools && candidate.kind === "compaction")
          || (!collapseCompactionWithTools
            && candidate.kind === "tool"
            && isContextCompactionTool(candidate))
          || (!collapseThinkingWithTools && candidate.kind === "thinking")
        ) break;
        if (
          hasNativeCompaction
          && candidate.kind === "tool"
          && isContextCompactionTool(candidate)
        ) {
          index += 1;
          continue;
        }
        run.push(candidate as AgentActivityItem);
        index += 1;
      }
      const only = run[0];
      const groupSinglePreferenceBoundary = run.length === 1 && (
        (collapseThinkingWithTools && only?.kind === "thinking")
        || (collapseCompactionWithTools && (
          only?.kind === "compaction"
          || (only?.kind === "tool" && isContextCompactionTool(only))
        ))
      );
      if (run.length < 2 && !groupSinglePreferenceBoundary) {
        if (only !== undefined) {
          activityRows.push({
            content: this.#renderItem(only, turnModels, turnDurationMs, presentation),
            expandedGroup: false,
          });
        }
        continue;
      }
      activityRows.push({
        content: this.#renderActivityGroup(
          unit,
          run,
          turnModels,
          turnDurationMs,
          presentation,
        ),
        expandedGroup: this.#activityGroupOpen(unit, run),
      });
    }
    flushActivityRows();
    return rows;
  }

  #renderVisibleThinking(
    item: Extract<ThreadChatItem, { readonly kind: "thinking" }>,
  ) {
    this.#ensureMarkdown();
    return html`
      <article
        class=${`message thinking-output ${item.complete ? "complete" : "running"}`}
        data-chat-anchor-id=${`item:${item.id}`}
      >
        <header class="thinking-header">
          <strong>${item.complete ? "Thought" : "Thinking"}</strong>
          <span class="thinking-header-spacer"></span>
          ${this.#renderCopyButton(
            `message:${item.id}`,
            item.content,
            "Copy thought process",
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

  #renderActivityGroup(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
    items: readonly AgentActivityItem[],
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
    const active = items.some((item) =>
      item.kind === "thinking"
        ? !item.complete
        : item.kind === "compaction"
          ? item.state.kind === "running"
          : item.status === "running" || item.status === "awaiting-approval"
    );
    const failed = items.some((item) =>
      item.kind === "compaction"
        ? item.state.kind === "failed"
        : item.kind === "tool"
          && (item.status === "error" || item.status === "denied" || item.status === "aborted")
    );
    const tone = failed
      ? "error"
      : needsApproval
        ? "warning"
        : active
          ? "active"
          : "complete";
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
          ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
            className: "disclosure-icon",
          })}
          <strong>${activityGroupSummary(items)}</strong>
        </summary>
        ${open
          ? html`<div class="activity-group-body">
              <div class=${`agent-activity-timeline activity-group-timeline ${
                items.length === 1 ? "single-activity" : ""
              }`}>
                ${items.map((item) => this.#renderGroupedActivityItem(
                  item,
                  turnModels,
                  turnDurationMs,
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
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
  ) {
    if (item.kind === "thinking") return this.#renderVisibleThinking(item);
    if (item.kind === "compaction") {
      return this.#renderCompactionMarker(item.state, item.id, {
        before: false,
        after: false,
        nested: true,
      });
    }
    if (isContextCompactionTool(item)) {
      return this.#renderCompactionMarker(this.#legacyCompactionState(item), item.id, {
        before: false,
        after: false,
        nested: true,
      });
    }
    return this.#renderItem(item, turnModels, turnDurationMs, presentation);
  }

  #activityGroupOpen(
    unit: Extract<ChatRenderUnit, { readonly kind: "agent" }>,
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
    this.#requestDisclosureUpdate();
  }

  #renderQueue(
    queue: readonly QueuedPrompt[],
    turnRunning: boolean,
    connectivityBlocked: boolean,
  ) {
    if (queue.length === 0) return nothing;
    const queueMutationBusy = this.#queueBusy !== ""
      || (this.#queueEditId !== "" && this.#attachmentPending);
    const controls = queueControlState({
      threadAvailable: this.threadId !== "",
      queueLength: queue.length,
      turnRunning,
      busy: queueMutationBusy,
      connectivityBlocked,
    });
    return html`
      <section class="queue-panel" aria-busy=${queueMutationBusy ? "true" : "false"}>
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
                  >${fontAwesomeIcon("grip-vertical")}</span>
                  <span class="queue-index" aria-hidden="true">${index + 1}.</span>
                  <p title=${prompt.content}>${queuePreview(prompt.content)}</p>
                  ${prompt.attachments === undefined || prompt.attachments.length === 0
                    ? nothing
                    : html`<span
                        class="queue-attachment-badge"
                        role="img"
                        aria-label=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                        title=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                      >${fontAwesomeIcon("paperclip")}${prompt.attachments.length}</span>`}
                  <div class="queue-actions" aria-label=${`Actions for queued prompt ${index + 1}`}>
                    ${turnRunning
                      ? nothing
                      : html`<button type="button" data-queue-action="send-now" aria-label="Send this queued prompt now" title="Send now" ?disabled=${controls.dispatchDisabled} @click=${() => this.#sendQueuedNow(queue, index)}>${fontAwesomeIcon("play")}</button>`}
                    <button type="button" data-queue-action="earlier" aria-label="Run earlier" title="Run earlier" ?disabled=${index === 0 || controls.mutationsDisabled} @click=${() => this.#moveQueued(queue, index, -1)}>${fontAwesomeIcon("arrow-up")}</button>
                    <button type="button" data-queue-action="later" aria-label="Run later" title="Run later" ?disabled=${index === queue.length - 1 || controls.mutationsDisabled} @click=${() => this.#moveQueued(queue, index, 1)}>${fontAwesomeIcon("arrow-down")}</button>
                    <button
                      type="button"
                      data-queue-action="edit"
                      aria-label=${this.#queueEditId === prompt.id
                        ? "Queued prompt is being edited"
                        : "Edit queued prompt"}
                      title=${this.#queueEditId === prompt.id ? "Editing" : "Edit"}
                      ?disabled=${controls.mutationsDisabled
                        || this.#requestPending
                        || this.#attachmentPending
                        || (this.#queueEditId !== "" && this.#queueEditId !== prompt.id)}
                      @click=${() => this.#startQueueEdit(prompt)}
                    >${fontAwesomeIcon("pen")}</button>
                    <button class="danger" type="button" data-queue-action="delete" aria-label="Remove from queue" title="Remove from queue" ?disabled=${controls.mutationsDisabled} @click=${() => this.#deleteQueued(queue, prompt.id)}>${fontAwesomeIcon("trash-can")}</button>
                  </div>
                </div>
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

  #captureChatDomAnchor(viewport: HTMLElement): ChatDomAnchor | undefined {
    const viewportRect = viewport.getBoundingClientRect();
    const viewportTop = viewportRect.top;
    const viewportBottom = viewportRect.bottom;
    const visible = [...viewport.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")]
      .map((element) => ({ element, rect: element.getBoundingClientRect() }))
      .filter(({ element, rect }) =>
        element.dataset["chatAnchorId"] !== undefined
        && rect.height > 0
        && rect.bottom > viewportTop
        && rect.top < viewportBottom
      );
    const crossingTop = visible
      .filter(({ rect }) => rect.top <= viewportTop + 0.5 && rect.bottom > viewportTop + 0.5)
      .sort((left, right) => left.rect.height - right.rect.height)[0];
    const nextVisible = visible
      .filter(({ rect }) => rect.top > viewportTop + 0.5)
      .sort((left, right) => left.rect.top - right.rect.top)[0];
    const candidate = crossingTop ?? nextVisible;
    if (candidate !== undefined) {
      const id = candidate.element.dataset["chatAnchorId"];
      if (id !== undefined) {
        return { id, offset: candidate.rect.top - viewportTop };
      }
    }

    const virtualRow = [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
      .map((element) => ({ element, rect: element.getBoundingClientRect() }))
      .filter(({ rect }) =>
        rect.height > 0
        && rect.bottom > viewportTop
        && rect.top < viewportBottom
      )
      .sort((left, right) => {
        const leftCrosses = left.rect.top <= viewportTop && left.rect.bottom > viewportTop;
        const rightCrosses = right.rect.top <= viewportTop && right.rect.bottom > viewportTop;
        if (leftCrosses !== rightCrosses) return leftCrosses ? -1 : 1;
        return left.rect.top - right.rect.top;
      })[0];
    const id = virtualRow?.element.dataset["virtualId"];
    return id === undefined || virtualRow === undefined
      ? undefined
      : { id: `virtual:${id}`, offset: virtualRow.rect.top - viewportTop };
  }

  #restoreHistoryPrependAnchor(viewport: HTMLElement): void {
    const anchor = this.#historyAnchorToRestore;
    this.#historyAnchorToRestore = undefined;
    if (anchor === undefined) return;
    this.#historyAnchorStabilizer = { ...anchor, scrollTop: viewport.scrollTop };

    const beforeMeasure = this.#virtualizer.window();
    for (const row of viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")) {
      const id = row.dataset["virtualId"];
      const height = row.getBoundingClientRect().height;
      if (id === undefined || height <= 0) continue;
      try {
        this.#virtualizer.measure(id, height);
      } catch {
        // A row can leave the window while this one-time history correction runs.
      }
    }
    const afterMeasure = this.#virtualizer.window();
    if (!sameVirtualRenderWindow(beforeMeasure, afterMeasure)) {
      this.#syncMountedVirtualGeometry(viewport, afterMeasure);
    }

    this.#correctHistoryAnchor(viewport);
    this.#scheduleHistoryAnchorRelease();
  }

  #correctHistoryAnchor(viewport: HTMLElement): void {
    const anchor = this.#historyAnchorStabilizer;
    if (anchor === undefined) return;
    const virtualId = anchor.id.startsWith("virtual:")
      ? anchor.id.slice("virtual:".length)
      : undefined;
    const candidates = virtualId === undefined
      ? [...viewport.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")]
        .filter((element) => element.dataset["chatAnchorId"] === anchor.id)
      : [...viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")]
        .filter((element) => element.dataset["virtualId"] === virtualId);
    const element = candidates[0];
    if (element === undefined) return;

    const delta = element.getBoundingClientRect().top
      - viewport.getBoundingClientRect().top
      - anchor.offset;
    const target = Math.max(0, viewport.scrollTop + delta);
    this.#virtualizer.setViewport(target, viewport.clientHeight, {
      userInitiated: true,
      atTail: false,
    });
    const correctedScrollTop = this.#virtualizer.window().scrollTop;
    this.#historyAnchorStabilizer = { ...anchor, scrollTop: correctedScrollTop };
    this.#setChatScrollTop(viewport, correctedScrollTop);
  }

  #scheduleHistoryAnchorRelease(): void {
    if (this.#historyAnchorSettleTimer !== undefined) {
      clearTimeout(this.#historyAnchorSettleTimer);
    }
    this.#historyAnchorSettleTimer = setTimeout(() => {
      this.#historyAnchorSettleTimer = undefined;
      this.#historyAnchorStabilizer = undefined;
    }, CHAT_HISTORY_ANCHOR_SETTLE_MS);
  }

  #clearHistoryAnchorStabilizer(): void {
    if (this.#historyAnchorSettleTimer !== undefined) {
      clearTimeout(this.#historyAnchorSettleTimer);
      this.#historyAnchorSettleTimer = undefined;
    }
    this.#historyAnchorStabilizer = undefined;
  }

  // WebKitGTK retains native scrollbar hit-testing while intermittently
  // failing to paint its thumb. This passive layer mirrors only the visual
  // thumb; pointer input still reaches the native scrollbar underneath. Keep
  // layout reads out of the scroll hot path so wheel momentum stays smooth.
  #refreshChatScrollIndicator(viewport: HTMLElement): void {
    const indicator = viewport.parentElement?.querySelector<HTMLElement>(
      ".chat-scroll-indicator",
    );
    if (indicator === null || indicator === undefined) return;
    const trackHeight = Math.max(
      0,
      viewport.clientHeight - CHAT_SCROLL_INDICATOR_INSET_PX * 2,
    );
    const maxScrollTop = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
    const proportionalHeight = viewport.scrollHeight <= 0
      ? trackHeight
      : trackHeight * viewport.clientHeight / viewport.scrollHeight;
    const thumbHeight = Math.min(
      trackHeight,
      Math.max(CHAT_SCROLL_INDICATOR_MIN_HEIGHT_PX, proportionalHeight),
    );
    const thumbTravel = Math.max(0, trackHeight - thumbHeight);
    const scrollable = maxScrollTop > CHAT_TAIL_EPSILON_PX && thumbTravel > 0;
    this.#scrollIndicatorMetrics = { maxScrollTop, thumbTravel };
    indicator.style.height = `${thumbHeight}px`;
    indicator.toggleAttribute("data-scrollable", scrollable);
    this.#syncChatScrollIndicatorPosition(viewport);
  }

  #syncChatScrollIndicatorPosition(viewport: HTMLElement): void {
    const indicator = viewport.parentElement?.querySelector<HTMLElement>(
      ".chat-scroll-indicator",
    );
    const metrics = this.#scrollIndicatorMetrics;
    if (indicator === null || indicator === undefined || metrics === undefined) return;
    const scrollTop = Math.min(
      metrics.maxScrollTop,
      Math.max(0, viewport.scrollTop),
    );
    const thumbTop = metrics.maxScrollTop > CHAT_TAIL_EPSILON_PX
      ? metrics.thumbTravel * scrollTop / metrics.maxScrollTop
      : 0;
    indicator.style.transform = `translate3d(0, ${thumbTop}px, 0)`;
  }

  #scheduleChatScrollIndicator(viewport: HTMLElement): void {
    this.#scrollIndicatorFrame ??= globalThis.requestAnimationFrame(() => {
      this.#scrollIndicatorFrame = undefined;
      if (
        this.isConnected
        && viewport.isConnected
        && viewport.dataset["threadId"] === this.threadId
      ) {
        this.#syncChatScrollIndicatorPosition(viewport);
      }
    });
  }

  #cancelScheduledScrollIndicator(): void {
    if (this.#scrollIndicatorFrame === undefined) return;
    globalThis.cancelAnimationFrame(this.#scrollIndicatorFrame);
    this.#scrollIndicatorFrame = undefined;
  }

  readonly #chatScrolled = (event: Event): void => {
    const viewport = event.currentTarget as HTMLElement;
    if (
      viewport.dataset["threadId"] !== this.threadId ||
      this.#restoredScrollThreadId !== this.threadId
    ) return;
    this.#scheduleChatScrollIndicator(viewport);
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
    if (atTail) {
      this.#clearHistoryAnchorStabilizer();
    } else if (this.#historyAnchorStabilizer !== undefined) {
      const anchor = this.#historyAnchorStabilizer;
      this.#historyAnchorStabilizer = {
        ...anchor,
        offset: anchor.offset - (viewport.scrollTop - anchor.scrollTop),
        scrollTop: viewport.scrollTop,
      };
      this.#scheduleHistoryAnchorRelease();
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
      { userInitiated, atTail },
    );
    const after = this.#virtualizer.window();
    const maxScrollTop = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
    const historyPrefetchThreshold = Math.min(
      maxScrollTop / 2,
      Math.max(
        CHAT_HISTORY_PREFETCH_MIN_PX,
        viewport.clientHeight * CHAT_HISTORY_PREFETCH_VIEWPORTS,
      ),
    );
    // Prepend corrections and virtual-row measurement can emit scroll events
    // while the reader remains inside the threshold. Only an actual input
    // gesture may advance another page; otherwise one wheel tick can walk the
    // complete transcript after each prepend settles.
    if (userInitiated && viewport.scrollTop <= historyPrefetchThreshold) {
      if (this.#historyLoading) {
        // Preserve one explicit reader gesture that arrives while the prior
        // page is still settling. This remains bounded: programmatic prepend
        // events cannot set the flag, and multiple gestures coalesce into one
        // additional page instead of replaying the full transcript.
        this.#historyLoadRequested = true;
      } else {
        void this.#loadOlderHistory(false);
      }
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

  readonly #chatScrollEnded = (event: Event): void => {
    this.#syncChatScrollIndicatorPosition(event.currentTarget as HTMLElement);
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
    this.#scheduleChatScrollIndicator(viewport);
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
    this.#clearHistoryAnchorStabilizer();
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

  #warmOlderHistory(): void {
    if (this.threadId === "" || this.#historyWarmThreads.has(this.threadId)) return;
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) return;
    const view = store.threadView(this.threadId);
    if (!view.hasOlder || view.itemOffset === 0) return;
    // Warm exactly one bounded page per thread. Further pages are loaded by
    // the rolling scroll threshold, avoiding both a visible boundary pause
    // and the old full-session replay behavior.
    this.#historyWarmThreads.add(this.threadId);
    if (!this.#historyLoading) void this.#loadOlderHistory(false);
  }

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
        const viewport = this.querySelector<HTMLElement>(".chat-stream");
        this.#pendingHistoryPrepend = {
          scrollTop: viewport?.scrollTop ?? virtualWindow.scrollTop,
          totalHeight: virtualWindow.totalHeight,
          followingTail: virtualWindow.followingTail,
          anchor: virtualWindow.followingTail || viewport === null
            ? undefined
            : this.#captureChatDomAnchor(viewport),
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
        const loadRequested = this.#historyLoadRequested;
        this.#historyLoadRequested = false;
        this.#historyLoading = false;
        this.requestUpdate();
        if (loadRequested) void this.#loadOlderHistory(false);
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
          <article
            class="message turn-card user-message"
            data-chat-anchor-id=${`item:${item.id}`}
          >
            <header class="message-header user-header ${open ? "" : "collapsed"}">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse your message" : "Expand your message"}
                @click=${() => this.#toggleMessageDisclosure(item.id, true)}
              >
                ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
                  className: "disclosure-icon",
                })}
                <strong>You</strong>
                ${open
                  ? html`<span class="agent-header-spacer"></span>`
                  : html`<small class="message-collapsed-preview">${preview}</small>`}
              </button>
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
        const turnState = presentation.turnStates.get(item.turn);
        const metadata = presentation.lastAssistantIds.has(item.id)
          && turnState?.kind === "completed"
          ? formatTurnMetadata(turnState.usage, turnDurationMs.get(item.turn))
          : "";
        return html`
          <article
            class="message turn-card assistant-message"
            data-chat-anchor-id=${`item:${item.id}`}
            @contextmenu=${(event: MouseEvent) =>
              this.#openMarkdownContextMenu(event, item.content)}
          >
            <header class="message-header agent-header ${open ? "" : "collapsed"}">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse agent message" : "Expand agent message"}
                @click=${() => this.#toggleMessageDisclosure(item.id, true)}
              >
                ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
                  className: "disclosure-icon",
                })}
                <strong>Agent</strong>
                ${turnModels.get(item.turn) === undefined
                  ? nothing
                  : html`<small class="agent-model-label">(${turnModels.get(item.turn)})</small>`}
                ${metadata === ""
                  ? nothing
                  : html`<small class="turn-metadata">${metadata}</small>`}
              </button>
              ${item.content === ""
                ? nothing
                : html`<span class="agent-copy-action">
                    ${this.#renderCopyButton(
                      `agent:${item.id}`,
                      assistantCopyText(item.content),
                      "Copy assistant output",
                    )}
                  </span>`}
            </header>
            ${open
              ? html`
                  <div class="message-body turn-body-stream">
                    <trouve-markdown-view
                      class="turn-markdown"
                      .content=${item.content}
                      .streaming=${!item.complete}
                    ></trouve-markdown-view>
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
          <article
            class=${`message thinking-card ${item.complete ? "complete" : "running"}`}
            data-chat-anchor-id=${`item:${item.id}`}
          >
            <header class="thinking-header">
              <button
                class="message-disclosure"
                type="button"
                aria-expanded=${open ? "true" : "false"}
                aria-label=${open ? "Collapse thought process" : "Expand thought process"}
                @click=${() => this.#toggleMessageDisclosure(item.id, defaultOpen)}
              >
                ${fontAwesomeIcon(open ? "caret-down" : "caret-right", {
                  className: "disclosure-icon",
                })}
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
      case "compaction":
        return this.#renderCompactionMarker(item.state, item.id);
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
            class=${`message tool-card tool-${item.status} ${approvalRequired ? "approval-required" : ""}`}
            data-chat-anchor-id=${`item:${item.id}`}
            data-call-id=${item.callId}
            ?open=${toolOpen}
            @keydown=${(event: KeyboardEvent) =>
              this.#approvalShortcut(event, item.callId)}
          >
            <summary
              @click=${(event: Event) =>
                this.#toggleToolDisclosure(event, item.callId, approvalRequired)}
            >
              ${fontAwesomeIcon(toolOpen ? "caret-down" : "caret-right", {
                className: "tool-disclosure",
              })}
              <span class="tool-status ${item.status}" aria-hidden="true">
                ${fontAwesomeIcon(toolStatusIcon(item.status), {
                  className: "tool-status-icon",
                  spin: item.status === "running",
                })}
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
                >${fontAwesomeIcon(raw ? "code" : "list")}</button>
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
                                ${fontAwesomeIcon(todo.icon)}<span>${todo.content}</span>
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
            <section
              class="message question-card question-resolved"
              data-chat-anchor-id=${`item:${item.id}`}
            >
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
              data-chat-anchor-id=${`item:${item.id}`}
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
                                ${fontAwesomeIcon(
                                  multiple
                                    ? checked ? "square-check" : "square"
                                    : checked ? "circle-dot" : "circle",
                                  { className: "question-option-mark" },
                                )}
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
                                ${fontAwesomeIcon(
                                  multiple
                                    ? checked ? "square-check" : "square"
                                    : checked ? "circle-dot" : "circle",
                                  { className: "question-option-mark" },
                                )}
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
                    <trouve-image-preview
                      .source=${path}
                      .name=${attachment.name}
                      lazy
                    ></trouve-image-preview>
                  `
                : html`<span class="attachment-icon">${fontAwesomeIcon(
                    isImageAttachment(attachment) ? "file-image" : "file",
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

  #renderMarkdownContextMenu() {
    const menu = this.#markdownContextMenu;
    if (menu === undefined) return nothing;
    return html`
      <div
        class="message-context-menu"
        role="menu"
        aria-label="Message actions"
        style=${`left:${menu.x}px;top:${menu.y}px`}
        @contextmenu=${(event: Event) => event.preventDefault()}
        @keydown=${this.#markdownContextMenuKeydown}
      >
        ${menu.selection === ""
          ? nothing
          : html`<button
              type="button"
              role="menuitem"
              @click=${() => void this.#copyMarkdownContextValue("selection")}
            >
              ${fontAwesomeIcon("copy")}
              <span>Copy</span>
            </button>`}
        <button
          type="button"
          role="menuitem"
          @click=${() => void this.#copyMarkdownContextValue("markdown")}
        >
          ${fontAwesomeIcon("file-lines")}
          <span>Copy as markdown</span>
        </button>
      </div>
    `;
  }

  #openMarkdownContextMenu(event: MouseEvent, markdown: string): void {
    if (markdown === "") return;
    const source = event.currentTarget;
    if (!(source instanceof HTMLElement)) return;
    const preserveNativeMenu = event.composedPath().some((target) =>
      target instanceof Element
      && target.matches(
        "a, img, video, input, textarea, select, .tool-card, .thinking-card, .thinking-output, .context-compaction-marker, .question-card",
      )
    );
    if (preserveNativeMenu) {
      this.#dismissMarkdownContextMenu();
      return;
    }

    event.preventDefault();
    const selection = this.#selectedTextWithin(source);
    const sourceBounds = source.getBoundingClientRect();
    const keyboardPosition = event.clientX === 0 && event.clientY === 0;
    const requestedX = keyboardPosition ? sourceBounds.left + 24 : event.clientX;
    const requestedY = keyboardPosition ? sourceBounds.top + 24 : event.clientY;
    const viewportWidth = globalThis.innerWidth || document.documentElement.clientWidth;
    const viewportHeight = globalThis.innerHeight || document.documentElement.clientHeight;
    const estimatedWidth = 220;
    const estimatedHeight = selection === "" ? 42 : 76;
    const menu: MarkdownContextMenu = {
      markdown,
      selection,
      x: Math.max(8, Math.min(requestedX, viewportWidth - estimatedWidth - 8)),
      y: Math.max(8, Math.min(requestedY, viewportHeight - estimatedHeight - 8)),
    };
    this.#markdownContextMenu = menu;
    this.#markdownContextMenuStatus = "";
    this.#markdownContextMenuReturnFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : undefined;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (this.#markdownContextMenu !== menu) return;
      this.querySelector<HTMLButtonElement>(
        '.message-context-menu [role="menuitem"]',
      )?.focus();
    });
  }

  #selectedTextWithin(source: HTMLElement): string {
    const selection = globalThis.getSelection?.();
    if (selection === undefined || selection === null || selection.rangeCount === 0) return "";
    const range = selection.getRangeAt(0);
    const commonAncestor = range.commonAncestorContainer;
    const root = commonAncestor.getRootNode();
    const inside = source.contains(commonAncestor)
      || (root instanceof ShadowRoot && source.contains(root.host));
    return inside ? selection.toString() : "";
  }

  readonly #dismissMarkdownContextMenuFromPointer = (event: PointerEvent): void => {
    if (this.#markdownContextMenu === undefined) return;
    const target = event.target;
    if (
      target instanceof Element
      && target.closest(".message-context-menu") !== null
    ) return;
    this.#dismissMarkdownContextMenu();
  };

  readonly #dismissMarkdownContextMenu = (): void => {
    this.#closeMarkdownContextMenu(false);
  };

  #closeMarkdownContextMenu(restoreFocus: boolean): void {
    if (this.#markdownContextMenu === undefined) return;
    const returnFocus = this.#markdownContextMenuReturnFocus;
    this.#markdownContextMenu = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
    this.requestUpdate();
    if (restoreFocus && returnFocus?.isConnected === true) returnFocus.focus();
  }

  readonly #markdownContextMenuKeydown = (event: KeyboardEvent): void => {
    const menu = event.currentTarget;
    if (!(menu instanceof HTMLElement)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      this.#closeMarkdownContextMenu(true);
      return;
    }
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = [...menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')];
    if (items.length === 0) return;
    event.preventDefault();
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "Home"
      ? 0
      : event.key === "End"
        ? items.length - 1
        : event.key === "ArrowDown"
          ? (current + 1 + items.length) % items.length
          : (current - 1 + items.length) % items.length;
    items[next]?.focus();
  };

  async #copyMarkdownContextValue(kind: "selection" | "markdown"): Promise<void> {
    const menu = this.#markdownContextMenu;
    if (menu === undefined) return;
    const copySelection = kind === "selection" || menu.selection !== "";
    const value = copySelection ? menu.selection : menu.markdown;
    const label = copySelection ? "Selection" : "Markdown";
    this.#markdownContextMenu = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
    this.requestUpdate();
    const result = await copyChatText(value, globalThis.navigator?.clipboard);
    this.#markdownContextMenuStatus = `${label}: ${copyActionLabel(result)}`;
    this.requestUpdate();
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
    if (
      this.#queueBusy !== ""
      || this.#requestPending
      || this.#attachmentPending
      || this.#connectivityBlocked()
    ) return;
    if (this.#queueEditId === prompt.id) {
      this.#focusComposerNow();
      return;
    }
    if (this.#queueEditId === "") {
      this.#persistComposerDraftNow();
      const currentDraft = this.#composerDraftSnapshot();
      this.#queueEditReturnDraft = {
        text: currentDraft.text,
        cursor: currentDraft.cursor,
        attachments: [...currentDraft.attachments],
      };
    }
    this.#composerDraftRestoreGeneration += 1;
    this.#queueEditId = prompt.id;
    this.#queueEditRetainedAttachments = [...(prompt.attachments ?? [])];
    this.#composerDraft = prompt.content;
    this.#composerCursor = prompt.content.length;
    this.#pendingAttachments = [];
    this.#completionSelected = 0;
    this.#completionDismissed = true;
    this.#restoreComposerSelection = true;
    this.#queueError = "";
    this.#requestError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.#focusComposerNow();
      this.#resizeComposer();
    });
  }

  readonly #cancelQueueEdit = (): void => {
    if (this.#queueBusy !== "" || this.#attachmentPending) return;
    this.#queueError = "";
    this.#restoreComposerAfterQueueEdit();
  };

  #restoreComposerAfterQueueEdit(): void {
    const draft = this.#queueEditReturnDraft
      ?? this.#services.value?.composerDrafts.read(this.#composerDraftThreadId);
    this.#queueEditId = "";
    this.#queueEditRetainedAttachments = [];
    this.#queueEditReturnDraft = undefined;
    this.#composerDraft = draft?.text ?? "";
    this.#composerCursor = draft?.cursor ?? 0;
    this.#pendingAttachments = [...(draft?.attachments ?? [])];
    this.#completionSelected = 0;
    this.#completionDismissed = false;
    this.#restoreComposerSelection = true;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.#focusComposerNow();
      this.#resizeComposer();
    });
  }

  async #saveQueued(form: HTMLFormElement): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const promptId = this.#queueEditId;
    const textarea = form.elements.namedItem("message") as HTMLTextAreaElement | null;
    const content = textarea?.value.trim() ?? "";
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || promptId === ""
      || (content === ""
        && this.#queueEditRetainedAttachments.length === 0
        && this.#pendingAttachments.length === 0)
      || this.#queueBusy !== ""
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    let saved = false;
    try {
      await services.protocol.updateQueuedPrompt(promptId, {
        content,
        retained_attachment_ids: this.#queueEditRetainedAttachments.map(({ id }) => id),
        attachments: this.#pendingAttachments.map(({ upload }) => upload),
      });
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      try {
        store.replaceThreadQueue(threadId, await services.protocol.listQueue(threadId));
      } catch {
        const retained = [...this.#queueEditRetainedAttachments];
        const view = store.threadView(threadId);
        store.replaceThreadQueue(
          threadId,
          view.queue.map((candidate) =>
            candidate.id === promptId
              ? { ...candidate, content, attachments: retained }
              : candidate,
          ),
        );
      }
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      saved = true;
      this.#restoreComposerAfterQueueEdit();
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queued prompt could not be updated. Your edit is still available.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        if (!saved) this.#focusComposerNow();
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
    const wasEditing = this.#queueEditId === promptId;
    try {
      await services.protocol.deleteQueuedPrompt(promptId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      const latest = store.threadView(threadId).queue;
      store.replaceThreadQueue(
        threadId,
        latest.filter((prompt) => prompt.id !== promptId),
      );
      if (wasEditing) this.#restoreComposerAfterQueueEdit();
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
        } else if (wasEditing) {
          this.#focusComposerNow();
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

  #draftThreadIdForCurrentScope(): string {
    if (this.sessionId === "" || this.threadId === "") return "";
    return this.#store.value?.thread(this.threadId)?.session_id === this.sessionId
      ? this.threadId
      : "";
  }

  #composerDraftSnapshot() {
    return {
      text: this.#composerDraft,
      cursor: this.#composerCursor,
      attachments: this.#pendingAttachments,
    } as const;
  }

  #stageCurrentComposerDraft(): void {
    if (this.#queueEditId !== "") return;
    const drafts = this.#services.value?.composerDrafts;
    if (drafts === undefined || this.#composerDraftThreadId === "") return;
    drafts.stage(this.#composerDraftThreadId, this.#composerDraftSnapshot());
  }

  #scheduleComposerDraftPersistence(): void {
    if (this.#queueEditId !== "") return;
    this.#stageCurrentComposerDraft();
    if (this.#composerDraftThreadId === "") return;
    if (this.#composerDraftPersistTimer !== undefined) {
      clearTimeout(this.#composerDraftPersistTimer);
    }
    const threadId = this.#composerDraftThreadId;
    this.#composerDraftPersistTimer = setTimeout(() => {
      this.#composerDraftPersistTimer = undefined;
      if (this.#composerDraftThreadId === threadId) {
        void this.#services.value?.composerDrafts.persist(threadId);
      }
    }, COMPOSER_DRAFT_PERSIST_DELAY_MS);
  }

  #persistComposerDraftNow(): void {
    if (this.#composerDraftPersistTimer !== undefined) {
      clearTimeout(this.#composerDraftPersistTimer);
      this.#composerDraftPersistTimer = undefined;
    }
    if (this.#queueEditId !== "") return;
    const drafts = this.#services.value?.composerDrafts;
    const threadId = this.#composerDraftThreadId;
    if (drafts === undefined || threadId === "") return;
    drafts.stage(threadId, this.#composerDraftSnapshot());
    void drafts.persist(threadId);
  }

  readonly #persistComposerDraftFromPageHide = (): void => {
    this.#persistComposerDraftNow();
  };

  #restoreComposerDraft(threadId: string): void {
    if (this.#composerDraftPersistTimer !== undefined) {
      clearTimeout(this.#composerDraftPersistTimer);
      this.#composerDraftPersistTimer = undefined;
    }
    const generation = ++this.#composerDraftRestoreGeneration;
    const drafts = this.#services.value?.composerDrafts;
    if (drafts === undefined) {
      this.#composerDraftThreadId = "";
      this.#composerDraft = "";
      this.#composerCursor = 0;
      this.#pendingAttachments = [];
      return;
    }

    this.#composerDraftThreadId = threadId;
    const draft = drafts.read(threadId);
    this.#composerDraft = draft.text;
    this.#composerCursor = draft.cursor;
    this.#pendingAttachments = [...draft.attachments];
    this.#restoreComposerSelection = threadId !== "";
    if (threadId === "") return;

    void drafts.hydrate(threadId).then((hydrated) => {
      if (
        generation !== this.#composerDraftRestoreGeneration
        || threadId !== this.#draftThreadIdForCurrentScope()
        || this.#queueEditId !== ""
      ) return;
      this.#composerDraft = hydrated.text;
      this.#composerCursor = hydrated.cursor;
      this.#pendingAttachments = [...hydrated.attachments];
      this.#restoreComposerSelection = true;
      this.requestUpdate();
    });
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
    if (this.#queueEditId !== "") {
      await this.#saveQueued(form);
      return;
    }
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
      void services.composerDrafts.clear(threadId).catch(() => undefined);
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
    if (this.#composerAttachmentCount() >= MAX_PENDING_ATTACHMENTS) {
      this.#requestError = `Attach at most ${MAX_PENDING_ATTACHMENTS} files at once.`;
      return false;
    }
    const total = this.#composerAttachmentBytes() + attachment.size;
    if (total > MAX_PENDING_ATTACHMENT_BYTES) {
      this.#requestError = "Pending attachments exceed the 20 MB mobile memory budget.";
      return false;
    }
    this.#pendingAttachments = [...this.#pendingAttachments, attachment];
    this.#scheduleComposerDraftPersistence();
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
        if (this.#composerAttachmentCount() >= MAX_PENDING_ATTACHMENTS) {
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
        const total = this.#composerAttachmentBytes() + attachment.size;
        if (total > MAX_PENDING_ATTACHMENT_BYTES) {
          this.#requestError = "Pending attachments exceed the 20 MB mobile memory budget.";
          break;
        }
        this.#pendingAttachments = [...this.#pendingAttachments, attachment];
        this.#scheduleComposerDraftPersistence();
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
    this.#scheduleComposerDraftPersistence();
    this.requestUpdate();
  }

  #removeRetainedQueueAttachment(index: number): void {
    if (
      this.#queueEditId === ""
      || this.#queueBusy !== ""
      || this.#attachmentPending
    ) return;
    this.#queueEditRetainedAttachments = this.#queueEditRetainedAttachments.filter(
      (_, candidate) => candidate !== index,
    );
    this.requestUpdate();
  }

  #composerAttachmentCount(): number {
    return this.#queueEditRetainedAttachments.length + this.#pendingAttachments.length;
  }

  #composerAttachmentBytes(): number {
    return this.#queueEditRetainedAttachments.reduce(
      (bytes, attachment) => bytes + attachment.size_bytes,
      this.#pendingAttachments.reduce((bytes, attachment) => bytes + attachment.size, 0),
    );
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
    this.#scheduleComposerDraftPersistence();
    if (!composing) this.requestUpdate();
  };

  #applyQuickReply(prompt: string): void {
    if (this.#requestPending || prompt === "") return;
    const current = this.#composerDraft.trimEnd();
    this.#composerDraft = current === "" ? prompt : `${current}\n${prompt}`;
    this.#composerCursor = this.#composerDraft.length;
    this.#completionSelected = 0;
    this.#completionDismissed = false;
    this.#scheduleComposerDraftPersistence();
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
    const layout = composerTextareaLayout(
      textarea.scrollHeight,
      textarea.value.length > 0,
    );
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
    this.#scheduleComposerDraftPersistence();
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
    this.#scheduleComposerDraftPersistence();
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
      this.#sessionPathsRevision += 1;
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
    this.#scheduleComposerDraftPersistence();
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
    const commands = this.#currentComposerCommands();
    this.#syncComposerCompletionEffect(commands);
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
    if (event.key === "Escape" && this.#queueEditId !== "") {
      event.preventDefault();
      this.#cancelQueueEdit();
      return;
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
      this.#store.value?.resolveApprovalOptimistically(threadId, callId, decision);
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
