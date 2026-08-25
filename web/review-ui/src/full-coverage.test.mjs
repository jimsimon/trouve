import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");

test("success is gated on the newest round covering the full branch", () => {
  // A clean partial round is pending, not settled: badge plus banner.
  assert.match(source, /reviewAwaitingFullCoverage/u);
  assert.match(source, /job\.scope !== "full"/u);
  // The server-recorded coverage flag is authoritative; the sha comparison
  // is only the legacy fallback (it misreads merge-base-refined rounds).
  assert.match(
    source,
    /!\(job\.covered_full_branch \?\? \(job\.review_base_sha \?\? ""\) === job\.base_ref\)/u,
  );
  assert.match(types, /covered_full_branch\?: boolean \| null;/u);
  assert.match(source, /full review pending/u);
  assert.match(source, /Full-branch confirmation pending/u);
  assert.match(source, /reviewed only the changes since the\s+last review/u);
  // Persistent guidance is not an assertive screen-reader alert.
  assert.doesNotMatch(
    source,
    /banner warning stacked" role="alert">\s*<strong>Full-branch confirmation pending/u,
  );
  // Open blocking findings outrank the pending state.
  assert.match(
    source,
    /if \(job\.open_issue_count != null && job\.open_issue_count > 0\) return "open"/u,
  );
});

test("the advisory tier is modeled but the per-PR churn signal is gone", () => {
  assert.match(types, /advisory_open_issue_count\?: number \| null;/u);
  // The aggregate churn *stats* panel remains; the per-job signal does not.
  assert.doesNotMatch(types, /ReviewChurnSignal/u);
  assert.doesNotMatch(source, /job\.churn/u);
  assert.doesNotMatch(source, /reviewChurnSoakPending/u);
});
