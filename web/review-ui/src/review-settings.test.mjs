import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_PARALLEL_REVIEWS,
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  reviewSettingsFromMinutes,
  timeoutMinutes,
} from "./review-settings.ts";

test("review timeout settings convert between minutes and protocol seconds", () => {
  assert.equal(timeoutMinutes(900), "15");
  assert.equal(timeoutMinutes(90), "1.5");
  assert.equal(timeoutMinutes(1), TIMEOUT_MINUTES_INPUT_MIN);
  assert.equal(TIMEOUT_MINUTES_INPUT_STEP, "any");
  assert.deepEqual(reviewSettingsFromMinutes("4", "20", "12", "6"), {
    max_parallel_reviews: 4,
    total_timeout_seconds: 1_200,
    reviewer_timeout_seconds: 720,
    coordinator_timeout_seconds: 360,
  });
  assert.deepEqual(reviewSettingsFromMinutes("2", "1.5", "1", "0.5"), {
    max_parallel_reviews: 2,
    total_timeout_seconds: 90,
    reviewer_timeout_seconds: 60,
    coordinator_timeout_seconds: 30,
  });
});

test("review timeout settings reject invalid deadlines", () => {
  assert.throws(
    () => reviewSettingsFromMinutes("2", "10", "11", "5"),
    /Reviewer timeout cannot exceed/,
  );
  assert.throws(
    () => reviewSettingsFromMinutes("2", "10", "5", "0"),
    /Final editor timeout must be a positive/,
  );
  assert.throws(
    () => reviewSettingsFromMinutes("2", "1", "0.01", "0.5"),
    /Reviewer timeout must be a positive number of whole seconds/,
  );
  assert.throws(
    () => reviewSettingsFromMinutes("1.5", "10", "5", "3"),
    /Max parallel reviews must be a whole number from 1 to 32/,
  );
  assert.throws(
    () => reviewSettingsFromMinutes(String(MAX_PARALLEL_REVIEWS + 1), "10", "5", "3"),
    /Max parallel reviews must be a whole number from 1 to 32/,
  );
});
