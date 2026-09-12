import { describe, expect, it } from "vitest";

import {
  attentionOrUnreadIndicatorPresentation,
  sessionIndicatorPresentation,
  type SessionIndicatorFields,
} from "./session-indicator-model.js";

const idle: SessionIndicatorFields = {
  active: false,
  attention: "none",
  outcome: "idle",
  unread: false,
};

describe("session indicator presentation", () => {
  it("distinguishes approval and question attention", () => {
    expect(sessionIndicatorPresentation({ ...idle, attention: "approval" }))
      .toEqual({
        kind: "approval",
        icon: "triangle-exclamation",
        tooltip: "Approval pending",
      });
    expect(sessionIndicatorPresentation({ ...idle, attention: "question" }))
      .toEqual({
        kind: "question",
        icon: "circle-question",
        tooltip: "Question awaiting an answer",
      });
    expect(sessionIndicatorPresentation({ ...idle, attention: "both" }))
      .toEqual({
        kind: "both",
        icon: "triangle-exclamation",
        tooltip: "Approval and question need attention",
      });
  });

  it("uses the unread outcome and live-activity presentations from the session list", () => {
    expect(sessionIndicatorPresentation({
      ...idle,
      outcome: "succeeded",
      unread: true,
    })).toEqual({ kind: "unread", icon: "circle", tooltip: "Unviewed work" });
    expect(sessionIndicatorPresentation({
      ...idle,
      outcome: "failed",
      unread: true,
    })).toEqual({ kind: "error", icon: "xmark", tooltip: "Turn ended with an error" });
    expect(sessionIndicatorPresentation({ ...idle, active: true })).toEqual({
      kind: "busy",
      icon: undefined,
      tooltip: "",
    });
    expect(sessionIndicatorPresentation(idle)).toEqual({
      kind: "none",
      icon: undefined,
      tooltip: "",
    });
  });

  it("keeps actionable attention ahead of outcomes and activity", () => {
    expect(sessionIndicatorPresentation({
      active: true,
      attention: "question",
      outcome: "failed",
      unread: true,
    }).kind).toBe("question");
  });

  it("aggregates only actionable or unread states for hidden-thread badges", () => {
    expect(attentionOrUnreadIndicatorPresentation([
      { ...idle, active: true, outcome: "running" },
    ]).kind).toBe("none");
    expect(attentionOrUnreadIndicatorPresentation([
      { ...idle, outcome: "succeeded", unread: true },
    ]).kind).toBe("unread");
    expect(attentionOrUnreadIndicatorPresentation([
      { ...idle, outcome: "failed", unread: true },
      { ...idle, outcome: "succeeded", unread: true },
    ]).kind).toBe("error");
    expect(attentionOrUnreadIndicatorPresentation([
      { ...idle, attention: "approval" },
      { ...idle, attention: "question" },
      { ...idle, outcome: "failed", unread: true },
    ])).toEqual({
      kind: "both",
      icon: "triangle-exclamation",
      tooltip: "Approval and question need attention",
    });
  });
});
