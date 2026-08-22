import createClient, { type Client } from "openapi-fetch";

import {
  desktopUpdateState,
  hostBootstrap,
  hostLifecycleBatch,
  hostPreferences,
  pickDirectoryResponse,
  pickFilesResponse,
  readClipboardImageResponse,
} from "../generated/host-validators.js";
import type {
  components as HostComponents,
  paths as HostPaths,
} from "../generated/host.js";
import type { HostCapabilities } from "./capabilities.js";
import {
  DEFAULT_GENERAL_PREFERENCES,
  type GeneralPreferences,
} from "./general-preferences.js";
import {
  DEFAULT_CHAT_PREFERENCES,
  type ChatPreferences,
} from "./chat-preferences.js";
import {
  DEFAULT_NOTIFICATION_PREFERENCES,
  type NotificationPreferences,
} from "./notification-preferences.js";
import { normalizeWorkspaceOrder } from "./workspace-order.js";
import { normalizePullRequestGroupOrder } from "./pull-request-group-order.js";
import {
  normalizeResumePreferences,
  type ResumePreferences,
} from "./resume-preferences.js";
import {
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "./attachments.js";
import { normalizeSystemFontFamilies } from "./system-fonts.js";

type ValidateFunction = (value: unknown) => boolean;

type HostBootstrapWire = HostComponents["schemas"]["HostBootstrap"];
type HostLifecycleBatchWire = HostComponents["schemas"]["HostLifecycleBatch"];
type PickDirectoryResponseWire = HostComponents["schemas"]["PickDirectoryResponse"];
type AttachmentPayloadWire = HostComponents["schemas"]["AttachmentPayload"];
type PickFilesResponseWire = HostComponents["schemas"]["PickFilesResponse"];
type ReadClipboardImageResponseWire =
  HostComponents["schemas"]["ReadClipboardImageResponse"];
export type HostPreferences = HostComponents["schemas"]["HostPreferences"];
type HostPreferencesUpdate = HostComponents["schemas"]["HostPreferencesUpdate"];
export type HostCloseDecision = HostComponents["schemas"]["CloseDecision"];
export type HostLocalFileAction = HostComponents["schemas"]["LocalFileAction"];

export interface HostPendingCloseRequest {
  readonly requestId: number;
  readonly waitingForIdle: boolean;
}

export interface HostLifecycleState {
  readonly focused: boolean;
  readonly visible: boolean;
  readonly occluded: boolean;
  readonly pendingClose: HostPendingCloseRequest | undefined;
}

export type HostLifecycleEvent =
  | { readonly type: "focus_changed"; readonly focused: boolean }
  | { readonly type: "visibility_changed"; readonly visible: boolean }
  | { readonly type: "occlusion_changed"; readonly occluded: boolean }
  | { readonly type: "close_requested"; readonly requestId: number }
  | {
      readonly type: "notification_activated";
      readonly notificationId: string;
      readonly sessionId: string;
      readonly threadId: string | undefined;
    };

export interface HostLifecycleEnvelope {
  readonly cursor: number;
  readonly event: HostLifecycleEvent;
}

export interface HostLifecycleBatch {
  readonly cursor: number;
  readonly state: HostLifecycleState;
  readonly events: readonly HostLifecycleEnvelope[];
}

export interface NativeNotificationRequest {
  readonly title: string;
  readonly body: string;
  readonly sound: boolean;
  readonly sessionId: string;
  readonly threadId: string | undefined;
}

export type DesktopUpdatePhase =
  | "disabled"
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "verifying"
  | "installing"
  | "restarting"
  | "error";

export interface DesktopUpdateState {
  readonly currentVersion: string;
  readonly availableVersion: string | undefined;
  readonly phase: DesktopUpdatePhase;
  readonly message: string;
  readonly progressPercent: number | undefined;
}

export interface WatchHostLifecycleOptions {
  readonly after?: number;
  readonly waitMs?: number;
  readonly signal?: AbortSignal;
}

const HOST_CAPABILITIES_PATH = "/__trouve/host/v1/capabilities" as const;
const HOST_PREFERENCES_PATH = "/__trouve/host/v1/preferences" as const;
const HOST_PICK_DIRECTORY_PATH = "/__trouve/host/v1/pick-directory" as const;
const HOST_PICK_FILES_PATH = "/__trouve/host/v1/pick-files" as const;
const HOST_READ_CLIPBOARD_IMAGE_PATH =
  "/__trouve/host/v1/read-clipboard-image" as const;
const HOST_OPEN_HTTPS_URL_PATH = "/__trouve/host/v1/open-https-url" as const;
const HOST_LIFECYCLE_PATH = "/__trouve/host/v1/lifecycle" as const;
const HOST_CLOSE_ACKNOWLEDGEMENT_PATH =
  "/__trouve/host/v1/close-acknowledgement" as const;
const HOST_CLOSE_DECISION_PATH = "/__trouve/host/v1/close-decision" as const;
const HOST_SLEEP_INHIBITION_PATH =
  "/__trouve/host/v1/sleep-inhibition" as const;
const HOST_NATIVE_NOTIFICATION_PATH =
  "/__trouve/host/v1/native-notification" as const;
const HOST_USER_ATTENTION_PATH =
  "/__trouve/host/v1/request-user-attention" as const;
const HOST_LOCAL_FILE_ACTION_PATH =
  "/__trouve/host/v1/local-file-action" as const;
const HOST_DESKTOP_UPDATE_PATH = "/__trouve/host/v1/update" as const;
const HOST_DESKTOP_UPDATE_CHECK_PATH = "/__trouve/host/v1/update/check" as const;
const HOST_DESKTOP_UPDATE_INSTALL_PATH = "/__trouve/host/v1/update/install" as const;
const CSRF_HEADER = "x-trouve-host-csrf";
const DIRECTORY_PICKER_BRIDGE_VERSION = 3;
const NATIVE_ATTACHMENT_BRIDGE_VERSION = 4;
const NATIVE_LIFECYCLE_BRIDGE_VERSION = 5;
const CLOSE_ACKNOWLEDGEMENT_BRIDGE_VERSION = 13;
const SELF_UPDATE_BRIDGE_VERSION = 14;
const MAX_LIFECYCLE_WAIT_MS = 25_000;
const MAX_LIFECYCLE_EVENTS = 128;
const MAX_HOST_ID_BYTES = 256;
const MAX_ATTACHMENT_NAME_BYTES = 1_024;
const MAX_ATTACHMENT_MIME_BYTES = 255;
const MAX_ENCODED_ATTACHMENT_BYTES = Math.ceil(MAX_ATTACHMENT_BYTES / 3) * 4;

type HostSchemaName =
  | "HostBootstrap"
  | "HostPreferences"
  | "HostLifecycleBatch"
  | "DesktopUpdateState"
  | "PickDirectoryResponse"
  | "PickFilesResponse"
  | "ReadClipboardImageResponse";

const schemaValidators = new Map<HostSchemaName, ValidateFunction>([
  ["HostBootstrap", hostBootstrap],
  ["HostPreferences", hostPreferences],
  ["HostLifecycleBatch", hostLifecycleBatch],
  ["DesktopUpdateState", desktopUpdateState],
  ["PickDirectoryResponse", pickDirectoryResponse],
  ["PickFilesResponse", pickFilesResponse],
  ["ReadClipboardImageResponse", readClipboardImageResponse],
]);

const validate = <T>(name: HostSchemaName, value: unknown): T => {
  const validator = schemaValidators.get(name);
  if (validator === undefined || !validator(value)) {
    throw new HostClientError("invalid-response", `desktop host returned invalid ${name}`);
  }
  return value as T;
};

export class HostClientError extends Error {
  constructor(
    readonly kind:
      | "request-failed"
      | "invalid-response"
      | "invalid-request"
      | "not-bootstrapped"
      | "capability-unavailable"
      | "action-busy",
    message: string,
  ) {
    super(message);
    this.name = "HostClientError";
  }
}

interface PendingPreferenceWrite {
  preferences: HostPreferences;
  waiters: Array<{
    resolve: (preferences: HostPreferences) => void;
    reject: (reason: unknown) => void;
  }>;
}

export const mapHostCapabilities = (
  wire: HostComponents["schemas"]["HostCapabilities"],
): HostCapabilities => {
  const hasLifecycleBridge =
    wire.bridge_version != null &&
    wire.bridge_version >= NATIVE_LIFECYCLE_BRIDGE_VERSION;
  return Object.freeze({
    kind: wire.kind,
    ...(wire.bridge_version == null ? {} : { bridgeVersion: wire.bridge_version }),
    directoryPicker:
      wire.directory_picker &&
      wire.bridge_version != null &&
      wire.bridge_version >= DIRECTORY_PICKER_BRIDGE_VERSION,
    filePicker:
      wire.file_picker &&
      wire.bridge_version != null &&
      wire.bridge_version >= NATIVE_ATTACHMENT_BRIDGE_VERSION,
    clipboardImage:
      wire.clipboard_image &&
      wire.bridge_version != null &&
      wire.bridge_version >= NATIVE_ATTACHMENT_BRIDGE_VERSION,
    lifecycleEvents: wire.lifecycle_events && hasLifecycleBridge,
    closeConfirmation: wire.close_confirmation && hasLifecycleBridge,
    openLocalFile: wire.open_local_file && hasLifecycleBridge,
    revealLocalFile: wire.reveal_local_file && hasLifecycleBridge,
    openHttpsUrl: wire.open_https_url,
    nativeNotifications: wire.native_notifications && hasLifecycleBridge,
    webNotifications: wire.web_notifications,
    userAttention: wire.user_attention && hasLifecycleBridge,
    sleepInhibition: wire.sleep_inhibition && hasLifecycleBridge,
    windowGeometry: wire.window_geometry,
    visibility: wire.visibility && hasLifecycleBridge,
    occlusion: wire.occlusion && hasLifecycleBridge,
    persistentPreferences: wire.persistent_preferences,
    installable: wire.installable,
    selfUpdate:
      (wire.self_update ?? false) &&
      wire.bridge_version != null &&
      wire.bridge_version >= SELF_UPDATE_BRIDGE_VERSION,
  });
};

const normalizeDesktopUpdateState = (
  wire: HostComponents["schemas"]["DesktopUpdateState"],
): DesktopUpdateState => Object.freeze({
  currentVersion: wire.current_version,
  availableVersion: wire.available_version ?? undefined,
  phase: wire.phase,
  message: wire.message,
  progressPercent: wire.progress_percent ?? undefined,
});

const DESKTOP_UPDATE_REQUEST_TIMEOUT_MS = 30_000;

const withDesktopUpdateDeadline = async <T>(
  request: (signal: AbortSignal) => Promise<T>,
): Promise<T> => {
  const controller = new AbortController();
  const timer = globalThis.setTimeout(
    () => controller.abort(),
    DESKTOP_UPDATE_REQUEST_TIMEOUT_MS,
  );
  try {
    return await request(controller.signal);
  } finally {
    globalThis.clearTimeout(timer);
  }
};

const samePreferenceValue = (left: unknown, right: unknown): boolean =>
  JSON.stringify(left) === JSON.stringify(right);

const rebasePreferenceLeaf = <T>(baseline: T, incoming: T, saved: T): T =>
  samePreferenceValue(incoming, baseline) ? saved : incoming;

const rebasePreferenceMap = <T>(
  baseline: Readonly<Record<string, T>> | undefined,
  incoming: Readonly<Record<string, T>> | undefined,
  saved: Readonly<Record<string, T>> | undefined,
): Record<string, T> => {
  const merged: Record<string, T> = { ...saved };
  for (const key of new Set([
    ...Object.keys(baseline ?? {}),
    ...Object.keys(incoming ?? {}),
  ])) {
    const before = baseline?.[key];
    const next = incoming?.[key];
    if (samePreferenceValue(next, before)) continue;
    if (next === undefined) delete merged[key];
    else merged[key] = next;
  }
  return merged;
};

const rebaseHostPreferenceChanges = (
  baseline: HostPreferences,
  incoming: HostPreferences,
  saved: HostPreferences,
): HostPreferences => {
  const baselineResume = baseline.resume ?? {};
  const incomingResume = incoming.resume ?? {};
  const savedResume = saved.resume ?? {};
  return {
    ...saved,
    appearance: {
      theme: rebasePreferenceLeaf(
        baseline.appearance.theme,
        incoming.appearance.theme,
        saved.appearance.theme,
      ),
      font_family: rebasePreferenceLeaf(
        baseline.appearance.font_family,
        incoming.appearance.font_family,
        saved.appearance.font_family,
      ),
      font_size: rebasePreferenceLeaf(
        baseline.appearance.font_size,
        incoming.appearance.font_size,
        saved.appearance.font_size,
      ),
      reduce_motion: rebasePreferenceLeaf(
        baseline.appearance.reduce_motion,
        incoming.appearance.reduce_motion,
        saved.appearance.reduce_motion,
      ),
    },
    general: {
      ...saved.general,
      prevent_sleep_while_running: rebasePreferenceLeaf(
        baseline.general?.prevent_sleep_while_running ?? true,
        incoming.general?.prevent_sleep_while_running ?? true,
        saved.general?.prevent_sleep_while_running ?? true,
      ),
      automatic_updates: rebasePreferenceLeaf(
        baseline.general?.automatic_updates ?? true,
        incoming.general?.automatic_updates ?? true,
        saved.general?.automatic_updates ?? true,
      ),
    },
    chat: {
      ...saved.chat,
      collapse_sequential_tool_calls: rebasePreferenceLeaf(
        baseline.chat?.collapse_sequential_tool_calls ?? true,
        incoming.chat?.collapse_sequential_tool_calls ?? true,
        saved.chat?.collapse_sequential_tool_calls ?? true,
      ),
      collapse_thinking_with_tools: rebasePreferenceLeaf(
        baseline.chat?.collapse_thinking_with_tools ?? false,
        incoming.chat?.collapse_thinking_with_tools ?? false,
        saved.chat?.collapse_thinking_with_tools ?? false,
      ),
      collapse_compaction_with_tools: rebasePreferenceLeaf(
        baseline.chat?.collapse_compaction_with_tools ?? false,
        incoming.chat?.collapse_compaction_with_tools ?? false,
        saved.chat?.collapse_compaction_with_tools ?? false,
      ),
      collapse_todo_updates_with_tools: rebasePreferenceLeaf(
        baseline.chat?.collapse_todo_updates_with_tools ?? false,
        incoming.chat?.collapse_todo_updates_with_tools ?? false,
        saved.chat?.collapse_todo_updates_with_tools ?? false,
      ),
    },
    notifications: {
      ...saved.notifications,
      enabled: rebasePreferenceLeaf(
        baseline.notifications?.enabled ?? true,
        incoming.notifications?.enabled ?? true,
        saved.notifications?.enabled ?? true,
      ),
      on_finish: rebasePreferenceLeaf(
        baseline.notifications?.on_finish ?? true,
        incoming.notifications?.on_finish ?? true,
        saved.notifications?.on_finish ?? true,
      ),
      on_fail: rebasePreferenceLeaf(
        baseline.notifications?.on_fail ?? true,
        incoming.notifications?.on_fail ?? true,
        saved.notifications?.on_fail ?? true,
      ),
      on_attention: rebasePreferenceLeaf(
        baseline.notifications?.on_attention ?? true,
        incoming.notifications?.on_attention ?? true,
        saved.notifications?.on_attention ?? true,
      ),
      sound: rebasePreferenceLeaf(
        baseline.notifications?.sound ?? false,
        incoming.notifications?.sound ?? false,
        saved.notifications?.sound ?? false,
      ),
    },
    workspace_order: rebasePreferenceLeaf(
      baseline.workspace_order ?? [],
      incoming.workspace_order ?? [],
      saved.workspace_order ?? [],
    ),
    pull_request_group_order: rebasePreferenceLeaf(
      baseline.pull_request_group_order ?? [],
      incoming.pull_request_group_order ?? [],
      saved.pull_request_group_order ?? [],
    ),
    resume: {
      ...savedResume,
      selected_session_id: rebasePreferenceLeaf(
        baselineResume.selected_session_id ?? "",
        incomingResume.selected_session_id ?? "",
        savedResume.selected_session_id ?? "",
      ),
      session_threads: rebasePreferenceMap(
        baselineResume.session_threads,
        incomingResume.session_threads,
        savedResume.session_threads,
      ),
      thread_scroll: rebasePreferenceMap(
        baselineResume.thread_scroll,
        incomingResume.thread_scroll,
        savedResume.thread_scroll,
      ),
      closed_thread_tabs: rebasePreferenceLeaf(
        baselineResume.closed_thread_tabs ?? [],
        incomingResume.closed_thread_tabs ?? [],
        savedResume.closed_thread_tabs ?? [],
      ),
      pinned_thread_tabs: rebasePreferenceLeaf(
        baselineResume.pinned_thread_tabs ?? [],
        incomingResume.pinned_thread_tabs ?? [],
        savedResume.pinned_thread_tabs ?? [],
      ),
    },
    navigation_width: rebasePreferenceLeaf(
      baseline.navigation_width,
      incoming.navigation_width,
      saved.navigation_width,
    ),
    inspection_width: rebasePreferenceLeaf(
      baseline.inspection_width,
      incoming.inspection_width,
      saved.inspection_width,
    ),
  };
};

export class HostClient {
  readonly #client: Client<HostPaths>;
  #csrfToken: string | undefined;
  #directoryPickerAvailable = false;
  #filePickerAvailable = false;
  #clipboardImageAvailable = false;
  #openHttpsUrlAvailable = false;
  #lifecycleAvailable = false;
  #closeConfirmationAvailable = false;
  #closeAcknowledgementAvailable = false;
  #sleepInhibitionAvailable = false;
  #nativeNotificationsAvailable = false;
  #userAttentionAvailable = false;
  #openLocalFileAvailable = false;
  #revealLocalFileAvailable = false;
  #selfUpdateAvailable = false;
  #fontFamilies: readonly string[] = Object.freeze([]);
  #notificationSequence = 0;
  readonly #notificationActivations = new Map<string, () => void>();
  #preferenceWriteRunning = false;
  #pendingPreferenceWrite: PendingPreferenceWrite | undefined;
  #preferenceBaseline: HostPreferences | undefined;

  constructor(baseUrl: string, fetchImplementation: typeof fetch = globalThis.fetch) {
    this.#client = createClient<HostPaths>({ baseUrl, fetch: fetchImplementation });
  }

  async bootstrap(): Promise<HostCapabilities> {
    let result;
    try {
      result = await this.#client.GET(HOST_CAPABILITIES_PATH);
    } catch {
      throw new HostClientError("request-failed", "desktop host bootstrap request failed");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop host bootstrap request failed");
    }
    const bootstrap = validate<HostBootstrapWire>("HostBootstrap", result.data);
    const capabilities = mapHostCapabilities(bootstrap.capabilities);
    this.#fontFamilies = normalizeSystemFontFamilies(bootstrap.font_families ?? []);
    this.#csrfToken = bootstrap.csrf_token;
    this.#directoryPickerAvailable = capabilities.directoryPicker;
    this.#filePickerAvailable = capabilities.filePicker;
    this.#clipboardImageAvailable = capabilities.clipboardImage;
    this.#openHttpsUrlAvailable = capabilities.openHttpsUrl;
    this.#lifecycleAvailable = capabilities.lifecycleEvents;
    this.#closeConfirmationAvailable = capabilities.closeConfirmation;
    this.#closeAcknowledgementAvailable = capabilities.closeConfirmation
      && (bootstrap.capabilities.bridge_version ?? 0) >= CLOSE_ACKNOWLEDGEMENT_BRIDGE_VERSION;
    this.#sleepInhibitionAvailable = capabilities.sleepInhibition;
    this.#nativeNotificationsAvailable = capabilities.nativeNotifications;
    this.#userAttentionAvailable = capabilities.userAttention;
    this.#openLocalFileAvailable = capabilities.openLocalFile;
    this.#revealLocalFileAvailable = capabilities.revealLocalFile;
    this.#selfUpdateAvailable = capabilities.selfUpdate;
    return capabilities;
  }

  /** Installed system font families captured by the latest host bootstrap. */
  systemFontFamilies(): readonly string[] {
    return this.#fontFamilies;
  }

  async pickDirectory(): Promise<string | undefined> {
    const csrfToken = this.#nativeActionToken(
      this.#directoryPickerAvailable,
      "desktop directory picker is unavailable",
    );
    let result;
    try {
      result = await this.#client.POST(HOST_PICK_DIRECTORY_PATH, {
        headers: { [CSRF_HEADER]: csrfToken },
      });
    } catch {
      throw new HostClientError("request-failed", "desktop directory picker failed");
    }
    if (result.response.status === 409) {
      throw new HostClientError("action-busy", "desktop directory picker is already open");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop directory picker failed");
    }
    const response = validate<PickDirectoryResponseWire>(
      "PickDirectoryResponse",
      result.data,
    );
    const path = response.path;
    if (path == null) return undefined;
    if (
      path === "" ||
      path.length > 32 * 1024 ||
      /[\u0000-\u001f\u007f]/u.test(path)
    ) {
      throw new HostClientError(
        "invalid-response",
        "desktop host returned invalid PickDirectoryResponse",
      );
    }
    return path;
  }

  async pickFiles(): Promise<readonly PendingAttachment[]> {
    const csrfToken = this.#nativeActionToken(
      this.#filePickerAvailable,
      "desktop file picker is unavailable",
    );
    let result;
    try {
      result = await this.#client.POST(HOST_PICK_FILES_PATH, {
        headers: { [CSRF_HEADER]: csrfToken },
      });
    } catch {
      throw new HostClientError("request-failed", "desktop file picker failed");
    }
    if (result.response.status === 409) {
      throw new HostClientError("action-busy", "a desktop file picker is already open");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop file picker failed");
    }
    const response = validate<PickFilesResponseWire>(
      "PickFilesResponse",
      result.data,
    );
    const attachments = response.attachments.map((attachment) =>
      pendingAttachment(attachment, false),
    );
    if (
      attachments.length > MAX_PENDING_ATTACHMENTS ||
      attachments.reduce((total, attachment) => total + attachment.size, 0) >
        MAX_PENDING_ATTACHMENT_BYTES
    ) {
      throw invalidAttachmentResponse("PickFilesResponse");
    }
    return Object.freeze(attachments);
  }

  async readClipboardImage(): Promise<PendingAttachment | undefined> {
    const csrfToken = this.#nativeActionToken(
      this.#clipboardImageAvailable,
      "desktop clipboard image reader is unavailable",
    );
    let result;
    try {
      result = await this.#client.POST(HOST_READ_CLIPBOARD_IMAGE_PATH, {
        headers: { [CSRF_HEADER]: csrfToken },
      });
    } catch {
      throw new HostClientError(
        "request-failed",
        "desktop clipboard image read failed",
      );
    }
    if (result.response.status === 409) {
      throw new HostClientError(
        "action-busy",
        "a desktop clipboard image read is already running",
      );
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError(
        "request-failed",
        "desktop clipboard image read failed",
      );
    }
    const response = validate<ReadClipboardImageResponseWire>(
      "ReadClipboardImageResponse",
      result.data,
    );
    return response.attachment === null
      ? undefined
      : pendingAttachment(response.attachment, true);
  }

  async openHttpsUrl(value: string): Promise<void> {
    const csrfToken = this.#nativeActionToken(
      this.#openHttpsUrlAvailable,
      "desktop external URL opening is unavailable",
    );
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      throw new HostClientError("invalid-request", "invalid desktop external URL");
    }
    if (
      url.protocol !== "https:" ||
      url.username !== "" ||
      url.password !== "" ||
      url.host === "" ||
      /[\u0000-\u001f\u007f]/u.test(value) ||
      url.href.length > 8_000
    ) {
      throw new HostClientError("invalid-request", "invalid desktop external URL");
    }
    let result;
    try {
      result = await this.#client.POST(HOST_OPEN_HTTPS_URL_PATH, {
        body: { url: url.href },
        headers: { [CSRF_HEADER]: csrfToken },
      });
    } catch {
      throw new HostClientError("request-failed", "desktop external URL open failed");
    }
    if (!result.response.ok) {
      throw new HostClientError("request-failed", "desktop external URL open failed");
    }
  }

  /** Reads a bounded batch of ephemeral native window events. This feed is
   * deliberately separate from the durable protocol event log. */
  async pollLifecycle(
    after = 0,
    waitMs = MAX_LIFECYCLE_WAIT_MS,
    signal?: AbortSignal,
  ): Promise<HostLifecycleBatch> {
    this.#nativeActionToken(
      this.#lifecycleAvailable,
      "desktop lifecycle events are unavailable",
    );
    if (
      !Number.isSafeInteger(after) ||
      after < 0 ||
      !Number.isSafeInteger(waitMs) ||
      waitMs < 0 ||
      waitMs > MAX_LIFECYCLE_WAIT_MS
    ) {
      throw new HostClientError("invalid-request", "invalid desktop lifecycle cursor");
    }
    let result;
    try {
      result = await this.#client.GET(HOST_LIFECYCLE_PATH, {
        params: { query: { after, wait_ms: waitMs } },
        ...(signal === undefined ? {} : { signal }),
      });
    } catch (error) {
      if (signal?.aborted === true) throw error;
      throw new HostClientError("request-failed", "desktop lifecycle request failed");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop lifecycle request failed");
    }
    const wire = validate<HostLifecycleBatchWire>(
      "HostLifecycleBatch",
      result.data,
    );
    const batch = normalizeLifecycleBatch(wire, after);
    this.#dispatchNotificationActivations(batch);
    return batch;
  }

  /** Continuously long-polls until aborted. Consumers own app-idle and close
   * policy; the native host only reports state and applies explicit choices. */
  async watchLifecycle(
    receive: (batch: HostLifecycleBatch) => void,
    options: WatchHostLifecycleOptions = {},
  ): Promise<void> {
    let cursor = options.after ?? 0;
    const waitMs = options.waitMs ?? MAX_LIFECYCLE_WAIT_MS;
    for (;;) {
      let batch: HostLifecycleBatch;
      try {
        batch = await this.pollLifecycle(cursor, waitMs, options.signal);
      } catch (error) {
        if (options.signal?.aborted === true) return;
        throw error;
      }
      if (options.signal?.aborted === true) return;
      cursor = batch.cursor;
      receive(batch);
    }
  }

  /** `quit_when_idle` only arms the host close request. The frontend must
   * derive idleness from protocol/app state and later send `quit_now` using
   * the same request id. */
  async resolveClose(requestId: number, decision: HostCloseDecision): Promise<void> {
    const csrfToken = this.#nativeActionToken(
      this.#closeConfirmationAvailable,
      "desktop close confirmation is unavailable",
    );
    if (
      !Number.isSafeInteger(requestId) ||
      requestId <= 0 ||
      !["cancel", "quit_now", "quit_when_idle"].includes(decision)
    ) {
      throw new HostClientError("invalid-request", "invalid desktop close decision");
    }
    await this.#performNativeAction(
      () =>
        this.#client.POST(HOST_CLOSE_DECISION_PATH, {
          body: { request_id: requestId, decision },
          headers: { [CSRF_HEADER]: csrfToken },
        }),
      "desktop close decision failed",
    );
  }

  /** Confirm that the exact pending request reached frontend-owned close UI.
   * This disarms the native broken-frontend watchdog without choosing. */
  async acknowledgeClose(requestId: number): Promise<void> {
    if (!Number.isSafeInteger(requestId) || requestId <= 0) {
      throw new HostClientError("invalid-request", "invalid desktop close request");
    }
    // Bridge 12 and earlier had no acknowledgement route and no healthy-UI
    // watchdog to disarm. Preserve their existing decision-only workflow.
    if (!this.#closeAcknowledgementAvailable) return;
    const csrfToken = this.#nativeActionToken(
      this.#closeConfirmationAvailable,
      "desktop close confirmation is unavailable",
    );
    await this.#performNativeAction(
      () =>
        this.#client.POST(HOST_CLOSE_ACKNOWLEDGEMENT_PATH, {
          body: { request_id: requestId },
          headers: { [CSRF_HEADER]: csrfToken },
        }),
      "desktop close acknowledgement failed",
    );
  }

  async setSleepInhibition(active: boolean): Promise<void> {
    const csrfToken = this.#nativeActionToken(
      this.#sleepInhibitionAvailable,
      "desktop sleep inhibition is unavailable",
    );
    if (typeof active !== "boolean") {
      throw new HostClientError("invalid-request", "invalid sleep inhibition state");
    }
    await this.#performNativeAction(
      () =>
        this.#client.POST(HOST_SLEEP_INHIBITION_PATH, {
          body: { active },
          headers: { [CSRF_HEADER]: csrfToken },
        }),
      "desktop sleep inhibition update failed",
    );
  }

  async showNativeNotification(
    request: NativeNotificationRequest,
    onActivate?: () => void,
  ): Promise<void> {
    const csrfToken = this.#nativeActionToken(
      this.#nativeNotificationsAvailable,
      "desktop native notifications are unavailable",
    );
    validateNotificationRequest(request);
    this.#notificationSequence = (this.#notificationSequence + 1) % 0x7fff_ffff;
    const notificationId = `notice-${Date.now().toString(36)}-${this.#notificationSequence.toString(36)}`;
    if (onActivate !== undefined) {
      while (this.#notificationActivations.size >= MAX_LIFECYCLE_EVENTS) {
        const oldest = this.#notificationActivations.keys().next().value as
          | string
          | undefined;
        if (oldest === undefined) break;
        this.#notificationActivations.delete(oldest);
      }
      this.#notificationActivations.set(notificationId, onActivate);
    }
    try {
      await this.#performNativeAction(
        () =>
          this.#client.POST(HOST_NATIVE_NOTIFICATION_PATH, {
            body: {
              notification_id: notificationId,
              title: request.title,
              body: request.body,
              sound: request.sound,
              session_id: request.sessionId,
              thread_id: request.threadId ?? null,
            },
            headers: { [CSRF_HEADER]: csrfToken },
          }),
        "desktop native notification failed",
      );
    } catch (error) {
      this.#notificationActivations.delete(notificationId);
      throw error;
    }
  }

  async requestUserAttention(): Promise<void> {
    const csrfToken = this.#nativeActionToken(
      this.#userAttentionAvailable,
      "desktop user attention is unavailable",
    );
    await this.#performNativeAction(
      () =>
        this.#client.POST(HOST_USER_ATTENTION_PATH, {
          headers: { [CSRF_HEADER]: csrfToken },
        }),
      "desktop user attention request failed",
    );
  }

  async actOnSessionFile(
    sessionId: string,
    relativePath: string,
    action: HostLocalFileAction,
  ): Promise<void> {
    const available =
      action === "open" ? this.#openLocalFileAvailable : this.#revealLocalFileAvailable;
    const csrfToken = this.#nativeActionToken(
      available,
      action === "open"
        ? "desktop local file opening is unavailable"
        : "desktop local file reveal is unavailable",
    );
    if (
      !validHostId(sessionId) ||
      !validRelativeFilePath(relativePath) ||
      (action !== "open" && action !== "reveal")
    ) {
      throw new HostClientError("invalid-request", "invalid session-local file action");
    }
    await this.#performNativeAction(
      () =>
        this.#client.POST(HOST_LOCAL_FILE_ACTION_PATH, {
          body: { session_id: sessionId, relative_path: relativePath, action },
          headers: { [CSRF_HEADER]: csrfToken },
        }),
      "desktop session-local file action failed",
    );
  }

  async getDesktopUpdate(): Promise<DesktopUpdateState> {
    this.#nativeActionToken(
      this.#selfUpdateAvailable,
      "desktop self-update is unavailable",
    );
    let result;
    try {
      result = await withDesktopUpdateDeadline((signal) =>
        this.#client.GET(HOST_DESKTOP_UPDATE_PATH, { signal })
      );
    } catch {
      throw new HostClientError("request-failed", "desktop update status failed");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop update status failed");
    }
    return normalizeDesktopUpdateState(
      validate<HostComponents["schemas"]["DesktopUpdateState"]>(
        "DesktopUpdateState",
        result.data,
      ),
    );
  }

  async checkDesktopUpdate(): Promise<DesktopUpdateState> {
    return this.#runDesktopUpdateAction(
      HOST_DESKTOP_UPDATE_CHECK_PATH,
      "desktop update check failed",
    );
  }

  async installDesktopUpdate(): Promise<DesktopUpdateState> {
    return this.#runDesktopUpdateAction(
      HOST_DESKTOP_UPDATE_INSTALL_PATH,
      "desktop update installation failed",
    );
  }

  async #runDesktopUpdateAction(
    path: typeof HOST_DESKTOP_UPDATE_CHECK_PATH | typeof HOST_DESKTOP_UPDATE_INSTALL_PATH,
    failureMessage: string,
  ): Promise<DesktopUpdateState> {
    const csrfToken = this.#nativeActionToken(
      this.#selfUpdateAvailable,
      "desktop self-update is unavailable",
    );
    let result;
    try {
      result = path === HOST_DESKTOP_UPDATE_CHECK_PATH
        ? await withDesktopUpdateDeadline((signal) =>
          this.#client.POST(HOST_DESKTOP_UPDATE_CHECK_PATH, {
              headers: { [CSRF_HEADER]: csrfToken },
              signal,
            })
        )
        : await this.#client.POST(HOST_DESKTOP_UPDATE_INSTALL_PATH, {
            // Installation may legitimately outlive the short status/check
            // deadline. Keep this request attached until the native host
            // reports its authoritative terminal outcome.
            headers: { [CSRF_HEADER]: csrfToken },
          });
    } catch {
      throw new HostClientError("request-failed", failureMessage);
    }
    if (result.response.status === 409) {
      throw new HostClientError("action-busy", "a desktop update action is already running");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", failureMessage);
    }
    return normalizeDesktopUpdateState(
      validate<HostComponents["schemas"]["DesktopUpdateState"]>(
        "DesktopUpdateState",
        result.data,
      ),
    );
  }

  async getPreferences(): Promise<HostPreferences> {
    let result;
    try {
      result = await this.#client.GET(HOST_PREFERENCES_PATH);
    } catch {
      throw new HostClientError("request-failed", "desktop preferences request failed");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop preferences request failed");
    }
    const preferences = validate<HostPreferences>("HostPreferences", result.data);
    this.#preferenceBaseline = preferences;
    return preferences;
  }

  putPreferences(preferences: HostPreferences): Promise<HostPreferences> {
    const result = new Promise<HostPreferences>((resolve, reject) => {
      if (this.#pendingPreferenceWrite === undefined) {
        this.#pendingPreferenceWrite = {
          preferences,
          waiters: [{ resolve, reject }],
        };
      } else {
        this.#pendingPreferenceWrite.preferences = preferences;
        this.#pendingPreferenceWrite.waiters.push({ resolve, reject });
      }
    });
    void this.#drainPreferenceWrites();
    return result;
  }

  async #drainPreferenceWrites(): Promise<void> {
    if (this.#preferenceWriteRunning) return;
    this.#preferenceWriteRunning = true;
    while (this.#pendingPreferenceWrite !== undefined) {
      const pending = this.#pendingPreferenceWrite;
      this.#pendingPreferenceWrite = undefined;
      try {
        const saved = await this.#putPreferencesNow(pending.preferences);
        for (const waiter of pending.waiters) waiter.resolve(saved);
        const queued = this.#queuedPreferenceWrite();
        if (queued !== undefined) {
          queued.preferences = rebaseHostPreferenceChanges(
            pending.preferences,
            queued.preferences,
            saved,
          );
        }
      } catch (error) {
        for (const waiter of pending.waiters) waiter.reject(error);
        if (this.#queuedPreferenceWrite() !== undefined) {
          try {
            const latest = await this.getPreferences();
            const queued = this.#queuedPreferenceWrite();
            if (queued !== undefined) {
              queued.preferences = rebaseHostPreferenceChanges(
                pending.preferences,
                queued.preferences,
                latest,
              );
            }
          } catch (refreshError) {
            const queued = this.#queuedPreferenceWrite();
            this.#pendingPreferenceWrite = undefined;
            if (queued !== undefined) {
              for (const waiter of queued.waiters) waiter.reject(refreshError);
            }
          }
        }
      }
    }
    this.#preferenceWriteRunning = false;
  }

  #queuedPreferenceWrite(): PendingPreferenceWrite | undefined {
    return this.#pendingPreferenceWrite;
  }

  async #putPreferencesNow(preferences: HostPreferences): Promise<HostPreferences> {
    if (this.#csrfToken === undefined) {
      throw new HostClientError(
        "not-bootstrapped",
        "desktop host must bootstrap before preference writes",
      );
    }
    const baseline = this.#preferenceBaseline ?? await this.getPreferences();
    const update: HostPreferencesUpdate = { baseline, preferences };
    let result;
    try {
      result = await this.#client.PUT(HOST_PREFERENCES_PATH, {
        body: update,
        headers: { [CSRF_HEADER]: this.#csrfToken },
      });
    } catch {
      throw new HostClientError("request-failed", "desktop preference update failed");
    }
    if (result.data === undefined || !result.response.ok) {
      throw new HostClientError("request-failed", "desktop preference update failed");
    }
    const saved = validate<HostPreferences>("HostPreferences", result.data);
    this.#preferenceBaseline = saved;
    return saved;
  }

  mutationHeaders(): Readonly<Record<string, string>> {
    return this.#csrfToken === undefined ? {} : { [CSRF_HEADER]: this.#csrfToken };
  }

  #nativeActionToken(available: boolean, unavailableMessage: string): string {
    if (this.#csrfToken === undefined) {
      throw new HostClientError(
        "not-bootstrapped",
        "desktop host must bootstrap before native actions",
      );
    }
    if (!available) {
      throw new HostClientError("capability-unavailable", unavailableMessage);
    }
    return this.#csrfToken;
  }

  #dispatchNotificationActivations(batch: HostLifecycleBatch): void {
    for (const envelope of batch.events) {
      if (envelope.event.type !== "notification_activated") continue;
      const activate = this.#notificationActivations.get(
        envelope.event.notificationId,
      );
      if (activate === undefined) continue;
      this.#notificationActivations.delete(envelope.event.notificationId);
      try {
        activate();
      } catch {
        // Native activation callbacks are best-effort UI navigation hooks.
      }
    }
  }

  async #performNativeAction(
    action: () => Promise<{ readonly response: Response }>,
    failureMessage: string,
  ): Promise<void> {
    let result;
    try {
      result = await action();
    } catch {
      throw new HostClientError("request-failed", failureMessage);
    }
    if (!result.response.ok) {
      throw new HostClientError("request-failed", failureMessage);
    }
  }
}

