import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const styles = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");

test("review jobs distinguish new findings from PR-wide open findings", () => {
  assert.match(types, /open_issue_count\?: number \| null/u);
  assert.match(types, /legacy_coverage_pending\?: boolean/u);
  assert.match(types, /legacy_coverage_exhausted\?: boolean/u);
  assert.match(source, /open across this pull request/u);
  assert.match(source, /A clean full-branch result does not resolve findings from earlier rounds/u);
  assert.match(source, /open across pull request/u);
  assert.match(source, /Open status unknown/u);
  assert.match(source, /legacy review predates PR-wide finding snapshots/u);
});

test("final-editor retry uses the server-authoritative capability", () => {
  assert.match(types, /final_editor_retryable_job_ids\?: string\[\]/u);
  assert.match(source, /finalEditorRetryable=\{\(dashboard\.final_editor_retryable_job_ids \?\? \[\]\)\.includes\(selectedId\)\}/u);
  assert.doesNotMatch(source, /finalEditorRetryable && unadjudicatedCandidates\.length > 0/u);
});

test("unknown PR-wide status is visually distinct from review failure", () => {
  assert.match(source, /if \(job\.open_issue_count == null\) return "unknown"/u);
  assert.match(source, /<span class="status warning">status unknown<\/span>/u);
  assert.doesNotMatch(source, /open_issue_count !== 0/u);
});

test("legacy partial success stays visibly pending until its full review", () => {
  assert.match(source, /if \(job\.legacy_coverage_pending\) return "coverage_pending"/u);
  assert.match(source, /<span class="status warning">full review pending<\/span>/u);
  assert.match(source, /Full-branch compatibility review pending/u);
  assert.match(source, /at most two automatic attempts/u);
  assert.match(source, /if \(job\.legacy_coverage_exhausted\) return "coverage_exhausted"/u);
  assert.match(source, /<span class="status warning">full review required<\/span>/u);
  assert.match(source, /Automatic full-branch compatibility attempts exhausted/u);
  assert.match(source, /Retry the whole review/u);
});

test("attention replaces succeeded and job rows reserve its full width", () => {
  assert.match(
    source,
    /attentionState === "open" \? \(\s*<span class="status warning">needs attention<\/span>\s*\) : attentionState === "unknown"/u,
  );
  assert.doesNotMatch(
    source,
    /<StatusPill status=\{job\.status\} \/>\s*\{attentionState === "open"/u,
  );
  assert.match(styles, /\.job-row \{[\s\S]*grid-template-columns: max-content minmax\(0, 1fr\) 92px;/u);
});

test("multi-line review warnings use a stacked banner", () => {
  assert.match(
    source,
    /unadjudicatedCandidates\.length > 0 && \(\s*<div class="banner warning stacked"/u,
  );
  assert.match(
    source,
    /hasOpenIssues && \(\s*<div class="banner warning stacked"/u,
  );
  assert.match(
    source,
    /openIssueStatusUnknown && \(\s*<div class="banner warning stacked"/u,
  );
  assert.match(
    source,
    /job\.legacy_coverage_pending && \(\s*<div class="banner warning stacked"/u,
  );
  assert.match(
    source,
    /job\.legacy_coverage_exhausted && \(\s*<div class="banner warning stacked"/u,
  );
  assert.match(styles, /\.banner\.stacked \{ flex-direction: column;/u);
});
