import { createContext } from "@lit/context";

import type { HostCapabilitiesController } from "../services/capabilities.js";
import type { BrowserNotificationAdapter } from "../services/browser-notifications.js";
import type {
  NotificationPreferences,
  NotificationPreferencesSignal,
} from "../services/notification-preferences.js";
import type {
  ThemeController,
  ThemePreference,
} from "../services/theme-controller.js";
import type {
  AppearancePreferences,
  AppearancePreferencesSignal,
} from "../services/appearance-preferences.js";
import type {
  GeneralPreferences,
  GeneralPreferencesSignal,
} from "../services/general-preferences.js";
import type {
  ChatPreferences,
  ChatPreferencesSignal,
} from "../services/chat-preferences.js";
import type { AppRouter } from "../router/app-router.js";
import type { ProtocolClient } from "../services/protocol-client.js";
import type { PendingAttachment } from "../services/attachments.js";
import type {
  HostCloseDecision,
  HostLifecycleBatch,
  HostLocalFileAction,
  NativeNotificationRequest,
  WatchHostLifecycleOptions,
} from "../services/host-client.js";
import type { AppStore } from "../state/app-store.js";
import type { ReadonlySignal } from "../state/reactivity.js";
import type { SubscriptionHealthController } from "../services/subscription-health-controller.js";
import type { ModelCatalogController } from "../services/model-catalog-controller.js";
import type { ComposerDraftController } from "../services/composer-drafts.js";
import type { ResumePreferences } from "../services/resume-preferences.js";

export interface NativeHostActions {
  /** Opens the desktop host's directory picker. `undefined` means cancel. */
  readonly pickDirectory: () => Promise<string | undefined>;
  /** Returns only bounded attachment payloads; native paths never cross. */
  readonly pickFiles: () => Promise<readonly PendingAttachment[]>;
  /** Returns `undefined` for text, empty content, or no clipboard image. */
  readonly readClipboardImage: () => Promise<PendingAttachment | undefined>;
  /** Optional bridge-v5 lifecycle stream; absent for a PWA or older host. */
  readonly watchLifecycle?: (
    receive: (batch: HostLifecycleBatch) => void,
    options?: WatchHostLifecycleOptions,
  ) => Promise<void>;
  /** The frontend owns confirmation and app-idle policy. */
  readonly resolveClose?: (
    requestId: number,
    decision: HostCloseDecision,
  ) => Promise<void>;
  readonly setSleepInhibition?: (active: boolean) => Promise<void>;
  readonly showNativeNotification?: (
    request: NativeNotificationRequest,
    onActivate?: () => void,
  ) => Promise<void>;
  readonly requestUserAttention?: () => Promise<void>;
  readonly actOnSessionFile?: (
    sessionId: string,
    relativePath: string,
    action: HostLocalFileAction,
  ) => Promise<void>;
}

export interface AppServices {
  readonly deployment: "desktop" | "pwa" | "browser";
  readonly now: () => Date;
  readonly notifications: BrowserNotificationAdapter;
  readonly notificationPreferences: NotificationPreferencesSignal;
  readonly setNotificationPreferences: (
    patch: Partial<NotificationPreferences>,
  ) => void;
  readonly router: AppRouter;
  readonly theme: ThemeController;
  readonly setThemePreference: (preference: ThemePreference) => void;
  readonly appearance: AppearancePreferencesSignal;
  readonly setAppearancePreferences: (
    patch: Partial<AppearancePreferences>,
  ) => void;
  readonly systemFontFamilies: ReadonlySignal<readonly string[]>;
  /** Installed UI font families from the native host or browser capability. */
  readonly loadSystemFontFamilies: () => Promise<readonly string[]>;
  readonly generalPreferences: GeneralPreferencesSignal;
  readonly setGeneralPreferences: (
    patch: Partial<GeneralPreferences>,
  ) => void;
  readonly chatPreferences: ChatPreferencesSignal;
  readonly setChatPreferences: (
    patch: Partial<ChatPreferences>,
  ) => void;
  readonly composerDrafts: ComposerDraftController;
  /** Synchronously invalidate active ingress and permanently discard every
   * known draft before a deleted session's store projections disappear. */
  readonly tombstoneSession: (sessionId: string) => void;
  readonly resumePreferences: ReadonlySignal<ResumePreferences>;
  readonly setThreadTabClosed: (threadId: string, closed: boolean) => void;
  readonly setThreadTabPinned: (threadId: string, pinned: boolean) => void;
  readonly protocol: ProtocolClient;
  readonly modelCatalog: ModelCatalogController;
  readonly subscriptionHealth: SubscriptionHealthController;
  readonly pullRequestGroupOrder: ReadonlySignal<readonly string[]>;
  readonly setPullRequestGroupOrder: (order: readonly string[]) => void;
  readonly nativeHost: NativeHostActions | undefined;
}

export interface WorkspaceScope {
  readonly workspaceId: string;
}

export interface SessionScope {
  readonly sessionId: string;
}

export interface ThreadScope {
  readonly threadId: string;
}

export interface TerminalScope {
  readonly terminalId: string;
}

export const appServicesContext = createContext<AppServices>(
  Symbol("trouve.app-services"),
);
export const appStoreContext = createContext<AppStore>(Symbol("trouve.app-store"));
export const hostCapabilitiesContext = createContext<HostCapabilitiesController>(
  Symbol("trouve.host-capabilities"),
);
export const workspaceContext = createContext<WorkspaceScope>(
  Symbol("trouve.workspace"),
);
export const sessionContext = createContext<SessionScope>(Symbol("trouve.session"));
export const threadContext = createContext<ThreadScope>(Symbol("trouve.thread"));
export const terminalContext = createContext<TerminalScope>(Symbol("trouve.terminal"));
