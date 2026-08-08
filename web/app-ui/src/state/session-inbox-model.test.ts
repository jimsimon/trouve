import { describe, expect, it } from "vitest";

import {
  groupWorkspaceSessions,
  inboxRecoverySession,
  sessionStatusText,
  sortInboxSessions,
  type InboxSessionOrderFields,
} from "./session-inbox-model.js";

const session = (
  id: string,
  overrides: Partial<InboxSessionOrderFields> = {},
): InboxSessionOrderFields => ({
  id,
  workspaceId: "ws-main",
  archived: false,
  active: false,
  attention: "none",
  outcome: "idle",
  updatedAt: "2026-08-01T12:00:00Z",
  ...overrides,
});

describe("session inbox model", () => {
  it("sorts visible sessions by attention, outcome urgency, recency, and stable ID", () => {
    const sessions = [
      session("succeeded", { outcome: "succeeded", updatedAt: "2026-08-01T12:09:00Z" }),
      session("idle", { updatedAt: "2026-08-01T12:07:00Z" }),
      session("running", { active: true, outcome: "idle", updatedAt: "2026-08-01T12:07:00Z" }),
      session("failed", { outcome: "failed", updatedAt: "2026-08-01T12:06:00Z" }),
      session("attention-succeeded", {
        attention: "question",
        outcome: "succeeded",
        updatedAt: "2026-08-01T12:10:00Z",
      }),
      session("attention-running", {
        attention: "approval",
        outcome: "running",
        updatedAt: "2026-08-01T12:05:00Z",
      }),
      session("attention-failed", {
        attention: "both",
        outcome: "failed",
        updatedAt: "2026-08-01T12:04:00Z",
      }),
      session("idle-z", { updatedAt: "2026-08-01T12:08:00Z" }),
      session("idle-a", { updatedAt: "2026-08-01T12:08:00Z" }),
    ];

    expect(sortInboxSessions(sessions).map(({ id }) => id)).toEqual([
      "attention-failed",
      "attention-running",
      "attention-succeeded",
      "failed",
      "running",
      "idle-a",
      "idle-z",
      "idle",
      "succeeded",
    ]);
  });

  it("keeps archived history separate and recency ordered", () => {
    const groups = groupWorkspaceSessions(
      [
        session("other-workspace", { workspaceId: "ws-other" }),
        session("active"),
        session("archived-old-attention", {
          archived: true,
          attention: "approval",
          outcome: "failed",
          updatedAt: "2026-08-01T12:00:00Z",
        }),
        session("archived-new", {
          archived: true,
          outcome: "succeeded",
          updatedAt: "2026-08-01T12:10:00Z",
        }),
      ],
      {
        workspaceId: "ws-main",
        selectedSessionId: undefined,
        archivedExpanded: false,
      },
    );

    expect(groups.active.map(({ id }) => id)).toEqual(["active"]);
    expect(groups.archived.map(({ id }) => id)).toEqual([
      "archived-new",
      "archived-old-attention",
    ]);
    expect(groups.archivedExpanded).toBe(false);
  });

  it("opens archived history when the selected session is archived in place", () => {
    const selected = session("selected");
    expect(
      groupWorkspaceSessions([selected], {
        workspaceId: "ws-main",
        selectedSessionId: selected.id,
        archivedExpanded: false,
      }).archivedExpanded,
    ).toBe(false);

    const archivedSelected = { ...selected, archived: true };
    const groups = groupWorkspaceSessions([archivedSelected], {
      workspaceId: "ws-main",
      selectedSessionId: selected.id,
      archivedExpanded: false,
    });
    expect(groups.active).toEqual([]);
    expect(groups.archived).toEqual([archivedSelected]);
    expect(groups.archivedExpanded).toBe(true);
  });

  it("recovers after deletion to the highest-priority non-archived session only", () => {
    const deleted = session("deleted", { attention: "approval" });
    const running = session("running", { outcome: "running" });
    const archived = session("archived", {
      archived: true,
      attention: "both",
      outcome: "failed",
    });

    expect(inboxRecoverySession([deleted, running, archived])).toBe(deleted);
    expect(inboxRecoverySession([running, archived])).toBe(running);
    expect(inboxRecoverySession([archived])).toBeUndefined();
  });

  it("provides explicit accessible text for every projected status", () => {
    expect(sessionStatusText(session("approval", { attention: "approval" })))
      .toBe("Needs attention: approval required");
    expect(sessionStatusText(session("question", { attention: "question" })))
      .toBe("Needs attention: question awaiting answer");
    expect(sessionStatusText(session("both", { attention: "both" })))
      .toBe("Needs attention: approval and question");
    expect(sessionStatusText(session("running", { active: true }))).toBe("Running");
    expect(sessionStatusText(session("failed", { outcome: "failed" }))).toBe("Failed");
    expect(sessionStatusText(session("done", { outcome: "succeeded" }))).toBe("Completed");
    expect(sessionStatusText(session("idle"))).toBe("Idle");
    expect(sessionStatusText(session("read-failure", {
      outcome: "failed",
      unread: false,
    }))).toBe("Idle");
    expect(sessionStatusText(session("read-success", {
      outcome: "succeeded",
      unread: false,
    }))).toBe("Idle");
  });

  it("does not prioritize read terminal outcomes above active idle work", () => {
    expect(sortInboxSessions([
      session("read-failure", { outcome: "failed", unread: false }),
      session("active-idle", { active: true, updatedAt: "2026-08-01T11:00:00Z" }),
    ]).map(({ id }) => id)).toEqual(["active-idle", "read-failure"]);
  });
});
