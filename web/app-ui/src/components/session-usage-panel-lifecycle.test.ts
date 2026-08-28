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
      "const localStatus = await services.protocol.localStatus();",
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
  });
});
