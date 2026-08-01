import assert from "node:assert/strict";
import test from "node:test";

import { liveModelElapsed } from "./review-progress.ts";

test("live model time extends the cumulative total from its latest snapshot", () => {
  const lastProgress = "2026-08-01T12:00:00Z";
  const now = new Date("2026-08-01T12:00:05Z").getTime();
  assert.equal(
    liveModelElapsed(
      {
        status: "running",
        model_elapsed_ms: 120_000,
        model_started_at: "2026-08-01T11:58:00Z",
        last_progress_at: lastProgress,
      },
      now,
    ),
    125_000,
  );
});

test("a null model clock stops live accumulation during repair dispatch", () => {
  assert.equal(
    liveModelElapsed(
      {
        status: "running",
        model_elapsed_ms: 120_000,
        model_started_at: null,
        last_progress_at: "2026-08-01T12:00:00Z",
      },
      new Date("2026-08-01T12:00:05Z").getTime(),
    ),
    120_000,
  );
});
