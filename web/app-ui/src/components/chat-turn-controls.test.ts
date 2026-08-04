import { describe, expect, it } from "vitest";

import { chatTurnControlState, type ChatTurnControlInput } from "./chat-turn-controls.js";

const state = (
  overrides: Partial<ChatTurnControlInput> = {},
): ReturnType<typeof chatTurnControlState> => chatTurnControlState({
  threadAvailable: true,
  durableTurnRunning: false,
  startPending: false,
  cancellationRequested: false,
  messageRequest: undefined,
  requestPending: false,
  attachmentPending: false,
  hasContent: false,
  connectivityBlocked: false,
  ...overrides,
});

describe("chat turn controls", () => {
  it("announces the request before the server acknowledges a start or queue", () => {
    expect(state({ messageRequest: "start", hasContent: true })).toMatchObject({
      action: "sending",
      label: "Sending…",
      disabled: true,
      effectiveTurnRunning: true,
      activityLabel: "Sending message…",
    });
    expect(state({
      durableTurnRunning: true,
      messageRequest: "queue",
      hasContent: true,
    })).toMatchObject({
      action: "queueing",
      label: "Queueing…",
      disabled: true,
      activityLabel: undefined,
    });
  });

  it("closes the accepted-start race without offering an invalid cancel", () => {
    expect(state({ startPending: true })).toMatchObject({
      action: "starting",
      label: "Starting…",
      disabled: true,
      effectiveTurnRunning: true,
      activityLabel: "Starting turn…",
    });
    expect(state({ startPending: true, hasContent: true })).toMatchObject({
      action: "queue",
      label: "Queue",
      submit: true,
      disabled: false,
    });
  });

  it("offers cancel for an empty running composer and queue for a draft", () => {
    expect(state({ durableTurnRunning: true })).toMatchObject({
      action: "cancel",
      label: "Cancel",
      submit: false,
      disabled: false,
    });
    expect(state({ durableTurnRunning: true, hasContent: true })).toMatchObject({
      action: "queue",
      label: "Queue",
      submit: true,
    });
  });

  it("allows an explicit follow-up immediately after cancellation is accepted", () => {
    expect(state({
      durableTurnRunning: true,
      cancellationRequested: true,
    })).toMatchObject({
      action: "cancelling",
      label: "Stopping…",
      disabled: true,
      activityLabel: "Cancelling turn…",
    });
    expect(state({
      durableTurnRunning: true,
      cancellationRequested: true,
      hasContent: true,
    })).toMatchObject({
      action: "send-after-cancel",
      label: "Send next",
      accessibleLabel: "Send after cancellation",
      submit: true,
      disabled: false,
    });
  });

  it("blocks duplicate mutations, attachment races, and offline sends", () => {
    expect(state().disabled).toBe(true);
    expect(state({ hasContent: true, requestPending: true }).disabled).toBe(true);
    expect(state({ hasContent: true, attachmentPending: true }).disabled).toBe(true);
    expect(state({ hasContent: true, connectivityBlocked: true }).disabled).toBe(true);
    expect(state({ hasContent: true, threadAvailable: false }).disabled).toBe(true);
  });
});