const utf8Length = (value: string): number =>
  new TextEncoder().encode(value).byteLength;

const validHostId = (value: string): boolean =>
  value.length > 0 &&
  utf8Length(value) <= MAX_HOST_ID_BYTES &&
  /^[A-Za-z0-9._-]+$/u.test(value);

const validHostText = (
  value: string,
  maximumBytes: number,
  allowEmpty: boolean,
): boolean =>
  (allowEmpty || value.trim() !== "") &&
  utf8Length(value) <= maximumBytes &&
  !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u.test(value);

const validRelativeFilePath = (value: string): boolean => {
  if (
    value === "" ||
    utf8Length(value) > 32 * 1024 ||
    value.startsWith("/") ||
    value.includes("\\") ||
    /[\u0000-\u001f\u007f]/u.test(value)
  ) return false;
  return value
    .split("/")
    .every((segment) => segment !== "" && segment !== "." && segment !== "..");
};

const validateNotificationRequest = (request: NativeNotificationRequest): void => {
  if (
    !validHostText(request.title, 256, false) ||
    !validHostText(request.body, 4 * 1024, true) ||
    typeof request.sound !== "boolean" ||
    !validHostId(request.sessionId) ||
    (request.threadId !== undefined && !validHostId(request.threadId))
  ) {
    throw new HostClientError("invalid-request", "invalid native notification");
  }
};

