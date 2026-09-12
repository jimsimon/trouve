import { describe, expect, it } from "vitest";

import {
  LIVE_OUTPUT_OMITTED_MARKER,
  appendBoundedReviewOutput,
  boundReviewOutput,
} from "./review-output.js";

describe("bounded review output", () => {
  it("review output remains unchanged below the browser-view limit", () => {
    expect(boundReviewOutput("complete output", 100)).toBe("complete output");
  });

  it("review output keeps a marked tail at the browser-view limit", () => {
    const bounded = boundReviewOutput("0123456789".repeat(20), 100);
    expect(bounded).toHaveLength(100);
    expect(bounded.startsWith(LIVE_OUTPUT_OMITTED_MARKER)).toBe(true);
    expect(bounded.endsWith("0123456789")).toBe(true);
  });

  it("appending to an already bounded transcript retains the newest output", () => {
    const initial = boundReviewOutput("a".repeat(200), 100);
    const appended = appendBoundedReviewOutput(initial, "NEWEST", 100);
    expect(appended).toHaveLength(100);
    expect(appended.startsWith(LIVE_OUTPUT_OMITTED_MARKER)).toBe(true);
    expect(appended.endsWith("NEWEST")).toBe(true);
    expect(appended.indexOf(LIVE_OUTPUT_OMITTED_MARKER)).toBe(
      appended.lastIndexOf(LIVE_OUTPUT_OMITTED_MARKER),
    );
  });

  it("the first review output delta does not prepend undefined", () => {
    expect(appendBoundedReviewOutput(undefined, "I'll examine the change.")).toBe(
      "I'll examine the change.",
    );
  });
});
