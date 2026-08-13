export type ChatTurnAction =
  | "send"
  | "queue"
  | "cancel"
  | "sending"
  | "queueing"
  | "starting"
  | "cancelling"
  | "send-after-cancel";

export interface ChatTurnControlInput {
  readonly threadAvailable: boolean;
  readonly durableTurnRunning: boolean;
  readonly startPending: boolean;
  readonly cancellationRequested: boolean;
  readonly messageRequest: "start" | "queue" | undefined;
  readonly requestPending: boolean;
  readonly attachmentPending: boolean;
  readonly hasContent: boolean;
  readonly connectivityBlocked: boolean;
}

export interface ChatTurnControlState {
  readonly action: ChatTurnAction;
  readonly label: string;
  readonly accessibleLabel: string;
  readonly submit: boolean;
  readonly disabled: boolean;
  readonly effectiveTurnRunning: boolean;
  readonly activityLabel: string | undefined;
}

/**
 * Derive the composer action across the HTTP-acknowledgement/durable-event
 * gap. A newly accepted turn is already claimed server-side before its
 * `turn.started` event reaches the browser, and a prompt sent after a cancel
 * acknowledgement is intentionally queued to resume after cancellation.
 */
export const chatTurnControlState = (
  input: ChatTurnControlInput,
): ChatTurnControlState => {
  const effectiveTurnRunning =
    input.durableTurnRunning
    || input.startPending
    || input.messageRequest === "start";
  const mutationDisabled =
    !input.threadAvailable
    || input.requestPending
    || input.connectivityBlocked;
  const sendDisabled = mutationDisabled || input.attachmentPending;

  if (input.messageRequest === "start") {
    return {
      action: "sending",
      label: "Sending…",
      accessibleLabel: "Sending message",
      submit: false,
      disabled: true,
      effectiveTurnRunning,
      activityLabel: "Sending message…",
    };
  }

  if (input.messageRequest === "queue") {
    return {
      action: "queueing",
      label: "Queueing…",
      accessibleLabel: "Queueing message",
      submit: false,
      disabled: true,
      effectiveTurnRunning,
      activityLabel: input.cancellationRequested
        ? "Cancelling turn…"
        : input.startPending ? "Starting turn…" : undefined,
    };
  }

  if (input.durableTurnRunning && input.cancellationRequested) {
    if (!input.hasContent) {
      return {
        action: "cancelling",
        label: "Stopping…",
        accessibleLabel: "Cancellation requested",
        submit: false,
        disabled: true,
        effectiveTurnRunning,
        activityLabel: "Cancelling turn…",
      };
    }
    return {
      action: "send-after-cancel",
      label: "Send next",
      accessibleLabel: "Send after cancellation",
      submit: true,
      disabled: sendDisabled,
      effectiveTurnRunning,
      activityLabel: "Cancelling turn…",
    };
  }

  if (input.startPending && !input.durableTurnRunning && !input.hasContent) {
    return {
      action: "starting",
      label: "Starting…",
      accessibleLabel: "Turn is starting",
      submit: false,
      disabled: true,
      effectiveTurnRunning,
      activityLabel: "Starting turn…",
    };
  }

  if (input.durableTurnRunning && !input.hasContent) {
    return {
      action: "cancel",
      label: "Cancel",
      accessibleLabel: "Cancel active turn",
      submit: false,
      disabled: mutationDisabled,
      effectiveTurnRunning,
      activityLabel: undefined,
    };
  }

  if (effectiveTurnRunning) {
    return {
      action: "queue",
      label: "Queue",
      accessibleLabel: "Queue message",
      submit: true,
      disabled: sendDisabled,
      effectiveTurnRunning,
      activityLabel: input.startPending ? "Starting turn…" : undefined,
    };
  }

  return {
    action: "send",
    label: "Send",
    accessibleLabel: "Send message",
    submit: true,
    disabled: sendDisabled || !input.hasContent,
    effectiveTurnRunning,
    activityLabel: undefined,
  };
};