const safeCursor = (value: number, allowZero = true): boolean =>
  Number.isSafeInteger(value) && value >= (allowZero ? 0 : 1);

const normalizeLifecycleBatch = (
  wire: HostLifecycleBatchWire,
  after: number,
): HostLifecycleBatch => {
  if (
    !safeCursor(wire.cursor) ||
    wire.cursor < after ||
    wire.events.length > MAX_LIFECYCLE_EVENTS
  ) {
    throw new HostClientError(
      "invalid-response",
      "desktop host returned invalid HostLifecycleBatch",
    );
  }
  const pending = wire.state.pending_close ?? undefined;
  if (pending !== undefined && !safeCursor(pending.request_id, false)) {
    throw new HostClientError(
      "invalid-response",
      "desktop host returned invalid HostLifecycleBatch",
    );
  }
  const state: HostLifecycleState = Object.freeze({
    focused: wire.state.focused,
    visible: wire.state.visible,
    occluded: wire.state.occluded,
    pendingClose:
      pending === undefined
        ? undefined
        : Object.freeze({
            requestId: pending.request_id,
            waitingForIdle: pending.waiting_for_idle,
          }),
  });
  let previous = after;
  const events = wire.events.map((envelope): HostLifecycleEnvelope => {
    if (
      !safeCursor(envelope.cursor, false) ||
      envelope.cursor <= previous ||
      envelope.cursor > wire.cursor
    ) {
      throw new HostClientError(
        "invalid-response",
        "desktop host returned invalid HostLifecycleBatch",
      );
    }
    previous = envelope.cursor;
    const event = normalizeLifecycleEvent(envelope.event);
    return Object.freeze({ cursor: envelope.cursor, event });
  });
  return Object.freeze({ cursor: wire.cursor, state, events: Object.freeze(events) });
};

