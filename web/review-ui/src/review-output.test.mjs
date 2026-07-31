import assert from "node:assert/strict";
import test from "node:test";

import {
  LIVE_OUTPUT_OMITTED_MARKER,
  appendBoundedReviewOutput,
  boundReviewOutput,
} from "./review-output.ts";

test("review output remains unchanged below the browser-view limit", () => {
  assert.equal(boundReviewOutput("complete output", 100), "complete output");
});

test("review output keeps a marked tail at the browser-view limit", () => {
  const bounded = boundReviewOutput("0123456789".repeat(20), 100);
  assert.equal(bounded.length, 100);
  assert.ok(bounded.startsWith(LIVE_OUTPUT_OMITTED_MARKER));
  assert.ok(bounded.endsWith("0123456789"));
});

test("appending to an already bounded transcript retains the newest output", () => {
  const initial = boundReviewOutput("a".repeat(200), 100);
  const appended = appendBoundedReviewOutput(initial, "NEWEST", 100);
  assert.equal(appended.length, 100);
  assert.ok(appended.startsWith(LIVE_OUTPUT_OMITTED_MARKER));
  assert.ok(appended.endsWith("NEWEST"));
  assert.equal(
    appended.indexOf(LIVE_OUTPUT_OMITTED_MARKER),
    appended.lastIndexOf(LIVE_OUTPUT_OMITTED_MARKER),
  );
});

test("the first review output delta does not prepend undefined", () => {
  assert.equal(
    appendBoundedReviewOutput(undefined, "I'll examine the change."),
    "I'll examine the change.",
  );
});
