import { ContextProvider } from "@lit/context";
import { html, LitElement, nothing } from "lit";
import { live } from "lit/directives/live.js";
import { repeat } from "lit/directives/repeat.js";

import {
  appServicesContext,
  appStoreContext,
  hostCapabilitiesContext,
  sessionContext,
  threadContext,
  type AppServices,
  workspaceContext,
} from "../contexts/app-contexts.js";
import {
  createBrowserRouter,
  parseRoute,
  routeKey,
  type AppRoute,
  type InspectionPanel,
} from "../router/app-router.js";
import {
  browserCapabilities,
  HostCapabilitiesController,
} from "../services/capabilities.js";
import { createBrowserNotificationAdapter } from "../services/browser-notifications.js";
import { BrowserWakeLockCoordinator } from "../services/browser-wake-lock.js";
import {
  isStandalonePwa,
  requestPwaInstall,
  type PwaInstallPromptEvent,
} from "../services/pwa-install.js";
import {
  createBrowserNotificationPreferencesController,
  type NotificationPreferences,
} from "../services/notification-preferences.js";
import {
  createBrowserGeneralPreferencesController,
  type GeneralPreferences,
} from "../services/general-preferences.js";
import {
  createBrowserChatPreferencesController,
  type ChatPreferences,
} from "../services/chat-preferences.js";
import { createBrowserComposerDraftController } from "../services/composer-drafts.js";
import { createBrowserWorkspaceOrderController } from "../services/workspace-order.js";
import {
  createBrowserWorkspaceListPreferencesController,
  type WorkspaceListGrouping,
  type WorkspaceListOrdering,
} from "../services/workspace-list-preferences.js";
import { createBrowserPullRequestGroupOrderController } from "../services/pull-request-group-order.js";
import {
  appearanceFontFamilyCssValue,
  createBrowserAppearancePreferencesController,
  type AppearancePreferences,
} from "../services/appearance-preferences.js";
import { queryBrowserSystemFontFamilies } from "../services/system-fonts.js";
import {
  AttachmentOperationCapacityError,
  AttachmentEncodingError,
  base64DecodedByteLength,
  encodeAttachment,
  isVideoMime,
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  PendingAttachmentOperations,
  pendingAttachmentPreviewUrl,
  type PendingAttachment,
} from "../services/attachments.js";
import {
  chatPreferencesFromHost,
  generalPreferencesFromHost,
  HostClient,
  HostClientError,
  notificationPreferencesFromHost,
  pullRequestGroupOrderFromHost,
  resumePreferencesFromHost,
  withHostChatPreferences,
  withHostGeneralPreferences,
  withHostNotificationPreferences,
  withHostPullRequestGroupOrder,
  withHostResumePreferences,
  withHostWorkspaceOrder,
  workspaceOrderFromHost,
  type HostPendingCloseRequest,
  type HostPreferences,
} from "../services/host-client.js";
import {
  chatBookmarkForNavigation,
  createBrowserResumePreferencesController,
  preferredSessionThreadId,
  type ChatScrollBookmark,
} from "../services/resume-preferences.js";
import {
  DesktopHostCoordinator,
  type DesktopCloseActions,
} from "../services/desktop-host-coordinator.js";
import { createBrowserProtocolIngress } from "../services/protocol-ingress.js";
import {
  createBrowserSessionNotificationDelivery,
  createNativeSessionNotificationDelivery,
  SessionNotificationCoordinator,
} from "../services/session-notifications.js";
import {
  ProtocolClient,
  type ProtocolAgentPersona,
  type ProtocolEventEnvelope,
  type ProtocolGeneratedSessionTitle,
  type ProtocolModelInfo,
  type ProtocolProvidersResponse,
  type ProtocolSubscriptionHealth,
} from "../services/protocol-client.js";
import { createBrowserThreadIngress } from "../services/thread-ingress.js";
import { SubscriptionHealthController } from "../services/subscription-health-controller.js";
import { ModelCatalogController } from "../services/model-catalog-controller.js";
import {
  createBrowserThemeController,
  isThemePreference,
  THEME_NAMES,
  type ThemePreference,
} from "../services/theme-controller.js";
import { AppStore } from "../state/app-store.js";
import { createSignal, readSignal, withSignalTracking } from "../state/reactivity.js";
import { inboxRecoverySession } from "../state/session-inbox-model.js";
import {
  applyNewSessionModelOptionChange,
  beginNewSessionSubmission,
  beginNewSessionOptionLoad,
  canSubmitNewSession,
  closeNewSessionSetup,
  completeNewSessionSetup,
  createNewSessionSetupLifecycle,
  createNewSessionOptionsLifecycle,
  createNewSessionThreadRequestFromSnapshot,
  createNewThreadOptionEdits,
  interruptNewSessionOptionLoad,
  failNewSessionSetup,
  mergeNewSessionModelCatalogs,
  NEW_SESSION_OPTIONS_TIMEOUT_MS,
  newSessionOptionsAreAuthoritative,
  newSessionOptionsAreLoading,
  newSessionOptionsBlockSubmission,
  newSessionOptionsCatalogWorkspaceId,
  navigateNewSessionSetup,
  newThreadInheritanceForWorkspace,
  reconcileNewThreadDefaults,
  resolveNewSessionBaseRef,
  resolveNewSessionModel,
  resolveNewThreadDefaults,
  openNewSessionSetup,
  openNewSessionSetupForWorkspace,
  sessionTitleFallback,
  settleNewSessionOptionLoad,
  snapshotNewSessionSubmission,
  thinkingOption,
  type NewThreadOptionEdits,
  type NewSessionSetupLifecycle,
} from "./new-session-model.js";
import {
  composerTextareaLayout,
  isComposerCompositionKey,
} from "../components/composer-input-model.js";
import {
  nextHorizontalTabIndex,
  rovingTabIndex,
} from "../components/tab-navigation.js";
import {
  type CommandPaletteActionDetail,
  type TrouveCommandPalette,
} from "../components/command-palette.js";
import type { TrouveThreadScreen } from "../components/thread-screen.js";
import type { TrouveInspectionWorkspace } from "../components/inspection-workspace.js";
import type {
  PullRequestChatDetail,
  PullRequestFixDetail,
} from "../components/pull-requests-dashboard.js";
import {
  sessionRelativeFilePath,
  type ChatFileTarget,
} from "../components/chat-file-link.js";
import { pickAndRegisterWorkspace } from "../components/workspace-settings-model.js";
import { modelHealthPresentations } from "../components/model-health.js";
import {
  modelOptionControls,
  sanitizeModelOptions,
  type ModelOptionChangeDetail,
} from "../components/model-option-controls.js";
import {
  WORKSPACE_PULL_REQUEST_FILTERS,
  WORKSPACE_STATUS_FILTERS,
} from "../components/workspace-session-list-model.js";
import { organizeWorkspaceList } from "../components/workspace-list-model.js";
import {
  fontAwesomeIcon,
  type FontAwesomeIconName,
} from "../components/font-awesome-icon.js";
import "../components/command-palette.js";
import "../components/image-preview.js";
import "../components/session-list.js";
import "../components/session-usage-panel.js";
import "../components/thread-screen.js";
import "../components/model-picker.js";
import "../components/model-options-editor.js";

const SESSION_TITLE_TIMEOUT_MS = 48_000;
const VIDEO_ATTACHMENT_DOWNLOAD_TIMEOUT_MS = 30_000;
const VIDEO_ATTACHMENT_OPEN_CONCURRENCY = 1;
const VIDEO_ATTACHMENT_OPEN_CAPACITY = 8;

const deployment =
  import.meta.env.MODE === "pwa"
    ? "pwa"
    : import.meta.env.MODE === "desktop"
      ? "desktop"
      : "browser";

const INSPECTION_PANELS = [
  "info",
  "diff",
  "files",
  "pr",
  "terminal",
] as const satisfies readonly InspectionPanel[];

const GITHUB_REFRESH_INTERVAL_MS = 30_000;
const SLEEP_ACTIVITY_RECONCILE_INTERVAL_MS = 15_000;
const AUTOMATIC_RETRY_MS = 5_000;

const INSPECTION_PANEL_LABELS: Readonly<Record<
  InspectionPanel,
  { readonly icon: FontAwesomeIconName; readonly label: string }
>> = {
  info: { icon: "circle-info", label: "Details" },
  diff: { icon: "code-compare", label: "Diff" },
  files: { icon: "file-lines", label: "Files" },
  pr: { icon: "code-pull-request", label: "Pull Requests" },
  terminal: { icon: "terminal", label: "Terminal" },
};