const normalizeLifecycleEvent = (
  event: HostComponents["schemas"]["HostLifecycleEvent"],
): HostLifecycleEvent => {
  switch (event.type) {
    case "focus_changed":
      return Object.freeze({ type: event.type, focused: event.focused });
    case "visibility_changed":
      return Object.freeze({ type: event.type, visible: event.visible });
    case "occlusion_changed":
      return Object.freeze({ type: event.type, occluded: event.occluded });
    case "close_requested":
      if (!safeCursor(event.request_id, false)) break;
      return Object.freeze({ type: event.type, requestId: event.request_id });
    case "notification_activated": {
      const threadId = event.thread_id ?? undefined;
      if (
        !validHostId(event.notification_id) ||
        !validHostId(event.session_id) ||
        (threadId !== undefined && !validHostId(threadId))
      ) break;
      return Object.freeze({
        type: event.type,
        notificationId: event.notification_id,
        sessionId: event.session_id,
        threadId,
      });
    }
  }
  throw new HostClientError(
    "invalid-response",
    "desktop host returned invalid HostLifecycleBatch",
  );
};

/** Maps backward-compatible host preference fields onto the frontend models. */
export const generalPreferencesFromHost = (
  preferences: HostPreferences,
): GeneralPreferences => Object.freeze({
  preventSleepWhileRunning:
    preferences.general?.prevent_sleep_while_running ??
    DEFAULT_GENERAL_PREFERENCES.preventSleepWhileRunning,
  automaticUpdates:
    preferences.general?.automatic_updates ??
    DEFAULT_GENERAL_PREFERENCES.automaticUpdates,
});

