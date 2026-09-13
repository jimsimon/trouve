import { describe, expect, it } from "vitest";

import { liveModelElapsed, mergeReviewTaskSnapshot } from "./review-progress.js";
import type { ReviewTask } from "./types.js";

// The merge logic only reads the progress fields; the fixtures omit the rest.
const task = (fields: Partial<ReviewTask>): ReviewTask => fields as ReviewTask;

describe("review task progress merging", () => {
  it("live model time extends the cumulative total from a local receive time", () => {
    const receivedAt = 10_000;
    const merged = mergeReviewTaskSnapshot(
      undefined,
      task({
        status: "running",
        model_elapsed_ms: 120_000,
        model_started_at: "2099-08-01T11:58:00Z",
        last_progress_at: "2099-08-01T12:00:00Z",
      }),
      receivedAt,
    );
    expect(liveModelElapsed(merged, receivedAt + 5_000)).toBe(125_000);
  });

  it("a new snapshot re-anchors live time without using the server clock", () => {
    const merged = mergeReviewTaskSnapshot(
      undefined,
      task({
        status: "running",
        model_elapsed_ms: 125_000,
        model_started_at: "1999-08-01T11:58:00Z",
        last_progress_at: "1999-08-01T12:00:00Z",
      }),
      20_000,
    );
    expect(liveModelElapsed(merged, 22_000)).toBe(127_000);
  });

  it("an unchanged running snapshot preserves its local receive anchor", () => {
    const snapshot = task({
      status: "running",
      lifecycle_stage: "running_model",
      provider_wait_ms: 2_000,
      model_elapsed_ms: 120_000,
      input_tokens: 100,
      cached_input_tokens: 20,
      output_tokens: 10,
      tool_call_count: 1,
      candidate_issue_count: 0,
      confirmed_issue_count: 0,
      model_started_at: "2026-08-01T11:58:00Z",
      last_progress_at: "2026-08-01T12:00:00Z",
      started_at: "2026-08-01T11:57:00Z",
      elapsed_ms: 180_000,
    });
    const current = mergeReviewTaskSnapshot(undefined, snapshot, 10_000);
    const reloaded = mergeReviewTaskSnapshot(
      current,
      { ...snapshot, elapsed_ms: 185_000 },
      15_000,
    );

    expect(reloaded.model_elapsed_snapshot_at).toBe(10_000);
    expect(reloaded.elapsed_ms).toBe(185_000);
    expect(liveModelElapsed(reloaded, 16_000)).toBe(126_000);
  });

  it("an older snapshot cannot replace newer SSE progress", () => {
    const current = mergeReviewTaskSnapshot(
      undefined,
      task({
        status: "running",
        lifecycle_stage: "running_tool",
        provider_wait_ms: 2_000,
        model_elapsed_ms: 130_000,
        input_tokens: 100,
        cached_input_tokens: 20,
        output_tokens: 30,
        tool_call_count: 2,
        candidate_issue_count: 1,
        confirmed_issue_count: 0,
        model_started_at: "2026-08-01T11:58:00Z",
        last_progress_at: "2026-08-01T12:00:10Z",
        elapsed_ms: 190_000,
      }),
      20_000,
    );
    const merged = mergeReviewTaskSnapshot(
      current,
      task({
        status: "running",
        lifecycle_stage: "running_model",
        provider_wait_ms: 2_000,
        model_elapsed_ms: 120_000,
        input_tokens: 100,
        cached_input_tokens: 20,
        output_tokens: 10,
        tool_call_count: 1,
        candidate_issue_count: 0,
        confirmed_issue_count: 0,
        model_started_at: "2026-08-01T11:58:00Z",
        last_progress_at: "2026-08-01T12:00:00Z",
        elapsed_ms: 180_000,
        prompt: "retained task detail",
      }),
      30_000,
    );

    expect(merged.lifecycle_stage).toBe("running_tool");
    expect(merged.model_elapsed_ms).toBe(130_000);
    expect(merged.tool_call_count).toBe(2);
    expect(merged.model_elapsed_snapshot_at).toBe(20_000);
    expect(merged.prompt).toBe("retained task detail");
  });

  it("a genuine stage change establishes a new local receive anchor", () => {
    const snapshot = task({
      status: "running",
      lifecycle_stage: "running_model",
      provider_wait_ms: 2_000,
      model_elapsed_ms: 120_000,
      input_tokens: 100,
      cached_input_tokens: 20,
      output_tokens: 10,
      tool_call_count: 1,
      candidate_issue_count: 0,
      confirmed_issue_count: 0,
      model_started_at: "2026-08-01T11:58:00Z",
      last_progress_at: "2026-08-01T12:00:00Z",
      elapsed_ms: 180_000,
    });
    const current = mergeReviewTaskSnapshot(undefined, snapshot, 10_000);
    const changed = mergeReviewTaskSnapshot(
      current,
      { ...snapshot, lifecycle_stage: "running_tool" },
      15_000,
    );

    expect(changed.model_elapsed_snapshot_at).toBe(15_000);
    expect(liveModelElapsed(changed, 16_000)).toBe(121_000);
  });

  it("a null model clock stops live accumulation during repair dispatch", () => {
    expect(
      liveModelElapsed(
        task({
          status: "running",
          model_elapsed_ms: 120_000,
          model_started_at: null,
          last_progress_at: "2026-08-01T12:00:00Z",
          model_elapsed_snapshot_at: 1_000,
        }),
        new Date("2026-08-01T12:00:05Z").getTime(),
      ),
    ).toBe(120_000);
  });
});
