import { describe, expect, it } from "vitest";

import {
  MAX_PARALLEL_REVIEWS,
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  reviewSettingsFromMinutes,
  timeoutMinutes,
} from "./review-settings.js";

describe("review execution settings", () => {
  it("review timeout settings convert between minutes and protocol seconds", () => {
    expect(timeoutMinutes(900)).toBe("15");
    expect(timeoutMinutes(90)).toBe("1.5");
    expect(timeoutMinutes(1)).toBe(TIMEOUT_MINUTES_INPUT_MIN);
    expect(TIMEOUT_MINUTES_INPUT_STEP).toBe("any");
    expect(reviewSettingsFromMinutes("4", "20", "12", "6")).toEqual({
      max_parallel_reviews: 4,
      total_timeout_seconds: 1_200,
      reviewer_timeout_seconds: 720,
      coordinator_timeout_seconds: 360,
    });
    expect(reviewSettingsFromMinutes("2", "1.5", "1", "0.5")).toEqual({
      max_parallel_reviews: 2,
      total_timeout_seconds: 90,
      reviewer_timeout_seconds: 60,
      coordinator_timeout_seconds: 30,
    });
  });

  it("review timeout settings reject invalid deadlines", () => {
    expect(() => reviewSettingsFromMinutes("2", "10", "11", "5")).toThrow(
      /Reviewer timeout cannot exceed/,
    );
    expect(() => reviewSettingsFromMinutes("2", "10", "5", "0")).toThrow(
      /Final editor timeout must be a positive/,
    );
    expect(() => reviewSettingsFromMinutes("2", "1", "0.01", "0.5")).toThrow(
      /Reviewer timeout must be a positive number of whole seconds/,
    );
    expect(() => reviewSettingsFromMinutes("1.5", "10", "5", "3")).toThrow(
      /Max parallel reviews must be a whole number from 1 to 32/,
    );
    expect(() =>
      reviewSettingsFromMinutes(String(MAX_PARALLEL_REVIEWS + 1), "10", "5", "3"),
    ).toThrow(/Max parallel reviews must be a whole number from 1 to 32/);
  });
});