export const chatPreferencesFromHost = (
  preferences: HostPreferences,
  fallback: ChatPreferences = DEFAULT_CHAT_PREFERENCES,
): ChatPreferences => Object.freeze({
  collapseSequentialToolCalls:
    preferences.chat?.collapse_sequential_tool_calls ??
    fallback.collapseSequentialToolCalls,
  collapseThinkingWithTools:
    preferences.chat?.collapse_thinking_with_tools ??
    fallback.collapseThinkingWithTools,
  collapseCompactionWithTools:
    preferences.chat?.collapse_compaction_with_tools ??
    fallback.collapseCompactionWithTools,
  collapseTodoUpdatesWithTools:
    preferences.chat?.collapse_todo_updates_with_tools ??
    fallback.collapseTodoUpdatesWithTools,
});

export const notificationPreferencesFromHost = (
  preferences: HostPreferences,
): NotificationPreferences => Object.freeze({
  enabled:
    preferences.notifications?.enabled ?? DEFAULT_NOTIFICATION_PREFERENCES.enabled,
  onFinish:
    preferences.notifications?.on_finish ??
    DEFAULT_NOTIFICATION_PREFERENCES.onFinish,
  onFail:
    preferences.notifications?.on_fail ?? DEFAULT_NOTIFICATION_PREFERENCES.onFail,
  onAttention:
    preferences.notifications?.on_attention ??
    DEFAULT_NOTIFICATION_PREFERENCES.onAttention,
  sound: preferences.notifications?.sound ?? DEFAULT_NOTIFICATION_PREFERENCES.sound,
});

