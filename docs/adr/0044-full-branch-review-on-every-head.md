# ADR 0044: Full-branch review on every head

Status: Accepted (2026-09)

## Context

The review service originally used the last successfully published head as an
incremental watermark. A clean incremental review could not make the Check Run
succeed: reconciliation first scheduled a full-branch confirmation. In
practice, those confirmations could still find substantial issues outside the
incremental window. The service therefore paid for two review rounds at the
point where confidence mattered while retaining ancestry checks, rewritten-
history handling, previously reviewed hunk filtering, coverage-debt state, and
a separate full-review action.

The durable pull-request finding ledger, root-cause history, prior candidate
rejections, external-thread reconciliation, and carried anchors are independent
of diff selection. They remain useful for convergence and churn control even
when each model round sees the complete branch.

## Decision

Every newly requested, automatically queued, or retried review covers the
complete pull-request branch from the Git merge base of the current base ref
through the exact head SHA. The service has one review command and one retry
path; `@trouve-ai review full` remains an accepted alias for
`@trouve-ai review`.

A successfully published round at the exact head makes the Check Run succeed
as soon as it has no open blocking findings. Newly created rounds have no
incremental coverage debt and no separate full-coverage confirmation round.

Pull-request-scoped structured history continues into later rounds, including
finding state and dismissal decisions, root-cause themes, recurrence evidence,
prior candidate rejections, external review threads, and carried finding
anchors. Prior reviewer output and a previous clean verdict are not treated as
coverage of a new head. Exact-head interrupted-job recovery may still reuse a
completed task when its persisted prompt and input digest are unchanged.

Legacy database rows retain their historical scope, watermark, and coverage
columns for migration compatibility. Protocol 8.0 removes the request scope
and the obsolete watermark and raw coverage response fields while keeping the
historical scope enum readable. A clean pre-8.0 partial result exposes one
derived compatibility-pending state while reconciliation makes at most two
full-branch attempts (the initial attempt and one automatic retry).

This supersedes only ADR 0014's incremental-watermark diff-selection decision;
its durable job, task, publication, and statistics decisions remain in force.

## Consequences

Diff selection and Check Run conclusions have one invariant, and a clean
result requires one trusted round rather than an incremental pass followed by
a full confirmation. Reconciliation and the user interfaces retain only the
derived compatibility-pending state needed to settle already-persisted
pre-8.0 partial results; it cannot arise for a newly created round.

Review cost and latency now scale with total branch size on every pushed head,
so large pull requests with frequent small pushes may consume more reviewer
tokens. Exact-head superseding still prevents stale work from publishing, and
bounded batching, semantic routing, deduplication, and durable finding history
remain the available cost and churn controls. Domain-oriented semantic routing
is a separate follow-up decision rather than part of this cutover.

## Alternatives rejected

- Keep incremental review and full confirmation: it preserves the extra
  machinery and commonly pays for both rounds before success.
- Keep incremental review as a non-gating preview: it adds a second result
  model and user experience without improving the merge verdict.
- Carry a prior clean verdict forward as coverage: a small change can alter the
  behavior of unchanged code, so review output from another head is context,
  not proof of the current head.
