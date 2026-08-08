import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  sessionMcpAvailability,
  summarizeSessionDiff,
} from "./session-info-panel.js";

const source = readFileSync(new URL("./session-info-panel.ts", import.meta.url), "utf8");

describe("session information overview", () => {
  it("summarizes additions and deletions across changed files", () => {
    expect(summarizeSessionDiff([
      { additions: 3, deletions: 1 },
      { additions: 4, deletions: 2 },
    ])).toEqual({ additions: 7, deletions: 3, files: 2 });
  });

  it("does not describe repository-controlled MCP definitions as active", () => {
    expect(sessionMcpAvailability({ health: "unknown", scope: "app-wide" }))
      .toMatchObject({ label: "Active", active: true });
    expect(sessionMcpAvailability({ health: "unknown", scope: "branch" }))
      .toMatchObject({ label: "Not trusted", active: false });
    expect(sessionMcpAvailability({ health: "disabled", scope: "app-wide" }))
      .toMatchObject({ label: "Disabled", active: false });
  });

  it("shares the authoritative pull-request projection and existing session endpoints", () => {
    expect(source).toContain("store?.sessionPullRequests(sessionId)");
    expect(source).toContain("services.protocol.sessionDiff(sessionId)");
    expect(source).toContain("services.protocol.sessionMcpServers(sessionId)");
    expect(source).toContain("services.protocol.sessionPrs(sessionId)");
    expect(source).toContain("replaceSessionPullRequests(sessionId, prResult.value)");
  });

  it("keeps navigation to the detailed diff and pull-request surfaces", () => {
    expect(source).toContain('this.#openInspection("diff")');
    expect(source).toContain('this.#openInspection("pr")');
    expect(source).toContain("trouve-open-external");
  });
});
