import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const styles = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");

test("review jobs distinguish new findings from PR-wide open findings", () => {
  assert.match(types, /open_issue_count\?: number \| null/u);
  assert.match(source, /open across this pull request/u);
  assert.match(source, /A clean incremental result does not resolve findings from earlier rounds/u);
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

test("multi-line review warnings use a stacked banner", () => {
  assert.equal(
    source.match(/class="banner warning stacked"/gu)?.length,
    2,
  );
  assert.match(styles, /\.banner\.stacked \{ flex-direction: column;/u);
});
