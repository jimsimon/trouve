import { describe, expect, it } from "vitest";

import {
  organizeWorkspaceSessions,
  pullRequestKind,
  workspaceSessionSectionCollapsed,
  workspaceSessionStatus,
  workspaceSessionUpdatedGroup,
  type WorkspaceSessionListFields,
} from "./workspace-session-list-model.js";

const session = (
  id: string,
  patch: Partial<WorkspaceSessionListFields> = {},
): WorkspaceSessionListFields => ({
  id,
  workspaceId: "ws",
  archived: false,
  active: false,
  attention: "none",
  outcome: "idle",
  unread: false,
  updatedAt: "2026-08-13T12:00:00Z",
  createdAt: "2026-08-01T12:00:00Z",
  pullRequestKind: "none",
  ...patch,
});

describe("workspace session list model", () => {
  it("uses actionable status precedence and PR buckets", () => {
    expect(workspaceSessionStatus(session("a", { active: true, unread: true }))).toBe("unread");
    expect(workspaceSessionStatus(session("b", { attention: "approval", active: true }))).toBe("attention");
    expect(workspaceSessionStatus(session("c", { pullRequestKind: "merged" }))).toBe("done");
    expect(pullRequestKind([{ state: "open", draft: true }, { state: "merged" }])).toBe("draft");
    expect(pullRequestKind([])).toBe("none");
  });

  it("filters, orders, and groups sessions by updated age", () => {
    const result = organizeWorkspaceSessions([
      session("today"),
      session("yesterday", { updatedAt: "2026-08-12T09:00:00Z" }),
      session("working", { active: true, updatedAt: "2026-08-13T13:00:00Z" }),
      session("other", { workspaceId: "other" }),
    ], {
      workspaceId: "ws",
      grouping: "updated",
      ordering: "updated",
      statusFilter: 0b1_1111,
      pullRequestFilter: 0b1_1111,
      now: Date.parse("2026-08-13T15:00:00Z"),
      timeZone: "UTC",
    });
    expect(result.sections.map(({ key }) => key)).toEqual(["today", "yesterday"]);
    expect(result.sections[0]?.sessions.map(({ id }) => id)).toEqual(["working", "today"]);
  });

  it("applies status and pull-request masks", () => {
    const result = organizeWorkspaceSessions([
      session("draft", { pullRequestKind: "draft" }),
      session("working", { active: true, pullRequestKind: "open" }),
    ], {
      workspaceId: "ws",
      grouping: "status",
      ordering: "status",
      statusFilter: 1 << 2,
      pullRequestFilter: 1 << 1,
      now: Date.now(),
    });
    expect(result.sections.map(({ sessions }) => sessions.map(({ id }) => id))).toEqual([
      ["working"],
    ]);
  });

  it("uses calendar dates across daylight-saving transitions", () => {
    expect(workspaceSessionUpdatedGroup(
      "2026-03-08T06:30:00Z",
      Date.parse("2026-03-09T04:30:00Z"),
      "America/Detroit",
    )[0]).toBe("yesterday");
    expect(workspaceSessionUpdatedGroup(
      "2026-11-01T04:30:00Z",
      Date.parse("2026-11-02T05:30:00Z"),
      "America/Detroit",
    )[0]).toBe("yesterday");
  });

  it("keeps a selected session's stored-collapsed section visible", () => {
    const section = { key: "today", label: "Today", sessions: [session("selected")] };
    expect(workspaceSessionSectionCollapsed(section, true, "selected")).toBe(false);
    expect(workspaceSessionSectionCollapsed(section, true, "other")).toBe(true);
  });
});
