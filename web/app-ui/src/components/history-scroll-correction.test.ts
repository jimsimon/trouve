import { describe, expect, it } from "vitest";

import { retainedHistoryScrollDelta } from "./history-scroll-correction.js";

describe("retainedHistoryScrollDelta", () => {
  it("keeps only previously measured turn deltas from a mixed observer batch", () => {
    expect(retainedHistoryScrollDelta([
      { id: "turn:first-measurement", previouslyMeasured: false, delta: 80 },
      { id: "ephemeral:activity", previouslyMeasured: true, delta: 40 },
      { id: "turn:late-growth", previouslyMeasured: true, delta: 13 },
      { id: "turn:late-shrink", previouslyMeasured: true, delta: -3 },
    ])).toBe(10);
  });
});
