# ADR 0041: Evidence-backed review churn controls

Status: Accepted (2026-08)

## Context

ADR 0040 made root-cause themes durable without adding debounce or a dedicated
model pass. A follow-up audit found three remaining ways the review service
could still create churn or hide useful feedback: the coordinator could only
consolidate reviewer candidates rather than sweep sibling manifestations,
flat GitHub review comments lacked resolved/outdated thread state, and grouped
publication state could be recorded before GitHub accepted its primary
comment. Count-bounded history could also consume unbounded prompt bytes, and
finding resolution did not identify the exact fixing revision.

## Decision

Keep immediate dispatch, superseding, and the single existing coordinator
pass. Reviewers sweep sibling manifestations within their diff batch. The
coordinator may use a candidate that exposes a root cause as provenance for
additional independently verified manifestations discovered while sweeping
the changed behavior.

Deduplicate only against unresolved, non-outdated GitHub review threads.
Commit grouped publication state only after GitHub accepts the primary
root-cause comment.

Persist the observed head, resolving head, and resolving review job for each
finding. Supply bounded exact prior-fix diffs to later coordinators. Serialize
compact review-history projections under explicit byte budgets rather than
sending complete protocol records.

Expose aggregate churn measurements: recurrence, fix regressions, previously
missed findings, grouped manifestations, external duplicates, weak-evidence
rejections, and review rounds required to reach a clean result.

## Consequences

The existing coordinator performs a genuine root-cause sweep without another
model invocation. Duplicate filtering cannot treat stale GitHub threads as
current feedback, and the web UI never claims an unpublished root-cause
comment represents grouped findings.

Later reviews can distinguish a regression in an attempted fix from a
previously missed manifestation using immutable revision evidence. Prompt
growth remains bounded on long-running pull requests, and maintainers can
measure whether review churn improves rather than relying on anecdotal review
sequences.
