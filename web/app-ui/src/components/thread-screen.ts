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
  ProtocolAgentPersona,
  ProtocolAttachment,
  ProtocolModelInfo,
  ProtocolResolveApprovalRequest,
  ProtocolResolveQuestionRequest,
  ProtocolSubscriptionHealth,
  ProtocolThread,
  ProtocolUpdateThreadRequest,
  ProtocolUsageSummary,
} from "../services/protocol-client.js";
import type { ComposerDraft } from "../services/composer-drafts.js";
import {
  DEFAULT_CHAT_PREFERENCES,
  effectiveChatCollapsePreferences,
} from "../services/chat-preferences.js";
import type { ChatScrollBookmark } from "../services/resume-preferences.js";
import { rankComposerCompletionsOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  attentionOrUnreadIndicatorPresentation,
  sessionIndicatorPresentation,
} from "../state/session-indicator-model.js";
import type {
  CompactionState,
  QueuedPrompt,
  QueueRevisionTracker,
  ThreadChatItem,
  TurnState,
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
  toolDetailText,
  toolExecutionMetadata,
  type ToolPresentation,
} from "./tool-presentation.js";
import {
  runningAgentActivity,
  type AgentActivityPresentation,
} from "./agent-activity-model.js";
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
import {
  modelHealthPresentation,
  modelHealthPresentations,
} from "./model-health.js";
import {
  retainedHistoryScrollDelta,
  type HistoryMeasurementCorrection,
} from "./history-scroll-correction.js";
import {
  fontAwesomeIcon,
  type FontAwesomeIconName,
} from "./font-awesome-icon.js";
import {
  droppedQueueIds,
  effectiveQueueDropPlacement,
  queueControlState,
  queueFocusAfterDelete,
  queuePreview,
  shouldMaterializeAcceptedQueuedPrompt,
  type QueueDropPlacement,
} from "./queue-controls.js";
import "./turn-metadata.js";
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
import { threadNavigationTitle } from "./thread-title.js";
import {
  durableThreadTabCapacity,
  threadSwitcherRows,
  threadWorkingSet,
  type ThreadSwitcherFilter,
  type ThreadSwitcherEntry,
  type ThreadSwitcherRow,
} from "./thread-switcher-model.js";
import { subagentThreadIsReadOnly } from "./subagent-access.js";
import type {
  NewThreadSetupCancelEvent,
  NewThreadSetupSubmitEvent,
} from "./new-thread-setup.js";
import {
  CheckpointActionScope,
  checkpointBoundaryAfterTurn,
  checkpointBoundaryBeforeTurn,
  type CheckpointActionToken,
  type TurnCheckpointBoundary,
} from "./turn-checkpoint-actions.js";
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
  | { readonly kind: "optimistic-prompt" }
  | { readonly kind: "compacting" }
  | { readonly kind: "activity"; readonly presentation: AgentActivityPresentation }
  | { readonly kind: "edge-spacer"; readonly edge: "start" }
);

interface OptimisticPromptSubmission {
  readonly id: string;
  readonly threadId: string;
  readonly content: string;
  readonly attachments: readonly PendingAttachment[];
  readonly minimumTurn: number;
  disposition: "turn" | "queue";
  turn?: number;
  durablePrompt?: QueuedPrompt;
  readonly queueRevision: QueueRevisionTracker;
}

interface NewThreadRequestToken {
  readonly workspaceId: string;
  readonly sessionId: string;
  readonly initialThreadId: string;
  createdThreadId?: string;
}

const CHAT_START_SPACER_ID = "ephemeral:chat-start-spacer";
const CHAT_TAIL_EPSILON_PX = 2;
const CHAT_POSITION_SETTLE_MS = 140;
const CHAT_NATIVE_SCROLL_CORRECTION_GUARD_MS = 240;
const CHAT_TAIL_CONVERGENCE_FRAMES = 3;
const CHAT_SCROLL_INDICATOR_INSET_PX = 3;
const CHAT_SCROLL_INDICATOR_MIN_HEIGHT_PX = 32;
const CHAT_HISTORY_PREFETCH_ROOT_MARGIN = "500% 0px 0px 0px";
const CHAT_HISTORY_STATUS_DELAY_MS = 180;
const CHAT_HISTORY_RETRY_DELAY_MS = 1_500;
// Title generation is optional metadata and must not make thread creation
// appear hung when the naming provider is slow or unavailable.
const THREAD_TITLE_TIMEOUT_MS = 2_000;

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
  readonly selectionRanges: readonly Range[];
  readonly x: number;
  readonly y: number;
}

interface PendingMarkdownContextSelection {
  readonly source: HTMLElement;
  readonly text: string;
  readonly ranges: readonly Range[];
}

interface ChatDomAnchor {
  readonly id: string;
  readonly offset: number;
}

