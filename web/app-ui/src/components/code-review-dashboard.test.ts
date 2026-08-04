import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("code-review dashboard repository order contract", () => {
  const source = readFileSync(
    new URL("./code-review-dashboard.ts", import.meta.url),
    "utf8",
  );

  it("offers touch-sized adjacent controls and keyboard position semantics", () => {
    expect(source).toContain("Move ${group.repository} up");
    expect(source).toContain("Move ${group.repository} down");
    expect(source).toContain('event.key === "ArrowUp"');
    expect(source).toContain('event.key === "ArrowDown"');
    expect(source).toContain('data-group-order-control="grip"');
    expect(source).toContain("moved to position ${position} of ${visible.length}");
  });

  it("keeps pointer drag/drop progressive and client-owned", () => {
    expect(source).toContain(".draggable=${groups.length > 1}");
    expect(source).toContain("@dragstart=${");
    expect(source).toContain("@drop=${");
    expect(source).toContain("createBrowserCodeReviewGroupOrderStorage()");
    expect(source).toContain("groupCodeReviewJobs(dashboard.jobs, this.#filter)");
  });
});