export const workspaceOrderFromHost = (
  preferences: HostPreferences,
): readonly string[] => normalizeWorkspaceOrder(preferences.workspace_order);

export const pullRequestGroupOrderFromHost = (
  preferences: HostPreferences,
): readonly string[] => normalizePullRequestGroupOrder(
  preferences.pull_request_group_order,
);

export const resumePreferencesFromHost = (
  preferences: HostPreferences,
): ResumePreferences => normalizeResumePreferences({
  selectedSessionId: preferences.resume?.selected_session_id ?? "",
  sessionThreads: preferences.resume?.session_threads ?? {},
  threadScroll: Object.fromEntries(
    Object.entries(preferences.resume?.thread_scroll ?? {}).map(
      ([threadId, bookmark]) => [threadId, {
        itemId: bookmark.item_id,
        offset: bookmark.offset,
      }],
    ),
  ),
  closedThreadTabs: preferences.resume?.closed_thread_tabs ?? [],
  pinnedThreadTabs: preferences.resume?.pinned_thread_tabs ?? [],
});

export const withHostGeneralPreferences = (
  preferences: HostPreferences,
  general: GeneralPreferences,
): HostPreferences => ({
  ...preferences,
  general: {
    prevent_sleep_while_running: general.preventSleepWhileRunning,
    automatic_updates: general.automaticUpdates,
  },
});

