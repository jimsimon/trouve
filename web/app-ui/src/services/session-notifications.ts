import type {
  ProtocolEventEnvelope,
  ProtocolSessionSummary,
} from "./protocol-client.js";
import type { BrowserNotificationAdapter } from "./browser-notifications.js";
import type { NotificationPreferencesSignal } from "./notification-preferences.js";
import type { HostClient } from "./host-client.js";

export type SessionNotificationKind = "finish" | "fail" | "attention";

export interface SessionNotificationRequest {
  readonly kind: SessionNotificationKind;
  readonly title: string;
  readonly body: string;
  readonly sessionId: string;
  readonly threadId: string | undefined;
  readonly sound: boolean;
}

export interface SessionNotificationDelivery {
  show(request: SessionNotificationRequest, onActivate: () => void): void | Promise<void>;
}

export interface SessionNotificationEnvironment {
  readonly now: () => number;
  readonly focused: () => boolean;
  readonly visibleSession: (
    sessionId: string,
    threadId: string | undefined,
  ) => boolean;
  readonly sessionTitle: (sessionId: string) => string;
  readonly activate: (sessionId: string, threadId: string | undefined) => void;
}

const FRESH_EVENT_WINDOW_MS = 10_000;

const notificationRequest = (
  envelope: Extract<ProtocolEventEnvelope, { readonly type: "session.notification" }>,
  title: string,
  sound: boolean,
): SessionNotificationRequest => {
  const session = title.trim();
  const detail = envelope.detail?.trim() ?? "";
  const body = detail === ""
    ? (session === "" ? "Trouve session" : session)
    : (session === "" ? detail : `${session}\n${detail}`);
  const base = {
    body,
    sessionId: envelope.session_id,
    threadId: envelope.thread_id,
    sound,
  } as const;

  switch (envelope.kind) {
    case "turn_completed":
      return { ...base, kind: "finish", title: "Agent finished" };
    case "turn_failed":
      return { ...base, kind: "fail", title: "Turn failed" };
    case "approval_requested":
      return { ...base, kind: "attention", title: "Approval needed" };
    case "question_requested":
      return { ...base, kind: "attention", title: "The agent has a question" };
  }
};

/** Applies client notification policy to compact durable server-scope edges.
 * This avoids retaining or replaying every inactive thread while preserving
 * the native category, thread target, and optional detail. */
export class SessionNotificationCoordinator {
  readonly #preferences: NotificationPreferencesSignal;
  readonly #delivery: SessionNotificationDelivery;
  readonly #environment: SessionNotificationEnvironment;
  #lastCursor = 0;

  constructor(
    preferences: NotificationPreferencesSignal,
    delivery: SessionNotificationDelivery,
    environment: SessionNotificationEnvironment,
  ) {
    this.#preferences = preferences;
    this.#delivery = delivery;
    this.#environment = environment;
  }

  replaceSnapshot(
    _summaries: readonly ProtocolSessionSummary[],
    cursor = this.#lastCursor,
  ): void {
    if (cursor < this.#lastCursor) return;
    this.#lastCursor = cursor;
  }

  receive(envelope: ProtocolEventEnvelope): void {
    if (envelope.type !== "session.notification") return;
    if (envelope.cursor <= this.#lastCursor) return;
    this.#lastCursor = envelope.cursor;

    const timestamp = Date.parse(envelope.ts);
    if (!Number.isFinite(timestamp)) return;
    const age = this.#environment.now() - timestamp;
    if (age >= FRESH_EVENT_WINDOW_MS) return;

    const preferences = this.#preferences.get();
    if (!preferences.enabled) return;
    const request = notificationRequest(
      envelope,
      this.#environment.sessionTitle(envelope.session_id),
      preferences.sound,
    );
    if (request.kind === "finish" && !preferences.onFinish) return;
    if (request.kind === "fail" && !preferences.onFail) return;
    if (request.kind === "attention" && !preferences.onAttention) return;
    if (
      this.#environment.focused() &&
      this.#environment.visibleSession(request.sessionId, request.threadId)
    ) return;

    try {
      const result = this.#delivery.show(request, () => {
        this.#environment.activate(request.sessionId, request.threadId);
      });
      if (result instanceof Promise) void result.catch(() => undefined);
    } catch {
      // Notification backends are best-effort and must never break ingress.
    }
  }
}

export const createBrowserSessionNotificationDelivery = (
  notifications: BrowserNotificationAdapter,
  focus: () => void = () => globalThis.focus(),
): SessionNotificationDelivery => ({
  show: (request, onActivate) => {
    if (notifications.permission() !== "granted") return;
    notifications.show(request.title, {
      body: request.body,
      tag: `trouve:${request.sessionId}:${request.kind}`,
      silent: !request.sound,
      data: {
        sessionId: request.sessionId,
        threadId: request.threadId,
      },
    }, () => {
      focus();
      onActivate();
    });
  },
});

/** Desktop delivery uses only the typed native bridge. Notification policy is
 * still evaluated by SessionNotificationCoordinator before this adapter runs. */
export const createNativeSessionNotificationDelivery = (
  host: Pick<HostClient, "showNativeNotification" | "requestUserAttention">,
): SessionNotificationDelivery => ({
  show: async (request, onActivate) => {
    await host.showNativeNotification(request, onActivate);
    if (request.kind === "attention") {
      await host.requestUserAttention().catch(() => undefined);
    }
  },
});