interface PendingHistoryPrepend {
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

const agentTurnLabels = (
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

const toolCallNeedsApproval = (
  item: AgentActivityItem,
): boolean =>
  item.kind === "tool"
  && item.status === "awaiting-approval";

type ActivityGroupStatus = "awaiting-approval" | "running" | "ok" | "mixed" | "error";

const activityGroupStatusLabel = (status: ActivityGroupStatus): string =>
  ({
    "awaiting-approval": "Approval needed",
    running: "Running",
    ok: "Completed",
    mixed: "Mixed results",
    error: "Failed",
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
  #optimisticPrompt: OptimisticPromptSubmission | undefined;
  #newThreadSetupOpen = false;
  #newThreadBusy = false;
  #newThreadError = "";
  #newThreadRequest: NewThreadRequestToken | undefined;
  #accessibleHistory = false;
  #historyLoading = false;
  #historyError = "";
  #historyStatusVisible = false;
  #historyGeneration = 0;
  #historyObserver: IntersectionObserver | undefined;
  #observedHistorySentinel: Element | undefined;
  #historyStatusTimer: ReturnType<typeof setTimeout> | undefined;
  #historyRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #activityRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #activityNowMs = Date.now();
  #pendingHistoryPrepend: PendingHistoryPrepend | undefined;
  #historyAnchorToRestore: ChatDomAnchor | undefined;
  #historyAnchorGeneration = 0;
  #parkedLayoutAnchor: ChatDomAnchor | undefined;
  #virtualizer = new Virtualizer<VirtualChatItem>({
    estimatedHeight: 120,
    overscanPx: 1_200,
    tailTolerancePx: 32,
  });
  #resizeObserver: ResizeObserver | undefined;
  readonly #observedVirtualRows = new Set<HTMLElement>();
  #viewportHeight = 0;
  #programmaticScrollFrame: number | undefined;
  #programmaticScrollTarget: number | undefined;
  #tailConvergenceFrame: number | undefined;
  #scrollRenderFrame: number | undefined;
  #scrollIndicatorFrame: number | undefined;
  #scrollIndicatorMetrics:
    | { readonly maxScrollTop: number; readonly thumbTravel: number }
    | undefined;
  #chatPositionTimer: ReturnType<typeof setTimeout> | undefined;
  #nativeScrollCorrectionBlockedUntil = 0;
  #followTailControlHeight = 0;
  #restoredScrollThreadId: string | undefined;
  #invalidScrollBookmarkThreadId: string | undefined;
  #markdownRequested = false;
  #toolDetailRequested = false;
  #toolDetailLoadFailed = false;
  #queueEditId = "";
  #queueEditRetainedAttachments: ProtocolAttachment[] = [];
  #queueEditReturnDraft: ComposerDraft | undefined;
  #queueBusy = "";
  #queueError = "";
  #queueStatus = "";
  #queueDragId = "";
  #queueDragImage: HTMLElement | undefined;
  #queueDropId = "";
  #queueDropPlacement: QueueDropPlacement = "before";
  #queueKeyboardDragId = "";
  #queueKeyboardOrder: readonly string[] = [];
  readonly #checkpointActions = new CheckpointActionScope();
  #checkpointErrorId = "";
  #checkpointError = "";
  #pendingAttachments: PendingAttachment[] = [];
  #attachmentPending = false;
  #attachmentGeneration = 0;
  #modes: readonly ProtocolAgentPersona[] = [];
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
  #pathsRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #sessionUsage: ProtocolUsageSummary | undefined;
  #usageRequestKey = "";
  #usageResolvedKey = "";
  #usagePending = false;
  #usageGeneration = 0;
  #copyFeedbackGeneration = 0;
  #markdownContextMenu: MarkdownContextMenu | undefined;
  #pendingMarkdownContextSelection: PendingMarkdownContextSelection | undefined;
  #markdownContextMenuStatus = "";
  #markdownContextMenuReturnFocus: HTMLElement | undefined;
  readonly #approvalSubmissions = new ApprovalSubmissionTracker();
  readonly #copyFeedback = new Map<string, ChatCopyResult>();
  readonly #messageDisclosure = new Map<string, boolean>();
  readonly #rawToolCalls = new Set<string>();
  readonly #toolDisclosure = new Map<string, boolean>();
  readonly #toolDetailLoading = new Set<string>();
  readonly #toolDetailErrors = new Map<string, string>();
  readonly #questionWizards = new Map<string, QuestionWizardState>();
  readonly #questionSubmissions = new Set<string>();
  #threadSwitcherOpen = false;
  #threadSwitcherQuery = "";
  #threadSwitcherFilter: ThreadSwitcherFilter = "all";
  #threadTabCapacity = 4;
  #recentThreadIds: readonly string[] = [];
  #workingThreadIds: readonly string[] = [];
  #threadTabResizeObserver: ResizeObserver | undefined;
  #observedThreadTabs: HTMLElement | undefined;
  #pendingThreadTabFocus = "";

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
      this.#newThreadRequest = undefined;
      this.#workspaceProvider.setValue({ workspaceId: this.workspaceId });
      this.#optionCatalogKey = "";
      this.#modes = [];
      this.#observedSubscriptionUsageCursor = 0;
    }
    if (changed.has("sessionId")) {
      this.#newThreadRequest = undefined;
      this.#threadSwitcherOpen = false;
      this.#threadSwitcherQuery = "";
      this.#threadSwitcherFilter = "all";
      this.#recentThreadIds = [];
      this.#workingThreadIds = [];
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
      this.#clearMentionPathsRetry();
      this.#completionSelected = 0;
      this.#completionDismissed = false;
      this.#queueEditId = "";
      this.#queueEditRetainedAttachments = [];
      this.#queueEditReturnDraft = undefined;
      this.#queueBusy = "";
      this.#queueError = "";
      this.#queueStatus = "";
      this.#queueDragId = "";
      this.#queueDropId = "";
      this.#queueDropPlacement = "before";
      this.#queueKeyboardDragId = "";
      this.#queueKeyboardOrder = [];
      this.#checkpointActions.reset();
      this.#checkpointErrorId = "";
      this.#checkpointError = "";
      this.#usageGeneration += 1;
      this.#sessionUsage = undefined;
      this.#usageRequestKey = "";
      this.#usageResolvedKey = "";
      this.#usagePending = false;
    }
    if (changed.has("threadId")) {
      const newThreadRequest = this.#newThreadRequest;
      if (
        newThreadRequest !== undefined
        && this.threadId !== newThreadRequest.initialThreadId
        && this.threadId !== newThreadRequest.createdThreadId
      ) {
        this.#newThreadRequest = undefined;
      }
      this.#checkpointActions.reset();
      this.#checkpointErrorId = "";
      this.#checkpointError = "";
      if (this.threadId !== "") {
        this.#services.value?.setThreadTabClosed(this.threadId, false);
        this.#recentThreadIds = [
          this.threadId,
          ...this.#recentThreadIds.filter((id) => id !== this.threadId),
        ].slice(0, 32);
      }
      this.#threadSwitcherOpen = false;
      this.#observedSubscriptionUsageCursor = 0;
      this.#turnRequestGeneration += 1;
      this.#attachmentGeneration += 1;
      this.#threadInteractionGeneration += 1;
      this.#historyGeneration += 1;
      this.#historyLoading = false;
      this.#historyError = "";
      this.#historyStatusVisible = false;
      this.#disconnectHistoryObserver();
      this.#clearHistoryStatusTimer();
      this.#clearHistoryRetryTimer();
      this.#pendingHistoryPrepend = undefined;
      this.#historyAnchorToRestore = undefined;
      this.#cancelHistoryAnchorCorrection();
      this.#parkedLayoutAnchor = undefined;
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
      this.#nativeScrollCorrectionBlockedUntil = 0;
      this.#scrollIndicatorMetrics = undefined;
      this.#virtualizer = new Virtualizer<VirtualChatItem>({
        estimatedHeight: 120,
        overscanPx: 1_200,
        tailTolerancePx: 32,
        mode: this.#accessibleHistory ? "accessible" : "virtual",
      });
      this.#viewportHeight = 0;
      this.#followTailControlHeight = 0;
      this.#restoredScrollThreadId = undefined;
      this.#invalidScrollBookmarkThreadId = undefined;
      this.#threadProvider.setValue({ threadId: this.threadId });
      this.#attachmentPending = false;
      this.#copyFeedbackGeneration += 1;
      this.#copyFeedback.clear();
      this.#markdownContextMenu = undefined;
      this.#pendingMarkdownContextSelection = undefined;
      this.#markdownContextMenuStatus = "";
      this.#markdownContextMenuReturnFocus = undefined;
      this.#messageDisclosure.clear();
      this.#rawToolCalls.clear();
      this.#toolDisclosure.clear();
      this.#toolDetailLoading.clear();
      this.#toolDetailErrors.clear();
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
      this.#queueStatus = "";
      this.#queueDragId = "";
      this.#queueDropId = "";
      this.#queueDropPlacement = "before";
      this.#queueKeyboardDragId = "";
      this.#queueKeyboardOrder = [];
      this.#pendingStartTurn = undefined;
      this.#cancelRequestedTurn = undefined;
      this.#messageRequest = undefined;
      this.#clearOptimisticPrompt();
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
  }

  protected override updated(): void {
    this.#syncActivityRefresh();
    if (
      this.threadId !== ""
      && (globalThis.document?.visibilityState ?? "visible") === "visible"
      && (globalThis.document?.hasFocus() ?? true)
    ) this.#store.value?.markThreadRead(this.threadId);
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
    this.#observeThreadWorkingSet();
    if (this.#pendingThreadTabFocus !== "") {
      const threadId = this.#pendingThreadTabFocus;
      const tab = [...this.querySelectorAll<HTMLButtonElement>("[data-thread-tab-id]")]
        .find((candidate) => candidate.dataset["threadTabId"] === threadId);
      if (tab !== undefined) {
        this.#pendingThreadTabFocus = "";
        tab.focus();
      }
    }
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
      this.#disconnectHistoryObserver();
      this.#followTailControlHeight = 0;
      this.#cancelScheduledScrollIndicator();
      this.#scrollIndicatorMetrics = undefined;
      return;
    }
    this.#followTailControlHeight = viewport
      .querySelector<HTMLElement>(".follow-tail")
      ?.offsetHeight ?? 0;
    this.#restoreHistoryPrependAnchor(viewport);
    this.#syncHistoryObserver(viewport);
    if (viewport.clientHeight !== this.#viewportHeight) {
      const before = this.#virtualizer.window();
      this.#viewportHeight = viewport.clientHeight;
      if (before.followingTail) {
        this.#virtualizer.resizeViewport(viewport.clientHeight);
        this.#setChatScrollTop(viewport, this.#transcriptTailScrollTop(viewport));
      } else {
        this.#virtualizer.setViewport(
          viewport.scrollTop,
          viewport.clientHeight,
          { userInitiated: true, atTail: false },
        );
      }
      this.#refreshChatScrollIndicator(viewport);
      if (!sameVirtualRenderWindow(before, this.#virtualizer.window())) {
        this.#scheduleScrollRender();
      }
    }
    const virtualWindow = this.#virtualizer.window();
    if (virtualWindow.followingTail) {
      this.#setChatScrollTop(viewport, this.#transcriptTailScrollTop(viewport));
    } else {
      // While reading history, the browser's native scrolling is the source
      // of truth. Synchronize the model without writing scrollTop back.
      this.#virtualizer.setViewport(
        viewport.scrollTop,
        viewport.clientHeight,
        { userInitiated: true, atTail: false },
      );
    }
    this.#refreshChatScrollIndicator(viewport);
    if (typeof ResizeObserver === "undefined") return;
    this.#resizeObserver ??= new ResizeObserver((entries) => {
      const activeViewport = this.querySelector<HTMLElement>(".chat-stream");
      if (activeViewport === null) return;
      const before = this.#virtualizer.window();
      const followingTail = before.followingTail;
      const layoutAnchor = followingTail ? undefined : this.#parkedLayoutAnchor;
      const nativeScrollActive = !followingTail
        && globalThis.performance.now() < this.#nativeScrollCorrectionBlockedUntil;
      let measured = false;
      let scrollCorrected = false;
      const measurementCorrections: HistoryMeasurementCorrection[] = [];
      for (const entry of entries) {
        const element = entry.target as HTMLElement;
        const id = element.dataset["virtualId"];
        if (id === undefined || entry.contentRect.height <= 0) continue;
        measured = true;
        try {
          const previouslyMeasured = this.#virtualizer.hasMeasurement(id);
          const correction = this.#virtualizer.measure(id, entry.contentRect.height);
          if (correction.delta !== 0) {
            scrollCorrected = true;
            measurementCorrections.push({
              id,
              previouslyMeasured,
              delta: correction.delta,
            });
          }
        } catch {
          // A row may have unmounted between delivery and measurement.
        }
      }
      const retainedHistoryDelta = retainedHistoryScrollDelta(measurementCorrections);
      let expectedScrollTop: number | undefined;
      if (!followingTail && measured && nativeScrollActive) {
        // Native wheel, touch, keyboard, and scrollbar movement is
        // authoritative while momentum is active. Re-anchor the model to the
        // browser plus only genuine late changes to already measured turn
        // rows. First-measure estimate calibration in the same observer batch
        // must not hitchhike into the scroll correction.
        this.#virtualizer.setViewport(Math.max(
          0,
          activeViewport.scrollTop + retainedHistoryDelta,
        ), activeViewport.clientHeight, {
          userInitiated: true,
          atTail: false,
        });
        if (retainedHistoryDelta !== 0) {
          expectedScrollTop = this.#virtualizer.window().scrollTop;
        }
      } else if (!followingTail && retainedHistoryDelta !== 0) {
        // Complete turn rows begin with estimates and can lay out again when
        // Markdown or attachments mount. Preserve the current virtual anchor
        // for changes above it.
        expectedScrollTop = this.#virtualizer.window().scrollTop;
      } else if (!followingTail && layoutAnchor === undefined && scrollCorrected) {
        // If a nested DOM anchor is temporarily unavailable after scrolling
        // has settled, the virtualizer's stable row anchor is the fallback.
        // This preserves late Markdown and attachment layout without writing
        // against active native momentum.
        expectedScrollTop = this.#virtualizer.window().scrollTop;
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
      } else if (
        !nativeScrollActive
        &&
        layoutAnchor !== undefined
        && this.#applyChatDomAnchor(activeViewport, layoutAnchor, false)
      ) {
        // The nested DOM anchor is more precise than a virtual-row anchor
        // when Markdown or attachments above it finish layout asynchronously.
      } else if (expectedScrollTop !== undefined) {
        this.#setChatScrollTop(activeViewport, expectedScrollTop);
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
    document.addEventListener("pointerup", this.#restoreMarkdownContextMenuSelectionFromPointer, true);
    document.addEventListener("keydown", this.#dismissMarkdownContextMenuFromKeyboard, true);
    document.addEventListener("pointerdown", this.#dismissThreadSwitcherFromPointer, true);
    document.addEventListener("scroll", this.#dismissMarkdownContextMenu, true);
    globalThis.addEventListener("resize", this.#dismissMarkdownContextMenu);
    globalThis.addEventListener("pagehide", this.#persistComposerDraftFromPageHide);
  }

  override disconnectedCallback(): void {
    this.#persistComposerDraftNow();
    this.#clearActivityRefresh();
    document.removeEventListener("pointerdown", this.#dismissMarkdownContextMenuFromPointer, true);
    document.removeEventListener("pointerup", this.#restoreMarkdownContextMenuSelectionFromPointer, true);
    document.removeEventListener("keydown", this.#dismissMarkdownContextMenuFromKeyboard, true);
    document.removeEventListener("pointerdown", this.#dismissThreadSwitcherFromPointer, true);
    document.removeEventListener("scroll", this.#dismissMarkdownContextMenu, true);
    globalThis.removeEventListener("resize", this.#dismissMarkdownContextMenu);
    globalThis.removeEventListener("pagehide", this.#persistComposerDraftFromPageHide);
    this.#resizeObserver?.disconnect();
    this.#resizeObserver = undefined;
    this.#threadTabResizeObserver?.disconnect();
    this.#threadTabResizeObserver = undefined;
    this.#observedThreadTabs = undefined;
    this.#observedVirtualRows.clear();
    this.#cancelProgrammaticScrollWindow();
    this.#cancelTailConvergence();
    this.#cancelScheduledScrollRender();
    this.#cancelScheduledScrollIndicator();
    this.#cancelScheduledChatPosition();
    this.#disconnectHistoryObserver();
    this.#clearHistoryStatusTimer();
    this.#clearHistoryRetryTimer();
    this.#cancelHistoryAnchorCorrection();
    this.#parkedLayoutAnchor = undefined;
    this.#scrollIndicatorMetrics = undefined;
    this.#copyFeedbackGeneration += 1;
    this.#usageGeneration += 1;
    this.#turnRequestGeneration += 1;
    this.#attachmentGeneration += 1;
    this.#threadInteractionGeneration += 1;
    this.#historyGeneration += 1;
    this.#historyLoading = false;
    this.#historyStatusVisible = false;
    this.#pendingHistoryPrepend = undefined;
    this.#newThreadRequest = undefined;
    this.#newThreadBusy = false;
    this.#checkpointActions.reset();
    this.#queueBusy = "";
    this.#requestPending = false;
    this.#attachmentPending = false;
    this.#messageRequest = undefined;
    this.#clearOptimisticPrompt();
    this.#clearMentionPathsRetry();
    this.#clearQueueDragImage();
    this.#markdownContextMenu = undefined;
    this.#pendingMarkdownContextSelection = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
    super.disconnectedCallback();
  }

  #syncActivityRefresh(): void {
    const running = this.threadId !== ""
      && (this.#store.value?.threadView(this.threadId)?.turnRunning ?? false);
    if (!running) {
      this.#clearActivityRefresh();
      return;
    }
    this.#activityNowMs = Date.now();
    if (this.#activityRefreshTimer !== undefined) return;
    this.#activityRefreshTimer = globalThis.setInterval(() => {
      const stillRunning = this.isConnected
        && this.threadId !== ""
        && (this.#store.value?.threadView(this.threadId)?.turnRunning ?? false);
      if (!stillRunning) {
        this.#clearActivityRefresh();
        return;
      }
      this.#activityNowMs = Date.now();
      this.requestUpdate();
    }, 1_000);
  }

  #clearActivityRefresh(): void {
    if (this.#activityRefreshTimer === undefined) return;
    globalThis.clearInterval(this.#activityRefreshTimer);
    this.#activityRefreshTimer = undefined;
  }

  #selectThreadWithKeyboard(
    event: KeyboardEvent,
    currentIndex: number,
    threads: readonly { readonly id: string }[],
    newThreadSetupOpen = this.#newThreadSetupOpen,
  ): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "Delete" && currentIndex < threads.length) {
      const thread = threads[currentIndex];
      if (thread === undefined) return;
      event.preventDefault();
      this.#closeThreadTabById(thread.id);
      return;
    }
    const tabCount = threads.length + (newThreadSetupOpen ? 1 : 0);
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

  #observeThreadWorkingSet(): void {
    const tabs = this.querySelector<HTMLElement>(".thread-tabs");
    if (tabs === null || tabs === this.#observedThreadTabs) return;
    this.#threadTabResizeObserver?.disconnect();
    this.#observedThreadTabs = tabs;
    const synchronize = (): void => {
      if (!tabs.isConnected || tabs.clientWidth <= 0) return;
      const style = getComputedStyle(tabs);
      const tabWidth = Number.parseFloat(
        style.getPropertyValue("--thread-tab-width"),
      ) || 145;
      const gap = Number.parseFloat(style.columnGap) || 6;
      const capacity = Math.max(
        1,
        Math.floor((tabs.clientWidth + gap) / (tabWidth + gap)),
      );
      if (capacity === this.#threadTabCapacity) return;
      this.#threadTabCapacity = capacity;
      this.requestUpdate();
    };
    synchronize();
    if (typeof ResizeObserver === "undefined") return;
    this.#threadTabResizeObserver ??= new ResizeObserver(synchronize);
    this.#threadTabResizeObserver.observe(tabs);
  }

  override render() {
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) {
      return html`<div class="screen-empty" role="status">Loading thread…</div>`;
    }
    const sessionThreads = store.threadsForSession(this.sessionId);
    const resumePreferences = readSignal(services.resumePreferences);
    const closedThreadTabs = new Set(resumePreferences.closedThreadTabs);
    const pinnedThreadTabs = new Set(resumePreferences.pinnedThreadTabs);
    const closedThreads = sessionThreads.filter((candidate) =>
      closedThreadTabs.has(candidate.id));
    const threads = sessionThreads.filter((candidate) =>
      candidate.id === this.threadId
      || !closedThreadTabs.has(candidate.id));
    const newThreadSetupOpen = this.#newThreadSetupOpen
      || (this.threadId === "" && sessionThreads.length > 0 && threads.length === 0);
    const thread = this.threadId === "" ? undefined : store.thread(this.threadId);
    const subagentReadOnly = thread === undefined
      ? false
      : subagentThreadIsReadOnly(thread, this.#modes);
    const view = this.threadId === "" ? undefined : store.threadView(this.threadId);
    this.#reconcileOptimisticPrompt(view?.items ?? [], view?.queue ?? []);
    const displayedQueue = this.#queueWithOptimisticPrompt(view?.queue ?? []);
    const turnLabels = agentTurnLabels(
      view?.turnModels,
      view?.turnThinkingLevels,
    );
    this.#reconcileTurnAcknowledgements(view?.items ?? [], view?.turnRunning ?? false);
    const session = readSignal(store.sessions).find(
      (session) => session.id === this.sessionId,
    );
    const sessionTitle = session?.title ?? "";
    const initialThreadId = sessionThreads[0]?.id;
    const labelForThread = (candidate: typeof sessionThreads[number]): string =>
      threadNavigationTitle({
        thread: candidate,
        sessionTitle,
        initialThreadId,
        modeDisplayName: this.#modes.find((mode) => mode.id === candidate.mode)?.display_name,
      });
    const workingThreadIds = threadWorkingSet(
      threads.map((candidate) => candidate.id),
      this.threadId,
      resumePreferences.pinnedThreadTabs,
      this.#recentThreadIds,
      durableThreadTabCapacity(this.#threadTabCapacity, newThreadSetupOpen),
      this.#workingThreadIds,
    );
    this.#workingThreadIds = workingThreadIds;
    const workingThreadIdSet = new Set(workingThreadIds);
    const sessionThreadById = new Map(sessionThreads.map((candidate) => [candidate.id, candidate]));
    const workingThreads = workingThreadIds
      .map((id) => sessionThreadById.get(id))
      .filter((candidate): candidate is ProtocolThread => candidate !== undefined);
    const selectedThreadIndex = workingThreads.findIndex(
      (candidate) => candidate.id === this.threadId,
    );
    const selectedTabIndex = newThreadSetupOpen
      ? workingThreads.length
      : selectedThreadIndex;
    const threadTabCount = workingThreads.length + (newThreadSetupOpen ? 1 : 0);
    const hiddenThreads = sessionThreads.filter(
      (candidate) => !workingThreadIdSet.has(candidate.id),
    );
    const overflowThreads = threads.filter(
      (candidate) => !workingThreadIdSet.has(candidate.id),
    );
    const hiddenThreadIndicator = attentionOrUnreadIndicatorPresentation(
      hiddenThreads.map((candidate) => store.threadIndicatorState(candidate.id)),
    );
    const hiddenThreadStatusLabel = hiddenThreadIndicator.tooltip;
    const switcherEntries: readonly ThreadSwitcherEntry[] = sessionThreads.map(
      (candidate) => {
        const indicatorState = store.threadIndicatorState(candidate.id);
        return {
        id: candidate.id,
        parentThreadId: candidate.parent_thread_id,
        title: labelForThread(candidate),
        detail: `${candidate.model} ${candidate.mode}`,
        closed: closedThreadTabs.has(candidate.id),
        pinned: pinnedThreadTabs.has(candidate.id),
        active: indicatorState.active || indicatorState.outcome === "running",
        needsAttention: indicatorState.attention !== "none"
          || indicatorState.outcome === "failed"
          || indicatorState.unread,
        };
      },
    );
    const pinnedSwitcherRows = threadSwitcherRows(
      switcherEntries.filter((entry) => !entry.closed && entry.pinned),
      this.#threadSwitcherQuery,
      this.#threadSwitcherFilter,
    );
    const openSwitcherRows = threadSwitcherRows(
      switcherEntries.filter((entry) => !entry.closed && !entry.pinned),
      this.#threadSwitcherQuery,
      this.#threadSwitcherFilter,
    );
    const closedSwitcherRows = threadSwitcherRows(
      switcherEntries.filter((entry) => entry.closed),
      this.#threadSwitcherQuery,
      this.#threadSwitcherFilter,
    );
    const serverOnline = readSignal(store.serverInfo)?.online;
    const models = this.#availableModels();
    const connectivityBlocked = serverOnline === false && models.length === 0;
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
      : models.find((model) => model.id === thread.model);
    const runningTurn = this.#latestRunningTurn(view?.items ?? []);
    const activeTurnSteerable = runningTurn !== undefined
      && view?.turnSteerable.get(runningTurn) === true;
    const steerPending = this.#requestPending && this.#messageRequest === undefined;
    const modelControls = modelOptionControls(selectedModel, thread?.model_options);
    const modelHealth = modelHealthPresentations(models, this.#subscriptionHealth);
    const selectedProviderId = thread?.model.split("/", 1)[0] ?? "";
    const selectedSubscription = this.#subscriptionHealth.find(
      (health) => health.provider_id === selectedProviderId,
    );
    const selectedModelHealth = selectedSubscription === undefined
      ? undefined
      : modelHealthPresentation(selectedSubscription);
    const subscriptionLoading = selectedModelHealth === undefined && (
      this.#optionCatalogKey === ""
      || (this.#services.value !== undefined
        && readSignal(this.#services.value.subscriptionHealth.loading))
    );
    const contextUsage = composerContextUsage(
      view?.lastUsage,
      selectedModel?.context_window,
      view?.compacting ?? false,
      thread?.model.startsWith("codex/") ?? false,
    );
    const sessionUsageText = formatSessionUsage(this.#sessionUsage);
    const leadingThreads = newThreadSetupOpen
      ? workingThreads
      : workingThreads.slice(0, -1);
    const trailingThread = newThreadSetupOpen ? undefined : workingThreads.at(-1);
    const renderThreadTab = (
      candidate: (typeof threads)[number],
      index: number,
    ) => {
      const label = labelForThread(candidate);
      const indicator = sessionIndicatorPresentation(
        store.threadIndicatorState(candidate.id),
      );
      const statusLabel = indicator.tooltip
        || (indicator.kind === "busy" ? "Processing" : "");
      return html`
        <span class="thread-tab-item" role="presentation">
          <button
            class="thread-tab-main"
            type="button"
            role="tab"
            aria-keyshortcuts="Delete"
            aria-label=${statusLabel === "" ? label : `${label}, ${statusLabel}`}
            title=${label}
            data-thread-tab-id=${candidate.id}
            aria-selected=${!newThreadSetupOpen && candidate.id === this.threadId ? "true" : "false"}
            tabindex=${rovingTabIndex(index, selectedTabIndex, threadTabCount)}
            @keydown=${(event: KeyboardEvent) =>
              this.#selectThreadWithKeyboard(event, index, workingThreads, newThreadSetupOpen)}
            @click=${() => this.#selectThread(candidate.id)}
          >
            <span class="thread-tab-label"><span
              class=${`session-indicator thread-tab-indicator ${indicator.kind}`}
              title=${statusLabel}
              aria-hidden="true"
            >${indicator.icon === undefined
              ? nothing
              : fontAwesomeIcon(indicator.icon)}</span>${candidate.spawned === true
              ? fontAwesomeIcon("code-branch")
              : nothing}${pinnedThreadTabs.has(candidate.id)
              ? fontAwesomeIcon("thumbtack", { className: "thread-tab-pin" })
              : nothing}<span class="thread-tab-title">${label}</span></span>
            ${threadTodoProgress(candidate.todos) === ""
              ? nothing
              : html`<span class="thread-todo-progress">${threadTodoProgress(candidate.todos)}</span>`}
          </button>
          <span
            class="thread-tab-close"
            aria-hidden="true"
            title="Close thread tab"
            @click=${(event: MouseEvent) =>
              this.#closeThreadTab(event, candidate.id)}
          >${fontAwesomeIcon("xmark")}</span>
        </span>
      `;
    };
    const renderSwitcherRow = (
      row: ThreadSwitcherRow,
      removed: boolean,
    ) => {
      const candidate = sessionThreadById.get(row.entry.id);
      if (candidate === undefined) return nothing;
      const indicator = sessionIndicatorPresentation(
        store.threadIndicatorState(candidate.id),
      );
      const statusLabel = indicator.tooltip
        || (indicator.kind === "busy" ? "Processing" : "");
      const current = candidate.id === this.threadId;
      const kindLabel = candidate.spawned === true
        ? subagentThreadIsReadOnly(candidate, this.#modes)
          ? "Read-only subagent"
          : "Interactive subagent"
        : "Conversation";
      return html`
        <div
          class=${`thread-switcher-row${current ? " current" : ""}`}
          role="treeitem"
          aria-level=${row.depth + 1}
          aria-current=${current ? "page" : nothing}
          aria-label=${statusLabel === ""
            ? row.entry.title
            : `${row.entry.title}, ${statusLabel}`}
          title=${row.entry.title}
          data-thread-switcher-id=${candidate.id}
          style=${`--thread-depth: ${Math.min(row.depth, 8)}`}
          tabindex="-1"
          @click=${() => this.#activateThreadSwitcherRow(candidate.id, removed)}
          @keydown=${this.#threadSwitcherRowKeydown}
        >
          <span class="thread-switcher-branch" aria-hidden="true">${row.depth > 0
            ? fontAwesomeIcon("code-branch")
            : nothing}</span>
          <span
            class=${`session-indicator thread-tab-indicator ${indicator.kind}`}
            title=${statusLabel}
            aria-hidden="true"
          >${indicator.icon === undefined
            ? nothing
            : fontAwesomeIcon(indicator.icon)}</span>
          <span class="thread-switcher-copy">
            <strong>${row.entry.title}</strong>
            <small>${kindLabel} · ${candidate.model}${row.entry.pinned
              ? html`<span class="thread-switcher-pinned">Pinned</span>`
              : nothing}${current
              ? html`<span class="thread-switcher-current">Current</span>`
              : nothing}${removed
              ? html`<span class="thread-switcher-removed">Removed from bar</span>`
              : nothing}</small>
          </span>
          <span class="thread-switcher-row-actions">
            ${row.entry.active ? html`<button
              class="thread-switcher-row-action"
              type="button"
              aria-label=${`Cancel active turn in ${row.entry.title}`}
              title="Cancel active turn"
              @click=${(event: MouseEvent) => this.#cancelThreadFromSwitcher(event, candidate.id)}
            >${fontAwesomeIcon("square")}</button>` : nothing}
            ${removed ? nothing : html`<button
              class=${`thread-switcher-row-action${row.entry.pinned ? " active" : ""}`}
              type="button"
              aria-label=${row.entry.pinned
                ? `Unpin ${row.entry.title}`
                : `Pin ${row.entry.title}`}
              aria-pressed=${row.entry.pinned ? "true" : "false"}
              title=${row.entry.pinned ? "Unpin thread" : "Pin thread"}
              @click=${(event: MouseEvent) =>
                this.#setThreadTabPinned(event, candidate.id, !row.entry.pinned)}
            >${fontAwesomeIcon("thumbtack")}</button>`}
            <button
              class="thread-switcher-row-action"
              type="button"
              aria-label=${removed
                ? `Add ${row.entry.title} to the working bar`
                : `Remove ${row.entry.title} from the working bar`}
              title=${removed ? "Add to working bar" : "Remove from working bar"}
              @click=${(event: MouseEvent) => {
                event.stopPropagation();
                if (removed) this.#reopenClosedThread(candidate.id);
                else this.#closeThreadTab(event, candidate.id);
              }}
            >${fontAwesomeIcon(removed ? "rotate-left" : "xmark")}</button>
          </span>
        </div>
      `;
    };
    return html`
      <header class="thread-header thread-tab-header">
        <div class="thread-tabs" role="tablist" aria-label="Threads">
          ${repeat(
            leadingThreads,
            (candidate) => candidate.id,
            renderThreadTab,
          )}
          <span class="thread-tab-tail" role="presentation">
            ${trailingThread === undefined
              ? nothing
              : renderThreadTab(trailingThread, workingThreads.length - 1)}
            ${newThreadSetupOpen
              ? html`
                <button
                  type="button"
                  role="tab"
                  class="provisional-thread-tab"
                  aria-selected="true"
                  tabindex=${rovingTabIndex(workingThreads.length, selectedTabIndex, threadTabCount)}
                  @keydown=${(event: KeyboardEvent) =>
                    this.#selectThreadWithKeyboard(
                      event,
                      workingThreads.length,
                      workingThreads,
                      newThreadSetupOpen,
                    )}
                >
                  <span class="thread-tab-label">New Thread</span>
                </button>
              `
              : nothing}
          </span>
        </div>
        <button
          class="new-thread-tab"
          type="button"
          aria-label="New thread"
          title="New thread"
          ?disabled=${this.sessionId === "" || newThreadSetupOpen || this.#newThreadBusy}
          @click=${this.openNewThreadSetup}
        >${fontAwesomeIcon("plus")}</button>
        <div class="thread-switcher">
          <button
            class="thread-switcher-toggle"
            type="button"
            aria-label=${`Threads (${sessionThreads.length})${hiddenThreadStatusLabel === ""
              ? ""
              : `, ${hiddenThreadStatusLabel}`}`}
            title="Browse all threads"
            aria-haspopup="dialog"
            aria-expanded=${this.#threadSwitcherOpen ? "true" : "false"}
            aria-controls="thread-switcher-panel"
            @click=${this.#toggleThreadSwitcher}
          >
            ${fontAwesomeIcon("folder-tree")}
            <span class="thread-switcher-toggle-label">Threads</span>
            <span class="thread-switcher-total">${sessionThreads.length}</span>
            ${hiddenThreadIndicator.kind === "none"
              ? nothing
              : html`<span
                  class=${`session-indicator thread-tab-indicator thread-switcher-status ${hiddenThreadIndicator.kind}`}
                  title=${hiddenThreadStatusLabel}
                  aria-hidden="true"
                >${hiddenThreadIndicator.icon === undefined
                  ? nothing
                  : fontAwesomeIcon(hiddenThreadIndicator.icon)}</span>`}
          </button>
          ${this.#threadSwitcherOpen
            ? html`
                <div
                  id="thread-switcher-panel"
                  class="thread-switcher-panel"
                  role="dialog"
                  aria-label="Threads"
                  @keydown=${this.#threadSwitcherKeydown}
                >
                  <header class="thread-switcher-panel-header">
                    <label>
                      <span class="sr-only">Search threads</span>
                      ${fontAwesomeIcon("magnifying-glass")}
                      <input
                        class="thread-switcher-search"
                        type="search"
                        placeholder="Search threads…"
                        autocomplete="off"
                        .value=${this.#threadSwitcherQuery}
                        @input=${this.#threadSwitcherSearchChanged}
                      />
                    </label>
                    <div class="thread-switcher-filters" role="group" aria-label="Filter threads">
                      ${(["all", "running", "attention", "removed"] as const).map((filter) =>
                        html`<button
                          type="button"
                          data-thread-filter=${filter}
                          aria-pressed=${this.#threadSwitcherFilter === filter ? "true" : "false"}
                          @click=${this.#threadSwitcherFilterChanged}
                        >${filter === "all" ? "All" : filter === "running"
                          ? "Running" : filter === "attention" ? "Needs attention" : "Removed"}</button>`)}
                    </div>
                    <small>${workingThreads.length} visible · ${overflowThreads.length} overflow · ${closedThreads.length} removed</small>
                  </header>
                  <div class="thread-switcher-sections">
                    <section aria-labelledby="pinned-threads-heading">
                      <h3 id="pinned-threads-heading">Pinned</h3>
                      <div class="thread-switcher-tree" role="tree" aria-label="Pinned threads">
                        ${pinnedSwitcherRows.length === 0
                          ? html`<p class="thread-switcher-empty">No matching pinned threads.</p>`
                          : repeat(
                              pinnedSwitcherRows,
                              (row) => row.entry.id,
                              (row) => renderSwitcherRow(row, false),
                            )}
                      </div>
                    </section>
                    <section aria-labelledby="open-threads-heading">
                      <h3 id="open-threads-heading">Open threads</h3>
                      <div class="thread-switcher-tree" role="tree" aria-label="Open threads">
                        ${openSwitcherRows.length === 0
                          ? html`<p class="thread-switcher-empty">No matching open threads.</p>`
                          : repeat(
                              openSwitcherRows,
                              (row) => row.entry.id,
                              (row) => renderSwitcherRow(row, false),
                            )}
                      </div>
                    </section>
                    <section aria-labelledby="removed-threads-heading">
                      <h3 id="removed-threads-heading">Removed from bar</h3>
                      <div class="thread-switcher-tree" role="tree" aria-label="Removed threads">
                        ${closedSwitcherRows.length === 0
                          ? html`<p class="thread-switcher-empty">${this.#threadSwitcherQuery === ""
                              ? "No threads have been removed."
                              : "No matching removed threads."}</p>`
                          : repeat(
                              closedSwitcherRows,
                              (row) => row.entry.id,
                              (row) => renderSwitcherRow(row, true),
                            )}
                      </div>
                    </section>
                  </div>
                </div>
              `
            : nothing}
        </div>
      </header>

      ${newThreadSetupOpen
        ? html`
            <trouve-new-thread-setup
              session-title=${sessionTitle}
              .busy=${this.#newThreadBusy}
              .errorMessage=${this.#newThreadError}
              .catalogModes=${this.#modes}
              .catalogModels=${models}
              .subscriptionHealth=${this.#subscriptionHealth}
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
        turnLabels,
        view?.turnModels ?? new Map<number, string>(),
        view?.turnStartedAt ?? new Map<number, string>(),
        view?.turnDurationMs ?? new Map<number, number>(),
        turnControls.activityLabel
          ?? (view?.turnPhase === "connecting_tools" ? "Connecting tools…" : undefined),
        view?.hasOlder ?? false,
      )}

      ${subagentReadOnly
        ? html`<footer class="subagent-readonly" role="note">
            ${fontAwesomeIcon("users")}
            <span><strong>Read-only subagent</strong> Exploration, audit, and review modes do not accept follow-up prompts.</span>
          </footer>`
        : html`
      ${this.#renderQueue(
        displayedQueue,
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
          <div class="composer-entry-actions">
          ${!queueEditing
            && activeTurnSteerable
            && view?.turnRunning === true
            && this.#cancelRequestedTurn === undefined
            ? html`<wa-button
                class="composer-steer"
                type="button"
                title="Steer active turn"
                aria-label="Steer active turn"
                ?disabled=${this.#requestPending
                  || this.#attachmentPending
                  || !hasComposerContent
                  || connectivityBlocked}
                @click=${this.#steerTurn}
              >${fontAwesomeIcon(steerPending ? "spinner" : "route", {
                spin: steerPending,
              })}</wa-button>`
            : nothing}
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
        </div>
        <div class="composer-controls" aria-label="Composer options">
          ${thread === undefined
            ? nothing
            : html`
                <label class="composer-option mode-option">
                  <span>Persona</span>
                  <select
                    aria-label="Persona"
                    .value=${thread.mode}
                    ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                    @change=${(event: Event) => this.#updateThreadSetting(
                      { mode: (event.currentTarget as HTMLSelectElement).value },
                      "Persona could not be changed.",
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
                    .models=${models}
                    .health=${modelHealth}
                    .disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                    @trouve-model-picked=${(event: CustomEvent<{ readonly modelId: string }>) => this.#updateThreadSetting(
                      { model: event.detail.modelId, model_options: {} },
                      "Model could not be changed.",
                    )}
                  ></trouve-model-picker>
                </div>
                <div class="composer-option subscription-option">
                  <span>Subscription</span>
                  ${selectedModelHealth === undefined
                    ? html`<div
                        class=${`model-health-pill ${subscriptionLoading ? "loading" : "unavailable"}`}
                        role="status"
                        aria-busy=${subscriptionLoading ? "true" : "false"}
                        aria-label=${subscriptionLoading
                          ? "Loading subscription status"
                          : "Subscription status is unavailable"}
                      >
                        <span class="model-health-placeholder-dot" aria-hidden="true"></span>
                        <span>${subscriptionLoading ? "Loading…" : "Not available"}</span>
                      </div>`
                    : html`
                      <div
                        class=${`model-health-pill tone-${selectedModelHealth.tone}`}
                        tabindex="0"
                        title=${selectedModelHealth.detail}
                        aria-label=${`Subscription status: ${selectedModelHealth.summary}. ${selectedModelHealth.detail}`}
                      >
                        <span class=${`model-health-dot tone-${selectedModelHealth.tone}`} aria-hidden="true"></span>
                        <span>${selectedModelHealth.summary}</span>
                      </div>`}
                </div>
                <label class="composer-option thinking-option">
                  <span>Thinking</span>
                  <select
                    aria-label="Thinking level"
                    .value=${modelControls.thinking?.selected ?? ""}
                    ?disabled=${turnControls.effectiveTurnRunning || this.#threadSettingsPending || connectivityBlocked}
                    @change=${(event: Event) => {
                      const thinking = modelControls.thinking;
                      if (thinking === undefined) return;
                      void this.#updateThreadModelOption(
                        thinking.key,
                        (event.currentTarget as HTMLSelectElement).value,
                        "Thinking level could not be changed.",
                      );
                    }}
                  >
                    <option value="">Model default</option>
                    ${(modelControls.thinking?.values ?? []).map(
                      (value) => html`<option
                        value=${value}
                        .selected=${value === modelControls.thinking?.selected}
                      >${modelOptionLabel(value)}</option>`,
                    )}
                  </select>
                </label>
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
                ${this.#renderContextUsage(contextUsage)}
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
            ? html`<p class="composer-completion-status completion-error" role="status">File suggestions are unavailable. trouve will retry automatically.</p>`
            : completion.matches.length === 0
              ? html`<p class="composer-completion-status" role="status">${completion.emptyMessage}</p>`
              : nothing}
      </div>
    `;
  }

  #renderOptimisticPrompt(optimistic: OptimisticPromptSubmission) {
    this.#ensureMarkdown();
    return html`
      <article
        class="message turn-card assistant-message agent-turn-card conversation-turn turn-running optimistic-turn"
        aria-label="Pending turn"
        aria-busy="true"
      >
        <header class="message-header agent-header turn-header">
          <div class="message-disclosure optimistic-turn-header">
            <strong>Pending turn</strong>
            <small class="agent-model-label">Awaiting durable acceptance</small>
            <span class="agent-header-spacer"></span>
            <small class="turn-metadata">Sending…</small>
          </div>
        </header>
        <div class="message-body turn-body-stream agent-body-stream turn-timeline">
          <section class="turn-rail-node turn-prompt-node user-message">
            <span class="turn-rail-marker prompt" aria-hidden="true">
              ${fontAwesomeIcon("user")}
            </span>
            <header class="turn-node-header"><strong>Prompt</strong></header>
            <div class="turn-node-body user-body-stream">
              ${optimistic.content === ""
                ? nothing
                : html`<trouve-markdown-view
                    .content=${optimistic.content}
                  ></trouve-markdown-view>`}
              ${optimistic.attachments.length === 0
                ? nothing
                : html`<ul class="attachment-list" aria-label="Pending message attachments">
                    ${optimistic.attachments.map((attachment) => {
                      const preview = pendingAttachmentPreviewUrl(attachment);
                      return html`<li class=${preview === undefined
                        ? "file-attachment"
                        : "image-attachment"}>
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
                      </li>`;
                    })}
                  </ul>`}
            </div>
          </section>
        </div>
      </article>
    `;
  }

  #renderChat(
    items: readonly ThreadChatItem[],
    turnRunning: boolean,
    effectiveTurnRunning: boolean,
    thinking: boolean,
    compacting: boolean,
    turnLabels: ReadonlyMap<number, string>,
    turnModels: ReadonlyMap<number, string>,
    turnStartedAt: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    activityOverride: string | undefined,
    hasOlder: boolean,
  ) {
    this.#syncQuestionWizards(items);
    const presentation = indexChatPresentation(items);
    const layout = buildChatLayout(items);
    let activeTurn: number | undefined;
    for (const [turn, state] of presentation.turnStates) {
      if (
        (state.kind === "waiting-for-capacity" || state.kind === "running")
        && (activeTurn === undefined || turn > activeTurn)
      ) {
        activeTurn = turn;
      }
    }
    const activityPresentation = activityOverride === undefined
      ? runningAgentActivity({
          items,
          turnRunning,
          thinking,
          compacting,
          turnModels,
          turnStartedAt,
          nowMs: this.#activityNowMs,
        })
      : {
          label: activityOverride,
          detail: "",
          announcementLabel: activityOverride,
        };
    let nestedActivityUnitId: string | undefined;
    if (activityPresentation !== undefined && activeTurn !== undefined) {
      for (let index = layout.units.length - 1; index >= 0; index -= 1) {
        const unit = layout.units[index];
        if (
          unit?.kind === "turn"
          && unit.turn === activeTurn
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
        Math.max(190, Math.min(760, 110 + unit.items.length * 90)),
      heavyweight: unit.items.some(
        (item) => item.kind === "tool" || item.kind === "questions",
      ),
    }));
    const optimistic = this.#optimisticPrompt;
    if (
      optimistic !== undefined
      && optimistic.threadId === this.threadId
      && optimistic.disposition === "turn"
    ) {
      virtualItems.push({
        id: optimistic.id,
        kind: "optimistic-prompt",
        estimatedHeight: Math.max(170, 120 + optimistic.attachments.length * 52),
      });
    }
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
    if (activityPresentation !== undefined && nestedActivityUnitId === undefined) {
      virtualItems.push({
        id: "ephemeral:activity",
        kind: "activity",
        presentation: activityPresentation,
        estimatedHeight: activityPresentation.detail === "" ? 32 : 48,
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
          @scroll=${this.#chatScrolled}
          @scrollend=${this.#chatScrollEnded}
        >
          ${virtualItems.length === 0
            ? nothing
            : html`<div
                class="chat-virtual-canvas"
                style=${`height:${window.totalHeight}px`}
              >${hasOlder
                ? html`<span class="chat-history-sentinel" aria-hidden="true"></span>`
                : nothing}${repeat(window.items, ({ item }) => item.id, ({ item, start }) => {
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
              if (item.kind === "optimistic-prompt") {
                const pending = this.#optimisticPrompt;
                return pending === undefined
                  ? nothing
                  : html`<div data-virtual-id=${item.id} style=${style}>
                      ${this.#renderOptimisticPrompt(pending)}
                    </div>`;
              }
              if (item.kind === "activity") {
                return html`<div data-virtual-id=${item.id} style=${style}>
                  ${this.#renderActivityRow(item.presentation)}
                </div>`;
              }
              const unit = layout.units[item.unitIndex];
              return unit === undefined
                ? nothing
                : html`<div data-virtual-id=${item.id} style=${style}>${this.#renderUnit(
                    unit,
                    turnLabels,
                    turnModels,
                    turnDurationMs,
                    presentation,
                    unit.id === nestedActivityUnitId ? activityPresentation : undefined,
                    effectiveTurnRunning,
                    item.unitIndex === layout.units.length - 1,
                  )}</div>`;
              })}</div>`}
          ${!window.followingTail && virtualItems.length > 0
            ? html`<button class="follow-tail" type="button" @click=${this.#followTail}>Jump to latest</button>`
            : nothing}
        </div>
        ${this.#historyStatusVisible || this.#historyError !== ""
          ? html`<div class="chat-history-status" role="status">
              ${this.#historyError === ""
                ? "Loading earlier messages…"
                : this.#historyError}
            </div>`
          : nothing}
        <span class="chat-scroll-indicator" aria-hidden="true"></span>
      </div>
    `;
  }

  #renderUnit(
    unit: ChatRenderUnit,
    turnLabels: ReadonlyMap<number, string>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityPresentation: AgentActivityPresentation | undefined,
    checkpointRestoreDisabled: boolean,
    finalUnit: boolean,
  ) {
    const trailingBoundary = checkpointBoundaryAfterTurn(unit.turn, presentation.turnStates);
    return html`
      ${unit.divider
        ? this.#renderTurnRule(
            unit.turn,
            presentation,
            checkpointRestoreDisabled,
          )
        : nothing}
      ${this.#renderTurnCard(
        unit,
        turnLabels,
        turnModels,
        turnDurationMs,
        presentation,
        activityPresentation,
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
    const checkpointAction = this.#checkpointActions.action;
    const busy = checkpointAction !== "";
    const restoreBusy = checkpointAction === `restore:${boundary.checkpointId}`;
    const forkBusy = checkpointAction === `fork:${boundary.checkpointId}`;
    const error = this.#checkpointErrorId === boundary.checkpointId
      ? this.#checkpointError
      : "";
    const restoreLabel = turnRunning
      ? `Restore after turn ${boundary.turn} once the current turn finishes`
      : `Restore files to the checkpoint after turn ${boundary.turn}`;
    return html`
      <div class="turn-rule with-checkpoint-actions">
        <span
          class="turn-rule-separator"
          role="separator"
          aria-label=${`Turn ${boundary.turn} checkpoint`}
        ></span>
        <span
          class="turn-rule-actions"
          role="group"
          aria-label=${`Actions after turn ${boundary.turn}`}
        >
          ${error === ""
            ? nothing
            : html`<span class="turn-rule-action-error" role="alert">${error}</span>`}
          <button
            type="button"
            aria-label=${restoreLabel}
            title=${restoreLabel}
            ?disabled=${busy || turnRunning}
            @click=${() => void this.#restoreTurnCheckpoint(boundary)}
          >${fontAwesomeIcon(restoreBusy ? "spinner" : "rotate-left", {
            spin: restoreBusy,
          })}</button>
          <button
            type="button"
            aria-label=${`Fork a new session from the checkpoint after turn ${boundary.turn}`}
            title=${`Fork a new session from the checkpoint after turn ${boundary.turn}`}
            ?disabled=${busy}
            @click=${() => void this.#forkTurnCheckpoint(boundary)}
          >${fontAwesomeIcon(forkBusy ? "spinner" : "code-branch", {
            spin: forkBusy,
          })}</button>
        </span>
      </div>
    `;
  }

  async #restoreTurnCheckpoint(boundary: TurnCheckpointBoundary): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const token = this.#checkpointActions.begin(`restore:${boundary.checkpointId}`);
    if (token === undefined) return;
    const sessionId = this.sessionId;
    const threadId = this.threadId;
    this.#checkpointErrorId = "";
    this.#checkpointError = "";
    this.requestUpdate();
    try {
      await services.protocol.restoreCheckpoint(boundary.checkpointId);
      if (!this.#isCurrentCheckpointAction(sessionId, threadId, token)) return;
      globalThis.dispatchEvent(new CustomEvent("trouve-checkpoint-restored", {
        detail: { sessionId },
      }));
    } catch {
      if (!this.#isCurrentCheckpointAction(sessionId, threadId, token)) return;
      this.#checkpointErrorId = boundary.checkpointId;
      this.#checkpointError = "Could not restore this checkpoint.";
    } finally {
      if (
        this.#isCurrentCheckpointAction(sessionId, threadId, token)
        && this.#checkpointActions.finish(token)
      ) this.requestUpdate();
    }
  }

  async #forkTurnCheckpoint(boundary: TurnCheckpointBoundary): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) return;
    const token = this.#checkpointActions.begin(`fork:${boundary.checkpointId}`);
    if (token === undefined) return;
    const sessionId = this.sessionId;
    const threadId = this.threadId;
    this.#checkpointErrorId = "";
    this.#checkpointError = "";
    this.requestUpdate();
    try {
      const fork = await services.protocol.forkCheckpoint(boundary.checkpointId);
      if (!this.#isCurrentCheckpointAction(sessionId, threadId, token)) return;
      store.upsertSessionMetadata(fork.session);
      store.upsertThread(fork.thread);
      store.markSessionRead(fork.session.id);
      const route = readSignal(services.router.route);
      services.router.navigate({
        kind: "session",
        workspaceId: fork.session.workspace_id,
        sessionId: fork.session.id,
        threadId: fork.thread.id,
        ...(route.kind === "session" && route.inspection !== undefined
          ? { inspection: route.inspection }
          : {}),
      });
    } catch {
      if (!this.#isCurrentCheckpointAction(sessionId, threadId, token)) return;
      this.#checkpointErrorId = boundary.checkpointId;
      this.#checkpointError = "Could not fork this checkpoint.";
    } finally {
      if (
        this.#isCurrentCheckpointAction(sessionId, threadId, token)
        && this.#checkpointActions.finish(token)
      ) this.requestUpdate();
    }
  }

  #renderTurnCard(
    unit: ChatRenderUnit,
    turnLabels: ReadonlyMap<number, string>,
    turnModels: ReadonlyMap<number, string>,
    turnDurationMs: ReadonlyMap<number, number>,
    presentation: ChatPresentationIndex,
    activityPresentation: AgentActivityPresentation | undefined,
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
    const turnState = unit.status?.state ?? presentation.turnStates.get(unit.turn);
    const promptPreview = unit.prompt === undefined
      ? ""
      : collapsedChatPreview(assistantCopyText(unit.prompt.content))
        || `${unit.prompt.attachments.length} attachment${unit.prompt.attachments.length === 1 ? "" : "s"}`;
    const preview = promptPreview || collapsedChatPreview(joined) || `Turn ${unit.turn}`;
    const activityRunning = unit.items.some((item) =>
      (item.kind === "assistant" || item.kind === "progress" || item.kind === "thinking")
        && !item.complete
      || item.kind === "compaction" && item.state.kind === "running"
      || item.kind === "tool" && (
        item.status === "running" || item.status === "awaiting-approval"
      )
      || item.kind === "questions" && item.answers === undefined
    );
    const stateKind = turnState?.kind ?? (
      activityRunning || unit.items.length === 0 ? "running" : "completed"
    );
    const modelLabel = turnLabels.get(unit.turn);
    const modelId = turnModels.get(unit.turn);
    const model = this.#availableModels().find((candidate) => candidate.id === modelId);
    const usage = turnState?.kind === "running" || turnState?.kind === "completed"
      ? turnState.usage
      : undefined;
    const turnContextUsage = composerContextUsage(
      usage,
      model?.context_window,
      compactionRunning,
      modelId?.startsWith("codex/") ?? false,
    );
    return html`
      <article
        class=${`message turn-card assistant-message agent-turn-card conversation-turn turn-${stateKind}`}
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
              : this.#renderContextUsage(turnContextUsage, "turn-context-usage")}
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
              )}
              ${activityPresentation === undefined
                ? nothing
                : this.#renderTransientActivityNode(activityPresentation)}
              ${unit.status === undefined
                ? nothing
                : this.#renderTerminalTurnState(unit.status)}
            </div>`
          : nothing}
      </article>
    `;
  }

  #renderUserNode(
    item: Extract<ThreadChatItem, { readonly kind: "user" | "steered" }>,
  ) {
    this.#ensureMarkdown();
    const steered = item.kind === "steered";
    const label = steered ? "Steered" : "Prompt";
    return html`
      <section
        class=${`turn-rail-node turn-${steered ? "steered" : "prompt"}-node user-message`}
        data-chat-anchor-id=${`item:${item.id}`}
        aria-label=${label}
      >
        <span class=${`turn-rail-marker ${steered ? "steered" : "prompt"}`} aria-hidden="true">
          ${fontAwesomeIcon(steered ? "route" : "user")}
        </span>
        <header class="turn-node-header"><strong>${label}</strong></header>
        <div class="turn-node-body user-body-stream">
          ${item.content === ""
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

  #renderContextUsage(
    contextUsage: ReturnType<typeof composerContextUsage>,
    additionalClass = "",
  ) {
    return html`<span
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
  }

  #renderActivityRow(activity: AgentActivityPresentation) {
    const accessibleLabel = activity.detail === ""
      ? activity.announcementLabel
      : `${activity.announcementLabel}. ${activity.detail}`;
    return html`<div class="activity-row agent-activity">
      <span class="activity-dots" aria-hidden="true"><i></i><i></i><i></i></span>
      <span class="agent-activity-copy" aria-hidden="true">
        <strong>${activity.label}</strong>
        ${activity.detail === "" ? nothing : html`<small>${activity.detail}</small>`}
      </span>
      <span
        class="visually-hidden"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >${accessibleLabel}</span>
    </div>`;
  }

  #renderTransientActivityNode(activity: AgentActivityPresentation) {
    const accessibleLabel = activity.detail === ""
      ? activity.announcementLabel
      : `${activity.announcementLabel}. ${activity.detail}`;
    return html`
      <section class="turn-rail-node turn-transient-activity">
        <span class="turn-rail-marker transient" aria-hidden="true">
          ${fontAwesomeIcon("spinner", {
            className: "turn-transient-spinner",
            spin: true,
          })}
        </span>
        <div class="turn-transient-activity-copy" aria-hidden="true">
          <header class="turn-node-header"><strong>${activity.label}</strong></header>
          ${activity.detail === "" ? nothing : html`<small>${activity.detail}</small>`}
        </div>
        <span
          class="visually-hidden"
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >${accessibleLabel}</span>
      </section>
    `;
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

  #renderSubagentNode(
    item: Extract<AgentChatItem, { readonly kind: "subagent" }>,
  ) {
    const prompt = collapsedChatPreview(item.prompt) || "Subagent transcript";
    return html`
      <button
        class="turn-rail-node subagent-rail-item"
        type="button"
        data-chat-anchor-id=${`item:${item.id}`}
        aria-label=${`Open subagent transcript: ${prompt}`}
        title="Open subagent transcript"
        @click=${() => this.#openSubagent(item)}
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
  ) {
    const chatPreferences = this.#services.value === undefined
      ? undefined
      : readSignal(this.#services.value.chatPreferences);
    const effectiveCollapse = effectiveChatCollapsePreferences(
      chatPreferences ?? DEFAULT_CHAT_PREFERENCES,
    );
    const collapseSequentialToolCalls = effectiveCollapse.collapseSequentialToolCalls;
    const collapseThinkingWithTools = effectiveCollapse.collapseThinkingWithTools;
    const collapseCompactionWithTools = effectiveCollapse.collapseCompactionWithTools;
    const collapseTodoUpdatesWithTools = effectiveCollapse.collapseTodoUpdatesWithTools;
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
    const activityFollows = (start: number): boolean => {
      for (let cursor = start; cursor < unit.items.length; cursor += 1) {
        const candidate = unit.items[cursor];
        if (candidate === undefined) return false;
        if (candidate.kind === "tool" && isContextCompactionTool(candidate)) continue;
        return candidate.kind === "progress"
          || candidate.kind === "thinking"
          || candidate.kind === "todo"
          || candidate.kind === "tool";
      }
      return false;
    };
    const hasNativeCompaction = unit.items.some((item) => item.kind === "compaction");
    let index = 0;
    while (index < unit.items.length) {
      const item = unit.items[index];
      if (item === undefined) break;
      if (item.kind === "steered") {
        flushActivityRows();
        rows.push(this.#renderUserNode(item));
        index += 1;
        continue;
      }
      if (item.kind === "subagent") {
        flushActivityRows();
        rows.push(this.#renderSubagentNode(item));
        index += 1;
        continue;
      }
      if (item.kind === "assistant") {
        flushActivityRows();
        const stretch: Extract<AgentChatItem, { readonly kind: "assistant" }>[] = [];
        while (index < unit.items.length && unit.items[index]?.kind === "assistant") {
          stretch.push(unit.items[index] as Extract<AgentChatItem, { readonly kind: "assistant" }>);
          index += 1;
        }
        const content = stretch.map((part) => part.content).filter(Boolean).join("\n\n");
        if (content !== "") {
          const response = index === unit.items.length
            && stretch.some((part) => presentation.lastAssistantIds.has(part.id));
          const streaming = stretch.some((part) => !part.complete);
          const turnState = unit.status?.state ?? presentation.turnStates.get(unit.turn);
          const tone = turnState?.kind === "failed"
            ? "failed"
            : turnState?.kind === "cancelled"
              ? "cancelled"
              : response && (streaming || turnState?.kind === "running")
                ? "running"
                : response
                  ? "complete"
                  : "update";
          const anchor = stretch.at(-1)?.id ?? stretch[0]?.id ?? unit.id;
          rows.push(html`<section
            class=${`turn-rail-node turn-response-node agent-text-block ${tone}`}
            data-chat-anchor-id=${`assistant:${anchor}`}
            aria-label=${response ? "Response" : "Agent progress"}
            @pointerdown=${this.#captureMarkdownContextMenuSelection}
            @mousedown=${this.#captureMarkdownContextMenuSelection}
            @contextmenu=${(event: MouseEvent) =>
              this.#openMarkdownContextMenu(event, content)}
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
          </section>`);
        }
        continue;
      }
      if (item.kind === "progress") {
        activityRows.push({
          content: this.#renderVisibleProgress(item),
          expandedGroup: false,
          endsWithExpandedToolGroup: false,
        });
        index += 1;
        continue;
      }
      if (item.kind === "questions") {
        flushActivityRows();
        rows.push(this.#renderItem(item, presentation));
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
          endsWithExpandedToolGroup: false,
        });
        index += 1;
        continue;
      }
      if (item.kind === "todo" && !collapseTodoUpdatesWithTools) {
        const repeatedTodos: Extract<AgentActivityItem, { readonly kind: "todo" }>[] = [item];
        let nextIndex = index + 1;
        while (nextIndex < unit.items.length) {
          const candidate = unit.items[nextIndex];
          if (candidate?.kind !== "todo" || candidate.state !== item.state) break;
          repeatedTodos.push(candidate);
          nextIndex += 1;
        }
        if (repeatedTodos.length === 1) {
          activityRows.push({
            content: this.#renderTodoUpdate(item),
            expandedGroup: false,
            endsWithExpandedToolGroup: false,
          });
        } else {
          activityRows.push({
            content: this.#renderActivityGroup(unit, repeatedTodos, presentation),
            expandedGroup: this.#activityGroupOpen(unit, repeatedTodos),
            endsWithExpandedToolGroup: false,
          });
        }
        index = nextIndex;
        continue;
      }
      // Approval controls must remain directly reachable. Running calls can
      // join the same collapsed activity run as soon as they are requested;
      // the transient tail describes the current action without adding a
      // shifting top-level tool node for each parallel call.
      if (toolCallNeedsApproval(item)) {
        activityRows.push({
          content: this.#renderItem(item, presentation),
          expandedGroup: false,
          endsWithExpandedToolGroup: false,
        });
        index += 1;
        continue;
      }
      if (item.kind === "tool" && !collapseSequentialToolCalls) {
        activityRows.push({
          content: this.#renderItem(item, presentation),
          expandedGroup: false,
          endsWithExpandedToolGroup: false,
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
          || candidate.kind === "steered"
          || candidate.kind === "questions"
          || candidate.kind === "progress"
          || (!collapseCompactionWithTools && candidate.kind === "compaction")
          || (!collapseCompactionWithTools
            && candidate.kind === "tool"
            && isContextCompactionTool(candidate))
          || (!collapseThinkingWithTools && candidate.kind === "thinking")
          || (!collapseTodoUpdatesWithTools && candidate.kind === "todo")
          || (candidate.kind === "tool" && toolCallNeedsApproval(candidate))
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
        if (only !== undefined) {
          activityRows.push({
            content: this.#renderItem(only, presentation),
            expandedGroup: false,
            endsWithExpandedToolGroup: false,
          });
        }
        continue;
      }
      const expandedGroup = this.#activityGroupOpen(unit, run);
      const finalGroupedItem = run.at(-1);
      const endsWithCollapsedTool = finalGroupedItem?.kind === "tool"
        && !isContextCompactionTool(finalGroupedItem)
        && finalGroupedItem.status !== "awaiting-approval"
        && !(this.#toolDisclosure.get(finalGroupedItem.callId) ?? false);
      activityRows.push({
        content: this.#renderActivityGroup(
          unit,
          run,
          presentation,
        ),
        expandedGroup,
        endsWithExpandedToolGroup: expandedGroup && endsWithCollapsedTool,
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
    this.#ensureMarkdown();
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
        @click=${() => this.dispatchEvent(new CustomEvent(
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
    this.#requestDisclosureUpdate();
  }

  #matchesOptimisticPrompt(
    content: string,
    attachments: readonly ProtocolAttachment[] | undefined,
    optimistic: OptimisticPromptSubmission,
  ): boolean {
    if (content !== optimistic.content) return false;
    const durable = attachments ?? [];
    return durable.length === optimistic.attachments.length
      && durable.every((attachment, index) => {
        const pending = optimistic.attachments[index];
        return pending !== undefined
          && attachment.name === pending.upload.name
          && attachment.mime === pending.upload.mime
          && attachment.size_bytes === pending.size;
      });
  }

  #clearOptimisticPrompt(expectedId?: string): void {
    const optimistic = this.#optimisticPrompt;
    if (optimistic === undefined || (expectedId !== undefined && optimistic.id !== expectedId)) return;
    optimistic.queueRevision.close();
    this.#optimisticPrompt = undefined;
  }

  #reconcileOptimisticPrompt(
    items: readonly ThreadChatItem[],
    queue: readonly QueuedPrompt[],
  ): void {
    const optimistic = this.#optimisticPrompt;
    if (optimistic === undefined || optimistic.threadId !== this.threadId) return;
    const durablePromptId = optimistic.durablePrompt?.id;
    if (
      durablePromptId !== undefined
      && (
        queue.some((prompt) => prompt.id === durablePromptId)
        || optimistic.queueRevision.queueChanged()
      )
    ) {
      this.#clearOptimisticPrompt(optimistic.id);
      return;
    }
    if (items.some((item) =>
      item.kind === "user"
      && item.turn >= optimistic.minimumTurn
      && (optimistic.turn === undefined || item.turn === optimistic.turn)
      && this.#matchesOptimisticPrompt(item.content, item.attachments, optimistic)
    )) {
      this.#clearOptimisticPrompt(optimistic.id);
    }
  }

  #queueWithOptimisticPrompt(queue: readonly QueuedPrompt[]): readonly QueuedPrompt[] {
    const optimistic = this.#optimisticPrompt;
    if (
      optimistic === undefined
      || optimistic.threadId !== this.threadId
      || optimistic.disposition !== "queue"
    ) return queue;
    if (optimistic.durablePrompt !== undefined) {
      return queue.some((prompt) => prompt.id === optimistic.durablePrompt?.id)
        ? queue
        : [...queue, optimistic.durablePrompt];
    }
    const position = queue.reduce(
      (maximum, prompt) => Math.max(maximum, prompt.position),
      0,
    ) + 1;
    return [
      ...queue,
      {
        id: optimistic.id,
        thread_id: optimistic.threadId,
        position,
        content: optimistic.content,
        created_at: new Date().toISOString(),
        attachments: optimistic.attachments.map((attachment, index) => ({
          id: `${optimistic.id}:attachment:${index}`,
          name: attachment.upload.name,
          mime: attachment.upload.mime,
          size_bytes: attachment.size,
        })),
      },
    ];
  }

  #renderQueue(
    queue: readonly QueuedPrompt[],
    turnRunning: boolean,
    connectivityBlocked: boolean,
  ) {
    if (queue.length === 0) return nothing;
    const optimisticQueuePending = queue.some((prompt) => prompt.id.startsWith("optimistic:"));
    const queueMutationBusy = optimisticQueuePending
      || this.#queueBusy !== ""
      || (this.#queueEditId !== "" && this.#attachmentPending);
    const keyboardReordering = this.#queueKeyboardStateValid(queue);
    const orderedQueue = this.#keyboardOrderedQueue(queue);
    const controls = queueControlState({
      threadAvailable: this.threadId !== "",
      queueLength: queue.length,
      turnRunning,
      busy: queueMutationBusy || keyboardReordering,
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
        <p id="queue-reorder-instructions" class="visually-hidden">
          Press Space or Enter to pick up this queued prompt. Then use Arrow Up,
          Arrow Down, Home, or End to choose its position. Press Space or Enter
          again to drop it, or Escape to cancel.
        </p>
        <p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
          ${this.#queueStatus}
        </p>
        <ol>
          ${repeat(
            orderedQueue,
            (prompt) => prompt.id,
            (prompt, index) => {
              const dropTarget = this.#queueDropId === prompt.id;
              const keyboardActive = keyboardReordering
                && this.#queueKeyboardDragId === prompt.id;
              const keyboardEnabled = !queueMutationBusy
                && !connectivityBlocked
                && orderedQueue.length > 1;
              const keyboardFocusable = keyboardActive
                || (keyboardEnabled && !keyboardReordering);
              const placeholder = html`<li
                class="queue-drop-placeholder"
                data-drop-placeholder="queue"
                aria-hidden="true"
                @dragover=${this.#keepQueueDropActive}
                @drop=${(event: DragEvent) => void this.#dropQueued(event, queue, prompt.id)}
              ></li>`;
              return html`
                ${dropTarget && this.#queueDropPlacement === "before"
                  ? placeholder
                  : nothing}
                <li
                  data-queue-id=${prompt.id}
                  data-keyboard-reordering=${keyboardActive ? "true" : nothing}
                  tabindex=${keyboardFocusable ? "0" : nothing}
                  aria-label=${keyboardFocusable
                    ? `${queuePreview(prompt.content)}. Position ${index + 1} of ${orderedQueue.length}.${keyboardActive
                      ? " Reordering."
                      : " Ready to reorder."}`
                    : nothing}
                  aria-describedby=${keyboardFocusable ? "queue-reorder-instructions" : nothing}
                  aria-keyshortcuts=${keyboardFocusable
                    ? "Space Enter ArrowUp ArrowDown Home End Escape"
                    : nothing}
                  draggable=${!controls.mutationsDisabled && queue.length > 1 ? "true" : "false"}
                  @keydown=${(event: KeyboardEvent) => this.#queueRowKeyDown(
                    event,
                    orderedQueue,
                    index,
                    queueMutationBusy || connectivityBlocked,
                  )}
                  @pointerdown=${this.#prepareQueueRowDrag}
                  @dragstart=${(event: DragEvent) => this.#startQueueDrag(
                    event,
                    prompt.id,
                    controls.mutationsDisabled || queue.length < 2,
                  )}
                  @dragend=${this.#endQueueDrag}
                  @dragover=${(event: DragEvent) => this.#dragQueueOver(event, queue, prompt.id)}
                  @drop=${(event: DragEvent) => void this.#dropQueued(event, queue, prompt.id)}
                >
                  <div class="queue-row">
                    <span class="queue-index" aria-hidden="true">${index + 1}.</span>
                    <p title=${prompt.content}>${queuePreview(prompt.content)}</p>
                    ${keyboardActive
                      ? html`<span class="queue-reorder-badge">Reordering</span>`
                      : nothing}
                    ${prompt.attachments === undefined || prompt.attachments.length === 0
                      ? nothing
                      : html`<span
                          class="queue-attachment-badge"
                          role="img"
                          aria-label=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                          title=${`${prompt.attachments.length} attachment${prompt.attachments.length === 1 ? "" : "s"}`}
                        >${fontAwesomeIcon("paperclip")}${prompt.attachments.length}</span>`}
                    <div class="queue-actions" aria-label=${`Actions for queued prompt ${index + 1}`}>
                      <button
                        type="button"
                        data-queue-action="send-now"
                        aria-label=${turnRunning
                          ? "Send this queued prompt now and stop the current turn"
                          : "Send this queued prompt now"}
                        title=${turnRunning ? "Send now and stop current turn" : "Send now"}
                        ?disabled=${controls.sendNowDisabled}
                        @click=${() => this.#sendQueuedNow(prompt.id)}
                      >${fontAwesomeIcon("play")}</button>
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
                ${dropTarget && this.#queueDropPlacement === "after"
                  ? placeholder
                  : nothing}
              `;
            },
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
    // A history response can arrive during wheel momentum. In the common
    // case, hit-testing at the viewport edge finds the precise nested anchor
    // without synchronously measuring every thought/tool node in a very large
    // mounted turn. Fall back to the exhaustive scan only when the edge lands
    // in whitespace or the host does not expose `elementsFromPoint`.
    if (typeof viewport.ownerDocument.elementsFromPoint === "function") {
      const width = Math.max(1, viewportRect.width);
      const sampleXs = [0.5, 0.25, 0.75].map((ratio) =>
        viewportRect.left + width * ratio);
      const maximumY = Math.max(viewportTop, viewportBottom - 0.5);
      for (const yOffset of [0.5, 4, 12, 24]) {
        const y = Math.min(maximumY, viewportTop + yOffset);
        for (const x of sampleXs) {
          for (const hit of viewport.ownerDocument.elementsFromPoint(x, y)) {
            const element = hit.closest<HTMLElement>(
              "[data-chat-anchor-id], [data-virtual-id]",
            );
            const chatId = element?.dataset["chatAnchorId"];
            const virtualId = element?.dataset["virtualId"];
            if (virtualId === CHAT_START_SPACER_ID) continue;
            const id = chatId ?? (virtualId === undefined ? undefined : `virtual:${virtualId}`);
            if (element === null || element === undefined || id === undefined) continue;
            if (!viewport.contains(element)) continue;
            const rect = element.getBoundingClientRect();
            if (rect.height > 0 && rect.bottom > viewportTop && rect.top < viewportBottom) {
              return { id, offset: rect.top - viewportTop };
            }
          }
        }
      }
    }

    let crossingTop:
      | { readonly element: HTMLElement; readonly rect: DOMRect }
      | undefined;
    let nextVisible:
      | { readonly element: HTMLElement; readonly rect: DOMRect }
      | undefined;
    for (const element of viewport.querySelectorAll<HTMLElement>("[data-chat-anchor-id]")) {
      if (element.dataset["chatAnchorId"] === undefined) continue;
      const rect = element.getBoundingClientRect();
      if (rect.height <= 0 || rect.bottom <= viewportTop || rect.top >= viewportBottom) continue;
      if (rect.top <= viewportTop + 0.5 && rect.bottom > viewportTop + 0.5) {
        if (crossingTop === undefined || rect.height < crossingTop.rect.height) {
          crossingTop = { element, rect };
        }
      } else if (
        rect.top > viewportTop + 0.5
        && (nextVisible === undefined || rect.top < nextVisible.rect.top)
      ) {
        nextVisible = { element, rect };
      }
    }
    const candidate = crossingTop ?? nextVisible;
    if (candidate !== undefined) {
      const id = candidate.element.dataset["chatAnchorId"];
      if (id !== undefined) {
        return { id, offset: candidate.rect.top - viewportTop };
      }
    }

    let virtualRow:
      | { readonly element: HTMLElement; readonly rect: DOMRect }
      | undefined;
    for (const element of viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")) {
      if (element.dataset["virtualId"] === CHAT_START_SPACER_ID) continue;
      const rect = element.getBoundingClientRect();
      if (rect.height <= 0 || rect.bottom <= viewportTop || rect.top >= viewportBottom) continue;
      const crosses = rect.top <= viewportTop && rect.bottom > viewportTop;
      const selectedCrosses = virtualRow !== undefined
        && virtualRow.rect.top <= viewportTop
        && virtualRow.rect.bottom > viewportTop;
      if (
        virtualRow === undefined
        || (crosses && !selectedCrosses)
        || (crosses === selectedCrosses && rect.top < virtualRow.rect.top)
      ) {
        virtualRow = { element, rect };
      }
    }
    const id = virtualRow?.element.dataset["virtualId"];
    return id === undefined || virtualRow === undefined
      ? undefined
      : { id: `virtual:${id}`, offset: virtualRow.rect.top - viewportTop };
  }

  #restoreHistoryPrependAnchor(viewport: HTMLElement): void {
    const anchor = this.#historyAnchorToRestore;
    this.#historyAnchorToRestore = undefined;
    if (anchor === undefined) return;
    const generation = ++this.#historyAnchorGeneration;
    // The anchor was captured after the page response, so it supersedes the
    // guard left by the scroll that triggered prefetch. Any subsequent native
    // scroll invalidates this generation before the correction can run.
    this.#nativeScrollCorrectionBlockedUntil = 0;
    globalThis.queueMicrotask(() => {
      if (
        generation !== this.#historyAnchorGeneration
        || !this.isConnected
        || !viewport.isConnected
        || viewport.dataset["threadId"] !== this.threadId
      ) return;
      this.#applyChatDomAnchor(viewport, anchor, true);
    });
  }

  #applyChatDomAnchor(
    viewport: HTMLElement,
    anchor: ChatDomAnchor,
    refreshMeasurements: boolean,
  ): boolean {
    if (refreshMeasurements) {
      const beforeMeasure = this.#virtualizer.window();
      for (const row of viewport.querySelectorAll<HTMLElement>("[data-virtual-id]")) {
        const id = row.dataset["virtualId"];
        const height = row.getBoundingClientRect().height;
        if (id === undefined || height <= 0) continue;
        try {
          this.#virtualizer.measure(id, height);
        } catch {
          // A row can leave the window while this one-time correction runs.
        }
      }
      const afterMeasure = this.#virtualizer.window();
      if (!sameVirtualRenderWindow(beforeMeasure, afterMeasure)) {
        this.#syncMountedVirtualGeometry(viewport, afterMeasure);
      }
    }
    const element = this.#chatDomAnchorElement(viewport, anchor);
    if (element === undefined) return false;
    const delta = element.getBoundingClientRect().top
      - viewport.getBoundingClientRect().top
      - anchor.offset;
    if (Math.abs(delta) > 0.5) {
      const target = Math.max(0, viewport.scrollTop + delta);
      this.#virtualizer.setViewport(target, viewport.clientHeight, {
        userInitiated: true,
        atTail: false,
      });
      this.#setChatScrollTop(viewport, this.#virtualizer.window().scrollTop);
    }
    this.#parkedLayoutAnchor = anchor;
    return true;
  }

  #chatDomAnchorElement(
    viewport: HTMLElement,
    anchor: ChatDomAnchor,
  ): HTMLElement | undefined {
    const virtualId = anchor.id.startsWith("virtual:")
      ? anchor.id.slice("virtual:".length)
      : undefined;
    const selector = virtualId === undefined ? "[data-chat-anchor-id]" : "[data-virtual-id]";
    const expected = virtualId ?? anchor.id;
    for (const element of viewport.querySelectorAll<HTMLElement>(selector)) {
      const candidate = virtualId === undefined
        ? element.dataset["chatAnchorId"]
        : element.dataset["virtualId"];
      if (candidate === expected) return element;
    }
    return undefined;
  }

  #cancelHistoryAnchorCorrection(): void {
    this.#historyAnchorGeneration += 1;
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
    if (this.#historyLoading && viewport.scrollTop <= CHAT_TAIL_EPSILON_PX) {
      this.#historyStatusVisible = true;
      this.requestUpdate();
    }
    if (
      typeof IntersectionObserver === "undefined"
      && !this.#historyLoading
      && this.#historyError === ""
      && viewport.scrollTop <= viewport.clientHeight * 5
    ) {
      void this.#loadOlderHistory(false);
    }
    const before = this.#virtualizer.window();
    // The sticky jump control is an overlay visually, but WebKit retains its
    // border-box in normal flow and includes it in scrollHeight. Its height is
    // cached after rendering so this hot path never forces synchronous layout.
    const tailGap = this.#chatTailGap(viewport);
    const atTail = tailGap <= CHAT_TAIL_EPSILON_PX;
    const programmaticScroll = this.#programmaticScrollTarget !== undefined
      && Math.abs(viewport.scrollTop - this.#programmaticScrollTarget) <= 0.5;
    if (programmaticScroll) {
      // Row measurement and tail corrections already updated the virtualizer.
      // Ignore their resulting DOM events instead of treating them as another
      // user scroll and starting a render/persistence loop.
      this.#programmaticScrollTarget = undefined;
      return;
    }
    this.#programmaticScrollTarget = undefined;
    this.#nativeScrollCorrectionBlockedUntil = globalThis.performance.now()
      + CHAT_NATIVE_SCROLL_CORRECTION_GUARD_MS;
    this.#cancelHistoryAnchorCorrection();
    // A native user scroll invalidates the prior nested layout anchor. A new
    // one is captured only after scrolling settles, keeping layout reads out
    // of the hot path and preventing corrections from fighting momentum.
    this.#parkedLayoutAnchor = undefined;
    if (before.followingTail && atTail) return;
    // Every native scroll not initiated by this controller is authoritative.
    // This covers wheel, touch, keyboard, and native scrollbar dragging.
    this.#cancelProgrammaticScrollWindow();
    this.#cancelTailConvergence();
    this.#virtualizer.setViewport(
      viewport.scrollTop,
      viewport.clientHeight,
      { userInitiated: true, atTail },
    );
    const after = this.#virtualizer.window();
    this.#parkedLayoutAnchor = undefined;
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
    const viewport = event.currentTarget as HTMLElement;
    if (
      viewport.dataset["threadId"] === this.threadId
      && this.#restoredScrollThreadId === this.threadId
    ) {
      this.#resumeFollowTailAtDomEnd(viewport);
    }
    this.#captureParkedLayoutAnchor();
    this.#syncChatScrollIndicatorPosition(viewport);
    this.#flushScheduledChatPosition();
  };

  #chatTailGap(viewport: HTMLElement): number {
    return Math.max(
      0,
      viewport.scrollHeight
        - this.#followTailControlHeight
        - viewport.clientHeight
        - viewport.scrollTop,
    );
  }

  #resumeFollowTailAtDomEnd(viewport: HTMLElement): void {
    const before = this.#virtualizer.window();
    if (
      before.followingTail
      || this.#chatTailGap(viewport) > CHAT_TAIL_EPSILON_PX
    ) return;
    this.#cancelProgrammaticScrollWindow();
    this.#cancelTailConvergence();
    this.#virtualizer.setViewport(
      viewport.scrollTop,
      viewport.clientHeight,
      { userInitiated: true, atTail: true },
    );
    const after = this.#virtualizer.window();
    this.#parkedLayoutAnchor = undefined;
    this.#cancelScheduledChatPosition();
    this.#scheduleTailConvergence();
    this.#emitChatPosition();
    if (!sameVirtualRenderWindow(before, after)) this.#scheduleScrollRender();
  }

  #setChatScrollTop(viewport: HTMLElement, scrollTop: number): void {
    if (Math.abs(viewport.scrollTop - scrollTop) <= 0.5) return;
    if (this.#programmaticScrollFrame !== undefined) {
      globalThis.cancelAnimationFrame(this.#programmaticScrollFrame);
    }
    this.#programmaticScrollFrame = globalThis.requestAnimationFrame(() => {
      this.#programmaticScrollFrame = globalThis.requestAnimationFrame(() => {
        this.#programmaticScrollFrame = undefined;
        this.#programmaticScrollTarget = undefined;
      });
    });
    viewport.scrollTop = scrollTop;
    this.#programmaticScrollTarget = viewport.scrollTop;
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
    this.#programmaticScrollTarget = undefined;
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
      if (this.isConnected) {
        this.#captureParkedLayoutAnchor();
        this.#emitChatPosition();
      }
    }, CHAT_POSITION_SETTLE_MS);
  }

  #flushScheduledChatPosition(): void {
    if (this.#chatPositionTimer === undefined) return;
    clearTimeout(this.#chatPositionTimer);
    this.#chatPositionTimer = undefined;
    this.#captureParkedLayoutAnchor();
    this.#emitChatPosition();
  }

  #cancelScheduledChatPosition(): void {
    if (this.#chatPositionTimer === undefined) return;
    clearTimeout(this.#chatPositionTimer);
    this.#chatPositionTimer = undefined;
  }

  readonly #followTail = (): void => {
    this.#cancelScheduledChatPosition();
    this.#parkedLayoutAnchor = undefined;
    this.#virtualizer.enableFollowTail();
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport !== null) {
      this.#setChatScrollTop(viewport, this.#transcriptTailScrollTop(viewport));
    }
    this.#scheduleTailConvergence();
    this.#emitChatPosition();
    this.requestUpdate();
  };

  #captureParkedLayoutAnchor(): void {
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (
      viewport === null
      || viewport.dataset["threadId"] !== this.threadId
      || this.#virtualizer.window().followingTail
    ) {
      this.#parkedLayoutAnchor = undefined;
      return;
    }
    this.#parkedLayoutAnchor = this.#captureChatDomAnchor(viewport);
  }

  #syncHistoryObserver(viewport: HTMLElement): void {
    const view = this.threadId === ""
      ? undefined
      : this.#store.value?.threadView(this.threadId);
    const sentinel = view?.hasOlder === true && this.#historyError === ""
      ? viewport.querySelector(".chat-history-sentinel")
      : null;
    if (sentinel === this.#observedHistorySentinel) return;
    this.#disconnectHistoryObserver();
    if (
      sentinel === null
      || sentinel === undefined
      || typeof IntersectionObserver === "undefined"
    ) return;
    this.#historyObserver = new IntersectionObserver((entries) => {
      if (
        entries.some((entry) => entry.isIntersecting)
        && !this.#historyLoading
        && this.#historyError === ""
      ) {
        void this.#loadOlderHistory(false);
      }
    }, {
      root: viewport,
      rootMargin: CHAT_HISTORY_PREFETCH_ROOT_MARGIN,
      threshold: 0,
    });
    this.#observedHistorySentinel = sentinel;
    this.#historyObserver.observe(sentinel);
  }

  #disconnectHistoryObserver(): void {
    this.#historyObserver?.disconnect();
    this.#historyObserver = undefined;
    this.#observedHistorySentinel = undefined;
  }

  #scheduleHistoryStatus(): void {
    this.#clearHistoryStatusTimer();
    this.#historyStatusTimer = setTimeout(() => {
      this.#historyStatusTimer = undefined;
      if (!this.#historyLoading || !this.isConnected) return;
      const viewport = this.querySelector<HTMLElement>(".chat-stream");
      const sentinel = viewport?.querySelector<HTMLElement>(".chat-history-sentinel");
      if (
        viewport === null
        || viewport === undefined
        || sentinel === null
        || sentinel === undefined
      ) return;
      const viewportRect = viewport.getBoundingClientRect();
      const sentinelRect = sentinel.getBoundingClientRect();
      if (sentinelRect.bottom >= viewportRect.top && sentinelRect.top <= viewportRect.bottom) {
        this.#historyStatusVisible = true;
        this.requestUpdate();
      }
    }, CHAT_HISTORY_STATUS_DELAY_MS);
  }

  #clearHistoryStatusTimer(): void {
    if (this.#historyStatusTimer === undefined) return;
    clearTimeout(this.#historyStatusTimer);
    this.#historyStatusTimer = undefined;
  }

  #scheduleHistoryRetry(): void {
    this.#clearHistoryRetryTimer();
    const generation = this.#historyGeneration;
    this.#historyRetryTimer = setTimeout(() => {
      this.#historyRetryTimer = undefined;
      if (generation !== this.#historyGeneration || !this.isConnected) return;
      this.#historyError = "";
      this.requestUpdate();
    }, CHAT_HISTORY_RETRY_DELAY_MS);
  }

  #clearHistoryRetryTimer(): void {
    if (this.#historyRetryTimer === undefined) return;
    clearTimeout(this.#historyRetryTimer);
    this.#historyRetryTimer = undefined;
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
    const sessionId = this.sessionId;
    if (sessionId === "" || store.isSessionTombstoned(sessionId)) return;
    const threadId = this.threadId;
    const generation = this.#historyGeneration;
    const initialView = store.threadView(threadId);
    if (!initialView.hasOlder || initialView.itemOffset === 0) return;

    this.#historyLoading = true;
    this.#historyError = "";
    this.#historyStatusVisible = false;
    this.#disconnectHistoryObserver();
    this.#scheduleHistoryStatus();
    this.requestUpdate();
    try {
      do {
        const view = store.threadView(threadId);
        if (!view.hasOlder || view.itemOffset === 0) break;
        const page = await services.protocol.threadView(threadId, view.itemOffset);
        if (!this.#isCurrentHistoryRequest(sessionId, threadId, generation)) return;
        const virtualWindow = this.#virtualizer.window();
        const viewport = this.querySelector<HTMLElement>(".chat-stream");
        this.#pendingHistoryPrepend = {
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
      if (this.#isCurrentHistoryRequest(sessionId, threadId, generation)) {
        this.#historyError = "Earlier messages could not be loaded.";
        this.#scheduleHistoryRetry();
      }
    } finally {
      if (this.#isCurrentHistoryRequest(sessionId, threadId, generation)) {
        this.#clearHistoryStatusTimer();
        this.#historyStatusVisible = false;
        this.#historyLoading = false;
        this.#disconnectHistoryObserver();
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
    item: Exclude<AgentChatItem, { readonly kind: "assistant" }>,
    presentation: ChatPresentationIndex,
  ) {
    switch (item.kind) {
      case "subagent":
        return this.#renderSubagentNode(item);
      case "progress":
        return this.#renderVisibleProgress(item);
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
        return this.#renderCompactionMarker(item.state, item.id);
      case "todo":
        return this.#renderTodoUpdate(item);
      case "tool": {
        const approvalPending = this.#approvalSubmissions.has(item.callId);
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
        @mousedown=${this.#preserveMarkdownContextMenuSelection}
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
    const pendingSelection = this.#pendingMarkdownContextSelection;
    this.#pendingMarkdownContextSelection = undefined;
    const preserveNativeMenu = this.#preservesNativeMarkdownContextMenu(event);
    if (preserveNativeMenu) {
      this.#dismissMarkdownContextMenu();
      return;
    }

    event.preventDefault();
    const selected = pendingSelection?.source === source
      ? { text: pendingSelection.text, ranges: pendingSelection.ranges }
      : this.#selectionWithin(source);
    const selection = selected.text;
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
      selectionRanges: selected.ranges,
      x: Math.max(8, Math.min(requestedX, viewportWidth - estimatedWidth - 8)),
      y: Math.max(8, Math.min(requestedY, viewportHeight - estimatedHeight - 8)),
    };
    this.#markdownContextMenu = menu;
    this.#markdownContextMenuStatus = "";
    this.#markdownContextMenuReturnFocus = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : undefined;
    this.#restoreMarkdownContextMenuSelection(menu);
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (this.#markdownContextMenu !== menu) return;
      // WebKit hides a shadow-root text selection when focus leaves that
      // root. Pointer users can activate either command without transferring
      // focus; keyboard-opened menus and menus without a selection retain the
      // normal roving menu focus behavior.
      if (keyboardPosition || selection === "") {
        this.querySelector<HTMLButtonElement>(
          '.message-context-menu [role="menuitem"]',
        )?.focus({ preventScroll: true });
      }
      this.#restoreMarkdownContextMenuSelection(menu);
      this.#restoreMarkdownContextMenuSelectionAfterBrowserDefault(menu, true);
    });
  }

  #preservesNativeMarkdownContextMenu(event: Event): boolean {
    return event.composedPath().some((target) =>
      target instanceof Element
      && target.matches(
        "a, img, video, input, textarea, select, .tool-card, .thinking-card, .thinking-output, .context-compaction-marker, .question-card",
      )
    );
  }

  readonly #captureMarkdownContextMenuSelection = (event: MouseEvent): void => {
    if (event.button !== 2) return;
    const source = event.currentTarget;
    if (
      !(source instanceof HTMLElement)
      || this.#preservesNativeMarkdownContextMenu(event)
    ) {
      this.#pendingMarkdownContextSelection = undefined;
      return;
    }
    const pendingSelection = this.#pendingMarkdownContextSelection;
    if (event.type === "mousedown" && pendingSelection?.source === source) {
      // WebKit may apply its right-button pointer default before dispatching
      // the compatibility mousedown event. Restore the range captured during
      // pointerdown before suppressing mousedown's own selection default.
      this.#restoreSelectionRanges(pendingSelection.ranges);
      event.preventDefault();
      return;
    }
    const selected = this.#selectionWithin(source);
    if (selected.text === "") {
      this.#pendingMarkdownContextSelection = undefined;
      return;
    }
    this.#pendingMarkdownContextSelection = {
      source,
      text: selected.text,
      ranges: selected.ranges,
    };
    if (event.type === "mousedown") {
      // Pointerdown records the range before WebKit's right-button default;
      // mousedown suppresses the later compatibility-event default without
      // preventing the contextmenu event itself.
      event.preventDefault();
    }
  };

  #selectionWithin(
    source: HTMLElement,
  ): { readonly text: string; readonly ranges: readonly Range[] } {
    const selections = new Set<Selection>();
    const addSelection = (selection: Selection | null | undefined): void => {
      if (selection !== null && selection !== undefined) selections.add(selection);
    };
    addSelection(globalThis.getSelection?.());
    for (const element of [source, ...source.querySelectorAll<HTMLElement>("*")]) {
      const root = element.shadowRoot as (ShadowRoot & {
        getSelection?: () => Selection | null;
      }) | null;
      addSelection(root?.getSelection?.());
    }
    for (const selection of selections) {
      if (selection.rangeCount === 0) continue;
      const range = selection.getRangeAt(0);
      const commonAncestor = range.commonAncestorContainer;
      const root = commonAncestor.getRootNode();
      const inside = source.contains(commonAncestor)
        || (root instanceof ShadowRoot && source.contains(root.host));
      if (!inside) continue;
      return {
        text: selection.toString(),
        ranges: Array.from(
          { length: selection.rangeCount },
          (_, index) => selection.getRangeAt(index).cloneRange(),
        ),
      };
    }
    return { text: "", ranges: [] };
  }

  #restoreMarkdownContextMenuSelection(menu: MarkdownContextMenu): void {
    this.#restoreSelectionRanges(menu.selectionRanges);
  }

  #restoreMarkdownContextMenuSelectionAfterBrowserDefault(
    menu: MarkdownContextMenu,
    requireOpen: boolean,
  ): void {
    globalThis.setTimeout(() => {
      if (
        !this.isConnected
        || (requireOpen
          ? this.#markdownContextMenu !== menu
          : this.#markdownContextMenu !== undefined)
      ) return;
      this.#restoreMarkdownContextMenuSelection(menu);
    }, 0);
  }

  #restoreSelectionRanges(selectionRanges: readonly Range[]): void {
    if (selectionRanges.length === 0) return;
    const ranges = selectionRanges.filter((range) =>
      range.startContainer.isConnected && range.endContainer.isConnected);
    if (ranges.length === 0) return;
    const root = ranges[0]?.commonAncestorContainer.getRootNode();
    const shadowSelection = root instanceof ShadowRoot
      ? (root as ShadowRoot & { getSelection?: () => Selection | null }).getSelection?.()
      : undefined;
    // The document selection remains authoritative for open shadow trees.
    // Chromium and WebKit can expose ShadowRoot.getSelection() as a readable
    // view that does not reliably accept addRange() after context-menu
    // defaults, so use it only when the document API is unavailable.
    const selection = globalThis.getSelection?.() ?? shadowSelection;
    if (selection === undefined || selection === null) return;
    selection.removeAllRanges();
    for (const range of ranges) {
      try {
        selection.addRange(range);
      } catch {
        // A concurrent transcript update may have replaced a selected node.
      }
    }
  }

  readonly #preserveMarkdownContextMenuSelection = (event: MouseEvent): void => {
    if (event.button === 0 && (this.#markdownContextMenu?.selectionRanges.length ?? 0) > 0) {
      // Keep the browser selection painted while either menu command is
      // clicked. Preventing mousedown focus does not suppress the click, and
      // keyboard focus remains on the menu item established when it opened.
      event.preventDefault();
    }
  };

  readonly #restoreMarkdownContextMenuSelectionFromPointer = (event: PointerEvent): void => {
    const menu = this.#markdownContextMenu;
    if (event.button !== 2 || menu === undefined || menu.selection === "") return;
    this.#restoreMarkdownContextMenuSelection(menu);
    this.#restoreMarkdownContextMenuSelectionAfterBrowserDefault(menu, true);
  };

  readonly #dismissMarkdownContextMenuFromPointer = (event: PointerEvent): void => {
    if (this.#markdownContextMenu === undefined) return;
    const target = event.target;
    if (
      target instanceof Element
      && target.closest(".message-context-menu") !== null
    ) return;
    this.#dismissMarkdownContextMenu();
  };

  readonly #dismissMarkdownContextMenuFromKeyboard = (event: KeyboardEvent): void => {
    if (this.#markdownContextMenu === undefined || event.key !== "Escape") return;
    event.preventDefault();
    this.#closeMarkdownContextMenu(true);
  };

  readonly #dismissMarkdownContextMenu = (): void => {
    this.#closeMarkdownContextMenu(false);
  };

  #closeMarkdownContextMenu(restoreFocus: boolean): void {
    if (this.#markdownContextMenu === undefined) return;
    const menu = this.#markdownContextMenu;
    const returnFocus = this.#markdownContextMenuReturnFocus;
    this.#markdownContextMenu = undefined;
    this.#pendingMarkdownContextSelection = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
    this.requestUpdate();
    if (restoreFocus) {
      void this.updateComplete.then(() => {
        if (returnFocus?.isConnected === true) returnFocus.focus({ preventScroll: true });
        this.#restoreMarkdownContextMenuSelection(menu);
      });
    }
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
    this.#pendingMarkdownContextSelection = undefined;
    this.#markdownContextMenuReturnFocus = undefined;
    this.requestUpdate();
    const result = await copyChatText(value, globalThis.navigator?.clipboard);
    this.#markdownContextMenuStatus = `${label}: ${copyActionLabel(result)}`;
    this.requestUpdate();
    await this.updateComplete;
    // Restore only after the final status render. Restoring between the menu
    // removal and status updates creates a one-frame selection that the
    // second render can clear, which is especially visible in WebKit/Wry.
    this.#restoreMarkdownContextMenuSelection(menu);
    this.#restoreMarkdownContextMenuSelectionAfterBrowserDefault(menu, false);
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
    void this.#ensureToolDetails(callId);
    this.#requestDisclosureUpdate();
  }

  async #ensureToolDetails(callId: string): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const threadId = this.threadId;
    if (
      services === undefined
      || store === undefined
      || threadId === ""
      || this.#toolDetailLoading.has(callId)
    ) return;
    const tool = store.threadView(threadId).findTool(callId);
    if (tool?.detailsDeferred !== true) return;
    const generation = this.#threadInteractionGeneration;
    this.#toolDetailLoading.add(callId);
    this.#toolDetailErrors.delete(callId);
    this.requestUpdate();
    try {
      const details = await services.protocol.threadToolDetails(threadId, callId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      if (!store.replaceThreadToolDetails(threadId, details)) {
        throw new Error("tool detail no longer belongs to this thread view");
      }
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#toolDetailErrors.set(callId, "Tool details could not be loaded.");
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#toolDetailLoading.delete(callId);
        this.requestUpdate();
      }
    }
  }

  #requestDisclosureUpdate(): void {
    const preserveTail = this.#virtualizer.window().followingTail;
    this.requestUpdate();
    if (!preserveTail) return;
    // A disclosure changes its box before ResizeObserver reports the new
    // virtual-row height. Converge on the real tail only when it is pinned.
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

  #ensureToolDetail(): void {
    if (this.#toolDetailRequested || this.#toolDetailLoadFailed) return;
    this.#toolDetailRequested = true;
    void import("./tool-detail-view.js").catch(() => {
      this.#toolDetailRequested = false;
      this.#toolDetailLoadFailed = true;
      this.requestUpdate();
    });
  }

  readonly #retryToolDetailImport = (): void => {
    this.#toolDetailLoadFailed = false;
    this.#toolDetailRequested = false;
    this.requestUpdate();
  };

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

  #availableModels(): readonly ProtocolModelInfo[] {
    const services = this.#services.value;
    const current = services === undefined
      ? []
      : readSignal(services.modelCatalog.current);
    return current.length > 0 ? current : this.#models;
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
    this.#subscriptionHealth = readSignal(services.subscriptionHealth.current);
    this.requestUpdate();
    void services.subscriptionHealth.refresh("if-stale").then((subscriptionHealth) => {
      const currentCatalogKey = this.#threadOptionCatalogKey(this.workspaceId);
      if (this.workspaceId !== workspaceId || currentCatalogKey !== catalogKey) return;
      this.#subscriptionHealth = subscriptionHealth;
      this.requestUpdate();
    }).catch(() => undefined);
    try {
      const [modes, models] = await Promise.all([
        services.protocol.personas(workspaceId),
        services.modelCatalog.refresh("if-stale"),
      ]);
      const currentCatalogKey = this.#threadOptionCatalogKey(this.workspaceId);
      if (this.workspaceId !== workspaceId || currentCatalogKey !== catalogKey) return;
      this.#modes = modes;
      this.#models = models;
      this.requestUpdate();
    } catch {
      if (
        this.workspaceId === workspaceId
        && this.#threadOptionCatalogKey(this.workspaceId) === catalogKey
      ) {
        this.#requestError = "Persona and model options could not be loaded.";
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
    } catch (error) {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        const detail = error instanceof Error ? error.message.trim() : "";
        this.#requestError = detail === "" || detail === "update thread request failed"
          ? errorMessage
          : `${errorMessage} ${detail}`;
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#threadSettingsPending = false;
        this.requestUpdate();
      }
    }
  }

  #isCurrentThreadInteraction(threadId: string, generation: number): boolean {
    return this.#threadInteractionGeneration === generation
      && this.#isCurrentThreadScope(this.sessionId, threadId);
  }

  #isCurrentThreadScope(sessionId: string, threadId: string): boolean {
    const services = this.#services.value;
    const route = services === undefined ? undefined : readSignal(services.router.route);
    return this.isConnected
      && this.sessionId === sessionId
      && this.threadId === threadId
      && this.#store.value?.isSessionTombstoned(sessionId) !== true
      && route?.kind === "session"
      && route.sessionId === sessionId
      && (route.threadId ?? "") === threadId;
  }

  #isCurrentHistoryRequest(
    sessionId: string,
    threadId: string,
    generation: number,
  ): boolean {
    return generation === this.#historyGeneration
      && this.#isCurrentThreadScope(sessionId, threadId);
  }

  #isCurrentTurnRequest(
    sessionId: string,
    threadId: string,
    generation: number,
  ): boolean {
    return generation === this.#turnRequestGeneration
      && this.#isCurrentThreadScope(sessionId, threadId);
  }

  #isCurrentCheckpointAction(
    sessionId: string,
    threadId: string,
    token: CheckpointActionToken,
  ): boolean {
    return this.#checkpointActions.isCurrent(token)
      && this.#isCurrentThreadScope(sessionId, threadId);
  }

  #isCurrentNewThreadRequest(token: NewThreadRequestToken): boolean {
    const services = this.#services.value;
    const route = services === undefined ? undefined : readSignal(services.router.route);
    const currentThreadIds = [token.initialThreadId, token.createdThreadId];
    return this.#newThreadRequest === token
      && this.isConnected
      && this.workspaceId === token.workspaceId
      && this.sessionId === token.sessionId
      && (
        this.threadId === token.initialThreadId
        || this.threadId === token.createdThreadId
      )
      && this.#store.value?.isSessionTombstoned(token.sessionId) !== true
      && route?.kind === "session"
      && route.workspaceId === token.workspaceId
      && route.sessionId === token.sessionId
      && currentThreadIds.includes(route.threadId ?? "");
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
    if (this.threadId !== "") this.#store.value?.markThreadRead(this.threadId);
    this.#store.value?.markThreadRead(threadId);
    this.#newThreadSetupOpen = false;
    this.#newThreadError = "";
    services.router.navigate({
      kind: "session",
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
      threadId,
    });
  }

  readonly #toggleThreadSwitcher = (): void => {
    this.#threadSwitcherOpen = !this.#threadSwitcherOpen;
    if (!this.#threadSwitcherOpen) this.#threadSwitcherQuery = "";
    this.requestUpdate();
    if (!this.#threadSwitcherOpen) return;
    void this.updateComplete.then(() => {
      this.querySelector<HTMLInputElement>(".thread-switcher-search")?.focus();
    });
  };

  readonly #dismissThreadSwitcherFromPointer = (event: PointerEvent): void => {
    if (!this.#threadSwitcherOpen) return;
    const target = event.target;
    if (
      target instanceof Element
      && target.closest(".thread-switcher") !== null
    ) return;
    this.#threadSwitcherOpen = false;
    this.#threadSwitcherQuery = "";
    this.requestUpdate();
  };

  readonly #threadSwitcherSearchChanged = (event: Event): void => {
    this.#threadSwitcherQuery = (event.currentTarget as HTMLInputElement).value;
    this.requestUpdate();
  };

  readonly #threadSwitcherFilterChanged = (event: Event): void => {
    const filter = (event.currentTarget as HTMLElement).dataset["threadFilter"];
    if (
      filter !== "all"
      && filter !== "running"
      && filter !== "attention"
      && filter !== "removed"
    ) return;
    this.#threadSwitcherFilter = filter;
    this.requestUpdate();
  };

  readonly #threadSwitcherKeydown = (event: KeyboardEvent): void => {
    const rows = [...this.querySelectorAll<HTMLElement>(
      ".thread-switcher-row",
    )];
    if (event.key === "Escape") {
      event.preventDefault();
      this.#threadSwitcherOpen = false;
      this.#threadSwitcherQuery = "";
      this.requestUpdate();
      void this.updateComplete.then(() => {
        this.querySelector<HTMLButtonElement>(".thread-switcher-toggle")?.focus();
      });
      return;
    }
    const target = event.target;
    if (
      target instanceof HTMLInputElement
      || target instanceof HTMLTextAreaElement
      || target instanceof HTMLSelectElement
      || target instanceof HTMLElement && target.isContentEditable
    ) return;
    const row = (event.target as Element | null)?.closest<HTMLElement>(
      ".thread-switcher-row",
    );
    const currentIndex = row === null || row === undefined ? -1 : rows.indexOf(row);
    const nextIndex = event.key === "Home"
      ? 0
      : event.key === "End"
        ? rows.length - 1
        : event.key === "ArrowDown"
          ? Math.min(currentIndex + 1, rows.length - 1)
          : event.key === "ArrowUp"
            ? Math.max(currentIndex - 1, 0)
            : undefined;
    if (nextIndex === undefined || rows.length === 0) return;
    event.preventDefault();
    rows[nextIndex]?.focus();
  };

  readonly #threadSwitcherRowKeydown = (event: KeyboardEvent): void => {
    if (event.key !== "Enter" && event.key !== " ") return;
    if ((event.target as Element | null)?.closest("button") !== null) return;
    const row = event.currentTarget as HTMLElement;
    const threadId = row.dataset["threadSwitcherId"];
    if (threadId === undefined) return;
    event.preventDefault();
    const closed = this.#services.value === undefined
      ? false
      : readSignal(this.#services.value.resumePreferences).closedThreadTabs.includes(threadId);
    this.#activateThreadSwitcherRow(threadId, closed);
  };

  #activateThreadSwitcherRow(threadId: string, removed: boolean): void {
    if (removed) {
      this.#reopenClosedThread(threadId);
      return;
    }
    this.#threadSwitcherOpen = false;
    this.#threadSwitcherQuery = "";
    this.#pendingThreadTabFocus = threadId;
    this.#selectThread(threadId);
    this.requestUpdate();
  }

  #setThreadTabPinned(event: MouseEvent, threadId: string, pinned: boolean): void {
    event.preventDefault();
    event.stopPropagation();
    const thread = this.#store.value?.thread(threadId);
    const services = this.#services.value;
    if (
      thread === undefined
      || thread.session_id !== this.sessionId
      || services === undefined
    ) return;
    services.setThreadTabPinned(threadId, pinned);
    this.requestUpdate();
  }

  #reopenClosedThread(threadId: string): void {
    const thread = this.#store.value?.thread(threadId);
    const services = this.#services.value;
    if (
      thread === undefined
      || thread.session_id !== this.sessionId
      || services === undefined
    ) return;
    services.setThreadTabClosed(threadId, false);
    this.#threadSwitcherOpen = false;
    this.#threadSwitcherQuery = "";
    this.#pendingThreadTabFocus = threadId;
    this.#selectThread(threadId);
    this.requestUpdate();
  }

  #closeThreadTab(event: MouseEvent, threadId: string): void {
    event.preventDefault();
    event.stopPropagation();
    this.#closeThreadTabById(threadId);
  }

  #closeThreadTabById(threadId: string): void {
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) return;
    const thread = store.thread(threadId);
    if (thread === undefined || thread.session_id !== this.sessionId) return;
    services.setThreadTabClosed(threadId, true);
    const closedThreadTabs = new Set(
      readSignal(services.resumePreferences).closedThreadTabs,
    );
    if (this.threadId === threadId) {
      const candidates = store
        .threadsForSession(this.sessionId)
        .filter((candidate) =>
          candidate.id !== threadId
          && !closedThreadTabs.has(candidate.id));
      const fallback = candidates.find((candidate) => candidate.spawned !== true)
        ?? candidates[0];
      if (fallback !== undefined) {
        this.#pendingThreadTabFocus = fallback.id;
        this.#selectThread(fallback.id);
      } else {
        services.router.navigate({
          kind: "session",
          workspaceId: this.workspaceId,
          sessionId: this.sessionId,
        });
        this.openNewThreadSetup();
      }
    } else {
      this.#pendingThreadTabFocus = this.threadId;
      this.requestUpdate();
    }
  }

  #cancelThreadFromSwitcher(event: MouseEvent, threadId: string): void {
    event.preventDefault();
    event.stopPropagation();
    const services = this.#services.value;
    if (services === undefined || this.#connectivityBlocked()) return;
    void services.protocol.cancelTurn(threadId).catch(() => {
      this.#requestError = "Turn could not be stopped.";
      this.requestUpdate();
    });
  }

  #openSubagent(item: Extract<AgentChatItem, { readonly kind: "subagent" }>): void {
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) return;
    services.setThreadTabClosed(item.threadId, false);
    this.#newThreadSetupOpen = false;
    this.#newThreadError = "";
    this.#pendingThreadTabFocus = item.threadId;
    const session = readSignal(store.sessions).find(
      (candidate) => candidate.id === item.sessionId,
    );
    services.router.navigate({
      kind: "session",
      workspaceId: session?.workspaceId ?? this.workspaceId,
      sessionId: item.sessionId,
      threadId: item.threadId,
    });
    this.requestUpdate();
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
      || store.isSessionTombstoned(event.detail.sessionId)
    ) return;
    event.preventDefault();
    const token: NewThreadRequestToken = {
      workspaceId: this.workspaceId,
      sessionId: this.sessionId,
      initialThreadId: this.threadId,
    };
    this.#newThreadRequest = token;
    this.#newThreadBusy = true;
    this.#newThreadError = "";
    this.requestUpdate();
    let createdThreadId: string | undefined;
    try {
      let request = event.detail.request;
      const prompt = event.detail.initialMessage?.content.trim() ?? "";
      if (prompt !== "") {
        const abort = new AbortController();
        const timeout = globalThis.setTimeout(() => abort.abort(), THREAD_TITLE_TIMEOUT_MS);
        try {
          const generated = await services.protocol.generateSessionTitle(prompt, {
            signal: abort.signal,
          });
          if (!this.#isCurrentNewThreadRequest(token)) return;
          if (generated.title.trim() !== "") {
            request = { ...request, title: generated.title.trim() };
          }
        } catch {
          if (!this.#isCurrentNewThreadRequest(token)) return;
          // The request already carries the same bounded prompt fallback used
          // when session-title generation is unavailable.
        } finally {
          globalThis.clearTimeout(timeout);
        }
      }
      if (!this.#isCurrentNewThreadRequest(token)) return;
      const thread = await services.protocol.createThread(request);
      if (!this.#isCurrentNewThreadRequest(token)) return;
      createdThreadId = thread.id;
      token.createdThreadId = thread.id;
      store.upsertThread(thread);
      this.#newThreadSetupOpen = false;
      services.router.navigate({
        kind: "session",
        workspaceId: token.workspaceId,
        sessionId: token.sessionId,
        threadId: thread.id,
      });
      this.requestUpdate();
      if (event.detail.initialMessage !== undefined) {
        if (!this.#isCurrentNewThreadRequest(token)) return;
        await services.protocol.sendMessage(thread.id, event.detail.initialMessage);
        if (!this.#isCurrentNewThreadRequest(token)) return;
      }
    } catch {
      if (!this.#isCurrentNewThreadRequest(token)) return;
      if (createdThreadId === undefined) {
        this.#newThreadError = "Thread could not be created. Review the setup and try again.";
      } else {
        const content = event.detail.initialMessage?.content ?? "";
        if (content !== "" && this.#composerDraft === "") {
          this.#composerDraft = content;
          this.#composerCursor = this.#composerDraft.length;
          this.#scheduleComposerDraftPersistence();
        }
        this.#requestError =
          "Thread was created, but its first message could not be sent. The message was not queued.";
      }
    } finally {
      if (this.#newThreadRequest === token) {
        this.#newThreadRequest = undefined;
        this.#newThreadBusy = false;
        if (this.isConnected) this.requestUpdate();
      }
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
        const queue = await services.protocol.listQueue(threadId);
        if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
        store.replaceThreadQueue(threadId, queue);
      } catch {
        if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
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

  #keyboardOrderedQueue(queue: readonly QueuedPrompt[]): readonly QueuedPrompt[] {
    if (!this.#queueKeyboardStateValid(queue)) return queue;
    const byId = new Map(queue.map((prompt) => [prompt.id, prompt]));
    const ordered = this.#queueKeyboardOrder.map((id) => byId.get(id));
    return ordered.every((prompt): prompt is QueuedPrompt => prompt !== undefined)
      ? ordered
      : queue;
  }

  #queueKeyboardStateValid(queue: readonly QueuedPrompt[]): boolean {
    if (
      this.#queueKeyboardDragId === ""
      || this.#queueKeyboardOrder.length !== queue.length
      || !this.#queueKeyboardOrder.includes(this.#queueKeyboardDragId)
    ) return false;
    const queueIds = new Set(queue.map(({ id }) => id));
    return this.#queueKeyboardOrder.every((id) => queueIds.has(id));
  }

  #queueRowKeyDown(
    event: KeyboardEvent,
    queue: readonly QueuedPrompt[],
    index: number,
    disabled: boolean,
  ): void {
    if (
      event.target !== event.currentTarget
      || event.altKey
      || event.ctrlKey
      || event.metaKey
      || event.isComposing
    ) return;
    const prompt = queue[index];
    if (prompt === undefined) return;
    if (
      this.#queueKeyboardDragId !== ""
      && !this.#queueKeyboardStateValid(queue)
    ) {
      this.#queueKeyboardDragId = "";
      this.#queueKeyboardOrder = [];
      this.#queueStatus = "The queue changed, so reordering was canceled.";
    }
    const pickOrDrop = event.key === " " || event.key === "Enter";
    if (this.#queueKeyboardDragId === "") {
      if (!pickOrDrop || disabled || queue.length < 2) return;
      event.preventDefault();
      this.#queueKeyboardDragId = prompt.id;
      this.#queueKeyboardOrder = queue.map(({ id }) => id);
      this.#queueStatus = `Picked up queued prompt ${index + 1} of ${queue.length}.`;
      this.requestUpdate();
      void this.updateComplete.then(() => this.#focusQueueRowNow(prompt.id));
      return;
    }
    if (this.#queueKeyboardDragId !== prompt.id) return;
    if (event.key === "Escape") {
      event.preventDefault();
      this.#cancelQueueKeyboardReorder(prompt.id);
      return;
    }
    if (pickOrDrop) {
      event.preventDefault();
      if (!disabled) void this.#commitQueueKeyboardReorder();
      return;
    }
    const current = this.#queueKeyboardOrder.indexOf(prompt.id);
    if (current < 0) return;
    const destination = event.key === "ArrowUp"
      ? current - 1
      : event.key === "ArrowDown"
        ? current + 1
        : event.key === "Home"
          ? 0
          : event.key === "End"
            ? this.#queueKeyboardOrder.length - 1
            : undefined;
    if (destination === undefined) return;
    event.preventDefault();
    if (destination < 0 || destination >= this.#queueKeyboardOrder.length) {
      this.#queueStatus = `Queued prompt is already at position ${current + 1} of ${queue.length}.`;
      this.requestUpdate();
      return;
    }
    const next = [...this.#queueKeyboardOrder];
    next.splice(current, 1);
    next.splice(destination, 0, prompt.id);
    this.#queueKeyboardOrder = next;
    this.#queueStatus = `Queued prompt moved to position ${destination + 1} of ${next.length}.`;
    this.requestUpdate();
    void this.updateComplete.then(() => this.#focusQueueRowNow(prompt.id));
  }

  #cancelQueueKeyboardReorder(promptId: string): void {
    this.#queueKeyboardDragId = "";
    this.#queueKeyboardOrder = [];
    this.#queueStatus = "Queue reordering canceled.";
    this.requestUpdate();
    void this.updateComplete.then(() => this.#focusQueueRowNow(promptId));
  }

  async #commitQueueKeyboardReorder(): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const promptId = this.#queueKeyboardDragId;
    const ids = [...this.#queueKeyboardOrder];
    if (
      services === undefined
      || store === undefined
      || this.threadId === ""
      || promptId === ""
      || ids.length < 2
      || this.#queueBusy !== ""
      || this.#connectivityBlocked()
    ) return;
    const threadId = this.threadId;
    const generation = this.#threadInteractionGeneration;
    const current = store.threadView(threadId).queue.map(({ id }) => id);
    const currentIds = new Set(current);
    if (
      current.length !== ids.length
      || ids.some((id) => !currentIds.has(id))
    ) {
      this.#queueKeyboardDragId = "";
      this.#queueKeyboardOrder = [];
      this.#queueStatus = "The queue changed, so reordering was canceled.";
      this.requestUpdate();
      void this.updateComplete.then(() => this.#focusQueueRowNow(promptId));
      return;
    }
    if (ids.every((id, position) => id === current[position])) {
      this.#queueKeyboardDragId = "";
      this.#queueKeyboardOrder = [];
      this.#queueStatus = "Queued prompt was dropped without changing its position.";
      this.requestUpdate();
      void this.updateComplete.then(() => this.#focusQueueRowNow(promptId));
      return;
    }
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
      const position = reordered.findIndex((prompt) => prompt.id === promptId);
      this.#queueStatus = position < 0
        ? "Queue order updated."
        : `Queued prompt dropped at position ${position + 1} of ${reordered.length}.`;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueError = "Queue order could not be changed.";
      }
    } finally {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        this.#queueKeyboardDragId = "";
        this.#queueKeyboardOrder = [];
        this.#queueBusy = "";
        this.requestUpdate();
        await this.updateComplete;
        this.#focusQueueRowNow(promptId);
      }
    }
  }

  readonly #prepareQueueRowDrag = (event: PointerEvent): void => {
    const row = event.currentTarget as HTMLElement;
    const target = event.target;
    row.dataset["queueDragBlocked"] = target instanceof Element
      && target.closest("[data-queue-action]") !== null
      ? "true"
      : "false";
  };

  #clearQueueDragImage(): void {
    this.#queueDragImage?.remove();
    this.#queueDragImage = undefined;
  }

  #installQueueDragImage(row: HTMLElement, transfer: DataTransfer): void {
    this.#clearQueueDragImage();
    const index = row.querySelector<HTMLElement>(".queue-index")?.textContent?.trim() ?? "";
    const content = row.querySelector<HTMLElement>(".queue-row p")?.textContent?.trim() ?? "";
    const preview = document.createElement("div");
    preview.className = "queue-drag-image";
    preview.ariaHidden = "true";
    preview.textContent = `${index} ${content}`.trim();
    document.body.append(preview);
    const bounds = preview.getBoundingClientRect();
    this.#queueDragImage = preview;
    try {
      transfer.setDragImage(
        preview,
        Math.min(16, Math.max(0, bounds.width - 1)),
        Math.max(0, bounds.height - 1),
      );
    } catch {
      // Keep reordering functional in engines that expose but do not yet
      // implement custom drag images.
      this.#clearQueueDragImage();
    }
  }

  #startQueueDrag(event: DragEvent, promptId: string, disabled: boolean): void {
    const row = event.currentTarget as HTMLElement;
    const blocked = row.dataset["queueDragBlocked"] === "true";
    delete row.dataset["queueDragBlocked"];
    if (disabled || blocked) {
      event.preventDefault();
      return;
    }
    this.#queueKeyboardDragId = "";
    this.#queueKeyboardOrder = [];
    this.#queueStatus = "";
    this.#queueDragId = promptId;
    this.#queueDropId = "";
    if (event.dataTransfer !== null) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", promptId);
      this.#installQueueDragImage(row, event.dataTransfer);
    }
  }

  #dragQueueOver(
    event: DragEvent,
    queue: readonly QueuedPrompt[],
    targetId: string,
  ): void {
    if (this.#queueDragId === "" || this.#queueDragId === targetId || this.#queueBusy !== "") {
      return;
    }
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
    const row = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const preferred: QueueDropPlacement = event.clientY >= row.top + row.height / 2
      ? "after"
      : "before";
    const currentQueue = this.#currentQueueForDrag(queue, this.#queueDragId, targetId);
    const placement = effectiveQueueDropPlacement(
      currentQueue,
      this.#queueDragId,
      targetId,
      preferred,
    );
    if (placement === undefined) return;
    if (this.#queueDropId === targetId && this.#queueDropPlacement === placement) return;
    this.#queueDropId = targetId;
    this.#queueDropPlacement = placement;
    this.requestUpdate();
  }

  #currentQueueForDrag(
    fallback: readonly QueuedPrompt[],
    sourceId: string,
    targetId: string,
  ): readonly QueuedPrompt[] {
    if (sourceId === "" || targetId === "" || this.threadId === "") return fallback;
    const current = this.#store.value?.threadView(this.threadId).queue;
    return current !== undefined
      && current.some(({ id }) => id === sourceId)
      && current.some(({ id }) => id === targetId)
      ? current
      : fallback;
  }

  readonly #keepQueueDropActive = (event: DragEvent): void => {
    if (this.#queueDragId === "") return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
  };

  async #dropQueued(
    event: DragEvent,
    queue: readonly QueuedPrompt[],
    targetId: string,
  ): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const store = this.#store.value;
    const promptId = this.#queueDragId
      || event.dataTransfer?.getData("text/plain")
      || "";
    const currentQueue = this.#currentQueueForDrag(queue, promptId, targetId);
    const ids = droppedQueueIds(
      currentQueue,
      promptId,
      targetId,
      this.#queueDropPlacement,
    );
    this.#endQueueDrag();
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
    const byId = new Map(currentQueue.map((prompt) => [prompt.id, prompt]));
    const optimistic = ids.map((id, position) => {
      const prompt = byId.get(id);
      return prompt === undefined ? undefined : { ...prompt, position };
    });
    if (optimistic.some((prompt) => prompt === undefined)) return;
    const optimisticQueue = optimistic as QueuedPrompt[];
    const previousQueue = [...currentQueue];
    this.#queueBusy = promptId;
    this.#queueError = "";
    store.replaceThreadQueue(threadId, optimisticQueue);
    this.requestUpdate();
    try {
      const reordered = await services.protocol.reorderQueue(threadId, ids);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      store.replaceThreadQueue(
        threadId,
        reordered,
      );
      const position = reordered.findIndex((prompt) => prompt.id === promptId);
      this.#queueStatus = position < 0
        ? "Queue order updated."
        : `Queued prompt dropped at position ${position + 1} of ${reordered.length}.`;
    } catch {
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      try {
        const queue = await services.protocol.listQueue(threadId);
        if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
        store.replaceThreadQueue(threadId, queue);
      } catch {
        if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
        const currentIds = store.threadView(threadId).queue.map(({ id }) => id);
        if (
          currentIds.length === ids.length
          && currentIds.every((id, index) => id === ids[index])
        ) {
          store.replaceThreadQueue(threadId, previousQueue);
        }
      }
      this.#queueError = "Queue order could not be changed.";
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
    this.#clearQueueDragImage();
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

  async #sendQueuedNow(promptId: string): Promise<void> {
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
    const view = store.threadView(threadId);
    const activeTurn = view.turnRunning
      ? (this.#latestActiveTurn(view.items) ?? -1)
      : undefined;
    const markedCancellation = activeTurn !== undefined
      && this.#cancelRequestedTurn === undefined;
    if (markedCancellation) this.#cancelRequestedTurn = activeTurn;
    this.#queueBusy = promptId;
    this.#queueError = "";
    this.requestUpdate();
    let dispatched = false;
    try {
      await services.protocol.dispatchQueuedPrompt(promptId);
      if (!this.#isCurrentThreadInteraction(threadId, generation)) return;
      dispatched = true;
    } catch {
      if (this.#isCurrentThreadInteraction(threadId, generation)) {
        if (markedCancellation && this.#cancelRequestedTurn === activeTurn) {
          this.#cancelRequestedTurn = undefined;
        }
        this.#queueError = "This prompt could not be sent now. It remains queued.";
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

  #focusQueueRowNow(promptId: string): void {
    [...this.querySelectorAll<HTMLElement>("[data-queue-id]")]
      .find((element) => element.dataset["queueId"] === promptId)
      ?.focus();
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
      && this.#availableModels().length === 0;
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
      const replacementTurnStarted = items.some((item) =>
        item.kind === "turn-status"
        && item.turn > cancelledTurn
        && (
          item.state.kind === "waiting-for-capacity"
          || item.state.kind === "running"
        ));
      const terminalAcknowledged = state?.kind === "turn-status"
        && state.state.kind !== "waiting-for-capacity"
        && state.state.kind !== "running";
      const noDurableTurnRemains = state === undefined
        && !durableTurnRunning
        && this.#pendingStartTurn === undefined;
      if (
        terminalAcknowledged
        || replacementTurnStarted
        || noDurableTurnRemains
      ) {
        this.#cancelRequestedTurn = undefined;
      }
    }
  }

  #latestActiveTurn(items: readonly ThreadChatItem[]): number | undefined {
    for (let index = items.length - 1; index >= 0; index -= 1) {
      const item = items[index];
      if (
        item?.kind === "turn-status"
        && (item.state.kind === "waiting-for-capacity" || item.state.kind === "running")
      ) {
        return item.turn;
      }
    }
    return undefined;
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

  readonly #steerTurn = async (event: Event): Promise<void> => {
    event.preventDefault();
    const form = (event.currentTarget as HTMLElement).closest<HTMLFormElement>("form");
    if (form !== null) await this.#submitComposer(form, true);
  };

  readonly #sendMessage = async (event: SubmitEvent): Promise<void> => {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    if (this.#queueEditId !== "") {
      await this.#saveQueued(form);
      return;
    }
    await this.#submitComposer(form, false);
  };

  async #submitComposer(form: HTMLFormElement, steering: boolean): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    const textarea = form.elements.namedItem("message") as HTMLTextAreaElement | null;
    const draftContent = textarea?.value ?? "";
    const content = draftContent.trim();
    const attachments = [...this.#pendingAttachments];
    const sessionId = this.sessionId;
    if (
      services === undefined ||
      store === undefined ||
      sessionId === "" ||
      store.isSessionTombstoned(sessionId) ||
      this.threadId === "" ||
      this.#connectivityBlocked() ||
      (content === "" && attachments.length === 0)
    ) return;
    const threadId = this.threadId;
    const requestGeneration = ++this.#turnRequestGeneration;
    const composerCursor = textarea?.selectionStart ?? this.#composerCursor;
    const view = store.threadView(threadId);
    const startingTurn = view?.turnRunning === true
      || this.#pendingStartTurn !== undefined
      || this.#cancelRequestedTurn !== undefined;
    const minimumTurn = (view?.items ?? []).reduce(
      (maximum, item) => "turn" in item ? Math.max(maximum, item.turn + 1) : maximum,
      1,
    );
    const optimistic = steering
      ? undefined
      : {
          id: `optimistic:${threadId}:${requestGeneration}`,
          threadId,
          content,
          attachments,
          minimumTurn,
          disposition: startingTurn ? "queue" as const : "turn" as const,
          queueRevision: view.trackQueueRevision(),
        } satisfies OptimisticPromptSubmission;
    this.#requestPending = true;
    if (!steering) {
      this.#messageRequest = startingTurn ? "queue" : "start";
      this.#optimisticPrompt = optimistic;
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
      if (this.#composerDraftPersistTimer !== undefined) {
        clearTimeout(this.#composerDraftPersistTimer);
        this.#composerDraftPersistTimer = undefined;
      }
      void services.composerDrafts.clear(threadId).catch(() => undefined);
    }
    this.#requestError = "";
    this.requestUpdate();
    try {
      const request = {
        content,
        ...(attachments.length === 0
          ? {}
          : { attachments: attachments.map(({ upload }) => upload) }),
      };
      let acceptedTurn: number | undefined;
      if (steering) {
        await services.protocol.steerTurn(threadId, request);
        if (!this.#isCurrentTurnRequest(sessionId, threadId, requestGeneration)) return;
      } else {
        const accepted = await services.protocol.sendMessage(threadId, request);
        if (!this.#isCurrentTurnRequest(sessionId, threadId, requestGeneration)) {
          if (optimistic !== undefined) this.#clearOptimisticPrompt(optimistic.id);
          return;
        }
        const pendingOptimistic = this.#optimisticPrompt;
        if (optimistic !== undefined && pendingOptimistic?.id === optimistic.id) {
          pendingOptimistic.disposition = accepted.queued ? "queue" : "turn";
          const queuedPrompt = accepted.queued_prompt;
          if (accepted.queued && queuedPrompt != null) {
            pendingOptimistic.durablePrompt = queuedPrompt;
            const acceptedView = this.#store.value?.threadView(threadId);
            const queueChangedSinceSubmission =
              pendingOptimistic.queueRevision.queueChanged();
            this.#reconcileOptimisticPrompt(
              acceptedView?.items ?? [],
              acceptedView?.queue ?? [],
            );
            const acceptedQueue = acceptedView?.queue;
            if (
              acceptedQueue !== undefined
              && shouldMaterializeAcceptedQueuedPrompt(
                optimistic.id,
                this.#optimisticPrompt?.id,
                queuedPrompt.id,
                acceptedQueue,
                queueChangedSinceSubmission,
              )
            ) {
              this.#store.value?.replaceThreadQueue(
                threadId,
                [...acceptedQueue, queuedPrompt],
              );
            }
          }
          if (!accepted.queued && accepted.turn > 0) {
            pendingOptimistic.turn = accepted.turn;
          }
          this.#reconcileOptimisticPrompt(
            this.#store.value?.threadView(threadId)?.items ?? [],
            this.#store.value?.threadView(threadId)?.queue ?? [],
          );
          pendingOptimistic.queueRevision.close();
        }
        if (accepted.queued !== true && accepted.turn > 0) {
          acceptedTurn = accepted.turn;
        }
      }
      if (!this.#isCurrentTurnRequest(sessionId, threadId, requestGeneration)) return;
      if (acceptedTurn !== undefined) this.#pendingStartTurn = acceptedTurn;
      if (steering) {
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
      }
    } catch {
      if (this.#isCurrentTurnRequest(sessionId, threadId, requestGeneration)) {
        if (
          !steering
          && optimistic !== undefined
          && this.#optimisticPrompt?.id === optimistic.id
        ) {
          this.#clearOptimisticPrompt(optimistic.id);
          if (textarea !== null) {
            textarea.value = draftContent;
            textarea.setSelectionRange(composerCursor, composerCursor);
            this.#resizeComposer(textarea);
          }
          this.#composerDraft = draftContent;
          this.#composerCursor = composerCursor;
          this.#pendingAttachments = attachments;
          void services.composerDrafts.save(threadId, {
            text: draftContent,
            cursor: composerCursor,
            attachments,
          });
        }
        this.#requestError = steering
          ? "The active turn could not be steered."
          : "Message could not be sent.";
      }
    } finally {
      if (this.#isCurrentTurnRequest(sessionId, threadId, requestGeneration)) {
        this.#messageRequest = undefined;
        this.#requestPending = false;
        this.requestUpdate();
        await this.updateComplete;
        this.#focusComposerNow();
      }
    }
  }

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
      this.#clearMentionPathsRetry();
    } catch {
      if (generation !== this.#pathsGeneration || this.sessionId !== sessionId) return;
      this.#pathsUnavailableSessionId = sessionId;
      this.#pathsRetryAfter = Date.now() + PATH_REFRESH_INTERVAL_MS;
      this.#scheduleMentionPathsRetry(sessionId);
    } finally {
      if (generation === this.#pathsGeneration) {
        this.#pathsLoadingSessionId = "";
        this.requestUpdate();
      }
    }
  }

  #scheduleMentionPathsRetry(sessionId: string): void {
    this.#clearMentionPathsRetry();
    this.#pathsRetryTimer = globalThis.setTimeout(() => {
      this.#pathsRetryTimer = undefined;
      const token = composerCompletionToken(this.#composerDraft, this.#composerCursor);
      if (!this.isConnected || this.sessionId !== sessionId || token?.kind !== "file") return;
      this.#pathsRetryAfter = 0;
      void this.#ensureSessionPaths();
    }, PATH_REFRESH_INTERVAL_MS);
  }

  #clearMentionPathsRetry(): void {
    if (this.#pathsRetryTimer === undefined) return;
    globalThis.clearTimeout(this.#pathsRetryTimer);
    this.#pathsRetryTimer = undefined;
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
    const requestedTurn = this.#latestActiveTurn(
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
    if (!open) void this.#ensureToolDetails(callId);
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
      await services.protocol.resolveApproval({
        thread_id: threadId,
        call_id: callId,
        decision,
      });
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
        thread_id: this.threadId,
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
    void this.#submitQuestion({
      thread_id: this.threadId,
      request_id: requestId,
      answers: null,
    });
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