export const withHostChatPreferences = (
  preferences: HostPreferences,
  chat: ChatPreferences,
): HostPreferences => ({
  ...preferences,
  chat: {
    collapse_sequential_tool_calls: chat.collapseSequentialToolCalls,
    collapse_thinking_with_tools: chat.collapseThinkingWithTools,
    collapse_compaction_with_tools: chat.collapseCompactionWithTools,
    collapse_todo_updates_with_tools: chat.collapseTodoUpdatesWithTools,
  },
});

export const withHostNotificationPreferences = (
  preferences: HostPreferences,
  notifications: NotificationPreferences,
): HostPreferences => ({
  ...preferences,
  notifications: {
    enabled: notifications.enabled,
    on_finish: notifications.onFinish,
    on_fail: notifications.onFail,
    on_attention: notifications.onAttention,
    sound: notifications.sound,
  },
});

export const withHostWorkspaceOrder = (
  preferences: HostPreferences,
  workspaceOrder: readonly string[],
): HostPreferences => ({
  ...preferences,
  workspace_order: normalizeWorkspaceOrder(workspaceOrder).filter(validHostId),
});

export const withHostPullRequestGroupOrder = (
  preferences: HostPreferences,
  groupOrder: readonly string[],
): HostPreferences => ({
  ...preferences,
  pull_request_group_order: [...normalizePullRequestGroupOrder(groupOrder)],
});

