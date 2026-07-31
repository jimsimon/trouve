import assert from "node:assert/strict";
import test from "node:test";

import { reviewSettingsFromMinutes, timeoutMinutes } from "./review-settings.ts";

test("review timeout settings convert between minutes and protocol seconds", () => {
  assert.equal(timeoutMinutes(900), "15");
  assert.deepEqual(reviewSettingsFromMinutes("20", "12", "6"), {
    total_timeout_seconds: 1_200,
    reviewer_timeout_seconds: 720,
    coordinator_timeout_seconds: 360,
  });
});

test("review timeout settings reject invalid deadlines", () => {
  assert.throws(
    () => reviewSettingsFromMinutes("10", "11", "5"),
    /Reviewer timeout cannot exceed/,
  );
  assert.throws(
    () => reviewSettingsFromMinutes("10", "5", "0"),
    /Final editor timeout must be a positive/,
  );
});
