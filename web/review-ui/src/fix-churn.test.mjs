import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");

test("review jobs model the server-derived fix-churn signal", () => {
  assert.match(types, /export interface ReviewChurnSignal \{/u);
  assert.match(types, /finding_round_streak: number;/u);
  assert.match(types, /required_clean_rounds: number;/u);
  assert.match(types, /churn\?: ReviewChurnSignal \| null;/u);
});

test("a clean round inside the churn soak is badged, not settled", () => {
  assert.match(source, /if \(reviewChurnSoakPending\(job\)\) return "churn"/u);
  assert.match(source, /<span class="status warning">fix churn<\/span>/u);
  // Open findings outrank the churn badge.
  assert.match(
    source,
    /if \(job\.open_issue_count != null && job\.open_issue_count > 0\) return "open"/u,
  );
});

test("the job detail explains recurring instability and the clean soak", () => {
  assert.match(source, /Recurring instability: \{job\.churn\.finding_round_streak\}/u);
  assert.match(source, /relocating the defect rather than resolving it/u);
  assert.match(source, /consecutive review rounds are clean/u);
  assert.match(source, /The clean-round soak is complete/u);
  assert.match(
    source,
    /job\.status === "succeeded" && job\.churn && \(\s*<div class="banner warning stacked"/u,
  );
});
