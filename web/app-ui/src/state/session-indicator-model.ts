import type {
  InboxSessionAttention,
  InboxSessionOutcome,
} from "./session-inbox-model.js";
import type { FontAwesomeIconName } from "../components/font-awesome-icon.js";

export interface SessionIndicatorFields {
  readonly active: boolean;
  readonly attention: InboxSessionAttention;
  readonly outcome: InboxSessionOutcome;
  readonly unread: boolean;
}

export type SessionIndicatorKind =
  | "approval"
  | "question"
  | "both"
  | "error"
  | "unread"
  | "busy"
  | "none";

export interface SessionIndicatorPresentation {
  readonly kind: SessionIndicatorKind;
  readonly icon: FontAwesomeIconName | undefined;
  readonly tooltip: string;
}

/** Shared presentation contract for every compact session picker. Keep this
 * established priority: actionable attention first,
 * then unseen terminal outcomes, live activity, and finally idle. */
export const sessionIndicatorPresentation = (
  session: SessionIndicatorFields,
): SessionIndicatorPresentation => {
  if (session.attention === "approval") {
    return {
      kind: "approval",
      icon: "triangle-exclamation",
      tooltip: "Approval pending",
    };
  }
  if (session.attention === "question") {
    return {
      kind: "question",
      icon: "circle-question",
      tooltip: "Question awaiting an answer",
    };
  }
  if (session.attention === "both") {
    return {
      kind: "both",
      icon: "triangle-exclamation",
      tooltip: "Approval and question need attention",
    };
  }
  if (session.unread && session.outcome === "failed") {
    return {
      kind: "error",
      icon: "xmark",
      tooltip: "Turn ended with an error",
    };
  }
  if (session.unread && session.outcome === "succeeded") {
    return { kind: "unread", icon: "circle", tooltip: "Unviewed work" };
  }
  if (session.active || session.outcome === "running") {
    return { kind: "busy", icon: undefined, tooltip: "" };
  }
  return { kind: "none", icon: undefined, tooltip: "" };
};

/** Summarize only states that require the user to revisit hidden content.
 * Live activity is intentionally excluded: a closed thread gets a menu badge
 * for actionable attention or an unread terminal outcome, not merely because
 * it is still processing in the background. */
export const attentionOrUnreadIndicatorPresentation = (
  sessions: readonly SessionIndicatorFields[],
): SessionIndicatorPresentation => {
  const approval = sessions.some((session) =>
    session.attention === "approval" || session.attention === "both");
  const question = sessions.some((session) =>
    session.attention === "question" || session.attention === "both");
  const attention = approval
    ? question ? "both" : "approval"
    : question ? "question" : "none";
  const failed = sessions.some((session) =>
    session.unread && session.outcome === "failed");
  const succeeded = sessions.some((session) =>
    session.unread && session.outcome === "succeeded");
  return sessionIndicatorPresentation({
    active: false,
    attention,
    outcome: failed ? "failed" : succeeded ? "succeeded" : "idle",
    unread: failed || succeeded,
  });
};
