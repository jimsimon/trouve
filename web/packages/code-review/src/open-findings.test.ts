import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const read = (file: string): string => readFileSync(new URL(file, import.meta.url), "utf8");

const jobDetail = read("./job-detail.ts");
const jobsPage = read("./jobs-page.ts");
const presentation = read("./presentation.ts");
const sharedViews = read("./shared-views.ts");
const styles = read("./styles.css");
const types = read("./types.ts");

describe("PR-wide open findings", () => {
  it("review jobs distinguish new findings from PR-wide open findings", () => {
    expect(types).toMatch(/open_issue_count\?: number \| null/u);
    expect(types).toMatch(/legacy_coverage_pending\?: boolean/u);
    expect(types).toMatch(/legacy_coverage_exhausted\?: boolean/u);
    expect(jobDetail).toMatch(/open across this pull request/u);
    expect(jobDetail).toMatch(
      /A clean full-branch result does not resolve findings from earlier rounds/u,
    );
    expect(jobDetail).toMatch(/open across pull request/u);
    expect(sharedViews).toMatch(/Open status unknown/u);
    expect(jobDetail).toMatch(/legacy review predates PR-wide finding snapshots/u);
  });

  it("final-editor retry uses the server-authoritative capability", () => {
    expect(types).toMatch(/final_editor_retryable_job_ids\?: string\[\]/u);
    expect(jobsPage).toMatch(
      /\.finalEditorRetryable=\$\{\(dashboard\.final_editor_retryable_job_ids \?\? \[\]\)\.includes\(selectedId\)\}/u,
    );
    expect(jobDetail).not.toMatch(/finalEditorRetryable && unadjudicatedCandidates\.length > 0/u);
  });

  it("unknown PR-wide status is visually distinct from review failure", () => {
    expect(presentation).toMatch(/if \(job\.open_issue_count == null\) return "unknown"/u);
    expect(sharedViews).toMatch(/<span class="status warning">status unknown<\/span>/u);
    expect(`${presentation}\n${sharedViews}\n${jobDetail}`).not.toMatch(
      /open_issue_count !== 0/u,
    );
  });

  it("legacy partial success stays visibly pending until its full review", () => {
    expect(presentation).toMatch(
      /if \(job\.legacy_coverage_pending\) return "coverage_pending"/u,
    );
    expect(sharedViews).toMatch(
      /<span class="status warning">full review pending<\/span>/u,
    );
    expect(jobDetail).toMatch(/Full-branch compatibility review pending/u);
    expect(jobDetail).toMatch(/at most two automatic attempts/u);
    expect(presentation).toMatch(
      /if \(job\.legacy_coverage_exhausted\) return "coverage_exhausted"/u,
    );
    expect(sharedViews).toMatch(
      /<span class="status warning">full review required<\/span>/u,
    );
    expect(jobDetail).toMatch(/Automatic full-branch compatibility attempts exhausted/u);
    expect(jobDetail).toMatch(/requestReview\(detail\.job\)/u);
    expect(jobDetail).toMatch(/Run whole review/u);
    expect(jobDetail).toMatch(/role="status" aria-live="polite"/u);
  });

  it("attention replaces succeeded and job rows reserve its full width", () => {
    expect(sharedViews).toMatch(
      /attentionState === "open"\s*\? html`<span class="status warning">needs attention<\/span>`\s*: attentionState === "unknown"/u,
    );
    // The job row renders the attention pill in place of the status pill
    // rather than alongside it.
    expect(sharedViews).toMatch(/\$\{attentionPill\(attentionState, job\.status\)\}/u);
    expect(sharedViews).not.toMatch(
      /\$\{statusPill\(job\.status\)\}\s*\$\{attentionState === "open"/u,
    );
    expect(styles).toMatch(
      /\.job-row \{[\s\S]*grid-template-columns: max-content minmax\(0, 1fr\) 92px;/u,
    );
  });

  it("multi-line review warnings use a stacked banner", () => {
    expect(jobDetail).toMatch(
      /unadjudicatedCandidates\.length > 0\s*\? html`<div class="banner warning stacked"/u,
    );
    expect(jobDetail).toMatch(/hasOpenIssues\s*\? html`<div class="banner warning stacked"/u);
    expect(jobDetail).toMatch(
      /openIssueStatusUnknown\s*\? html`<div class="banner warning stacked"/u,
    );
    expect(jobDetail).toMatch(
      /job\.legacy_coverage_pending\s*\? html`<div class="banner warning stacked"/u,
    );
    expect(jobDetail).toMatch(
      /job\.legacy_coverage_exhausted\s*\? html`<div class="banner warning stacked"/u,
    );
    expect(styles).toMatch(/\.banner\.stacked \{ flex-direction: column;/u);
  });
});
