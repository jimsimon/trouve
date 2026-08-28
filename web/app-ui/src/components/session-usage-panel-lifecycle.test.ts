import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./session-usage-panel.ts", import.meta.url), "utf8");

describe("session usage panel asynchronous lifecycle guards", () => {
  it("guards a local-status response before publishing it", () => {
    const branchStart = source.indexOf('if (this.model.startsWith("local/"))');
    const branchEnd = source.indexOf("} else {", branchStart);
    expect(branchStart).toBeGreaterThanOrEqual(0);
    expect(branchEnd).toBeGreaterThan(branchStart);
    const branch = source.slice(branchStart, branchEnd);

    const awaitedAt = branch.indexOf(
      "const [localStatusResult, sessionResult, threadResult] = await Promise.allSettled([",
    );
    const guardAt = branch.indexOf(
      "if (generation !== this.#generation) return;",
      awaitedAt,
    );
    const publishAt = branch.indexOf("this.#localStatus = localStatus;", guardAt);
    expect(awaitedAt).toBeGreaterThanOrEqual(0);
    expect(guardAt).toBeGreaterThan(awaitedAt);
    expect(publishAt).toBeGreaterThan(guardAt);
    expect(branch).not.toContain("this.#localStatus = await");
    expect(branch.indexOf("this.#sessionSummary = sessionSummary;", guardAt))
      .toBeGreaterThan(guardAt);
    expect(branch.indexOf("this.#threadSummary = threadSummary;", guardAt))
      .toBeGreaterThan(guardAt);
  });

  it("loads and renders active-thread and session usage scopes", () => {
    expect(source).toContain("services.protocol.threadUsage(this.threadId)");
    expect(source).toContain("services.protocol.sessionUsage(this.sessionId)");
    expect(source).toContain(
      'this.#renderUsageScope("Active thread", this.#threadSummary)',
    );
    expect(source).toContain(
      'this.#renderUsageScope("Session", this.#sessionSummary)',
    );
  });

  it("invalidates session totals when any thread in the session completes", () => {
    expect(source).toContain("store?.sessionUsageRevision(this.sessionId)");
    expect(source).toContain("String(sessionUsageRevision)");
  });

  it("keeps complete model labels visible without hover-only truncation", () => {
    expect(source).toContain("html`<small>${this.model}</small>`");
    expect(source).toContain("<span>${row.label}</span>");
    expect(source).not.toContain("title=${this.model}");
    expect(source).not.toContain("title=${row.label}");
  });
});