export class TrouveApp extends withSignalTracking(LitElement) {
  /** Application screens render in the light DOM so the shared theme and
   * layout styles remain the single visual source of truth. Leaf widgets may
   * still use shadow DOM when their encapsulation is useful. */
  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  readonly #theme = createBrowserThemeController(deployment !== "desktop");
  readonly #router = createBrowserRouter();
  readonly #store = new AppStore({ maxThreadViews: deployment === "pwa" ? 2 : 8 });
  readonly #notifications = createBrowserNotificationAdapter();
  readonly #notificationPreferences = createBrowserNotificationPreferencesController(
    deployment !== "desktop",
  );
  readonly #generalPreferences = createBrowserGeneralPreferencesController(
    deployment !== "desktop",
  );
  // Keep a same-origin mirror even on desktop. The native host remains
  // authoritative whenever it exposes `chat`, while the mirror preserves a
  // newly HMR-added preference across reloads against an older live host that
  // does not know that field yet.
  readonly #chatPreferences = createBrowserChatPreferencesController();
  readonly #composerDrafts = createBrowserComposerDraftController();
  readonly #workspaceOrder = createBrowserWorkspaceOrderController(
    deployment !== "desktop",
  );
  // This is frontend-only presentation state. Keep a same-origin mirror in
  // desktop WebViews as well as browser/PWA deployments so it survives reloads
  // without adding sidebar concerns to the harness protocol.
  readonly #workspaceListPreferences = createBrowserWorkspaceListPreferencesController();
  readonly #pullRequestGroupOrder = createBrowserPullRequestGroupOrderController(
    deployment !== "desktop",
  );
  readonly #appearance = createBrowserAppearancePreferencesController(
    deployment !== "desktop",
  );
  readonly #systemFontFamilies = createSignal<readonly string[]>(Object.freeze([]));
  readonly #resume = createBrowserResumePreferencesController(
    deployment !== "desktop",
  );
  readonly #capabilities = new HostCapabilitiesController(
    browserCapabilities(deployment, this.#notifications),
  );
  readonly #hostClient =
    deployment === "desktop" ? new HostClient(globalThis.location.origin) : undefined;
  readonly #pendingVideoOpens = new PendingAttachmentOperations(
    VIDEO_ATTACHMENT_OPEN_CONCURRENCY,
    VIDEO_ATTACHMENT_OPEN_CAPACITY,
  );
  readonly #desktopCoordinator = this.#hostClient === undefined
    ? undefined
    : new DesktopHostCoordinator(this.#hostClient, {
        confirmAutomaticClose: () => this.#confirmAutomaticDesktopClose(),
        onCloseRequested: (request, actions) =>
          this.#desktopCloseRequested(request, actions),
        onDiagnostic: () => {
          this.#shellNotice = "A desktop integration action could not be completed.";
          this.requestUpdate();
        },
      });
  readonly #browserWakeLock = deployment === "pwa"
    ? new BrowserWakeLockCoordinator()
    : undefined;
  readonly #protocolClient = new ProtocolClient(globalThis.location.origin, {
    mutationHeaders: () => this.#hostClient?.mutationHeaders() ?? {},
  });
  readonly #subscriptionHealth = new SubscriptionHealthController(
    this.#protocolClient,
  );
  readonly #modelCatalog = new ModelCatalogController(this.#protocolClient);
  readonly #sessionNotifications = new SessionNotificationCoordinator(
    this.#notificationPreferences.current,
    this.#hostClient === undefined
      ? createBrowserSessionNotificationDelivery(
          this.#notifications,
          () => globalThis.focus(),
        )
      : createNativeSessionNotificationDelivery(this.#hostClient),
    {
      now: () => Date.now(),
      focused: () => this.#isWindowFocused(),
      visibleSession: (sessionId, threadId) => {
        const route = readSignal(this.#router.route);
        return route.kind === "session" &&
          route.sessionId === sessionId &&
          (threadId === undefined || route.threadId === threadId);
      },
      sessionTitle: (sessionId) =>
        readSignal(this.#store.sessions).find((session) => session.id === sessionId)?.title ?? "",
      activate: (sessionId, threadId) => {
        const session = readSignal(this.#store.sessions).find((item) => item.id === sessionId);
        if (session === undefined) return;
        this.#store.markSessionRead(sessionId);
        this.#router.navigate({
          kind: "session",
          workspaceId: session.workspaceId,
          sessionId,
          ...(threadId === undefined ? {} : { threadId }),
        });
      },
    },
  );
  readonly #nativeHost =
    this.#hostClient === undefined
      ? undefined
      : Object.freeze({
          pickDirectory: () => this.#hostClient!.pickDirectory(),
          pickFiles: () => this.#hostClient!.pickFiles(),
          readClipboardImage: () => this.#hostClient!.readClipboardImage(),
          actOnSessionFile: (
            sessionId: string,
            relativePath: string,
            action: "open" | "reveal",
          ) => this.#hostClient!.actOnSessionFile(sessionId, relativePath, action),
          showNativeNotification: (request: {
            readonly title: string;
            readonly body: string;
            readonly sound: boolean;
            readonly sessionId: string;
            readonly threadId: string | undefined;
          }, onActivate?: () => void) =>
            this.#hostClient!.showNativeNotification(request, onActivate),
          requestUserAttention: () => this.#hostClient!.requestUserAttention(),
        });
  readonly #services: AppServices = Object.freeze({
    deployment,
    now: () => new Date(),
    notifications: this.#notifications,
    notificationPreferences: this.#notificationPreferences.current,
    setNotificationPreferences: (patch: Partial<NotificationPreferences>) =>
      this.#updateNotificationPreferences(patch),
    router: this.#router,
    theme: this.#theme,
    setThemePreference: (preference: ThemePreference) =>
      this.#applyThemePreference(preference),
    appearance: this.#appearance.current,
    setAppearancePreferences: (patch: Partial<AppearancePreferences>) =>
      this.#updateAppearancePreferences(patch),
    systemFontFamilies: this.#systemFontFamilies,
    loadSystemFontFamilies: () => this.#loadSystemFontFamilies(),
    generalPreferences: this.#generalPreferences.current,
    setGeneralPreferences: (patch: Partial<GeneralPreferences>) =>
      this.#updateGeneralPreferences(patch),
    chatPreferences: this.#chatPreferences.current,
    setChatPreferences: (patch: Partial<ChatPreferences>) =>
      this.#updateChatPreferences(patch),
    composerDrafts: this.#composerDrafts,
    tombstoneSession: (sessionId: string) => this.#tombstoneSession(sessionId),
    resumePreferences: this.#resume.current,
    setThreadTabClosed: (threadId: string, closed: boolean) =>
      this.#setThreadTabClosed(threadId, closed),
    setThreadTabPinned: (threadId: string, pinned: boolean) =>
      this.#setThreadTabPinned(threadId, pinned),
    protocol: this.#protocolClient,
    modelCatalog: this.#modelCatalog,
    subscriptionHealth: this.#subscriptionHealth,
    pullRequestGroupOrder: this.#pullRequestGroupOrder.order,
    setPullRequestGroupOrder: (order: readonly string[]) =>
      this.#updatePullRequestGroupOrder(order),
    nativeHost: this.#nativeHost,
  });
  readonly #protocolIngress = createBrowserProtocolIngress(
    this.#protocolClient,
    this.#store,
    {
      onKnownEvent: (event) => this.#receiveKnownServerEvent(event),
      onSessionSummaries: (summaries, cursor) => {
        this.#sessionNotifications.replaceSnapshot(summaries, cursor);
      },
    },
  );
  readonly #threadIngress = createBrowserThreadIngress(this.#protocolClient, this.#store);
  #hostPreferences: HostPreferences | undefined;
  #hostPreferenceWriteGeneration = 0;
  #browserFontFamilies: Promise<readonly string[]> | undefined;
  #hostLoadStarted = false;
  #hostError = false;
  #protocolLoadStarted = false;
  #protocolReady = false;
  #protocolError = false;
  #loadedRouteKey = "";
  #routeGeneration = 0;
  #routeLoading = false;
  #routeError = "";
  #mobilePane: "navigation" | "thread" | "inspection" = "thread";
  #shellNotice = "";
  #connectivityNotice = "";
  #connectivityNoticeTimer: ReturnType<typeof setTimeout> | undefined;
  #workspacePickerPending = false;
  #pwaActivate: (() => void) | undefined;
  #pwaInstallPrompt: PwaInstallPromptEvent | undefined;
  #pwaInstallPending = false;
  #pwaInstallStatus = "";
  #newSessionSetup: NewSessionSetupLifecycle = createNewSessionSetupLifecycle();
  #newThreadSetupOpen = false;
  #newSessionPending = false;
  #newSessionError = "";
  #newSessionWorkspaceId = "";
  #newSessionPreferredBaseRef = "";
  #newSessionBranches: readonly string[] = [];
  #newSessionBaseRef = "";
  #newSessionBranchesPending = false;
  #newSessionBranchError = "";
  #newSessionBranchGeneration = 0;
  #newSessionModes: readonly ProtocolAgentPersona[] = [];
  #newSessionModels: readonly ProtocolModelInfo[] = [];
  #newSessionProviders: ProtocolProvidersResponse | undefined;
  #newSessionSubscriptionHealth: readonly ProtocolSubscriptionHealth[] = [];
  #newSessionModeId = "";
  #newSessionModelId = "";
  #newSessionPermissionMode = "";
  #newSessionThinking = "";
  #newSessionInheritedPermissionMode: string | undefined;
  #newSessionInheritedThinking: string | undefined;
  #newSessionOptionsLifecycle = createNewSessionOptionsLifecycle();
  #newSessionOptionEdits: NewThreadOptionEdits = createNewThreadOptionEdits();
  #newSessionModelOptions: Readonly<Record<string, unknown>> = {};
  #newSessionOptionsError = "";
  #newSessionOptionsStatus = "";
  #newSessionOptionsGeneration = 0;
  #newSessionLiveUnsubscribe: (() => void) | undefined;
  #newSessionPrompt = "";
  #newSessionPromptComposing = false;
  #newSessionAttachments: PendingAttachment[] = [];
  #newSessionAttachmentPending = false;
  #newSessionAttachmentGeneration = 0;
  #pullRequestActionPending = false;
  #collapsedWorkspaceIds = new Set<string>();
  #showArchivedWorkspaceIds = new Set<string>();
  #workspaceActionMenuId = "";
  #workspaceListOptionsOpen = false;
  #workspaceClosePendingId = "";
  #workspaceOrderStatus = "";
  #draggedWorkspaceId = "";
  #workspaceDropTarget = "";
  #workspaceDropAfter = false;
  #pendingFileReveal:
    | (ChatFileTarget & { readonly sessionId: string; readonly path: string })
    | undefined;
  #fileRevealActive = false;
  #desktopClosePrompt:
    | {
        readonly request: HostPendingCloseRequest;
        readonly actions: DesktopCloseActions;
        readonly armed: boolean;
      }
    | undefined;
  #desktopClosePending: "cancel" | "quit-now" | "quit-when-idle" | "" = "";
  #resumePersistTimer: ReturnType<typeof setTimeout> | undefined;
  #githubRefreshTimer: ReturnType<typeof setTimeout> | undefined;
  #githubRefreshPending = false;
  #sleepActivityReconcileTimer: ReturnType<typeof setTimeout> | undefined;
  #sleepActivityReconcilePending = false;
  #protocolRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #hostRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #routeRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #navigationWidth = 260;
  #inspectionWidth = 460;
  #activeResize:
    | {
        readonly side: "navigation" | "inspection";
        readonly pointerId: number;
        readonly startX: number;
        readonly startWidth: number;
      }
    | undefined;
  #stopRouteChanges: (() => void) | undefined;

  readonly #servicesProvider = new ContextProvider(this, {
    context: appServicesContext,
    initialValue: this.#services,
  });
  readonly #storeProvider = new ContextProvider(this, {
    context: appStoreContext,
    initialValue: this.#store,
  });
  readonly #capabilitiesProvider = new ContextProvider(this, {
    context: hostCapabilitiesContext,
    initialValue: this.#capabilities,
  });
  readonly #workspaceScopeProvider = new ContextProvider(this, {
    context: workspaceContext,
    initialValue: { workspaceId: "" },
  });
  readonly #sessionScopeProvider = new ContextProvider(this, {
    context: sessionContext,
    initialValue: { sessionId: "" },
  });
  readonly #threadScopeProvider = new ContextProvider(this, {
    context: threadContext,
    initialValue: { threadId: "" },
  });
  #providedWorkspaceId = "";
  #providedSessionId = "";
  #providedThreadId = "";
  /** Terminal panels that have been opened at least once. Keeping one keyed
   * panel per live session preserves each xterm parser and stream while the
   * user visits another inspection tab or session, like the native
   * controller's per-session TermState map. */
  readonly #terminalSessionIds = new Set<string>();

  override connectedCallback(): void {
    super.connectedCallback();
    this.#stopRouteChanges ??= this.#router.subscribe(this.#routeChanged);
    const connectedRoute = readSignal(this.#router.route);
    if (
      this.#newSessionSetup.status === "open"
      && this.#newSessionSetup.routeKey !== routeKey(connectedRoute)
    ) {
      this.#routeChanged(connectedRoute);
    }
    globalThis.addEventListener("trouve-pwa-update-ready", this.#pwaUpdateReady);
    if (deployment === "pwa") {
      globalThis.addEventListener("beforeinstallprompt", this.#pwaInstallAvailable);
      globalThis.addEventListener("appinstalled", this.#pwaInstalled);
    }
    globalThis.addEventListener("online", this.#retryProtocolAfterConnectivity);
    globalThis.addEventListener("focus", this.#browserWindowFocused);
    globalThis.document?.addEventListener(
      "visibilitychange",
      this.#retryProtocolAfterVisibility,
    );
    globalThis.document?.addEventListener(
      "pointerdown",
      this.#dismissWorkspaceListOptionsFromPointer,
      true,
    );
    globalThis.document?.addEventListener(
      "keydown",
      this.#dismissWorkspaceListOptionsFromKeyboard,
      true,
    );
    this.#browserWakeLock?.start();
    if (this.#hostClient !== undefined) {
      if (!this.#hostLoadStarted) {
        void this.#startDesktopHost();
      } else if (this.#hostPreferences !== undefined && !this.#hostError) {
        this.#startProtocolIngress();
      }
    } else {
      this.#applyAppearanceToElement(this.#appearance.current.get());
      this.#startProtocolIngress();
    }
    if (this.#newSessionSetup.status === "open" && this.#newSessionWorkspaceId !== "") {
      void this.#loadNewSessionOptions(this.#newSessionWorkspaceId, true);
    }
  }

  override disconnectedCallback(): void {
    this.#stopRouteChanges?.();
    this.#stopRouteChanges = undefined;
    globalThis.removeEventListener("trouve-pwa-update-ready", this.#pwaUpdateReady);
    globalThis.removeEventListener("beforeinstallprompt", this.#pwaInstallAvailable);
    globalThis.removeEventListener("appinstalled", this.#pwaInstalled);
    globalThis.removeEventListener("online", this.#retryProtocolAfterConnectivity);
    globalThis.removeEventListener("focus", this.#browserWindowFocused);
    globalThis.document?.removeEventListener(
      "visibilitychange",
      this.#retryProtocolAfterVisibility,
    );
    this.#newSessionOptionsGeneration += 1;
    this.#newSessionAttachmentGeneration += 1;
    this.#newSessionAttachmentPending = false;
    this.#unsubscribeFromNewSessionLiveModels();
    this.#newSessionOptionsLifecycle = interruptNewSessionOptionLoad(
      this.#newSessionOptionsLifecycle,
    );
    globalThis.document?.removeEventListener(
      "pointerdown",
      this.#dismissWorkspaceListOptionsFromPointer,
      true,
    );
    globalThis.document?.removeEventListener(
      "keydown",
      this.#dismissWorkspaceListOptionsFromKeyboard,
      true,
    );
    this.#protocolIngress.stop();
    this.#threadIngress.close();
    if (this.#githubRefreshTimer !== undefined) {
      clearTimeout(this.#githubRefreshTimer);
      this.#githubRefreshTimer = undefined;
    }
    this.#githubRefreshPending = false;
    if (this.#sleepActivityReconcileTimer !== undefined) {
      clearTimeout(this.#sleepActivityReconcileTimer);
      this.#sleepActivityReconcileTimer = undefined;
    }
    this.#sleepActivityReconcilePending = false;
    this.#flushResumePreferences();
    this.#desktopCoordinator?.stop();
    this.#browserWakeLock?.stop();
    this.#clearAutomaticRetryTimers();
    if (this.#connectivityNoticeTimer !== undefined) {
      clearTimeout(this.#connectivityNoticeTimer);
      this.#connectivityNoticeTimer = undefined;
    }
    this.#routeGeneration += 1;
    this.#loadedRouteKey = "";
    this.#protocolLoadStarted = false;
    this.#protocolReady = false;
    super.disconnectedCallback();
  }

  async #startDesktopHost(): Promise<void> {
    if (this.#hostLoadStarted) return;
    if (this.#hostRetryTimer !== undefined) {
      clearTimeout(this.#hostRetryTimer);
      this.#hostRetryTimer = undefined;
    }
    this.#hostLoadStarted = true;
    this.#hostError = false;
    this.requestUpdate();
    try {
      await this.#loadDesktopHost();
      if (!this.isConnected) return;
      this.#desktopCoordinator?.start();
      this.#shellNotice = "";
      this.#startProtocolIngress();
    } catch {
      this.#hostLoadStarted = false;
      this.#hostError = true;
      this.#shellNotice = "Desktop host unavailable; retrying automatically.";
      this.#scheduleHostRetry();
      this.requestUpdate();
    }
  }

  #startProtocolIngress(): void {
    if (this.#protocolLoadStarted || !this.isConnected) return;
    if (this.#protocolRetryTimer !== undefined) {
      clearTimeout(this.#protocolRetryTimer);
      this.#protocolRetryTimer = undefined;
    }
    this.#protocolLoadStarted = true;
    this.#protocolReady = false;
    void this.#protocolIngress
      .start()
      .then(() => {
        this.#protocolReady = true;
        this.#protocolError = false;
        this.#scheduleGithubRefresh(0);
        this.requestUpdate();
      })
      .catch(() => {
        this.#protocolLoadStarted = false;
        this.#protocolReady = false;
        this.#protocolError = true;
        this.#scheduleProtocolRetry();
        this.requestUpdate();
      });
  }

  #scheduleHostRetry(): void {
    if (!this.isConnected || this.#hostRetryTimer !== undefined) return;
    this.#hostRetryTimer = setTimeout(() => {
      this.#hostRetryTimer = undefined;
      if (globalThis.document?.visibilityState === "hidden") {
        this.#scheduleHostRetry();
        return;
      }
      void this.#startDesktopHost();
    }, AUTOMATIC_RETRY_MS);
  }

  #scheduleProtocolRetry(): void {
    if (!this.isConnected || this.#protocolRetryTimer !== undefined) return;
    this.#protocolRetryTimer = setTimeout(() => {
      this.#protocolRetryTimer = undefined;
      if (
        globalThis.document?.visibilityState === "hidden"
        || globalThis.navigator?.onLine === false
      ) {
        this.#scheduleProtocolRetry();
        return;
      }
      this.#startProtocolIngress();
    }, AUTOMATIC_RETRY_MS);
  }

  readonly #retryProtocolAfterConnectivity = (): void => {
    if (this.#protocolError || !this.#protocolLoadStarted) {
      this.#startProtocolIngress();
    }
  };

  #isWindowFocused(): boolean {
    const coordinator = this.#desktopCoordinator;
    if (coordinator === undefined) return globalThis.document?.hasFocus() ?? true;
    const lifecycle = readSignal(coordinator.lifecycle);
    return lifecycle.focused && lifecycle.visible && !lifecycle.occluded;
  }

  readonly #browserWindowFocused = (): void => {
    const route = readSignal(this.#router.route);
    if (route.kind === "session") this.#store.markSessionRead(route.sessionId);
  };

  readonly #receiveKnownServerEvent = (event: ProtocolEventEnvelope): void => {
    this.#sessionNotifications.receive(event);
    if (
      event.type === "session.deleted"
      || (event.type === "session.summary_updated" && event.summary === null)
    ) {
      this.#tombstoneSession(event.session_id);
      return;
    }
    if (event.type === "session.summary_updated") {
      // ProtocolIngress invokes this hook immediately before folding the
      // replacement summary. Defer one microtask so a focused visible session
      // advances its client-local seen cursor to the just-applied state, as
      // the native controller does instead of flashing an unread badge.
      queueMicrotask(() => {
        const route = readSignal(this.#router.route);
        if (
          route.kind === "session"
          && route.sessionId === event.session_id
          && this.#isWindowFocused()
        ) {
          this.#store.markSessionRead(event.session_id);
        }
      });
    }
    if (event.type !== "server.connectivity_changed") return;
    const wasOffline = readSignal(this.#store.serverInfo)?.online === false;
    if (event.online && wasOffline) {
      // The catalog may contain only local models when bootstrap overlaps a
      // transient offline probe. Recovery makes the remote roster usable
      // again, so invalidate both the static and live snapshots immediately.
      void this.#modelCatalog.refresh("force").catch(() => undefined);
    }
    if (this.#connectivityNoticeTimer !== undefined) {
      clearTimeout(this.#connectivityNoticeTimer);
      this.#connectivityNoticeTimer = undefined;
    }
    this.#connectivityNotice = event.online && wasOffline
      ? "Back online — the full model list is available again."
      : "";
    if (this.#connectivityNotice !== "") {
      this.#connectivityNoticeTimer = setTimeout(() => {
        this.#connectivityNoticeTimer = undefined;
        this.#connectivityNotice = "";
        this.requestUpdate();
      }, 6_000);
    }
    this.requestUpdate();
  };

  #tombstoneSession(sessionId: string): void {
    const threadIds = new Set(this.#store.sessionThreadIds(sessionId));
    const route = readSignal(this.#router.route);
    if (route.kind === "session" && route.sessionId === sessionId) {
      if (route.threadId !== undefined) threadIds.add(route.threadId);
      // Invalidate every route await and live callback synchronously, before
      // the store loses the session/thread association used for draft cleanup.
      this.#threadIngress.invalidateSession(sessionId);
      this.#loadedRouteKey = "";
      this.#router.navigate({ kind: "inbox" }, true);
    }
    this.#store.removeSession(sessionId);
    for (const threadId of threadIds) {
      void this.#composerDrafts.discard(threadId).catch(() => undefined);
    }
  }

  readonly #retryProtocolAfterVisibility = (): void => {
    if (globalThis.document?.visibilityState !== "visible") return;
    if (this.#hostError) void this.#startDesktopHost();
    this.#retryProtocolAfterConnectivity();
    if (this.#protocolReady) this.#scheduleGithubRefresh(0);
  };

  #clearAutomaticRetryTimers(): void {
    if (this.#protocolRetryTimer !== undefined) clearTimeout(this.#protocolRetryTimer);
    if (this.#hostRetryTimer !== undefined) clearTimeout(this.#hostRetryTimer);
    if (this.#routeRetryTimer !== undefined) clearTimeout(this.#routeRetryTimer);
    this.#protocolRetryTimer = undefined;
    this.#hostRetryTimer = undefined;
    this.#routeRetryTimer = undefined;
  }

  #scheduleGithubRefresh(delayMs = GITHUB_REFRESH_INTERVAL_MS): void {
    if (!this.isConnected || !this.#protocolReady) return;
    if (this.#githubRefreshTimer !== undefined) clearTimeout(this.#githubRefreshTimer);
    this.#githubRefreshTimer = setTimeout(() => {
      this.#githubRefreshTimer = undefined;
      void this.#refreshGithubPullRequests();
    }, Math.max(0, delayMs));
  }

  async #refreshGithubPullRequests(): Promise<void> {
    if (!this.isConnected || !this.#protocolReady || this.#githubRefreshPending) return;
    if (
      globalThis.document?.visibilityState === "hidden" ||
      globalThis.navigator?.onLine === false
    ) {
      this.#scheduleGithubRefresh();
      return;
    }
    this.#githubRefreshPending = true;
    try {
      // Account snapshots are durable server events. Refreshing them at the
      // shell level keeps session PR badges current even
      // when the full Pull Requests route has never been opened.
      await this.#protocolClient.refreshGithubPrs();
    } catch {
      // This background refresh is intentionally quiet. Projection consumers
      // retain their last durable snapshot while the next scheduled pass
      // retries after connectivity returns.
    } finally {
      this.#githubRefreshPending = false;
      this.#scheduleGithubRefresh();
    }
  }

  async #loadDesktopHost(): Promise<void> {
    const host = this.#hostClient;
    if (host === undefined) return;
    this.#capabilities.update(await host.bootstrap());
    this.#systemFontFamilies.set(host.systemFontFamilies());
    const preferences = await host.getPreferences();
    this.#hostPreferences = preferences;
    this.#applyHostPreferences(preferences);
    if (isThemePreference(preferences.appearance.theme)) {
      this.#theme.setPreference(preferences.appearance.theme);
    }
  }

  #loadSystemFontFamilies(): Promise<readonly string[]> {
    const host = this.#hostClient;
    if (host !== undefined) {
      const families = host.systemFontFamilies();
      this.#systemFontFamilies.set(families);
      return Promise.resolve(families);
    }
    this.#browserFontFamilies ??= queryBrowserSystemFontFamilies().then((families) => {
      this.#systemFontFamilies.set(families);
      return families;
    });
    return this.#browserFontFamilies;
  }

  #applyHostPreferences(preferences: HostPreferences): void {
    this.#navigationWidth = preferences.navigation_width;
    this.#inspectionWidth = preferences.inspection_width;
    this.style.setProperty(
      "--trouve-navigation-width",
      `${preferences.navigation_width}px`,
    );
    this.style.setProperty(
      "--trouve-inspection-width",
      `${preferences.inspection_width}px`,
    );
    const appearance = this.#appearance.replace({
      fontFamily: preferences.appearance.font_family,
      fontSize: preferences.appearance.font_size,
      reduceMotion: preferences.appearance.reduce_motion,
    }, false);
    this.#applyAppearanceToElement(appearance);
    this.#generalPreferences.replace(generalPreferencesFromHost(preferences), false);
    this.#chatPreferences.replace(chatPreferencesFromHost(
      preferences,
      readSignal(this.#chatPreferences.current),
    ));
    this.#notificationPreferences.replace(
      notificationPreferencesFromHost(preferences),
      false,
    );
    this.#workspaceOrder.replace(workspaceOrderFromHost(preferences), false);
    this.#pullRequestGroupOrder.replace(
      pullRequestGroupOrderFromHost(preferences),
      false,
    );
    this.#resume.replace(resumePreferencesFromHost(preferences), false);
    this.#syncDesktopActivity();
  }

  #applyAppearanceToElement(preferences: AppearancePreferences): void {
    this.style.setProperty("--trouve-font-size", `${preferences.fontSize}px`);
    this.style.setProperty(
      "--trouve-settings-info-font-size",
      `${preferences.fontSize * (11 / 13)}px`,
    );
    if (preferences.fontFamily === "") {
      this.style.removeProperty("--trouve-font-sans");
    } else {
      this.style.setProperty(
        "--trouve-font-sans",
        appearanceFontFamilyCssValue(preferences.fontFamily),
      );
    }
    this.toggleAttribute("data-reduce-motion", preferences.reduceMotion);
  }

  #updateAppearancePreferences(patch: Partial<AppearancePreferences>): void {
    const appearance = this.#appearance.update(patch);
    this.#applyAppearanceToElement(appearance);
    const host = this.#hostClient;
    const current = this.#hostPreferences;
    if (host === undefined || current === undefined) return;
    const next: HostPreferences = {
      ...current,
      appearance: {
        ...current.appearance,
        font_family: appearance.fontFamily,
        font_size: appearance.fontSize,
        reduce_motion: appearance.reduceMotion,
      },
    };
    this.#persistHostPreferences(next, false);
  }

  #updateGeneralPreferences(patch: Partial<GeneralPreferences>): void {
    const general = this.#generalPreferences.update(patch);
    const current = this.#hostPreferences;
    if (current !== undefined) {
      this.#persistHostPreferences(withHostGeneralPreferences(current, general));
    }
    this.#syncDesktopActivity();
    this.requestUpdate();
  }

  #updateChatPreferences(patch: Partial<ChatPreferences>): void {
    const chat = this.#chatPreferences.update(patch);
    const current = this.#hostPreferences;
    if (current !== undefined) {
      this.#persistHostPreferences(withHostChatPreferences(current, chat));
    }
    this.requestUpdate();
  }

  #updateNotificationPreferences(patch: Partial<NotificationPreferences>): void {
    const notifications = this.#notificationPreferences.update(patch);
    const current = this.#hostPreferences;
    if (current !== undefined) {
      this.#persistHostPreferences(
        withHostNotificationPreferences(current, notifications),
      );
    }
    this.requestUpdate();
  }

  #persistHostPreferences(preferences: HostPreferences, reportError = true): void {
    const host = this.#hostClient;
    if (host === undefined) return;
    this.#hostPreferences = preferences;
    const generation = ++this.#hostPreferenceWriteGeneration;
    void host.putPreferences(preferences).then((saved) => {
      // HostClient coalesces queued writes, so only the newest caller may
      // adopt its response. An older request can finish after a newer local
      // edit was submitted; applying that response would erase the edit.
      if (generation !== this.#hostPreferenceWriteGeneration) return;
      this.#hostPreferences = saved;
      this.#applyHostPreferences(saved);
      if (isThemePreference(saved.appearance.theme)) {
        this.#theme.setPreference(saved.appearance.theme);
      }
      this.requestUpdate();
    }).catch(() => {
      if (generation !== this.#hostPreferenceWriteGeneration || !reportError) return;
      this.#shellNotice = "Desktop preferences could not be saved.";
      this.requestUpdate();
    });
  }

  #persistWorkspaceOrder(order = readSignal(this.#workspaceOrder.order)): void {
    const current = this.#hostPreferences;
    if (current === undefined) return;
    this.#persistHostPreferences(withHostWorkspaceOrder(current, order));
  }

  #updatePullRequestGroupOrder(order: readonly string[]): void {
    const normalized = this.#pullRequestGroupOrder.replace(order);
    const current = this.#hostPreferences;
    if (current === undefined) return;
    this.#persistHostPreferences(
      withHostPullRequestGroupOrder(current, normalized),
    );
  }

  #flushResumePreferences(): void {
    if (this.#resumePersistTimer !== undefined) {
      clearTimeout(this.#resumePersistTimer);
      this.#resumePersistTimer = undefined;
    }
    this.#resume.persist();
    const current = this.#hostPreferences;
    if (current !== undefined) {
      this.#persistHostPreferences(
        withHostResumePreferences(current, readSignal(this.#resume.current)),
      );
    }
  }

  #recordResumeSelection(
    route: Extract<AppRoute, { kind: "session" }>,
  ): void {
    const before = readSignal(this.#resume.current);
    const after = this.#resume.select(route.sessionId, route.threadId, false);
    if (after !== before) this.#flushResumePreferences();
  }

  #setThreadTabClosed(threadId: string, closed: boolean): void {
    const before = readSignal(this.#resume.current);
    const after = this.#resume.setThreadTabClosed(threadId, closed, false);
    if (after !== before) this.#flushResumePreferences();
  }

  #setThreadTabPinned(threadId: string, pinned: boolean): void {
    const before = readSignal(this.#resume.current);
    const after = this.#resume.setThreadTabPinned(threadId, pinned, false);
    if (after !== before) this.#flushResumePreferences();
  }

  readonly #chatPositionChanged = (event: CustomEvent<{
    readonly threadId: string;
    readonly bookmark: ChatScrollBookmark | undefined;
  }>): void => {
    const route = readSignal(this.#router.route);
    if (route.kind === "session" && this.#isWindowFocused()) {
      this.#store.markSessionRead(route.sessionId);
    }
    if (route.kind !== "session" || route.threadId !== event.detail.threadId) return;
    const before = readSignal(this.#resume.current);
    const after = this.#resume.setThreadScroll(
      event.detail.threadId,
      event.detail.bookmark,
      false,
    );
    if (after === before || this.#resumePersistTimer !== undefined) return;
    this.#resumePersistTimer = setTimeout(() => {
      this.#resumePersistTimer = undefined;
      this.#flushResumePreferences();
    }, 250);
  };

  #syncDesktopActivity(): void {
    const workRunning = readSignal(this.#store.sessions).some((session) => session.active);
    const preventSleepWhileRunning =
      readSignal(this.#generalPreferences.current).preventSleepWhileRunning;
    const shouldPreventSleep = workRunning && preventSleepWhileRunning;
    this.#desktopCoordinator?.updateActivity({
      authoritative: this.#protocolReady,
      idle: !workRunning,
      workRunning,
      preventSleepWhileRunning,
    });
    this.#browserWakeLock?.setDesired(shouldPreventSleep);
    this.#scheduleSleepActivityReconciliation(shouldPreventSleep);
  }

  #scheduleSleepActivityReconciliation(shouldPreventSleep: boolean): void {
    if (!shouldPreventSleep) {
      if (this.#sleepActivityReconcileTimer !== undefined) {
        clearTimeout(this.#sleepActivityReconcileTimer);
        this.#sleepActivityReconcileTimer = undefined;
      }
      return;
    }
    if (
      this.#sleepActivityReconcileTimer !== undefined
      || this.#sleepActivityReconcilePending
      || !this.isConnected
    ) return;
    this.#sleepActivityReconcileTimer = setTimeout(() => {
      this.#sleepActivityReconcileTimer = undefined;
      if (!this.isConnected) return;
      this.#sleepActivityReconcilePending = true;
      void this.#protocolIngress.reconcileSessionActivity()
        .catch(() => undefined)
        .finally(() => {
          this.#sleepActivityReconcilePending = false;
          if (this.isConnected) this.#syncDesktopActivity();
        });
    }, SLEEP_ACTIVITY_RECONCILE_INTERVAL_MS);
  }

  #desktopCloseRequested(
    request: HostPendingCloseRequest,
    actions: DesktopCloseActions,
  ): void {
    this.#desktopClosePrompt = { request, actions, armed: request.waitingForIdle };
    this.#desktopClosePending = "";
    this.requestUpdate();
  }

  async #confirmAutomaticDesktopClose(): Promise<boolean> {
    if (!this.#protocolReady || !this.isConnected) {
      throw new Error("protocol activity is not authoritative");
    }
    const reconciled = await this.#protocolIngress.reconcileSessionActivity();
    if (!reconciled) {
      throw new Error("protocol activity could not be reconciled");
    }
    if (!this.#protocolReady || !this.isConnected) {
      throw new Error("protocol activity became unavailable");
    }
    const workRunning = readSignal(this.#store.sessions).some((session) => session.active);
    this.#syncDesktopActivity();
    return !workRunning;
  }

  async #resolveDesktopClose(
    decision: "cancel" | "quit-now" | "quit-when-idle",
  ): Promise<void> {
    const prompt = this.#desktopClosePrompt;
    if (prompt === undefined || this.#desktopClosePending !== "") return;
    this.#desktopClosePending = decision;
    if (decision === "quit-when-idle") {
      this.#desktopClosePrompt = { ...prompt, armed: true };
    }
    this.requestUpdate();
    try {
      if (decision === "cancel") await prompt.actions.cancel();
      else if (decision === "quit-now") await prompt.actions.quitNow();
      else await prompt.actions.quitWhenIdle();
      if (decision === "cancel") this.#desktopClosePrompt = undefined;
    } catch {
      this.#shellNotice = decision === "cancel"
        ? "Automatic quit could not be cancelled."
        : "The desktop close request could not be completed.";
      if (decision === "quit-when-idle") {
        this.#desktopClosePrompt = { ...prompt, armed: false };
      }
    } finally {
      this.#desktopClosePending = "";
      this.requestUpdate();
    }
  }

  readonly #desktopCloseCancelled = (event: Event): void => {
    event.preventDefault();
    void this.#resolveDesktopClose("cancel");
  };

  #panelWidthBounds(side: "navigation" | "inspection"): readonly [number, number] {
    const minimum = side === "navigation" ? 180 : 240;
    const configuredMaximum = side === "navigation" ? 600 : 1_000;
    const other = side === "navigation" ? this.#inspectionWidth : this.#navigationWidth;
    const available = globalThis.innerWidth - other - 420 - 10;
    return [minimum, Math.max(minimum, Math.min(configuredMaximum, available))];
  }

  #setPanelWidth(side: "navigation" | "inspection", requested: number): void {
    const [minimum, maximum] = this.#panelWidthBounds(side);
    const width = Math.round(Math.min(maximum, Math.max(minimum, requested)));
    if (side === "navigation") {
      this.#navigationWidth = width;
      this.style.setProperty("--trouve-navigation-width", `${width}px`);
    } else {
      this.#inspectionWidth = width;
      this.style.setProperty("--trouve-inspection-width", `${width}px`);
    }
    this.requestUpdate();
  }

  #startPanelResize(
    event: PointerEvent,
    side: "navigation" | "inspection",
  ): void {
    if (event.button !== 0) return;
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture(event.pointerId);
    this.#activeResize = {
      side,
      pointerId: event.pointerId,
      startX: event.clientX,
      startWidth:
        side === "navigation" ? this.#navigationWidth : this.#inspectionWidth,
    };
    event.preventDefault();
  }

  readonly #movePanelResize = (event: PointerEvent): void => {
    const resize = this.#activeResize;
    if (resize === undefined || resize.pointerId !== event.pointerId) return;
    const direction = resize.side === "navigation" ? 1 : -1;
    this.#setPanelWidth(
      resize.side,
      resize.startWidth + (event.clientX - resize.startX) * direction,
    );
  };

  readonly #finishPanelResize = (event: PointerEvent): void => {
    if (this.#activeResize?.pointerId !== event.pointerId) return;
    this.#activeResize = undefined;
    this.#persistPanelWidths();
  };

  #resizePanelWithKeyboard(
    event: KeyboardEvent,
    side: "navigation" | "inspection",
  ): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const direction = event.key === "ArrowRight" ? 1 : -1;
    const current = side === "navigation" ? this.#navigationWidth : this.#inspectionWidth;
    this.#setPanelWidth(side, current + direction * (side === "navigation" ? 10 : -10));
    this.#persistPanelWidths();
  }

  #persistPanelWidths(): void {
    const host = this.#hostClient;
    const current = this.#hostPreferences;
    if (host === undefined || current === undefined) return;
    const next: HostPreferences = {
      ...current,
      navigation_width: this.#navigationWidth,
      inspection_width: this.#inspectionWidth,
    };
    this.#persistHostPreferences(next, false);
  }

  #selectTheme(event: Event): void {
    const preference = (event.currentTarget as HTMLSelectElement).value;
    if (!isThemePreference(preference)) return;
    this.#applyThemePreference(preference);
  }

  #applyThemePreference(preference: ThemePreference): void {
    this.#theme.setPreference(preference);
    const host = this.#hostClient;
    const current = this.#hostPreferences;
    if (host === undefined || current === undefined) return;
    const next: HostPreferences = {
      ...current,
      appearance: { ...current.appearance, theme: preference },
    };
    this.#applyHostPreferences(next);
    this.#persistHostPreferences(next, false);
  }

  protected override updated(): void {
    this.#syncDesktopActivity();
    const quitDialog = this.querySelector<HTMLDialogElement>("#desktop-quit-dialog");
    if (this.#desktopClosePrompt !== undefined && quitDialog !== null && !quitDialog.open) {
      try {
        quitDialog.showModal();
      } catch {
        try {
          quitDialog.show();
        } catch {
          // The next update retries after any competing modal closes.
        }
      }
    } else if (this.#desktopClosePrompt === undefined && quitDialog?.open === true) {
      quitDialog.close();
    }
    if (this.#protocolReady) this.#reconcileWorkspaceOrder();
    const route = readSignal(this.#router.route);
    if (route.kind === "settings") void import("../components/settings-screen.js");
    if (route.kind === "automations") void import("../components/automations-screen.js");
    if (route.kind === "reviews") void import("../components/pull-requests-dashboard.js");
    const inspectionVisible =
      !globalThis.matchMedia("(max-width: 760px)").matches ||
      this.#mobilePane === "inspection";
    if (route.kind === "session" && inspectionVisible) {
      const inspection = route.inspection ?? "info";
      if (inspection === "info") void import("../components/session-info-panel.js");
      if (inspection === "terminal") void import("../components/terminal-panel.js");
      if (inspection === "diff" || inspection === "files") {
        void import("../components/inspection-workspace.js");
      }
      if (inspection === "pr") void import("../components/session-pr-panel.js");
    }
    const sessions = readSignal(this.#store.sessions);
    const resume = readSignal(this.#resume.current);
    if (
      this.#protocolReady
      && route.kind === "session"
      && !sessions.some((session) => session.id === route.sessionId)
    ) {
      this.#router.navigate({ kind: "inbox" }, true);
      return;
    }
    const recoverySession = sessions.find(
      (session) => session.id === resume.selectedSessionId,
    ) ?? inboxRecoverySession(sessions);
    if (route.kind === "inbox" && recoverySession !== undefined) {
      const session = recoverySession;
      const threadId = preferredSessionThreadId(
        resume,
        session.id,
        session.latestThreadId,
        this.#store.threadsForSession(session.id).map((thread) => thread.id),
      );
      this.#router.navigate(
        {
          kind: "session",
          workspaceId: session.workspaceId,
          sessionId: session.id,
          ...(threadId === undefined ? {} : { threadId }),
        },
        true,
      );
      return;
    }
    if (route.kind !== "session") {
      if (this.#routeRetryTimer !== undefined) {
        clearTimeout(this.#routeRetryTimer);
        this.#routeRetryTimer = undefined;
      }
      if (this.#loadedRouteKey !== "") {
        this.#loadedRouteKey = "";
        this.#threadIngress.close();
      }
      return;
    }
    if (route.threadId !== undefined) {
      const view = this.#store.threadView(route.threadId);
      const parked = readSignal(this.#resume.current).threadScroll[route.threadId];
      if (parked !== undefined && (view.turnRunning || view.queue.length > 0)) {
        const before = readSignal(this.#resume.current);
        const after = this.#resume.setThreadScroll(route.threadId, undefined, false);
        if (after !== before) this.#flushResumePreferences();
      }
    }
    this.#recordResumeSelection(route);
    const key = `${route.sessionId}\u0000${route.threadId ?? ""}`;
    if (key === this.#loadedRouteKey) return;
    this.#loadedRouteKey = key;
    void this.#loadRoute(route);
  }

  async #loadRoute(route: Extract<AppRoute, { kind: "session" }>): Promise<void> {
    if (this.#routeRetryTimer !== undefined) {
      clearTimeout(this.#routeRetryTimer);
      this.#routeRetryTimer = undefined;
    }
    const generation = ++this.#routeGeneration;
    this.#routeLoading = true;
    this.#routeError = "";
    this.requestUpdate();
    try {
      const selected = await this.#threadIngress.openSession(
        route.sessionId,
        route.threadId,
        readSignal(this.#resume.current).closedThreadTabs,
      );
      if (generation !== this.#routeGeneration) return;
      if (selected !== undefined && selected !== route.threadId) {
        this.#router.navigate({ ...route, threadId: selected }, true);
      }
    } catch {
      if (generation === this.#routeGeneration) {
        this.#routeError = "This session could not be loaded.";
        this.#scheduleRouteRetry(route);
      }
    } finally {
      if (generation === this.#routeGeneration) {
        this.#routeLoading = false;
        this.requestUpdate();
      }
    }
  }

  #scheduleRouteRetry(route: Extract<AppRoute, { kind: "session" }>): void {
    if (!this.isConnected || this.#routeRetryTimer !== undefined) return;
    this.#routeRetryTimer = setTimeout(() => {
      this.#routeRetryTimer = undefined;
      const current = readSignal(this.#router.route);
      if (
        current.kind !== "session"
        || current.sessionId !== route.sessionId
        || current.threadId !== route.threadId
      ) return;
      if (
        globalThis.document?.visibilityState === "hidden"
        || globalThis.navigator?.onLine === false
      ) {
        this.#scheduleRouteRetry(route);
        return;
      }
      this.#loadedRouteKey = "";
      this.requestUpdate();
    }, AUTOMATIC_RETRY_MS);
  }

  #selectInspection(panel: InspectionPanel): void {
    const route = readSignal(this.#router.route);
    if (route.kind !== "session") return;
    this.#router.navigate({ ...route, inspection: panel });
  }

  readonly #openInspection = (
    event: CustomEvent<{ readonly panel: InspectionPanel }>,
  ): void => {
    if (!INSPECTION_PANELS.includes(event.detail.panel)) return;
    this.#selectInspection(event.detail.panel);
    this.#showMobilePane("inspection");
  };

  #selectInspectionWithKeyboard(event: KeyboardEvent, currentIndex: number): void {
    if (event.altKey || event.ctrlKey || event.metaKey) return;
    const nextIndex = nextHorizontalTabIndex(
      event.key,
      currentIndex,
      INSPECTION_PANELS.length,
    );
    const nextPanel = nextIndex === undefined ? undefined : INSPECTION_PANELS[nextIndex];
    if (nextIndex === undefined || nextPanel === undefined) return;
    event.preventDefault();
    const tablist = (event.currentTarget as HTMLElement).closest('[role="tablist"]');
    tablist?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus();
    this.#selectInspection(nextPanel);
  }

  #showMobilePane(pane: "navigation" | "thread" | "inspection"): void {
    this.#mobilePane = pane;
    this.requestUpdate();
  }

  #showNewSession(workspaceId?: string, preferredBaseRef = ""): void {
    const currentRouteKey = routeKey(readSignal(this.#router.route));
    let opening = openNewSessionSetupForWorkspace(
      this.#newSessionSetup,
      currentRouteKey,
      this.#newSessionWorkspaceId,
      workspaceId,
    );
    let { restoringDraft } = opening;
    const workspaces = readSignal(this.#store.workspaces);
    let workspace = restoringDraft
      ? workspaces.find((candidate) => candidate.id === this.#newSessionWorkspaceId)
      : workspaces.find((candidate) => candidate.id === workspaceId) ?? workspaces[0];
    if (restoringDraft && workspace === undefined) {
      this.#resetNewSession();
      opening = openNewSessionSetupForWorkspace(
        this.#newSessionSetup,
        currentRouteKey,
        "",
        workspaceId,
      );
      restoringDraft = false;
      workspace = workspaces.find((candidate) => candidate.id === workspaceId) ?? workspaces[0];
    }
    if (workspace === undefined) return;
    const opened = opening.lifecycle;
    if (opened === this.#newSessionSetup) return;
    this.#newSessionSetup = opened;
    if (!restoringDraft) {
      this.#newSessionError = "";
      this.#newSessionPrompt = "";
      this.#newSessionPromptComposing = false;
      this.#newSessionAttachments = [];
      this.#newSessionAttachmentGeneration += 1;
      this.#newSessionAttachmentPending = false;
      this.#newSessionWorkspaceId = workspace.id;
      this.#resetNewSessionOptionsForWorkspace(workspace.id);
      this.#newSessionPreferredBaseRef = preferredBaseRef;
    } else {
      this.#shellNotice = "";
    }
    this.requestUpdate();
    void this.updateComplete.then(() => {
      const textarea = this.querySelector<HTMLTextAreaElement>(
        "#new-session-screen textarea[name=prompt]",
      );
      textarea?.focus();
      this.#resizeNewSessionPrompt(textarea);
    });
    const setupWorkspaceId = this.#newSessionWorkspaceId;
    if (!restoringDraft) void this.#loadNewSessionBranches(setupWorkspaceId);
    void this.#loadNewSessionOptions(setupWorkspaceId, restoringDraft);
  }

  async #openWorkspace(): Promise<void> {
    const nativeHost = this.#nativeHost;
    if (
      nativeHost === undefined ||
      this.#workspacePickerPending ||
      !readSignal(this.#capabilities.current).directoryPicker
    ) {
      return;
    }
    this.#workspacePickerPending = true;
    this.#shellNotice = "";
    this.requestUpdate();
    try {
      const workspace = await pickAndRegisterWorkspace(
        nativeHost,
        this.#protocolClient,
      );
      if (workspace === undefined) return;
      const current = readSignal(this.#store.workspaces);
      this.#store.replaceWorkspaces([
        ...current.filter((candidate) => candidate.id !== workspace.id),
        workspace,
      ]);
      const collapsed = new Set(this.#collapsedWorkspaceIds);
      collapsed.delete(workspace.id);
      this.#collapsedWorkspaceIds = collapsed;
      this.#shellNotice = `${workspace.name} is ready for new sessions.`;
    } catch {
      this.#shellNotice =
        "Workspace could not be opened. Verify the selected repository and try again.";
    } finally {
      this.#workspacePickerPending = false;
      this.requestUpdate();
    }
  }

  readonly #openCommandPalette = (): void => {
    this.querySelector<TrouveCommandPalette>("trouve-command-palette")?.openPalette();
  };

  readonly #commandPaletteAction = (
    event: CustomEvent<CommandPaletteActionDetail>,
  ): void => {
    event.stopPropagation();
    const { action } = event.detail;
    if (action.kind === "navigate") {
      this.#router.navigate(action.route);
      this.#showMobilePane(action.mobilePane);
      return;
    }
    if (action.kind === "new-session") {
      this.#showNewSession(action.workspaceId);
      return;
    }
    this.#openThreadSetupFromCommandPalette(
      action.workspaceId,
      action.sessionId,
    );
  };

  #openThreadSetupFromCommandPalette(
    workspaceId: string,
    sessionId: string,
  ): void {
    const route = readSignal(this.#router.route);
    if (
      route.kind !== "session"
      || route.workspaceId !== workspaceId
      || route.sessionId !== sessionId
    ) {
      this.#router.navigate({
        kind: "session",
        workspaceId,
        sessionId,
      });
    }
    this.#showMobilePane("thread");
    void this.updateComplete.then(() => {
      this.querySelector<TrouveThreadScreen>("trouve-thread-screen")
        ?.openNewThreadSetup();
    });
  }

  #toggleWorkspace(workspaceId: string): void {
    const collapsed = new Set(this.#collapsedWorkspaceIds);
    if (collapsed.has(workspaceId)) collapsed.delete(workspaceId);
    else collapsed.add(workspaceId);
    this.#collapsedWorkspaceIds = collapsed;
    this.requestUpdate();
  }

  #toggleWorkspaceActions(workspaceId: string): void {
    this.#workspaceListOptionsOpen = false;
    this.#workspaceActionMenuId = this.#workspaceActionMenuId === workspaceId
      ? ""
      : workspaceId;
    this.requestUpdate();
  }

  readonly #toggleWorkspaceListOptions = (): void => {
    this.#workspaceActionMenuId = "";
    this.#workspaceListOptionsOpen = !this.#workspaceListOptionsOpen;
    this.requestUpdate();
  };

  #closeWorkspaceListOptions(restoreFocus: boolean): void {
    if (!this.#workspaceListOptionsOpen) return;
    this.#workspaceListOptionsOpen = false;
    this.requestUpdate();
    if (!restoreFocus) return;
    void this.updateComplete.then(() => {
      if (!this.isConnected) return;
      this.querySelector<HTMLButtonElement>(".workspace-list-options-button")?.focus();
    });
  }

  readonly #dismissWorkspaceListOptionsFromPointer = (event: PointerEvent): void => {
    if (!this.#workspaceListOptionsOpen) return;
    if (event.composedPath().some((target) =>
      target instanceof Element && target.closest(".workspace-list-options-wrap") !== null
    )) return;
    this.#closeWorkspaceListOptions(false);
  };

  readonly #dismissWorkspaceListOptionsFromKeyboard = (event: KeyboardEvent): void => {
    if (!this.#workspaceListOptionsOpen || event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    this.#closeWorkspaceListOptions(true);
  };

  #setWorkspaceListGrouping(event: Event): void {
    const grouping = (event.currentTarget as HTMLSelectElement).value as WorkspaceListGrouping;
    this.#workspaceListPreferences.update({ grouping });
    this.requestUpdate();
  }

  #setWorkspaceListOrdering(event: Event): void {
    const ordering = (event.currentTarget as HTMLSelectElement).value as WorkspaceListOrdering;
    this.#workspaceListPreferences.update({ ordering });
    this.requestUpdate();
  }

  #toggleWorkspaceListShow(option: "showBranches" | "showStatus"): void {
    const current = readSignal(this.#workspaceListPreferences.current);
    this.#workspaceListPreferences.update({ [option]: !current[option] });
    this.requestUpdate();
  }

  #toggleWorkspaceListFilter(
    workspaceId: string,
    category: "status" | "pullRequest",
    index: number,
  ): void {
    this.#workspaceListPreferences.toggleFilter(workspaceId, category, index);
    this.requestUpdate();
  }

  #collapseWorkspaceFromMenu(workspaceId: string): void {
    const collapsed = new Set(this.#collapsedWorkspaceIds);
    collapsed.add(workspaceId);
    this.#collapsedWorkspaceIds = collapsed;
    this.#workspaceActionMenuId = "";
    this.requestUpdate();
  }

  #markWorkspaceRead(workspaceId: string): void {
    for (const session of readSignal(this.#store.sessions)) {
      if (session.workspaceId === workspaceId) this.#store.markSessionRead(session.id);
    }
    this.#workspaceActionMenuId = "";
    this.requestUpdate();
  }

  #toggleArchivedWorkspaceSessions(workspaceId: string): void {
    const next = new Set(this.#showArchivedWorkspaceIds);
    if (next.has(workspaceId)) next.delete(workspaceId);
    else next.add(workspaceId);
    this.#showArchivedWorkspaceIds = next;
    this.#workspaceActionMenuId = "";
    this.requestUpdate();
  }

  async #closeWorkspaceFromNavigation(workspaceId: string, name: string): Promise<void> {
    if (this.#workspaceClosePendingId !== "") return;
    this.#workspaceClosePendingId = workspaceId;
    this.#workspaceActionMenuId = "";
    this.#shellNotice = "";
    this.requestUpdate();
    try {
      await this.#protocolClient.closeWorkspace(workspaceId);
      this.#store.replaceWorkspaces(
        readSignal(this.#store.workspaces).filter((workspace) => workspace.id !== workspaceId),
      );
      const collapsed = new Set(this.#collapsedWorkspaceIds);
      collapsed.delete(workspaceId);
      this.#collapsedWorkspaceIds = collapsed;
      const showArchived = new Set(this.#showArchivedWorkspaceIds);
      showArchived.delete(workspaceId);
      this.#showArchivedWorkspaceIds = showArchived;
      this.#workspaceListPreferences.removeWorkspace(workspaceId);
      const route = readSignal(this.#router.route);
      if (route.kind === "session" && route.workspaceId === workspaceId) {
        this.#router.navigate({ kind: "inbox" }, true);
      }
      this.#shellNotice = `${name} was closed. Its stored sessions and worktrees were kept.`;
    } catch {
      this.#shellNotice = `${name} could not be closed. Try again.`;
    } finally {
      this.#workspaceClosePendingId = "";
      this.requestUpdate();
    }
  }

  #workspaceIdsInDisplayOrder(): readonly string[] {
    return this.#workspaceOrder
      .ordered(readSignal(this.#store.workspaces))
      .map(({ id }) => id);
  }

  #prepareWorkspaceOrder(): readonly string[] {
    const before = readSignal(this.#workspaceOrder.order);
    const next = this.#workspaceOrder
      .reconcile(readSignal(this.#store.workspaces))
      .map(({ id }) => id);
    if (
      next.length !== before.length
      || next.some((id, index) => id !== before[index])
    ) this.#persistWorkspaceOrder(next);
    return next;
  }

  #reconcileWorkspaceOrder(): void {
    this.#prepareWorkspaceOrder();
  }

  #moveWorkspace(workspaceId: string, offset: number): void {
    this.#prepareWorkspaceOrder();
    if (!this.#workspaceOrder.move(workspaceId, offset)) return;
    const after = this.#workspaceIdsInDisplayOrder();
    this.#persistWorkspaceOrder(after);
    const position = after.indexOf(workspaceId);
    this.#workspaceOrderStatus = position < 0
      ? "Workspace order was not changed."
      : `Workspace moved to position ${position + 1} of ${after.length}.`;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.querySelector<HTMLButtonElement>(
        `.workspace-grip[data-workspace-id="${CSS.escape(workspaceId)}"]`,
      )?.focus();
    });
  }

  #moveWorkspaceTo(workspaceId: string, destination: "first" | "last"): void {
    const order = this.#prepareWorkspaceOrder();
    const index = order.indexOf(workspaceId);
    if (index < 0) return;
    const target = destination === "first" ? 0 : order.length - 1;
    this.#moveWorkspace(workspaceId, target - index);
  }

  #workspaceOrderKeyDown(event: KeyboardEvent, workspaceId: string): void {
    if (event.altKey || event.ctrlKey || event.metaKey || event.isComposing) return;
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      this.#moveWorkspace(workspaceId, event.key === "ArrowUp" ? -1 : 1);
      return;
    }
    if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      this.#moveWorkspaceTo(workspaceId, event.key === "Home" ? "first" : "last");
    }
  }

  #startWorkspaceDrag(event: DragEvent, workspaceId: string): void {
    this.#prepareWorkspaceOrder();
    this.#draggedWorkspaceId = workspaceId;
    event.dataTransfer?.setData("application/x-trouve-workspace", workspaceId);
    event.dataTransfer?.setData("text/plain", workspaceId);
    if (event.dataTransfer !== null) event.dataTransfer.effectAllowed = "move";
  }

  #dragOverWorkspace(event: DragEvent, workspaceId: string): void {
    if (this.#draggedWorkspaceId === "" || this.#draggedWorkspaceId === workspaceId) return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
    const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect();
    this.#workspaceDropTarget = workspaceId;
    this.#workspaceDropAfter = event.clientY >= bounds.top + bounds.height / 2;
    this.requestUpdate();
  }

  readonly #keepWorkspaceDropActive = (event: DragEvent): void => {
    if (this.#draggedWorkspaceId === "") return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
  };

  #dropWorkspace(event: DragEvent, targetId: string): void {
    event.preventDefault();
    const workspaceId = this.#draggedWorkspaceId;
    const after = this.#workspaceDropAfter;
    this.#finishWorkspaceDrag();
    if (workspaceId === "" || !this.#workspaceOrder.drop(workspaceId, targetId, after)) return;
    const order = this.#workspaceIdsInDisplayOrder();
    this.#persistWorkspaceOrder(order);
    const position = order.indexOf(workspaceId);
    this.#workspaceOrderStatus = `Workspace moved to position ${position + 1} of ${order.length}.`;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.querySelector<HTMLButtonElement>(
        `.workspace-grip[data-workspace-id="${CSS.escape(workspaceId)}"]`,
      )?.focus();
    });
  }

  readonly #finishWorkspaceDrag = (): void => {
    if (
      this.#draggedWorkspaceId === "" &&
      this.#workspaceDropTarget === ""
    ) return;
    this.#draggedWorkspaceId = "";
    this.#workspaceDropTarget = "";
    this.#workspaceDropAfter = false;
    this.requestUpdate();
  };

  async #loadNewSessionBranches(workspaceId: string): Promise<void> {
    const generation = ++this.#newSessionBranchGeneration;
    this.#newSessionBranchesPending = true;
    this.#newSessionBranchError = "";
    this.#newSessionBranches = [];
    this.#newSessionBaseRef = "HEAD";
    this.requestUpdate();
    try {
      const result = await this.#protocolClient.workspaceBranches(workspaceId);
      if (generation !== this.#newSessionBranchGeneration) return;
      // `default_branch` is selection metadata, not a synthetic option.
      this.#newSessionBranches = [...new Set([...result.branches, "HEAD"])];
      this.#newSessionBaseRef = resolveNewSessionBaseRef(
        result.branches,
        this.#newSessionPreferredBaseRef,
        result.default_branch ?? "",
      );
    } catch {
      if (generation !== this.#newSessionBranchGeneration) return;
      this.#newSessionBranchError =
        "Branches could not be loaded. HEAD will be used.";
      this.#newSessionBranches = ["HEAD"];
      this.#newSessionBaseRef = "HEAD";
    } finally {
      if (generation === this.#newSessionBranchGeneration) {
        this.#newSessionBranchesPending = false;
        this.requestUpdate();
      }
    }
  }

  #unsubscribeFromNewSessionLiveModels(): void {
    this.#newSessionLiveUnsubscribe?.();
    this.#newSessionLiveUnsubscribe = undefined;
  }

  #subscribeToNewSessionLiveModels(
    generation: number,
    workspaceId: string,
    models: readonly ProtocolModelInfo[],
  ): void {
    this.#unsubscribeFromNewSessionLiveModels();
    this.#newSessionLiveUnsubscribe = this.#modelCatalog.subscribeLive(() => {
      if (
        generation !== this.#newSessionOptionsGeneration
        || this.#newSessionOptionsLifecycle.workspaceId !== workspaceId
      ) return;
      this.#reconcileNewSessionDefaults(models);
      this.requestUpdate();
    });
  }

  async #loadNewSessionOptions(
    workspaceId: string,
    preserveSelections = false,
  ): Promise<void> {
    const generation = ++this.#newSessionOptionsGeneration;
    this.#unsubscribeFromNewSessionLiveModels();
    this.#newSessionOptionsError = "";
    this.#newSessionOptionsStatus = "";
    const loadState = beginNewSessionOptionLoad({
      lifecycle: this.#newSessionOptionsLifecycle,
      edits: this.#newSessionOptionEdits,
      inheritedThinking: this.#newSessionInheritedThinking,
      inheritedPermissionMode: this.#newSessionInheritedPermissionMode,
    }, workspaceId, preserveSelections);
    this.#newSessionOptionsLifecycle = loadState.lifecycle;
    this.#newSessionOptionEdits = loadState.edits;
    this.#newSessionInheritedThinking = loadState.inheritedThinking;
    this.#newSessionInheritedPermissionMode = loadState.inheritedPermissionMode;
    // Keep degraded and loading forms reconciled with every live publication;
    // the generation/workspace checks prevent stale loads from changing them.
    this.#subscribeToNewSessionLiveModels(
      generation,
      workspaceId,
      this.#newSessionModels,
    );
    this.#newSessionSubscriptionHealth = readSignal(this.#subscriptionHealth.current);
    this.requestUpdate();

    // Provider usage may launch vendor helpers. It decorates the model picker
    // but is not part of the static setup catalog, so refresh it independently.
    void this.#subscriptionHealth.refresh("if-stale").then(
      (subscriptionHealth) => {
        if (generation !== this.#newSessionOptionsGeneration) return;
        this.#newSessionSubscriptionHealth = subscriptionHealth;
        this.requestUpdate();
      },
      () => undefined,
    );
    // Settle live availability independently, then reconcile it only after
    // this workspace's static metadata has established the initial form.
    const liveModelsPending = this.#modelCatalog.liveModels("if-stale").then(
      () => true,
      () => false,
    );
    const wasBlocking = newSessionOptionsBlockSubmission(loadState.lifecycle);
    const timeout = globalThis.setTimeout(() => {
      if (generation !== this.#newSessionOptionsGeneration) return;
      this.#newSessionOptionsLifecycle = settleNewSessionOptionLoad(
        this.#newSessionOptionsLifecycle,
        workspaceId,
        "timed-out",
      );
      if (wasBlocking) {
        this.#newSessionOptionsError =
          "Agent defaults timed out. Server defaults will be used unless loading finishes.";
      }
      this.requestUpdate();
    }, NEW_SESSION_OPTIONS_TIMEOUT_MS);
    try {
      const [modes, models, providers] = await Promise.all([
        this.#protocolClient.personas(workspaceId),
        this.#modelCatalog.staticModels(),
        this.#protocolClient.providers(),
      ]);
      if (generation !== this.#newSessionOptionsGeneration) return;
      const completedAfterTimeout = this.#newSessionOptionsLifecycle.status === "timed-out";
      this.#newSessionModes = modes;
      this.#newSessionModels = models;
      this.#newSessionProviders = providers;
      this.#newSessionOptionsLifecycle = settleNewSessionOptionLoad(
        this.#newSessionOptionsLifecycle,
        workspaceId,
        "ready",
      );
      this.#newSessionOptionsError = "";
      this.#newSessionOptionsStatus = completedAfterTimeout
        ? wasBlocking
          ? "Agent defaults finished loading. Untouched selections were updated."
          : "Agent defaults refresh finished. Untouched selections were updated."
        : "";
      this.#reconcileNewSessionDefaults(models);
      this.#subscribeToNewSessionLiveModels(generation, workspaceId, models);
      void liveModelsPending.then((liveLoaded) => {
        if (
          !liveLoaded
          || generation !== this.#newSessionOptionsGeneration
          || !newSessionOptionsAreAuthoritative(
            this.#newSessionOptionsLifecycle,
            workspaceId,
          )
        ) return;
        this.#reconcileNewSessionDefaults(models);
        this.requestUpdate();
      });
    } catch {
      if (generation !== this.#newSessionOptionsGeneration) return;
      this.#newSessionOptionsStatus = "";
      this.#newSessionOptionsLifecycle = settleNewSessionOptionLoad(
        this.#newSessionOptionsLifecycle,
        workspaceId,
        "failed",
      );
      this.#newSessionOptionsError = wasBlocking
        ? "Persona and model choices could not be loaded. Server defaults will be used."
        : "Persona and model choices could not be refreshed. Existing choices were preserved.";
    } finally {
      globalThis.clearTimeout(timeout);
      if (generation === this.#newSessionOptionsGeneration) this.requestUpdate();
    }
  }

  #availableNewSessionModels(): readonly ProtocolModelInfo[] {
    return mergeNewSessionModelCatalogs(
      this.#newSessionModels,
      readSignal(this.#modelCatalog.current),
      readSignal(this.#modelCatalog.liveLoaded),
      this.#newSessionOptionEdits.model ? this.#newSessionModelId : undefined,
    );
  }

  #resetNewSessionOptionsForWorkspace(workspaceId: string): void {
    this.#newSessionOptionEdits = createNewThreadOptionEdits();
    this.#newSessionModelOptions = {};
    if (!newSessionOptionsAreAuthoritative(
      this.#newSessionOptionsLifecycle,
      workspaceId,
    )) {
      this.#newSessionModes = [];
      this.#newSessionOptionsLifecycle = createNewSessionOptionsLifecycle();
    }
    const staticModels = this.#newSessionModels.length > 0
      ? this.#newSessionModels
      : readSignal(this.#modelCatalog.staticCurrent);
    const defaults = resolveNewThreadDefaults(
      this.#newSessionModes,
      staticModels,
      this.#newSessionProviders,
    );
    this.#newSessionModeId = defaults.modeId;
    this.#newSessionModelId = defaults.modelId;
    this.#newSessionThinking = defaults.thinking;
    this.#newSessionPermissionMode = defaults.permissionMode;
    const inheritance = newThreadInheritanceForWorkspace(
      defaults,
      newSessionOptionsCatalogWorkspaceId(this.#newSessionOptionsLifecycle),
      workspaceId,
    );
    this.#newSessionInheritedThinking = inheritance.inheritedThinking;
    this.#newSessionInheritedPermissionMode = inheritance.inheritedPermissionMode;
  }

  #reconcileNewSessionDefaults(models: readonly ProtocolModelInfo[]): void {
    const previousMode = this.#newSessionModes.find(
      (mode) => mode.id === this.#newSessionModeId,
    );
    const previousModelId = resolveNewSessionModel(
      this.#newSessionModelId,
      previousMode,
      this.#newSessionProviders,
    );
    const defaults = reconcileNewThreadDefaults(
      {
        modeId: this.#newSessionModeId,
        modelId: this.#newSessionModelId,
        thinking: this.#newSessionThinking,
        permissionMode: this.#newSessionPermissionMode,
      },
      this.#newSessionModes,
      models,
      this.#newSessionProviders,
      this.#newSessionOptionEdits,
      this.#availableNewSessionModels(),
    );
    this.#newSessionModeId = defaults.modeId;
    this.#newSessionModelId = defaults.modelId;
    this.#newSessionThinking = defaults.thinking;
    this.#newSessionPermissionMode = defaults.permissionMode;
    const inheritance = newThreadInheritanceForWorkspace(
      defaults,
      newSessionOptionsCatalogWorkspaceId(this.#newSessionOptionsLifecycle),
      this.#newSessionWorkspaceId,
    );
    this.#newSessionInheritedThinking = inheritance.inheritedThinking;
    this.#newSessionInheritedPermissionMode = inheritance.inheritedPermissionMode;
    const nextMode = this.#newSessionModes.find((mode) => mode.id === defaults.modeId);
    const nextModelId = resolveNewSessionModel(
      defaults.modelId,
      nextMode,
      this.#newSessionProviders,
    );
    const nextModel = this.#availableNewSessionModels().find(
      (model) => model.id === nextModelId,
    );
    this.#newSessionModelOptions = previousModelId === nextModelId
      ? sanitizeModelOptions(nextModel, this.#newSessionModelOptions)
      : {};
  }

  /** Match the retained controller's bounded title-model request. Session
   * creation must remain usable when the managed model or a remote provider
   * accepts a connection but never completes it. */
  async #generateSessionTitle(prompt: string): Promise<ProtocolGeneratedSessionTitle> {
    const abort = new AbortController();
    const timeout = globalThis.setTimeout(() => abort.abort(), SESSION_TITLE_TIMEOUT_MS);
    try {
      return await this.#protocolClient.generateSessionTitle(prompt, {
        signal: abort.signal,
      });
    } finally {
      globalThis.clearTimeout(timeout);
    }
  }

  /** Upgrade a prompt-derived title without delaying session creation or the
   * first turn. A manual rename made while generation is in flight wins. */
  #upgradeSessionTitleInBackground(
    sessionId: string,
    provisionalTitle: string,
    prompt: string,
  ): void {
    void (async () => {
      try {
        const generated = await this.#generateSessionTitle(prompt);
        const title = generated.title.trim();
        if (title === "" || title === provisionalTitle) return;
        if (this.#store.sessionMetadata(sessionId)?.title !== provisionalTitle) return;
        const session = await this.#protocolClient.updateSession(sessionId, {
          title,
          expected_title: provisionalTitle,
        });
        this.#store.upsertSessionMetadata(session);
      } catch {
        // Naming is cosmetic; the deterministic provisional title remains.
      }
    })();
  }

  readonly #selectNewSessionWorkspace = (event: Event): void => {
    const workspaceId = (event.currentTarget as HTMLSelectElement).value;
    this.#newSessionWorkspaceId = workspaceId;
    this.#resetNewSessionOptionsForWorkspace(workspaceId);
    this.#newSessionPreferredBaseRef = "";
    // Catalog loads are asynchronous. Drop every workspace-specific selection
    // before starting them so a fast submit cannot combine the new workspace
    // with the previous workspace's mode, model, or model options.
    this.#newSessionModeId = "";
    this.#newSessionModelId = "";
    this.#newSessionModelOptions = {};
    this.#newSessionModes = [];
    this.#newSessionModels = [];
    this.#newSessionProviders = undefined;
    void this.#loadNewSessionBranches(workspaceId);
    void this.#loadNewSessionOptions(workspaceId);
  };

  readonly #closeNewSession = (): void => {
    if (this.#newSessionPending) return;
    this.#resetNewSession();
    this.requestUpdate();
  };

  readonly #routeChanged = (route: AppRoute): void => {
    if (this.#newSessionSetup.status !== "open") return;
    const next = navigateNewSessionSetup(
      this.#newSessionSetup,
      routeKey(route),
      this.#newSessionPending,
    );
    if (next === this.#newSessionSetup) return;
    const restoreFocus = this.querySelector("#new-session-screen")
      ?.contains(globalThis.document?.activeElement ?? null) === true;
    if (this.#newSessionPending) {
      this.#newSessionSetup = next;
      // Navigation cannot cancel a create request already accepted by the
      // server. Hide its setup without clearing attachments that the request
      // still needs while it finishes in the background.
      this.requestUpdate();
      this.#restoreFocusAfterNewSessionDismissal(restoreFocus);
      return;
    }
    this.#resetNewSession();
    this.requestUpdate();
    this.#restoreFocusAfterNewSessionDismissal(restoreFocus);
  };

  #restoreFocusAfterNewSessionDismissal(restoreFocus: boolean): void {
    if (!restoreFocus) return;
    void this.updateComplete.then(() => {
      this.querySelector<HTMLElement>("main.app-shell")?.focus();
    });
  }

  #resetNewSession(): void {
    this.#newSessionSetup = closeNewSessionSetup(this.#newSessionSetup);
    this.#newSessionError = "";
    this.#newSessionBranchGeneration += 1;
    this.#newSessionOptionsGeneration += 1;
    this.#unsubscribeFromNewSessionLiveModels();
    this.#newSessionBranchesPending = false;
    this.#newSessionBranchError = "";
    this.#newSessionOptionsLifecycle = interruptNewSessionOptionLoad(
      this.#newSessionOptionsLifecycle,
    );
    this.#newSessionOptionsError = "";
    this.#newSessionOptionsStatus = "";
    this.#newSessionAttachments = [];
    this.#newSessionAttachmentGeneration += 1;
    this.#newSessionAttachmentPending = false;
    this.#newSessionPrompt = "";
    this.#newSessionPromptComposing = false;
    this.#newSessionPermissionMode = "";
    this.#newSessionModelOptions = {};
    this.#newSessionPreferredBaseRef = "";
  }

  #resizeNewSessionPrompt(
    textarea = this.querySelector<HTMLTextAreaElement>(
      "#new-session-screen textarea[name=prompt]",
    ),
  ): void {
    if (textarea === null) return;
    textarea.style.height = "auto";
    const layout = composerTextareaLayout(textarea.scrollHeight, textarea.value.length > 0);
    textarea.style.height = `${layout.height}px`;
    textarea.style.overflowY = layout.overflowY;
  }

  readonly #newSessionPromptChanged = (event: InputEvent): void => {
    const textarea = event.currentTarget as HTMLTextAreaElement;
    this.#resizeNewSessionPrompt(textarea);
    this.#newSessionPrompt = textarea.value;
    this.requestUpdate();
  };

  readonly #newSessionPromptCompositionStarted = (): void => {
    this.#newSessionPromptComposing = true;
  };

  readonly #newSessionPromptCompositionEnded = (event: CompositionEvent): void => {
    this.#newSessionPromptComposing = false;
    this.#resizeNewSessionPrompt(event.currentTarget as HTMLTextAreaElement);
  };

  readonly #newSessionPromptKeydown = (event: KeyboardEvent): void => {
    if (isComposerCompositionKey({
      key: event.key,
      keyCode: event.keyCode,
      isComposing: event.isComposing,
      compositionActive: this.#newSessionPromptComposing,
    })) return;
    if (event.key !== "Enter" || event.shiftKey) return;
    event.preventDefault();
    (event.currentTarget as HTMLTextAreaElement).form?.requestSubmit();
  };

  readonly #newSessionFilesSelected = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const files = input.files === null ? [] : [...input.files];
    input.value = "";
    void this.#addNewSessionAttachments(files);
  };

  readonly #newSessionAttachmentPickerClicked = (event: MouseEvent): void => {
    if (
      this.#nativeHost === undefined ||
      !readSignal(this.#capabilities.current).filePicker
    ) {
      return;
    }
    // Keep the existing visually hidden file input as the PWA/remote fallback;
    // only replace its default action when the native bridge is advertised.
    event.preventDefault();
    void this.#pickNewSessionNativeFiles();
  };

  readonly #newSessionPaste = (event: ClipboardEvent): void => {
    // Rich clipboard payloads commonly contain both text and an image. Match
    // the native composer by letting ordinary text paste win in that case.
    if (event.clipboardData?.types.includes("text/plain") === true) return;
    const files = event.clipboardData?.files;
    if (files !== undefined && files.length > 0) {
      event.preventDefault();
      void this.#addNewSessionAttachments([...files]);
      return;
    }
    if (
      this.#nativeHost === undefined ||
      !readSignal(this.#capabilities.current).clipboardImage
    ) {
      return;
    }
    event.preventDefault();
    void this.#readNewSessionClipboardImage();
  };

  async #pickNewSessionNativeFiles(): Promise<void> {
    if (this.#nativeHost === undefined || this.#newSessionAttachmentPending) return;
    const generation = ++this.#newSessionAttachmentGeneration;
    this.#newSessionAttachmentPending = true;
    this.#newSessionError = "";
    this.requestUpdate();
    try {
      const attachments = await this.#nativeHost.pickFiles();
      if (generation !== this.#newSessionAttachmentGeneration) return;
      for (const attachment of attachments) {
        if (!this.#stageNewSessionAttachment(attachment)) break;
      }
    } catch {
      if (generation !== this.#newSessionAttachmentGeneration) return;
      this.#newSessionError = "Files could not be read from the desktop picker.";
    } finally {
      if (generation === this.#newSessionAttachmentGeneration) {
        this.#newSessionAttachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  async #readNewSessionClipboardImage(): Promise<void> {
    if (this.#nativeHost === undefined || this.#newSessionAttachmentPending) return;
    const generation = ++this.#newSessionAttachmentGeneration;
    this.#newSessionAttachmentPending = true;
    this.#newSessionError = "";
    this.requestUpdate();
    try {
      const attachment = await this.#nativeHost.readClipboardImage();
      if (generation !== this.#newSessionAttachmentGeneration) return;
      if (attachment !== undefined) this.#stageNewSessionAttachment(attachment);
    } catch {
      if (generation !== this.#newSessionAttachmentGeneration) return;
      this.#newSessionError = "The desktop clipboard image could not be read.";
    } finally {
      if (generation === this.#newSessionAttachmentGeneration) {
        this.#newSessionAttachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  #stageNewSessionAttachment(attachment: PendingAttachment): boolean {
    if (this.#newSessionAttachments.length >= MAX_PENDING_ATTACHMENTS) {
      this.#newSessionError = `Attach at most ${MAX_PENDING_ATTACHMENTS} files at once.`;
      return false;
    }
    const total = this.#newSessionAttachments.reduce(
      (bytes, pending) => bytes + pending.size,
      attachment.size,
    );
    if (total > MAX_PENDING_ATTACHMENT_BYTES) {
      this.#newSessionError = "Pending attachments exceed the 20 MB mobile memory budget.";
      return false;
    }
    this.#newSessionAttachments = [...this.#newSessionAttachments, attachment];
    return true;
  }

  async #addNewSessionAttachments(files: readonly File[]): Promise<void> {
    if (files.length === 0 || this.#newSessionAttachmentPending) return;
    const generation = ++this.#newSessionAttachmentGeneration;
    this.#newSessionAttachmentPending = true;
    this.#newSessionError = "";
    this.requestUpdate();
    try {
      for (const [index, file] of files.entries()) {
        let attachment: PendingAttachment;
        try {
          attachment = await encodeAttachment(
            file,
            `new-session-${Date.now()}-${index + 1}.bin`,
          );
          if (generation !== this.#newSessionAttachmentGeneration) return;
        } catch (error) {
          if (generation !== this.#newSessionAttachmentGeneration) return;
          const kind = error instanceof AttachmentEncodingError
            ? error.kind
            : "read-failed";
          this.#newSessionError = kind === "too-large"
            ? `${file.name || "Attachment"} is larger than the 10 MB limit.`
            : kind === "empty"
              ? `${file.name || "Attachment"} is empty.`
              : `${file.name || "Attachment"} could not be read.`;
          continue;
        }
        if (!this.#stageNewSessionAttachment(attachment)) break;
      }
    } finally {
      if (generation === this.#newSessionAttachmentGeneration) {
        this.#newSessionAttachmentPending = false;
        this.requestUpdate();
      }
    }
  }

  #removeNewSessionAttachment(index: number): void {
    this.#newSessionAttachments = this.#newSessionAttachments.filter(
      (_, candidate) => candidate !== index,
    );
    this.requestUpdate();
  }

  #formatAttachmentBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  readonly #createSession = async (event: SubmitEvent): Promise<void> => {
    event.preventDefault();
    if (!canSubmitNewSession({
      sessionPending: this.#newSessionPending,
      optionsBlocking: newSessionOptionsBlockSubmission(this.#newSessionOptionsLifecycle),
      attachmentPending: this.#newSessionAttachmentPending,
    })) return;
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const retainedCreateRequest = this.#newSessionSetup.createRequest;
    const workspaceId = retainedCreateRequest?.workspaceId
      ?? String(data.get("workspace_id") ?? "");
    const prompt = String(data.get("prompt") ?? "").trim();
    const baseRef = retainedCreateRequest?.baseRef
      ?? String(data.get("base_ref") ?? "");
    const fetchLatest = retainedCreateRequest?.fetchLatest
      ?? data.get("fetch_latest") === "on";
    if (
      workspaceId === ""
      || (prompt === "" && this.#newSessionAttachments.length === 0)
    ) return;
    const submissionOptions = snapshotNewSessionSubmission({
      selections: {
        modeId: this.#newSessionModeId,
        modelId: this.#newSessionModelId,
        thinking: this.#newSessionThinking,
        permissionMode: this.#newSessionPermissionMode,
      },
      modelOptions: this.#newSessionModelOptions,
      edits: this.#newSessionOptionEdits,
      modes: this.#newSessionModes,
      providers: this.#newSessionProviders,
      selectableModels: this.#availableNewSessionModels(),
      inheritedPermissionMode: this.#newSessionInheritedPermissionMode,
      inheritedThinking: this.#newSessionInheritedThinking,
      optionsAuthoritative: newSessionOptionsAreAuthoritative(
        this.#newSessionOptionsLifecycle,
        workspaceId,
      ),
    });
    const submissionAttachments = this.#newSessionAttachments.map(({ upload }) => upload);
    const title = retainedCreateRequest?.title ?? sessionTitleFallback(prompt);
    const createRequest = {
      workspaceId,
      title,
      baseRef,
      fetchLatest,
    };
    this.#newSessionSetup = beginNewSessionSubmission(
      this.#newSessionSetup,
      () => globalThis.crypto.randomUUID(),
      createRequest,
    );
    const createIdempotencyKey = this.#newSessionSetup.idempotencyKey;
    const submittedCreateRequest = this.#newSessionSetup.createRequest ?? createRequest;
    this.#newSessionPending = true;
    this.#newSessionError = "";
    this.#shellNotice = "";
    this.requestUpdate();

    let session;
    try {
      session = await this.#protocolClient.createSession({
        workspace_id: submittedCreateRequest.workspaceId,
        idempotency_key: createIdempotencyKey,
        title: submittedCreateRequest.title,
        ...(submittedCreateRequest.baseRef === ""
          ? {}
          : { base_ref: submittedCreateRequest.baseRef }),
        fetch_latest: submittedCreateRequest.fetchLatest,
      });
    } catch {
      this.#newSessionSetup = navigateNewSessionSetup(
        this.#newSessionSetup,
        routeKey(readSignal(this.#router.route)),
        true,
      );
      this.#newSessionPending = false;
      this.#newSessionSetup = failNewSessionSetup(this.#newSessionSetup);
      this.#newSessionError = "Session could not be created.";
      if (this.#newSessionSetup.status === "background-failed") {
        this.#shellNotice =
          "Session could not be created. Open New Session to retry with your saved draft.";
      }
      this.requestUpdate();
      return;
    }

    this.#store.upsertSessionMetadata(session);
    if (retainedCreateRequest === undefined) {
      this.#upgradeSessionTitleInBackground(session.id, submittedCreateRequest.title, prompt);
    }
    let threadId: string | undefined;
    try {
      const thread = await this.#protocolClient.createThread(
        createNewSessionThreadRequestFromSnapshot({
          sessionId: session.id,
          title: session.title,
          snapshot: submissionOptions,
        }),
      );
      this.#store.upsertThread(thread);
      threadId = thread.id;
    } catch {
      this.#shellNotice = "Session created, but its first thread could not be created; the prompt was not sent.";
    }
    if (threadId !== undefined) {
      try {
        await this.#protocolClient.sendMessage(threadId, {
          content: prompt,
          ...(submissionAttachments.length === 0
            ? {}
            : { attachments: submissionAttachments }),
        });
      } catch {
        this.#shellNotice = "Session and thread created, but the initial prompt could not be sent.";
      }
    }
    this.#newSessionSetup = navigateNewSessionSetup(
      this.#newSessionSetup,
      routeKey(readSignal(this.#router.route)),
      true,
    );
    const completion = completeNewSessionSetup(this.#newSessionSetup);
    this.#newSessionPending = false;
    this.#newSessionSetup = completion.lifecycle;
    this.#newSessionBranchGeneration += 1;
    this.#newSessionOptionsGeneration += 1;
    this.#unsubscribeFromNewSessionLiveModels();
    this.#newSessionOptionsLifecycle = interruptNewSessionOptionLoad(
      this.#newSessionOptionsLifecycle,
    );
    this.#newSessionPrompt = "";
    this.#newSessionAttachments = [];
    this.#newSessionAttachmentGeneration += 1;
    this.#newSessionModelOptions = {};
    this.#newSessionAttachmentGeneration += 1;
    this.#newSessionAttachmentPending = false;
    this.#newSessionPreferredBaseRef = "";
    form.reset();
    if (!completion.navigateToSession) {
      const notice = `Session “${session.title}” was created in the background.`;
      this.#shellNotice = this.#shellNotice === ""
        ? notice
        : `${notice} ${this.#shellNotice}`;
      this.requestUpdate();
      return;
    }
    this.#router.navigate({
      kind: "session",
      workspaceId: session.workspace_id,
      sessionId: session.id,
      ...(threadId === undefined ? {} : { threadId }),
    });
    this.#showMobilePane("thread");
    this.requestUpdate();
  };

  readonly #openPullRequestChat = (
    event: CustomEvent<PullRequestChatDetail>,
  ): void => {
    void this.#navigateToPullRequestChat(event.detail);
  };

  async #navigateToPullRequestChat(
    detail: PullRequestChatDetail,
  ): Promise<void> {
    const session = readSignal(this.#store.sessions).find((candidate) =>
      candidate.workspaceId === detail.workspaceId &&
      candidate.branch === detail.branch);
    if (session === undefined) {
      this.#showNewSession(detail.workspaceId, detail.branch);
      return;
    }
    const resume = readSignal(this.#resume.current);
    const threadId = resume.sessionThreads[session.id] ?? session.latestThreadId;
    this.#router.navigate({
      kind: "session",
      workspaceId: session.workspaceId,
      sessionId: session.id,
      ...(threadId === undefined ? {} : { threadId }),
    });
    this.#showMobilePane("thread");
    this.requestUpdate();
  }

  readonly #fixPullRequestReview = (
    event: CustomEvent<PullRequestFixDetail>,
  ): void => {
    void this.#startPullRequestFix(event.detail);
  };

  async #startPullRequestFix(detail: PullRequestFixDetail): Promise<void> {
    if (
      this.#pullRequestActionPending ||
      detail.workspaceId === "" ||
      detail.branch === "" ||
      detail.prompt.trim() === ""
    ) return;
    this.#pullRequestActionPending = true;
    this.#shellNotice = "Starting a code thread for the review fix…";
    this.requestUpdate();
    let session = readSignal(this.#store.sessions).find((candidate) =>
      candidate.workspaceId === detail.workspaceId &&
      candidate.branch === detail.branch);
    let sessionId = session?.id;
    let createdSession: Awaited<ReturnType<ProtocolClient["createSession"]>> | undefined;
    try {
      if (sessionId === undefined) {
        const title = sessionTitleFallback(detail.prompt);
        createdSession = await this.#protocolClient.createSession({
          workspace_id: detail.workspaceId,
          title,
          base_ref: detail.branch,
          fetch_latest: true,
        });
        this.#store.upsertSessionMetadata(createdSession);
        this.#upgradeSessionTitleInBackground(createdSession.id, title, detail.prompt);
        sessionId = createdSession.id;
      }

      const thread = await this.#protocolClient.createThread({
        session_id: sessionId,
        title: sessionTitleFallback(detail.prompt),
        mode: "code",
      });
      this.#store.upsertThread(thread);
      let messageSent = true;
      try {
        await this.#protocolClient.sendMessage(thread.id, {
          content: detail.prompt.trim(),
        });
      } catch {
        messageSent = false;
      }
      const workspaceId = createdSession?.workspace_id ?? session?.workspaceId ?? detail.workspaceId;
      this.#router.navigate({
        kind: "session",
        workspaceId,
        sessionId,
        threadId: thread.id,
      });
      this.#showMobilePane("thread");
      this.#shellNotice = messageSent
        ? "The review fix is running in a fresh code thread."
        : "The code thread was created, but the review prompt could not be sent.";
    } catch {
      this.#shellNotice = sessionId === undefined
        ? "A session for this review could not be created."
        : "A code thread for this review could not be created.";
    } finally {
      this.#pullRequestActionPending = false;
      this.requestUpdate();
    }
  }

  readonly #openInternal = (event: CustomEvent<{ readonly href: string }>): void => {
    let pathname: string;
    try {
      const url = new URL(event.detail.href, globalThis.location.origin);
      if (url.origin !== globalThis.location.origin) return;
      pathname = url.pathname;
    } catch {
      return;
    }
    const route = parseRoute(pathname);
    if (route.kind !== "not-found") this.#router.navigate(route);
  };

  readonly #closeFullScreenRoute = (): void => {
    const resume = readSignal(this.#resume.current);
    const sessions = readSignal(this.#store.sessions);
    const session = sessions.find(({ id }) => id === resume.selectedSessionId)
      ?? sessions.find(({ archived }) => !archived)
      ?? sessions[0];
    if (session === undefined) {
      this.#router.navigate({ kind: "inbox" });
    } else {
      const threadId = preferredSessionThreadId(
        resume,
        session.id,
        session.latestThreadId,
        this.#store.threadsForSession(session.id).map((thread) => thread.id),
      );
      this.#router.navigate({
        kind: "session",
        workspaceId: session.workspaceId,
        sessionId: session.id,
        ...(threadId === undefined ? {} : { threadId }),
      });
    }
    this.#showMobilePane("thread");
  };

  readonly #openExternal = (event: CustomEvent<{ readonly href: string }>): void => {
    let url: URL;
    try {
      url = new URL(event.detail.href);
    } catch {
      return;
    }
    if (
      url.protocol !== "https:" ||
      url.username !== "" ||
      url.password !== "" ||
      url.host === ""
    ) {
      return;
    }
    if (deployment === "desktop") {
      const host = this.#hostClient;
      if (host === undefined || !readSignal(this.#capabilities.current).openHttpsUrl) {
        this.#shellNotice = "External links are unavailable in this desktop preview.";
        this.requestUpdate();
        return;
      }
      void host.openHttpsUrl(url.href).catch(() => {
        this.#shellNotice = "The external link could not be opened.";
        this.requestUpdate();
      });
      return;
    }
    globalThis.open(url.href, "_blank", "noopener,noreferrer");
  };

  readonly #openVideo = (event: CustomEvent<{
    readonly source: string;
    readonly name: string;
    readonly mime: string;
  }>): void => {
    event.stopPropagation();
    if (!isVideoMime(event.detail.mime)) return;
    if (deployment === "desktop") {
      const host = this.#hostClient;
      if (
        host === undefined
        || !readSignal(this.#capabilities.current).openVideoAttachment
      ) {
        this.#shellNotice = "External video playback is unavailable in this desktop preview.";
        this.requestUpdate();
        return;
      }
      const pending = this.#pendingVideoOpens.run(event.detail.source, async () => {
        const attachment = await this.#pendingVideoAttachment(event.detail);
        await host.openVideoAttachment(attachment);
      });
      if (pending === undefined) return;
      void pending
        .catch((error: unknown) => {
          this.#shellNotice = error instanceof AttachmentOperationCapacityError
            || (error instanceof HostClientError && error.kind === "video-capacity")
            ? "Temporary video playback capacity is full. Restart trouve to open a different video."
            : "The video could not be opened in the system player.";
          this.requestUpdate();
        });
      return;
    }

    const target = this.#browserVideoTarget(event.detail);
    if (target === undefined) return;
    // `noopener` makes successful `window.open` calls return null in some
    // browsers, which is indistinguishable from popup blocking. Open a blank
    // same-origin context first, sever its opener, then navigate it.
    const opened = globalThis.open("about:blank", "_blank");
    if (opened === null) {
      if (target.revoke) URL.revokeObjectURL(target.href);
      this.#shellNotice = "The browser blocked video playback. Allow pop-ups for trouve and try again.";
      this.requestUpdate();
      return;
    }
    try {
      opened.opener = null;
      opened.location.replace(target.href);
    } catch {
      opened.close();
      if (target.revoke) URL.revokeObjectURL(target.href);
      this.#shellNotice = "The video player could not be opened. Try again or download the attachment.";
      this.requestUpdate();
      return;
    }
    if (target.revoke) {
      globalThis.setTimeout(() => URL.revokeObjectURL(target.href), 60_000);
    }
  };

  #videoDataAttachment(detail: {
    readonly source: string;
    readonly name: string;
    readonly mime: string;
  }): PendingAttachment | undefined {
    const mime = detail.mime.toLowerCase();
    const prefix = `data:${mime};base64,`;
    if (!detail.source.startsWith(prefix)) return undefined;
    const data = detail.source.slice(prefix.length);
    const size = base64DecodedByteLength(data);
    if (size === undefined || size > MAX_ATTACHMENT_BYTES) return undefined;
    return {
      upload: { name: detail.name, mime, data },
      size,
    };
  }

  #protocolVideoUrl(source: string): URL | undefined {
    let url: URL;
    try {
      url = new URL(source, globalThis.location.origin);
    } catch {
      return undefined;
    }
    if (
      url.origin !== globalThis.location.origin
      || !url.pathname.startsWith("/v1/attachments/")
      || url.search !== ""
      || url.hash !== ""
    ) return undefined;
    return url;
  }

  async #pendingVideoAttachment(detail: {
    readonly source: string;
    readonly name: string;
    readonly mime: string;
  }): Promise<PendingAttachment> {
    const encoded = this.#videoDataAttachment(detail);
    if (encoded !== undefined) return encoded;
    const url = this.#protocolVideoUrl(detail.source);
    if (url === undefined) throw new Error("invalid video attachment source");
    const abort = new AbortController();
    const timeout = globalThis.setTimeout(
      () => abort.abort(),
      VIDEO_ATTACHMENT_DOWNLOAD_TIMEOUT_MS,
    );
    let blob: Blob;
    try {
      const response = await globalThis.fetch(url, {
        credentials: "same-origin",
        cache: "no-store",
        signal: abort.signal,
      });
      if (!response.ok) throw new Error("video attachment download failed");
      blob = await response.blob();
    } finally {
      globalThis.clearTimeout(timeout);
    }
    return encodeAttachment(
      new File([blob], detail.name, { type: detail.mime }),
      detail.name,
    );
  }

  #browserVideoTarget(detail: {
    readonly source: string;
    readonly name: string;
    readonly mime: string;
  }): { readonly href: string; readonly revoke: boolean } | undefined {
    const encoded = this.#videoDataAttachment(detail);
    if (encoded === undefined) {
      const url = this.#protocolVideoUrl(detail.source);
      return url === undefined ? undefined : { href: url.href, revoke: false };
    }
    try {
      const binary = globalThis.atob(encoded.upload.data);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) {
        bytes[index] = binary.charCodeAt(index);
      }
      return {
        href: URL.createObjectURL(new Blob([bytes], { type: encoded.upload.mime })),
        revoke: true,
      };
    } catch {
      return undefined;
    }
  }

  readonly #openFile = (event: CustomEvent<ChatFileTarget>): void => {
    event.stopPropagation();
    const route = readSignal(this.#router.route);
    if (route.kind !== "session") return;
    const metadata = this.#store.sessionMetadata(route.sessionId);
    if (metadata === undefined) {
      this.#shellNotice = "The session metadata needed to open that file is not loaded yet.";
      this.requestUpdate();
      return;
    }
    const relativePath = sessionRelativeFilePath(
      event.detail.path,
      metadata.worktree_path,
    );
    if (relativePath === undefined) {
      this.#shellNotice = "That file is outside the active session worktree and was not opened.";
      this.requestUpdate();
      return;
    }
    this.#pendingFileReveal = {
      sessionId: route.sessionId,
      path: relativePath,
      from: event.detail.from,
      to: event.detail.to,
    };
    this.#router.navigate({ ...route, inspection: "files" });
    this.#showMobilePane("inspection");
    void this.#flushPendingFileReveal();
  };

  async #flushPendingFileReveal(): Promise<void> {
    if (this.#fileRevealActive || this.#pendingFileReveal === undefined) return;
    this.#fileRevealActive = true;
    try {
      await import("../components/inspection-workspace.js");
      await this.updateComplete;
      const pending = this.#pendingFileReveal;
      const route = readSignal(this.#router.route);
      if (
        pending === undefined ||
        route.kind !== "session" ||
        route.sessionId !== pending.sessionId ||
        (route.inspection ?? "info") !== "files"
      ) return;
      const workspace = this.querySelector<TrouveInspectionWorkspace>(
        "trouve-inspection-workspace",
      );
      if (workspace === null) {
        this.requestUpdate();
        return;
      }
      this.#pendingFileReveal = undefined;
      await workspace.openFile(pending.path, pending.from, pending.to);
    } catch {
      this.#shellNotice = "The linked file could not be opened.";
      this.requestUpdate();
    } finally {
      this.#fileRevealActive = false;
    }
  }

  readonly #pwaUpdateReady = (
    event: CustomEvent<{ readonly activate: () => void }>,
  ): void => {
    this.#pwaActivate = event.detail.activate;
    this.requestUpdate();
  };

  readonly #pwaInstallAvailable = (event: Event): void => {
    if (deployment !== "pwa" || isStandalonePwa()) return;
    event.preventDefault();
    this.#pwaInstallPrompt = event as PwaInstallPromptEvent;
    this.#pwaInstallStatus = "";
    this.requestUpdate();
  };

  readonly #pwaInstalled = (): void => {
    if (deployment !== "pwa") return;
    this.#pwaInstallPrompt = undefined;
    this.#pwaInstallPending = false;
    this.#pwaInstallStatus = "Trouve was installed.";
    this.requestUpdate();
  };

  async #installPwa(): Promise<void> {
    const prompt = this.#pwaInstallPrompt;
    if (prompt === undefined || this.#pwaInstallPending) return;
    this.#pwaInstallPending = true;
    this.#pwaInstallStatus = "Opening the install prompt…";
    this.requestUpdate();
    const result = await requestPwaInstall(prompt);
    if (!this.isConnected) return;
    this.#pwaInstallPrompt = undefined;
    this.#pwaInstallPending = false;
    this.#pwaInstallStatus = result === "accepted"
      ? "Trouve installation accepted."
      : result === "dismissed"
        ? "Installation dismissed. You can install later from the browser menu."
        : "Trouve could not open the install prompt.";
    this.requestUpdate();
  }

  /** Keep selected route identity in stable, typed contexts at the shell
   * boundary. Inspection surfaces consume these values directly, which keeps
   * one scope definition for siblings and avoids threading IDs through every
   * intermediate template. Gallery/test isolation still works because each
   * consumer retains an explicit-property fallback. */
  #provideRouteScope(route: AppRoute): void {
    const workspaceId = route.kind === "session" ? route.workspaceId : "";
    const sessionId = route.kind === "session" ? route.sessionId : "";
    const threadId = route.kind === "session" ? (route.threadId ?? "") : "";
    if (workspaceId !== this.#providedWorkspaceId) {
      this.#providedWorkspaceId = workspaceId;
      this.#workspaceScopeProvider.setValue({ workspaceId });
    }
    if (sessionId !== this.#providedSessionId) {
      this.#providedSessionId = sessionId;
      this.#sessionScopeProvider.setValue({ sessionId });
    }
    if (threadId !== this.#providedThreadId) {
      this.#providedThreadId = threadId;
      this.#threadScopeProvider.setValue({ threadId });
    }
  }

  override render() {
    const theme = readSignal(this.#theme.theme);
    const themePreference = readSignal(this.#theme.preference);
    const route = readSignal(this.#router.route);
    this.#provideRouteScope(route);
    const resume = readSignal(this.#resume.current);
    const sessions = readSignal(this.#store.sessions);
    const activeSessionCount = sessions.filter((session) => session.active).length;
    const knownWorkspaces = readSignal(this.#store.workspaces);
    readSignal(this.#workspaceOrder.order);
    const orderedWorkspaces = this.#workspaceOrder.ordered(knownWorkspaces);
    const workspaceListPreferences = readSignal(this.#workspaceListPreferences.current);
    const workspaceGroups = organizeWorkspaceList(orderedWorkspaces, workspaceListPreferences.grouping);
    const displayedWorkspaces = workspaceGroups.flatMap(({ workspaces }) => workspaces);
    const workspaceReorderingEnabled = workspaceGroups.every(
      ({ workspaces }) => workspaces.length === 1,
    );
    const repositoryGroupPresentations = new Map(
      workspaceGroups.flatMap((group, groupIndex) =>
        group.repository && group.workspaces.length > 1
          ? group.workspaces.map((workspace, index) => [
              workspace.id,
              {
                headingId: `repository-group-${groupIndex}`,
                label: group.label,
                first: index === 0,
              },
            ] as const)
          : []),
    );
    const capabilities = readSignal(this.#capabilities.current);
    const directoryPickerAvailable =
      capabilities.directoryPicker &&
      this.#nativeHost !== undefined &&
      this.#protocolReady &&
      !this.#hostError;
    const knownWorkspaceIds = new Set(knownWorkspaces.map((workspace) => workspace.id));
    const orphanWorkspaceIds = [
      ...new Set(
        sessions
          .map((session) => session.workspaceId)
          .filter((workspaceId) => !knownWorkspaceIds.has(workspaceId)),
      ),
    ];
    const activeView =
      route.kind === "session" && route.threadId !== undefined
        ? this.#store.threadView(route.threadId)
        : undefined;
    const selectedInspection =
      route.kind === "session" ? (route.inspection ?? "info") : "info";
    const liveSessionIds = new Set(sessions.map((session) => session.id));
    for (const sessionId of this.#terminalSessionIds) {
      if (!liveSessionIds.has(sessionId)) this.#terminalSessionIds.delete(sessionId);
    }
    if (route.kind === "session" && selectedInspection === "terminal") {
      this.#terminalSessionIds.add(route.sessionId);
    }
    const selectedInspectionIndex = INSPECTION_PANELS.indexOf(selectedInspection);
    const activeThread =
      route.kind === "session" && route.threadId !== undefined
        ? this.#store.thread(route.threadId)
        : undefined;
    const selectedNewSessionMode = this.#newSessionModes.find(
      (mode) => mode.id === this.#newSessionModeId,
    );
    const effectiveNewSessionModel = resolveNewSessionModel(
      this.#newSessionModelId,
      selectedNewSessionMode,
      this.#newSessionProviders,
    );
    const newSessionModels = this.#availableNewSessionModels();
    const newSessionModelHealth = modelHealthPresentations(
      newSessionModels,
      this.#newSessionSubscriptionHealth,
    );
    const effectiveNewSessionModelInfo = newSessionModels.find(
      (model) => model.id === effectiveNewSessionModel,
    );
    const newSessionThinkingOption = thinkingOption(effectiveNewSessionModelInfo);
    const newSessionModelOptions = modelOptionControls(
      effectiveNewSessionModelInfo,
      this.#newSessionModelOptions,
      newSessionThinkingOption === undefined || this.#newSessionThinking === ""
        ? {}
        : { [newSessionThinkingOption.key]: this.#newSessionThinking },
    );
    const newSessionOptionsLoading = newSessionOptionsAreLoading(
      this.#newSessionOptionsLifecycle,
    );
    const newSessionOptionsBlocking = newSessionOptionsBlockSubmission(
      this.#newSessionOptionsLifecycle,
    );
    const newSessionCanSubmit = canSubmitNewSession({
      sessionPending: this.#newSessionPending,
      optionsBlocking: newSessionOptionsBlocking,
      attachmentPending: this.#newSessionAttachmentPending,
    });
    const serverOffline = readSignal(this.#store.serverInfo)?.online === false;
    const connectionLabel = this.#hostError
      ? "Host unavailable"
      : this.#protocolError
        ? "Disconnected"
      : serverOffline
        ? "Offline · local models only"
      : this.#routeLoading
        ? "Loading"
        : readSignal(this.#threadIngress.state) === "error"
          ? "Reconnect needed"
          : "Connected";
    const statusActionable =
      this.#shellNotice !== "" ||
      this.#connectivityNotice !== "" ||
      this.#hostError ||
      this.#pwaActivate !== undefined ||
      this.#pwaInstallPrompt !== undefined;
    const fullScreenRoute = route.kind === "settings"
      || route.kind === "reviews"
      || route.kind === "automations";
    return html`
      <main
        tabindex="-1"
        class="app-shell mobile-pane-${this.#mobilePane} ${fullScreenRoute
          ? "full-screen-route"
          : ""} ${this.#newSessionSetup.status === "open" ? "new-session-open" : ""}"
        data-theme=${theme}
        aria-label="trouve application"
        @trouve-open-internal=${this.#openInternal}
        @trouve-open-external=${this.#openExternal}
        @trouve-open-video=${this.#openVideo}
        @trouve-open-file=${this.#openFile}
        @trouve-open-inspection=${this.#openInspection}
        @trouve-chat-position=${this.#chatPositionChanged}
        @trouve-command-palette-action=${this.#commandPaletteAction}
        @trouve-pull-request-chat=${this.#openPullRequestChat}
        @trouve-pull-request-fix=${this.#fixPullRequestReview}
        @trouve-close-full-screen=${this.#closeFullScreenRoute}
        @trouve-new-thread-setup-state=${(event: CustomEvent<{ readonly open: boolean }>) => {
          this.#newThreadSetupOpen = event.detail.open;
          this.requestUpdate();
        }}
      >
        <nav
          class="navigation-panel"
          aria-label="Workspaces and sessions"
        >
          <div class="primary-links" aria-label="Application sections">
            <button class="navigation-icon-button" type="button" aria-label="Pull Requests" data-tooltip="Pull Requests" aria-current=${route.kind === "reviews" ? "page" : "false"} @click=${() => { this.#router.navigate({ kind: "reviews" }); this.#showMobilePane("thread"); }}>${fontAwesomeIcon("code-pull-request")}</button>
            <button class="navigation-icon-button" type="button" aria-label="Automations" data-tooltip="Automations" aria-current=${route.kind === "automations" ? "page" : "false"} @click=${() => { this.#router.navigate({ kind: "automations" }); this.#showMobilePane("thread"); }}>${fontAwesomeIcon("stopwatch")}</button>
            <button class="navigation-icon-button" type="button" aria-label="Settings" data-tooltip="Settings" aria-current=${route.kind === "settings" ? "page" : "false"} @click=${() => { this.#router.navigate({ kind: "settings" }); this.#showMobilePane("thread"); }}>${fontAwesomeIcon("gear", { className: "settings-link-icon" })}</button>
          </div>
          <div class="workspace-list-heading">
            <h2>Workspaces</h2>
            <span class="workspace-list-options-wrap">
              <button
                class="workspace-list-options-button"
                type="button"
                aria-label="Workspace list options"
                title="Workspace list options"
                aria-expanded=${this.#workspaceListOptionsOpen ? "true" : "false"}
                @click=${this.#toggleWorkspaceListOptions}
              >${fontAwesomeIcon("ellipsis")}</button>
              ${this.#workspaceListOptionsOpen
                ? html`<span
                    class="workspace-list-options-menu"
                    role="group"
                    aria-label="Workspace list options"
                  >
                    <label>
                      <span>Grouping</span>
                      <select
                        aria-label="Group sessions by"
                        .value=${workspaceListPreferences.grouping}
                        @change=${(event: Event) => this.#setWorkspaceListGrouping(event)}
                      >
                        <option value="repository">Repository</option>
                        <option value="workspace">Workspace</option>
                        <option value="updated">Updated</option>
                        <option value="status">Status</option>
                      </select>
                    </label>
                    <label>
                      <span>Ordering</span>
                      <select
                        aria-label="Order sessions by"
                        .value=${workspaceListPreferences.ordering}
                        @change=${(event: Event) => this.#setWorkspaceListOrdering(event)}
                      >
                        <option value="updated">Updated</option>
                        <option value="status">Status</option>
                        <option value="created">Created</option>
                      </select>
                    </label>
                    <label class="workspace-list-show-option">
                      <input
                        type="checkbox"
                        .checked=${workspaceListPreferences.showBranches}
                        @change=${() => this.#toggleWorkspaceListShow("showBranches")}
                      />
                      <span>Branch names</span>
                    </label>
                    <label class="workspace-list-show-option">
                      <input
                        type="checkbox"
                        .checked=${workspaceListPreferences.showStatus}
                        @change=${() => this.#toggleWorkspaceListShow("showStatus")}
                      />
                      <span>Status indicators</span>
                    </label>
                  </span>`
                : nothing}
            </span>
            <button
              class="command-palette-compact"
              type="button"
              aria-label="Open command palette"
              aria-haspopup="dialog"
              aria-keyshortcuts="Control+K Meta+K"
              title="Command palette (Ctrl/Cmd-K)"
              @click=${this.#openCommandPalette}
            >${fontAwesomeIcon("magnifying-glass")}</button>
            <button
              class="workspace-open-button"
              type="button"
              aria-label="Open workspace"
              title=${directoryPickerAvailable
                ? "Choose a repository folder"
                : "Browse is unavailable here; register a server-host path in Settings"}
              ?disabled=${!directoryPickerAvailable || this.#workspacePickerPending}
              @click=${() => void this.#openWorkspace()}
            >${fontAwesomeIcon(this.#workspacePickerPending ? "spinner" : "plus", {
                spin: this.#workspacePickerPending,
              })}</button>
          </div>
          <p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">${this.#workspaceOrderStatus}</p>
          ${displayedWorkspaces.map(
            (workspace, index) => {
              const collapsed = this.#collapsedWorkspaceIds.has(workspace.id);
              const workspaceFilters = this.#workspaceListPreferences.filtersFor(workspace.id);
              const repositoryGroup = repositoryGroupPresentations.get(workspace.id);
              const dropTarget = this.#workspaceDropTarget === workspace.id;
              const placeholder = html`<div
                class="workspace-drop-placeholder"
                data-drop-placeholder="workspace"
                aria-hidden="true"
                @dragover=${this.#keepWorkspaceDropActive}
                @drop=${(event: DragEvent) => this.#dropWorkspace(event, workspace.id)}
              ></div>`;
              return html`
                ${repositoryGroup?.first
                  ? html`<h3 id=${repositoryGroup.headingId} class="repository-group-heading">${repositoryGroup.label}</h3>`
                  : nothing}
                ${dropTarget && !this.#workspaceDropAfter ? placeholder : nothing}
                <section
                  class="workspace-group"
                  aria-labelledby=${repositoryGroup === undefined
                    ? `workspace-${index}`
                    : `${repositoryGroup.headingId} workspace-${index}`}
                  @dragover=${(event: DragEvent) => this.#dragOverWorkspace(event, workspace.id)}
                  @drop=${(event: DragEvent) => this.#dropWorkspace(event, workspace.id)}
                >
                  <header
                    class="workspace-row"
                    data-controls-visible=${
                      this.#workspaceActionMenuId === workspace.id
                      || this.#draggedWorkspaceId === workspace.id
                    }
                  >
                    <button
                      class="workspace-toggle"
                      type="button"
                      aria-expanded=${collapsed ? "false" : "true"}
                      aria-controls=${`workspace-sessions-${index}`}
                      @click=${() => this.#toggleWorkspace(workspace.id)}
                    >
                      ${fontAwesomeIcon(collapsed ? "caret-right" : "caret-down")}
                      ${repositoryGroup === undefined
                        ? html`<h3 id=${`workspace-${index}`}>${workspace.name}</h3>`
                        : html`<h4 id=${`workspace-${index}`}>${workspace.name}</h4>`}
                    </button>
                    ${workspaceReorderingEnabled
                      ? html`<span
                          class="workspace-order-controls"
                          aria-label=${`Position of ${workspace.name}, ${index + 1} of ${displayedWorkspaces.length}`}
                        >
                          <button
                            class="workspace-grip"
                            type="button"
                            data-workspace-id=${workspace.id}
                            .draggable=${displayedWorkspaces.length > 1}
                            aria-label=${`Reorder ${workspace.name}. Position ${index + 1} of ${displayedWorkspaces.length}. Use Up and Down arrow keys or drag.`}
                            title="Drag to reorder, or use arrow keys"
                            @keydown=${(event: KeyboardEvent) => this.#workspaceOrderKeyDown(event, workspace.id)}
                            @dragstart=${(event: DragEvent) => this.#startWorkspaceDrag(event, workspace.id)}
                            @dragend=${this.#finishWorkspaceDrag}
                          >${fontAwesomeIcon("grip-vertical")}</button>
                        </span>`
                      : nothing}
                    <span class="workspace-actions-wrap">
                      <button
                        class="workspace-actions-button"
                        type="button"
                        aria-label=${`Actions for ${workspace.name}`}
                        title="Workspace actions"
                        aria-expanded=${this.#workspaceActionMenuId === workspace.id ? "true" : "false"}
                        @click=${() => this.#toggleWorkspaceActions(workspace.id)}
                      >${fontAwesomeIcon("ellipsis")}</button>
                      ${this.#workspaceActionMenuId === workspace.id
                        ? html`<span class="workspace-actions-menu" role="menu" aria-label=${`${workspace.name} workspace actions`}>
                            <strong>Workspace</strong>
                            <button
                              type="button"
                              role="menuitemcheckbox"
                              aria-checked=${this.#showArchivedWorkspaceIds.has(workspace.id) ? "true" : "false"}
                              @click=${() => this.#toggleArchivedWorkspaceSessions(workspace.id)}
                            ><span>Archived</span><span>${this.#showArchivedWorkspaceIds.has(workspace.id) ? fontAwesomeIcon("check") : nothing}</span></button>
                            <span class="workspace-actions-menu-section" role="group" aria-label="Status filters">
                              <strong>Status</strong>
                              ${WORKSPACE_STATUS_FILTERS.map(([, label], filterIndex) => html`<button
                                type="button"
                                role="menuitemcheckbox"
                                aria-checked=${(workspaceFilters.status & (1 << filterIndex)) !== 0 ? "true" : "false"}
                                @click=${() => this.#toggleWorkspaceListFilter(
                                  workspace.id,
                                  "status",
                                  filterIndex,
                                )}
                              ><span>${label}</span><span>${(workspaceFilters.status & (1 << filterIndex)) !== 0
                                  ? fontAwesomeIcon("check")
                                  : nothing}</span></button>`)}
                            </span>
                            <span class="workspace-actions-menu-section" role="group" aria-label="Pull request filters">
                              <strong>Pull request</strong>
                              ${WORKSPACE_PULL_REQUEST_FILTERS.map(([, label], filterIndex) => html`<button
                                type="button"
                                role="menuitemcheckbox"
                                aria-checked=${(workspaceFilters.pullRequest & (1 << filterIndex)) !== 0 ? "true" : "false"}
                                @click=${() => this.#toggleWorkspaceListFilter(
                                  workspace.id,
                                  "pullRequest",
                                  filterIndex,
                                )}
                              ><span>${label}</span><span>${(workspaceFilters.pullRequest & (1 << filterIndex)) !== 0
                                  ? fontAwesomeIcon("check")
                                  : nothing}</span></button>`)}
                            </span>
                            <span class="workspace-actions-menu-section" role="group" aria-label="Workspace commands">
                              <button
                                type="button"
                                role="menuitem"
                                @click=${() => this.#collapseWorkspaceFromMenu(workspace.id)}
                              ><span>Collapse workspace</span></button>
                              <button
                                type="button"
                                role="menuitem"
                                @click=${() => this.#markWorkspaceRead(workspace.id)}
                              ><span>Mark all as read</span></button>
                            </span>
                            <button
                              class="danger"
                              type="button"
                              role="menuitem"
                              ?disabled=${this.#workspaceClosePendingId !== ""}
                              @click=${() => void this.#closeWorkspaceFromNavigation(workspace.id, workspace.name)}
                            >${this.#workspaceClosePendingId === workspace.id ? "Closing…" : "Close workspace"}</button>
                          </span>`
                        : nothing}
                    </span>
                    <button
                      class="workspace-new-session"
                      type="button"
                      aria-label=${`New session in ${workspace.name}`}
                      title=${`New session in ${workspace.name}`}
                      @click=${() => this.#showNewSession(workspace.id)}
                    >${fontAwesomeIcon("plus")}</button>
                  </header>
                  <trouve-session-list
                    id=${`workspace-sessions-${index}`}
                    workspace-id=${workspace.id}
                    .showArchived=${this.#showArchivedWorkspaceIds.has(workspace.id)}
                    .grouping=${workspaceListPreferences.grouping}
                    .ordering=${workspaceListPreferences.ordering}
                    .showBranches=${workspaceListPreferences.showBranches}
                    .showStatus=${workspaceListPreferences.showStatus}
                    .statusFilter=${workspaceFilters.status}
                    .pullRequestFilter=${workspaceFilters.pullRequest}
                    ?hidden=${collapsed}
                    @trouve-session-open=${() => this.#showMobilePane("thread")}
                  ></trouve-session-list>
                </section>
                ${dropTarget && this.#workspaceDropAfter ? placeholder : nothing}
              `;
            },
          )}
          ${orphanWorkspaceIds.map(
            (workspaceId, index) => {
              const collapsed = this.#collapsedWorkspaceIds.has(workspaceId);
              const workspaceFilters = this.#workspaceListPreferences.filtersFor(workspaceId);
              return html`
                <section
                  class="workspace-group"
                  aria-labelledby=${`workspace-orphan-${index}`}
                >
                  <header class="workspace-row">
                    <button
                      class="workspace-toggle"
                      type="button"
                      aria-expanded=${collapsed ? "false" : "true"}
                      aria-controls=${`workspace-orphan-sessions-${index}`}
                      @click=${() => this.#toggleWorkspace(workspaceId)}
                    >
                      ${fontAwesomeIcon(collapsed ? "caret-right" : "caret-down")}
                      <h3 id=${`workspace-orphan-${index}`}>Workspace</h3>
                    </button>
                  </header>
                  <trouve-session-list
                    id=${`workspace-orphan-sessions-${index}`}
                    workspace-id=${workspaceId}
                    .grouping=${workspaceListPreferences.grouping}
                    .ordering=${workspaceListPreferences.ordering}
                    .showBranches=${workspaceListPreferences.showBranches}
                    .showStatus=${workspaceListPreferences.showStatus}
                    .statusFilter=${workspaceFilters.status}
                    .pullRequestFilter=${workspaceFilters.pullRequest}
                    ?hidden=${collapsed}
                    @trouve-session-open=${() => this.#showMobilePane("thread")}
                  ></trouve-session-list>
                </section>
              `;
            },
          )}
          ${sessions.length === 0
            ? html`<div class="screen-empty navigation-empty">
                <strong>${this.#protocolError ? "Server unavailable" : "No sessions"}</strong>
                <span>${this.#protocolError
                  ? "Reconnect to load your workspaces and sessions."
                  : knownWorkspaces.length === 0
                    ? "Register a server-host repository under Settings → Workspaces."
                    : "Create a session with the + button above."}</span>
              </div>`
            : nothing}
          <trouve-session-usage-panel
            session-id=${route.kind === "session" ? route.sessionId : ""}
            thread-id=${route.kind === "session" ? route.threadId ?? "" : ""}
            model=${activeThread?.model ?? ""}
            .placeholder=${sessions.length === 0
              || this.#newSessionSetup.status === "open"
              || (route.kind === "session" && this.#newThreadSetupOpen)}
          ></trouve-session-usage-panel>
        </nav>

        <div
          class="panel-splitter navigation-splitter"
          role="separator"
          tabindex="0"
          aria-label="Resize session navigation"
          aria-orientation="vertical"
          aria-valuemin="180"
          aria-valuemax=${this.#panelWidthBounds("navigation")[1]}
          aria-valuenow=${Math.round(this.#navigationWidth)}
          @pointerdown=${(event: PointerEvent) => this.#startPanelResize(event, "navigation")}
          @pointermove=${this.#movePanelResize}
          @pointerup=${this.#finishPanelResize}
          @pointercancel=${this.#finishPanelResize}
          @keydown=${(event: KeyboardEvent) => this.#resizePanelWithKeyboard(event, "navigation")}
        ></div>

        ${route.kind === "session"
          ? this.#routeError === ""
            ? html`
                <trouve-thread-screen
                  class="thread-panel"
                  aria-label="Active thread"
                  workspace-id=${route.workspaceId}
                  session-id=${route.sessionId}
                  thread-id=${route.threadId ?? ""}
                  .scrollBookmark=${route.threadId === undefined
                    ? undefined
                    : chatBookmarkForNavigation(
                        resume.threadScroll[route.threadId],
                        activeView?.turnRunning ?? false,
                        (activeView?.queue.length ?? 0) > 0,
                      )}
                ></trouve-thread-screen>
              `
            : html`<section class="thread-panel app-page"><div class="screen-empty" role="alert"><strong>Unable to load session</strong><span>${this.#routeError}</span><span>Retrying automatically.</span></div></section>`
          : route.kind === "settings"
            ? html`<trouve-settings-screen
                class="thread-panel"
                aria-label="Settings"
                section=${route.section ?? "general"}
              ></trouve-settings-screen>`
            : route.kind === "reviews"
              ? html`<trouve-pull-requests-dashboard class="thread-panel" aria-label="Pull requests"></trouve-pull-requests-dashboard>`
              : route.kind === "automations"
                ? html`<trouve-automations-screen class="thread-panel" aria-label="Automations"></trouve-automations-screen>`
            : route.kind === "not-found"
              ? html`<section class="thread-panel app-page"><div class="screen-empty" role="alert"><strong>Page not found</strong><button type="button" @click=${() => this.#router.navigate({ kind: "inbox" }, true)}>Return to sessions</button></div></section>`
              : this.#protocolError
                ? html`<section class="thread-panel app-page"><div class="screen-empty" role="alert"><strong>Could not connect</strong><span>The frontend will reconnect automatically when the server is available.</span></div></section>`
                : html`<trouve-thread-screen
                    class="thread-panel"
                    aria-label="No active session"
                    workspace-id=""
                    session-id=""
                    thread-id=""
                  ></trouve-thread-screen>`}

        <div
          class="panel-splitter inspection-splitter"
          role="separator"
          tabindex="0"
          aria-label="Resize inspection panel"
          aria-orientation="vertical"
          aria-valuemin="240"
          aria-valuemax=${this.#panelWidthBounds("inspection")[1]}
          aria-valuenow=${Math.round(this.#inspectionWidth)}
          @pointerdown=${(event: PointerEvent) => this.#startPanelResize(event, "inspection")}
          @pointermove=${this.#movePanelResize}
          @pointerup=${this.#finishPanelResize}
          @pointercancel=${this.#finishPanelResize}
          @keydown=${(event: KeyboardEvent) => this.#resizePanelWithKeyboard(event, "inspection")}
        ></div>

        <aside class="inspection-panel" aria-label="Inspection">
          <div class="inspection-tabs" role="tablist" aria-label="Inspection views">
            ${INSPECTION_PANELS.map(
              (panel, index) => html`
                <button
                  type="button"
                  role="tab"
                  aria-selected=${selectedInspection === panel ? "true" : "false"}
                  tabindex=${route.kind === "session"
                    ? rovingTabIndex(
                        index,
                        selectedInspectionIndex,
                        INSPECTION_PANELS.length,
                      )
                    : -1}
                  ?disabled=${route.kind !== "session"}
                  @keydown=${(event: KeyboardEvent) =>
                    this.#selectInspectionWithKeyboard(event, index)}
                  @click=${() => this.#selectInspection(panel)}
                >${fontAwesomeIcon(INSPECTION_PANEL_LABELS[panel].icon)}${INSPECTION_PANEL_LABELS[panel].label}</button>
              `,
            )}
          </div>
          ${repeat(
            [...this.#terminalSessionIds],
            (sessionId) => sessionId,
            (sessionId) => {
              const visible = route.kind === "session" &&
                route.sessionId === sessionId &&
                selectedInspection === "terminal";
              return html`<trouve-terminal-panel
                class="inspection-content retained-terminal-panel"
                session-id=${sessionId}
                ?hidden=${!visible}
              ></trouve-terminal-panel>`;
            },
          )}
          ${route.kind === "session" && selectedInspection === "terminal"
            ? nothing
            : route.kind === "session" && selectedInspection === "info"
            ? html`<trouve-session-info-panel
                class="inspection-content"
              ></trouve-session-info-panel>`
            : route.kind === "session" &&
                (selectedInspection === "diff" || selectedInspection === "files")
            ? html`<trouve-inspection-workspace
                class="inspection-content"
                panel=${selectedInspection}
              ></trouve-inspection-workspace>`
            : route.kind === "session" && selectedInspection === "pr"
            ? html`<trouve-session-pr-panel
                class="inspection-content"
                session-title=${this.#store.sessionMetadata(route.sessionId)?.title ?? ""}
              ></trouve-session-pr-panel>`
            : html`<section class="inspection-content"><div class="screen-empty"><strong>${selectedInspection[0]?.toUpperCase()}${selectedInspection.slice(1)}</strong><span>${route.kind === "session" ? "This live surface will populate when the session exposes matching data." : "Select a session to inspect it."}</span></div></section>`}
        </aside>

        <section
          id="new-session-screen"
          class="thread-panel new-session-screen"
          aria-labelledby="new-session-title"
          ?hidden=${this.#newSessionSetup.status !== "open"}
        >
          <form @submit=${this.#createSession}>
            <header>
              <div>
                <h2 id="new-session-title">New session</h2>
                <p>${this.#newSessionSetup.createRequest === undefined
                  ? "Pick where to work, what to branch from, and how the agent should run."
                  : "Retrying the original session creation. Its workspace, title, and branch are fixed; you can still edit the first message."}</p>
              </div>
            </header>
            <label class="new-session-workspace">
              <span>Workspace</span>
              <select
                name="workspace_id"
                required
                .value=${this.#newSessionWorkspaceId}
                @change=${this.#selectNewSessionWorkspace}
                ?disabled=${this.#newSessionPending
                  || this.#newSessionSetup.createRequest !== undefined}
              >
                ${orderedWorkspaces.map(
                  (workspace) => html`<option value=${workspace.id}>${workspace.name}</option>`,
                )}
              </select>
            </label>
            <label class="new-session-branch">
              <span>Base branch</span>
              <select
                name="base_ref"
                .value=${this.#newSessionBaseRef}
                @change=${(event: Event) => {
                  this.#newSessionBaseRef = (event.currentTarget as HTMLSelectElement).value;
                  this.#newSessionPreferredBaseRef = this.#newSessionBaseRef;
                }}
                ?disabled=${this.#newSessionPending
                  || this.#newSessionBranchesPending
                  || this.#newSessionSetup.createRequest !== undefined}
              >
                ${this.#newSessionBranchesPending
                  ? html`<option value="">Loading branches…</option>`
                  : nothing}
                ${this.#newSessionBranches.map(
                  (branch) => html`<option
                    value=${branch}
                    .selected=${live(branch === this.#newSessionBaseRef)}
                  >${branch}</option>`,
                )}
              </select>
            </label>
            ${this.#newSessionBranchError === ""
              ? nothing
              : html`<p class="dialog-warning new-session-branch-warning" role="status">${this.#newSessionBranchError}</p>`}
            <label class="dialog-checkbox">
              ${this.#newSessionSetup.createRequest === undefined
                ? html`<input
                    name="fetch_latest"
                    type="checkbox"
                    checked
                    ?disabled=${this.#newSessionPending}
                  />`
                : html`<input
                    name="fetch_latest"
                    type="checkbox"
                    .checked=${this.#newSessionSetup.createRequest.fetchLatest}
                    disabled
                  />`}
              <span>Use latest remote branch</span>
            </label>
            ${this.#newSessionAttachments.length === 0
              ? nothing
              : html`<ul class="attachment-list pending-attachments" aria-label="Initial prompt attachments">
                  ${this.#newSessionAttachments.map(
                    (attachment, index) => {
                      const preview = pendingAttachmentPreviewUrl(attachment);
                      const video = isVideoMime(attachment.upload.mime);
                      return html`<li class=${preview === undefined ? "file-attachment" : "image-attachment"}>
                        ${preview === undefined
                          ? html`<span class="attachment-icon">${fontAwesomeIcon("file")}</span>`
                          : html`<trouve-image-preview
                              .source=${preview}
                              .name=${attachment.upload.name}
                              .mime=${attachment.upload.mime}
                              .video=${video}
                            ></trouve-image-preview>`}
                        <div class="attachment-details">
                          <strong title=${attachment.upload.name}>${attachment.upload.name}</strong>
                          <small>${attachment.upload.mime} · ${this.#formatAttachmentBytes(attachment.size)}</small>
                        </div>
                        <button
                          class="attachment-remove"
                          type="button"
                          aria-label=${`Remove ${attachment.upload.name}`}
                          ?disabled=${this.#newSessionPending}
                          @click=${() => this.#removeNewSessionAttachment(index)}
                        >${fontAwesomeIcon("xmark")}</button>
                      </li>`;
                    },
                  )}
                </ul>`}
            <label class="new-session-prompt">
              <span>First message</span>
              <textarea
                name="prompt"
                maxlength="100000"
                rows="1"
                autocomplete="off"
                placeholder="What should the agent do?  (Shift+Enter for a new line)"
                .value=${this.#newSessionPrompt}
                ?disabled=${this.#newSessionPending}
                @input=${this.#newSessionPromptChanged}
                @keydown=${this.#newSessionPromptKeydown}
                @compositionstart=${this.#newSessionPromptCompositionStarted}
                @compositionend=${this.#newSessionPromptCompositionEnded}
                @paste=${this.#newSessionPaste}
              ></textarea>
            </label>
            <label
              class=${`attachment-button dialog-attachment new-session-attachment ${this.#newSessionPending || this.#newSessionAttachmentPending ? "disabled" : ""}`}
              aria-disabled=${this.#newSessionPending || this.#newSessionAttachmentPending ? "true" : "false"}
              title="Attach files to the initial prompt"
            >
              ${fontAwesomeIcon("paperclip")}<span class="visually-hidden">${this.#newSessionAttachmentPending ? "Reading files…" : "Attach files"}</span>
              <input
                type="file"
                multiple
                ?disabled=${this.#newSessionPending || this.#newSessionAttachmentPending}
                @click=${this.#newSessionAttachmentPickerClicked}
                @change=${this.#newSessionFilesSelected}
              />
            </label>
            <div class="dialog-option-grid">
              <label class="new-session-mode">
                <span>Agent persona</span>
                <select
                  name="mode"
                  .value=${this.#newSessionModeId}
                  ?disabled=${this.#newSessionPending}
                  @change=${(event: Event) => {
                    this.#newSessionOptionEdits = {
                      mode: true,
                      model: false,
                      thinking: this.#newSessionOptionEdits.thinking,
                      permission: false,
                    };
                    this.#newSessionModeId =
                      (event.currentTarget as HTMLSelectElement).value;
                    this.#reconcileNewSessionDefaults(this.#newSessionModels);
                    this.requestUpdate();
                  }}
                >
                  ${this.#newSessionModes.length === 0
                    ? html`<option value="code">Code</option>`
                    : this.#newSessionModes.map(
                        (mode) => html`<option
                          value=${mode.id}
                          .selected=${live(mode.id === this.#newSessionModeId)}
                        >${mode.display_name}</option>`,
                      )}
                </select>
              </label>
              <div class="dialog-field new-session-model">
                <span>Model</span>
                <trouve-model-picker
                  accessible-label="Model"
                  placement="down"
                  placeholder=${newSessionOptionsLoading
                    ? "Loading models…"
                    : "No model available"}
                  empty-label=""
                  .value=${this.#newSessionModelId}
                  .models=${newSessionModels}
                  .health=${newSessionModelHealth}
                  .disabled=${this.#newSessionPending}
                  @trouve-model-picked=${(event: CustomEvent<{ readonly modelId: string }>) => {
                    this.#newSessionOptionEdits = {
                      ...this.#newSessionOptionEdits,
                      model: true,
                      thinking: false,
                    };
                    const previousModel = resolveNewSessionModel(
                      this.#newSessionModelId,
                      selectedNewSessionMode,
                      this.#newSessionProviders,
                    );
                    const defaults = resolveNewThreadDefaults(
                      this.#newSessionModes,
                      newSessionModels,
                      this.#newSessionProviders,
                      {
                        modeId: this.#newSessionModeId,
                        modelId: event.detail.modelId,
                      },
                    );
                    this.#newSessionModelId = defaults.modelId;
                    this.#newSessionThinking = defaults.thinking;
                    this.#newSessionInheritedThinking = newThreadInheritanceForWorkspace(
                      defaults,
                      newSessionOptionsCatalogWorkspaceId(this.#newSessionOptionsLifecycle),
                      this.#newSessionWorkspaceId,
                    ).inheritedThinking;
                    const nextModel = resolveNewSessionModel(
                      defaults.modelId,
                      selectedNewSessionMode,
                      this.#newSessionProviders,
                    );
                    if (nextModel !== previousModel) this.#newSessionModelOptions = {};
                    this.requestUpdate();
                  }}
                ></trouve-model-picker>
              </div>
              <label class="new-session-permission">
                <span class=${this.#newSessionPermissionMode === "yolo" ? "permission-yolo" : ""}>${this.#newSessionPermissionMode === "yolo" ? fontAwesomeIcon("triangle-exclamation") : nothing}Permission mode</span>
                <select
                  name="permission_mode"
                  class=${this.#newSessionPermissionMode === "yolo" ? "permission-yolo" : ""}
                  .value=${this.#newSessionPermissionMode}
                  ?disabled=${this.#newSessionPending}
                  @change=${(event: Event) => {
                    this.#newSessionOptionEdits = {
                      ...this.#newSessionOptionEdits,
                      permission: true,
                    };
                    const value = (event.currentTarget as HTMLSelectElement).value;
                    this.#newSessionPermissionMode = value === "ask"
                        || value === "allow_list"
                        || value === "yolo"
                      ? value
                      : resolveNewThreadDefaults(
                          this.#newSessionModes,
                          newSessionModels,
                          this.#newSessionProviders,
                          { modeId: this.#newSessionModeId, modelId: this.#newSessionModelId },
                        ).permissionMode;
                    this.#newSessionInheritedPermissionMode = undefined;
                    this.requestUpdate();
                  }}
                >
                  <option value="ask" .selected=${live(this.#newSessionPermissionMode === "ask")}>Ask</option>
                  <option value="allow_list" .selected=${live(this.#newSessionPermissionMode === "allow_list")}>Allow list</option>
                  <option value="yolo" .selected=${live(this.#newSessionPermissionMode === "yolo")}>Yolo</option>
                </select>
              </label>
              ${newSessionModelOptions.length === 0
                ? nothing
                : html`<trouve-model-options-editor
                    class="new-session-model-options"
                    .controls=${newSessionModelOptions}
                    .disabled=${this.#newSessionPending}
                    @trouve-model-option-changed=${(
                      event: CustomEvent<ModelOptionChangeDetail>,
                    ) => {
                      const defaults = resolveNewThreadDefaults(
                        this.#newSessionModes,
                        newSessionModels,
                        this.#newSessionProviders,
                        {
                          modeId: this.#newSessionModeId,
                          modelId: this.#newSessionModelId,
                        },
                      );
                      const inheritance = newThreadInheritanceForWorkspace(
                        defaults,
                        newSessionOptionsCatalogWorkspaceId(
                          this.#newSessionOptionsLifecycle,
                        ),
                        this.#newSessionWorkspaceId,
                      );
                      const updated = applyNewSessionModelOptionChange({
                        modelOptions: this.#newSessionModelOptions,
                        thinking: this.#newSessionThinking,
                        inheritedThinking: this.#newSessionInheritedThinking,
                        change: event.detail,
                        defaults: {
                          thinking: defaults.thinking,
                          inheritedThinking: inheritance.inheritedThinking,
                        },
                      });
                      this.#newSessionModelOptions = updated.modelOptions;
                      this.#newSessionThinking = updated.thinking;
                      this.#newSessionInheritedThinking = updated.inheritedThinking;
                      this.#newSessionOptionEdits = {
                        ...this.#newSessionOptionEdits,
                        thinking: updated.thinkingEdit,
                      };
                      this.requestUpdate();
                    }}
                  ></trouve-model-options-editor>`}
            </div>
            ${this.#newSessionPermissionMode === "yolo"
              ? html`<div class="new-session-yolo-warning" role="note"><strong>${fontAwesomeIcon("triangle-exclamation")} Unattended execution (YOLO) is dangerous</strong><span>The agent can run commands and change or delete files without asking for approval.</span></div>`
              : nothing}
            ${newSessionOptionsBlocking
              ? html`<p class="dialog-warning new-session-options-loading" role="status" aria-live="polite">Loading agent defaults before this session can start…</p>`
              : nothing}
            ${this.#newSessionOptionsError === ""
              ? nothing
              : html`<p class="dialog-warning new-session-options-warning" role="status">${this.#newSessionOptionsError}</p>`}
            ${this.#newSessionOptionsStatus === ""
              ? nothing
              : html`<p class="new-session-options-status" role="status">${this.#newSessionOptionsStatus}</p>`}
            ${this.#newSessionError === ""
              ? nothing
              : html`<p class="dialog-error new-session-error" role="alert">${this.#newSessionError}</p>`}
            <footer>
              <button class="primary" type="submit" ?disabled=${!newSessionCanSubmit || (this.#newSessionPrompt.trim() === "" && this.#newSessionAttachments.length === 0)}>${this.#newSessionPending ? "Starting…" : newSessionOptionsBlocking ? "Loading defaults…" : "Start session"}</button>
              <button type="button" ?disabled=${this.#newSessionPending} @click=${this.#closeNewSession}>Cancel</button>
            </footer>
          </form>
        </section>

        <dialog
          id="desktop-quit-dialog"
          class="app-dialog desktop-quit-dialog"
          data-close-request-id=${this.#desktopClosePrompt?.request.requestId ?? nothing}
          aria-labelledby="desktop-quit-title"
          aria-describedby="desktop-quit-description"
          aria-busy=${this.#desktopClosePending === "" ? "false" : "true"}
          @cancel=${this.#desktopCloseCancelled}
        >
          ${this.#desktopClosePrompt === undefined
            ? nothing
            : html`
                <form @submit=${(event: SubmitEvent) => event.preventDefault()}>
                  <header>
                    <div>
                      <h2 id="desktop-quit-title">${this.#desktopClosePrompt.armed
                        ? activeSessionCount === 1
                          ? "Waiting for an agent to finish"
                          : `Waiting for ${activeSessionCount} active sessions`
                        : activeSessionCount === 1
                          ? "1 active session is still running"
                          : `${activeSessionCount} active sessions are still running`}</h2>
                      <p id="desktop-quit-description">${this.#desktopClosePrompt.armed
                        ? "trouve will quit automatically when the running work finishes."
                        : "Quitting now stops the running work mid-turn. You can also let the agents finish and quit automatically."}</p>
                    </div>
                  </header>
                  <footer>
                    <button
                      type="button"
                      ?disabled=${this.#desktopClosePending !== ""}
                      @click=${() => void this.#resolveDesktopClose("cancel")}
                    >${this.#desktopClosePending === "cancel"
                      ? "Cancelling…"
                      : this.#desktopClosePrompt.armed
                        ? "Cancel automatic quit"
                        : "Cancel"}</button>
                    ${this.#desktopClosePrompt.armed
                      ? nothing
                      : html`
                          <button
                            type="button"
                            ?disabled=${this.#desktopClosePending !== ""}
                            @click=${() => void this.#resolveDesktopClose("quit-when-idle")}
                          >${this.#desktopClosePending === "quit-when-idle"
                            ? "Arming…"
                            : "Quit when agents finish"}</button>
                        `}
                    <button
                      class="primary"
                      type="button"
                      ?disabled=${this.#desktopClosePending !== ""}
                      @click=${() => void this.#resolveDesktopClose("quit-now")}
                    >${this.#desktopClosePending === "quit-now" ? "Quitting…" : "Quit"}</button>
                  </footer>
                </form>
              `}
        </dialog>

        <trouve-command-palette></trouve-command-palette>

        <footer class="status-bar ${statusActionable ? "actionable" : ""}">
          <span class="online-dot ${this.#protocolError || this.#hostError || serverOffline ? "offline" : ""}"></span><span class="status-copy">${connectionLabel}</span><span class="status-spacer"></span>
          ${this.#connectivityNotice === "" ? nothing : html`<span class="status-copy" role="status" aria-live="polite">${this.#connectivityNotice}</span>`}
          ${this.#shellNotice === "" ? nothing : html`<span class="status-copy shell-notice" role="status">${this.#shellNotice}</span>`}
          ${this.#pwaActivate === undefined
            ? nothing
            : html`<button
                class="update-action"
                type="button"
                @click=${() => this.#pwaActivate?.()}
              >Update available · reload</button>`}
          ${this.#pwaInstallPrompt === undefined
            ? nothing
            : html`<button
                class="update-action"
                type="button"
                ?disabled=${this.#pwaInstallPending}
                @click=${() => void this.#installPwa()}
              >${this.#pwaInstallPending ? "Opening install…" : "Install Trouve"}</button>`}
          ${this.#pwaInstallStatus === ""
            ? nothing
            : html`<span class="status-copy" role="status" aria-live="polite">${this.#pwaInstallStatus}</span>`}
          <label class="theme-picker">
            <span class="visually-hidden">Theme</span>
            <select
              aria-label="Theme"
              .value=${themePreference}
              @change=${this.#selectTheme}
            >
              <option value="system">System</option>
              ${THEME_NAMES.map(
                (name) => html`<option value=${name}>${name.replaceAll("-", " ")}</option>`,
              )}
            </select>
          </label>
          ${activeThread === undefined
            ? nothing
            : html`
                <span class="status-copy">${activeThread.model}</span>
                <span
                  class="status-copy permission-status ${activeThread.permission_mode === "yolo" ? "permission-yolo" : ""}"
                  title="Active thread permission mode"
                >Permission: ${activeThread.permission_mode === "allow_list"
                    ? "Allow list"
                    : activeThread.permission_mode === "yolo"
                      ? "Yolo"
                      : "Ask"}</span>
              `}
          <nav class="mobile-nav" aria-label="Primary navigation">
            <button type="button" aria-pressed=${this.#mobilePane === "navigation"} @click=${() => this.#showMobilePane("navigation")}>Sessions</button>
            <button type="button" aria-pressed=${this.#mobilePane === "thread"} @click=${() => this.#showMobilePane("thread")}>Chat</button>
            <button type="button" aria-pressed=${this.#mobilePane === "inspection"} @click=${() => this.#showMobilePane("inspection")}>Inspect</button>
            <button type="button" aria-pressed=${route.kind === "reviews"} @click=${() => { this.#showMobilePane("thread"); this.#router.navigate({ kind: "reviews" }); }}>Reviews</button>
            <button type="button" @click=${() => { this.#showMobilePane("thread"); this.#router.navigate({ kind: "settings" }); }}>Settings</button>
          </nav>
        </footer>
      </main>
    `;
  }
}

customElements.define("trouve-app", TrouveApp);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-app": TrouveApp;
  }
}