export const withHostResumePreferences = (
  preferences: HostPreferences,
  resume: ResumePreferences,
): HostPreferences => {
  const normalized = normalizeResumePreferences(resume);
  return {
    ...preferences,
    resume: {
      selected_session_id: normalized.selectedSessionId,
      session_threads: { ...normalized.sessionThreads },
      thread_scroll: Object.fromEntries(
        Object.entries(normalized.threadScroll).map(([threadId, bookmark]) => [
          threadId,
          { item_id: bookmark.itemId, offset: bookmark.offset },
        ]),
      ),
      closed_thread_tabs: [...normalized.closedThreadTabs],
      pinned_thread_tabs: [...normalized.pinnedThreadTabs],
    },
  };
};

const invalidAttachmentResponse = (schema: string): HostClientError =>
  new HostClientError("invalid-response", `desktop host returned invalid ${schema}`);

const base64DecodedLength = (data: string): number | undefined => {
  if (
    data.length === 0 ||
    data.length > MAX_ENCODED_ATTACHMENT_BYTES ||
    data.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/u.test(data)
  ) {
    return undefined;
  }
  const padding = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
  return (data.length / 4) * 3 - padding;
};

const pendingAttachment = (
  wire: AttachmentPayloadWire,
  imageOnly: boolean,
): PendingAttachment => {
  const nameBytes = new TextEncoder().encode(wire.name).byteLength;
  const decodedLength = base64DecodedLength(wire.data);
  if (
    wire.name.trim() === "" ||
    wire.name === "." ||
    wire.name === ".." ||
    nameBytes > MAX_ATTACHMENT_NAME_BYTES ||
    /[\u0000-\u001f\u007f/\\]/u.test(wire.name) ||
    wire.mime.length > MAX_ATTACHMENT_MIME_BYTES ||
    !/^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/iu.test(wire.mime) ||
    (imageOnly && !wire.mime.startsWith("image/")) ||
    !Number.isSafeInteger(wire.size_bytes) ||
    wire.size_bytes < 1 ||
    wire.size_bytes > MAX_ATTACHMENT_BYTES ||
    decodedLength !== wire.size_bytes
  ) {
    throw invalidAttachmentResponse(
      imageOnly ? "ReadClipboardImageResponse" : "PickFilesResponse",
    );
  }
  return Object.freeze({
    upload: Object.freeze({
      name: wire.name,
      mime: wire.mime,
      data: wire.data,
    }),
    size: wire.size_bytes,
  });
};
