import assert from "node:assert/strict";
import test from "node:test";

import {
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  reviewSettingsFromMinutes,
  timeoutMinutes,
} from "./review-settings.ts";

test("review timeout settings convert between minutes and protocol seconds", () => {
  assert.equal(timeoutMinutes(900), "15");
  assert.equal(timeoutMinutes(90), "1.5");
  assert.equal(timeoutMinutes(1), TIMEOUT_MINUTES_INPUT_MIN);
  assert.equal(TIMEOUT_MINUTES_INPUT_STEP, TIMEOUT_MINUTES_INPUT_MIN);
  assert.deepEqual(reviewSettingsFromMinutes("20", "12", "6"), {
    total_timeout_seconds: 1_200,
    reviewer_timeout_seconds: 720,
    coordinator_timeout_seconds: 360,
  });
  assert.deepEqual(reviewSettingsFromMinutes("1.5", "1", "0.5"), {
    total_timeout_seconds: 90,
    reviewer_timeout_seconds: 60,
    coordinator_timeout_seconds: 30,
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
  assert.throws(
    () => reviewSettingsFromMinutes("1", "0.01", "0.5"),
    /Reviewer timeout must be a positive number of whole seconds/,
  );
});
