import assert from "node:assert/strict";
import test from "node:test";

import {
  liveModelElapsed,
  mergeReviewTaskSnapshot,
} from "./review-progress.ts";

test("live model time extends the cumulative total from a local receive time", () => {
  const receivedAt = 10_000;
  const task = mergeReviewTaskSnapshot(
    undefined,
    {
      status: "running",
      model_elapsed_ms: 120_000,
      model_started_at: "2099-08-01T11:58:00Z",
      last_progress_at: "2099-08-01T12:00:00Z",
    },
    receivedAt,
  );
  assert.equal(
    liveModelElapsed(task, receivedAt + 5_000),
    125_000,
  );
});

test("a new snapshot re-anchors live time without using the server clock", () => {
  const task = mergeReviewTaskSnapshot(
    undefined,
    {
      status: "running",
      model_elapsed_ms: 125_000,
      model_started_at: "1999-08-01T11:58:00Z",
      last_progress_at: "1999-08-01T12:00:00Z",
    },
    20_000,
  );
  assert.equal(liveModelElapsed(task, 22_000), 127_000);
});

test("an unchanged running snapshot preserves its local receive anchor", () => {
  const snapshot = {
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
  };
  const current = mergeReviewTaskSnapshot(undefined, snapshot, 10_000);
  const reloaded = mergeReviewTaskSnapshot(
    current,
    { ...snapshot, elapsed_ms: 185_000 },
    15_000,
  );

  assert.equal(reloaded.model_elapsed_snapshot_at, 10_000);
  assert.equal(reloaded.elapsed_ms, 185_000);
  assert.equal(liveModelElapsed(reloaded, 16_000), 126_000);
});

test("an older snapshot cannot replace newer SSE progress", () => {
  const current = mergeReviewTaskSnapshot(
    undefined,
    {
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
    },
    20_000,
  );
  const merged = mergeReviewTaskSnapshot(
    current,
    {
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
    },
    30_000,
  );

  assert.equal(merged.lifecycle_stage, "running_tool");
  assert.equal(merged.model_elapsed_ms, 130_000);
  assert.equal(merged.tool_call_count, 2);
  assert.equal(merged.model_elapsed_snapshot_at, 20_000);
  assert.equal(merged.prompt, "retained task detail");
});

test("a genuine stage change establishes a new local receive anchor", () => {
  const snapshot = {
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
  };
  const current = mergeReviewTaskSnapshot(undefined, snapshot, 10_000);
  const changed = mergeReviewTaskSnapshot(
    current,
    { ...snapshot, lifecycle_stage: "running_tool" },
    15_000,
  );

  assert.equal(changed.model_elapsed_snapshot_at, 15_000);
  assert.equal(liveModelElapsed(changed, 16_000), 121_000);
});

test("a null model clock stops live accumulation during repair dispatch", () => {
  assert.equal(
    liveModelElapsed(
      {
        status: "running",
        model_elapsed_ms: 120_000,
        model_started_at: null,
        last_progress_at: "2026-08-01T12:00:00Z",
        model_elapsed_snapshot_at: 1_000,
      },
      new Date("2026-08-01T12:00:05Z").getTime(),
    ),
    120_000,
  );
});
