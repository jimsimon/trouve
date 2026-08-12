import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  completedThreadDurationMs,
  mcpServersNeedHealthReconciliation,
  sessionMcpAvailability,
} from "./session-info-panel.js";

const source = readFileSync(new URL("./session-info-panel.ts", import.meta.url), "utf8");

describe("session information overview", () => {
  it("describes MCP enablement and runtime health independently", () => {
    expect(sessionMcpAvailability({ health: "unknown", scope: "app-wide" }))
      .toMatchObject({
        enablement: { label: "Enabled" },
        health: { label: "Unknown" },
        active: false,
      });
    expect(sessionMcpAvailability({ health: "ok", scope: "app-wide" }))
      .toMatchObject({
        enablement: { label: "Enabled" },
        health: { label: "Ready" },
        active: true,
      });
    expect(sessionMcpAvailability({ health: "error", scope: "app-wide" }))
      .toMatchObject({
        enablement: { label: "Enabled" },
        health: { label: "Error" },
        active: false,
      });
    expect(sessionMcpAvailability({ health: "unknown", scope: "branch" }))
      .toMatchObject({
        enablement: { label: "Enabled" },
        health: { label: "Error" },
        active: false,
      });
    expect(sessionMcpAvailability({ health: "disabled", scope: "app-wide" }))
      .toMatchObject({
        enablement: { label: "Disabled" },
        health: { label: "Unknown" },
        active: false,
      });
    expect(sessionMcpAvailability({ enabled: false, health: "unknown", scope: "app-wide" }))
      .toMatchObject({
        enablement: { label: "Disabled" },
        health: { label: "Unknown" },
        active: false,
      });
  });

  it("reconciles only unknown trusted enabled MCP servers", () => {
    expect(mcpServersNeedHealthReconciliation([
      { enabled: true, health: "unknown", scope: "app-wide" },
    ])).toBe(true);
    expect(mcpServersNeedHealthReconciliation([
      { enabled: false, health: "unknown", scope: "app-wide" },
      { enabled: true, health: "unknown", scope: "workspace" },
      { enabled: true, health: "ok", scope: "app-wide" },
      { enabled: true, health: "error", scope: "app-wide" },
    ])).toBe(false);
  });

  it("derives a stable completed subagent duration from projected timestamps", () => {
    expect(completedThreadDurationMs({
      started_at: "2026-08-09T14:00:00.000Z",
      completed_at: "2026-08-09T14:02:03.250Z",
    })).toBe(123_250);
    expect(completedThreadDurationMs({
      started_at: "2026-08-09T14:02:03.250Z",
      completed_at: "2026-08-09T14:00:00.000Z",
    })).toBe(0);
    expect(completedThreadDurationMs(undefined)).toBeUndefined();
  });

  it("shares the authoritative pull-request projection and existing session endpoints", () => {
    expect(source).toContain("store?.sessionPullRequests(sessionId)");
    expect(source).toContain("services.protocol.sessionDiffSummary(sessionId)");
    expect(source).not.toContain("services.protocol.sessionDiff(sessionId)");
    expect(source).toContain("services.protocol.sessionMcpServers(sessionId)");
    expect(source).toContain("services.protocol.threadSubagents(threadId, true)");
    expect(source).toContain("services.protocol.threadStatuses(childSessionId)");
    expect(source).toContain("store?.replaceThreadStatusesForSession(result.childSessionId");
    expect(source).not.toContain("services.protocol.sessionPrs(sessionId)");
    expect(source).not.toContain("services.protocol.refreshGithubPrs(true)");
    expect(source).toContain("RESOURCE_REFRESH_MS");
    expect(source).toContain("void this.#refreshResources()");
    expect(source).toContain("MCP_CONFIG_CHANGED_EVENT");
    expect(source).toContain("services.protocol.mcpServers(workspaceId, true)");
    expect(source).toContain("void this.#reconcileUnknownMcpHealth()");
  });

  it("keeps navigation to the detailed diff and pull-request surfaces", () => {
    expect(source).toContain('this.#openInspection("diff")');
    expect(source).toContain('this.#openInspection("pr")');
    expect(source).toContain("trouve-open-external");
  });

  it("groups session identity, changes, and pull requests under one overview card", () => {
    expect(source).not.toContain('class="session-info-header"');
    expect(source).toContain(
      '<section class="session-info-card" aria-labelledby="session-info-title">',
    );
    expect(source).toMatch(
      /<h3 id="session-info-title">Session overview<\/h3>[\s\S]*class="session-info-session-groups"[\s\S]*#renderChanges\(\)[\s\S]*#renderPullRequests\(pullRequests\)/u,
    );
    expect(source).toContain(
      '<section class="session-info-session-group" aria-labelledby="session-info-changes-title">',
    );
    expect(source).toContain(
      '<section class="session-info-session-group" aria-labelledby="session-info-pr-title">',
    );
  });

  it("uses consistent TODO terminology in the thread overview", () => {
    expect(source).toContain("TODOs and subagents for ${threadTitle}.");
    expect(source).toContain('<strong id="session-info-todos-title">TODOs</strong>');
    expect(source).toContain("No TODOs are defined for this thread.");
    expect(source).not.toMatch(/\bTodos\b/u);
  });
});
