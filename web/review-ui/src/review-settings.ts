import type { CodeReviewSettings } from "./types";

export const TIMEOUT_MINUTES_INPUT_MIN = String(1 / 60);
export const TIMEOUT_MINUTES_INPUT_STEP = TIMEOUT_MINUTES_INPUT_MIN;
export const MAX_PARALLEL_REVIEWS = 32;

export function timeoutMinutes(seconds: number): string {
  return String(seconds / 60);
}

function timeoutSeconds(value: string, label: string): number {
  const minutes = Number(value);
  const seconds = minutes * 60;
  if (!Number.isFinite(minutes) || minutes <= 0 || !Number.isSafeInteger(seconds)) {
    throw new Error(`${label} must be a positive number of whole seconds`);
  }
  return seconds;
}

export function reviewSettingsFromMinutes(
  maxParallel: string,
  total: string,
  reviewer: string,
  coordinator: string,
): CodeReviewSettings {
  const maxParallelReviews = Number(maxParallel);
  if (
    !Number.isSafeInteger(maxParallelReviews) ||
    maxParallelReviews <= 0 ||
    maxParallelReviews > MAX_PARALLEL_REVIEWS
  ) {
    throw new Error(`Max parallel reviews must be a whole number from 1 to ${MAX_PARALLEL_REVIEWS}`);
  }
  const settings = {
    max_parallel_reviews: maxParallelReviews,
    total_timeout_seconds: timeoutSeconds(total, "Total review timeout"),
    reviewer_timeout_seconds: timeoutSeconds(reviewer, "Reviewer timeout"),
    coordinator_timeout_seconds: timeoutSeconds(coordinator, "Final editor timeout"),
  };
  if (settings.reviewer_timeout_seconds > settings.total_timeout_seconds) {
    throw new Error("Reviewer timeout cannot exceed the total review timeout");
  }
  if (settings.coordinator_timeout_seconds > settings.total_timeout_seconds) {
    throw new Error("Final editor timeout cannot exceed the total review timeout");
  }
  return settings;
}
