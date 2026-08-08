import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { sessionMcpAvailability } from "./session-info-panel.js";

const source = readFileSync(new URL("./session-info-panel.ts", import.meta.url), "utf8");

describe("session information overview", () => {
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
    expect(source).toContain("services.protocol.sessionDiffSummary(sessionId)");
    expect(source).not.toContain("services.protocol.sessionDiff(sessionId)");
    expect(source).toContain("services.protocol.sessionMcpServers(sessionId)");
    expect(source).not.toContain("services.protocol.sessionPrs(sessionId)");
    expect(source).toContain("services.protocol.refreshGithubPrs(true)");
    expect(source).toContain("services.protocol.serverProjectionSnapshot()");
    expect(source).toContain("replaceServerProjection(projection.cursor, projection.value)");
  });

  it("keeps navigation to the detailed diff and pull-request surfaces", () => {
    expect(source).toContain('this.#openInspection("diff")');
    expect(source).toContain('this.#openInspection("pr")');
    expect(source).toContain("trouve-open-external");
  });
});
