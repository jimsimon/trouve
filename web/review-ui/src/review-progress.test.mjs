import assert from "node:assert/strict";
import test from "node:test";

import {
  liveModelElapsed,
  receiveReviewTaskSnapshot,
} from "./review-progress.ts";

test("live model time extends the cumulative total from a local receive time", () => {
  const receivedAt = 10_000;
  const task = receiveReviewTaskSnapshot(
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
  const task = receiveReviewTaskSnapshot(
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
