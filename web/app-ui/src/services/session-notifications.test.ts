import { describe, expect, it, vi } from "vitest";

import type {
  ProtocolEventEnvelope,
  ProtocolSessionSummary,
} from "./protocol-client.js";
import {
  SessionNotificationCoordinator,
  createBrowserSessionNotificationDelivery,
  createNativeSessionNotificationDelivery,
  type SessionNotificationRequest,
} from "./session-notifications.js";
import { NotificationPreferencesController } from "./notification-preferences.js";

const summary = (
  patch: Partial<ProtocolSessionSummary> = {},
): ProtocolSessionSummary => ({
  session_id: "se-1",
  workspace_id: "ws-1",
  archived: false,
  active: true,
  attention: "none",
  outcome: "running",
  latest_cursor: 4,
  latest_thread_id: "th-1",
  updated_at: "2026-08-03T12:00:00Z",
  ...patch,
});

const summaryEvent = (
  cursor: number,
  next: ProtocolSessionSummary | null,
  ts = "2026-08-03T12:00:05Z",
): ProtocolEventEnvelope => ({
  type: "session.summary_updated",
  cursor,
  scope: "server",
  ts,
  session_id: "se-1",
  summary: next,
});

const notificationEvent = (
  cursor: number,
  kind: "turn_completed" | "turn_failed" | "approval_requested" | "question_requested",
  detail?: string,
  ts = "2026-08-03T12:00:05Z",
): ProtocolEventEnvelope => ({
  type: "session.notification",
  cursor,
  scope: "server",
  ts,
  session_id: "se-1",
  thread_id: "th-1",
  kind,
  ...(detail === undefined ? {} : { detail }),
});

const setup = (options: { readonly focused?: boolean; readonly visible?: boolean } = {}) => {
  const preferences = new NotificationPreferencesController();
  const requests: SessionNotificationRequest[] = [];
  const activate = vi.fn();
  const coordinator = new SessionNotificationCoordinator(
    preferences.current,
    { show: (request) => { requests.push(request); } },
    {
      now: () => Date.parse("2026-08-03T12:00:06Z"),
      focused: () => options.focused ?? false,
      visibleSession: () => options.visible ?? false,
      sessionTitle: () => "Port the frontend",
      activate,
    },
  );
  coordinator.replaceSnapshot([summary()], 2);
  return { coordinator, preferences, requests, activate };
};

describe("session notification coordinator", () => {
  it("delivers every native notification edge, including repeated attention", () => {
    const { coordinator, requests } = setup();
    coordinator.receive(notificationEvent(3, "turn_completed"));
    coordinator.receive(notificationEvent(4, "turn_failed", "provider unavailable"));
    coordinator.receive(notificationEvent(5, "approval_requested"));
    coordinator.receive(notificationEvent(6, "approval_requested"));
    coordinator.receive(notificationEvent(7, "question_requested", "Choose a target"));
    expect(requests.map(({ kind, title }) => ({ kind, title }))).toEqual([
      { kind: "finish", title: "Agent finished" },
      { kind: "fail", title: "Turn failed" },
      { kind: "attention", title: "Approval needed" },
      { kind: "attention", title: "Approval needed" },
      { kind: "attention", title: "The agent has a question" },
    ]);
    expect(requests[1]?.body).toBe("Port the frontend\nprovider unavailable");
    expect(requests[4]?.body).toBe("Port the frontend\nChoose a target");
  });

  it("honors master/event preferences and focused-visible suppression", () => {
    const masterDisabled = setup();
    masterDisabled.preferences.update({ enabled: false });
    masterDisabled.coordinator.receive(notificationEvent(3, "turn_failed"));
    expect(masterDisabled.requests).toEqual([]);

    const disabled = setup();
    disabled.preferences.update({ onFinish: false });
    disabled.coordinator.receive(notificationEvent(3, "turn_completed"));
    expect(disabled.requests).toEqual([]);

    const visible = setup({ focused: true, visible: true });
    visible.coordinator.receive(notificationEvent(3, "question_requested"));
    expect(visible.requests).toEqual([]);
  });

  it("ignores stale, replayed, invalid, and initial snapshot state", () => {
    const { coordinator, requests } = setup();
    coordinator.receive(notificationEvent(2, "approval_requested"));
    coordinator.receive(notificationEvent(3, "approval_requested", undefined, "2026-08-03T11:59:00Z"));
    coordinator.receive(notificationEvent(4, "question_requested", undefined, "not-a-date"));
    coordinator.receive(summaryEvent(5, summary({ attention: "approval" })));
    expect(requests).toEqual([]);
  });

  it("uses notification detail as the body when session metadata is unavailable", () => {
    const preferences = new NotificationPreferencesController();
    const requests: SessionNotificationRequest[] = [];
    const coordinator = new SessionNotificationCoordinator(
      preferences.current,
      { show: (request) => { requests.push(request); } },
      {
        now: () => Date.parse("2026-08-03T12:00:06Z"),
        focused: () => false,
        visibleSession: () => false,
        sessionTitle: () => "",
        activate: vi.fn(),
      },
    );
    coordinator.receive(notificationEvent(1, "turn_failed", "provider unavailable"));
    expect(requests[0]?.body).toBe("provider unavailable");
  });
});

describe("browser notification delivery", () => {
  it("uses permission, sound, scoped tags, and click activation", () => {
    let click: (() => void) | undefined;
    const show = vi.fn((_title: string, _options: NotificationOptions, onActivate?: () => void) => {
      click = onActivate;
    });
    const focus = vi.fn();
    const activate = vi.fn();
    const delivery = createBrowserSessionNotificationDelivery({
      permission: () => "granted",
      requestPermission: async () => "granted",
      show,
    }, focus);
    delivery.show({
      kind: "attention",
      title: "Approval needed",
      body: "Port the frontend",
      sessionId: "se-1",
      threadId: "th-1",
      sound: false,
    }, activate);
    expect(show).toHaveBeenCalledWith(
      "Approval needed",
      expect.objectContaining({ silent: true, tag: "trouve:se-1:th-1:attention" }),
      expect.any(Function),
    );
    click?.();
    expect(focus).toHaveBeenCalledOnce();
    expect(activate).toHaveBeenCalledOnce();
  });
});

describe("native notification delivery", () => {
  it("uses the typed host and requests attention only for actionable events", async () => {
    const showNativeNotification = vi.fn(async () => undefined);
    const requestUserAttention = vi.fn(async () => undefined);
    const delivery = createNativeSessionNotificationDelivery({
      showNativeNotification,
      requestUserAttention,
    });
    const activate = vi.fn();
    const request: SessionNotificationRequest = {
      kind: "attention",
      title: "Approval needed",
      body: "Port the frontend",
      sessionId: "se-1",
      threadId: "th-1",
      sound: false,
    };
    await delivery.show(request, activate);
    expect(showNativeNotification).toHaveBeenCalledWith(request, activate);
    expect(requestUserAttention).toHaveBeenCalledOnce();

    await delivery.show({ ...request, kind: "finish" }, activate);
    expect(requestUserAttention).toHaveBeenCalledOnce();
  });
});
