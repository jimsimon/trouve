# ADR 0040: Durable root-cause history for code review

Status: Accepted (2026-08)

## Context

First-party code review currently stores confirmed findings by immutable review
job. Later review rounds can see unresolved findings, but resolved findings stop
providing useful context. A coordinator can therefore rediscover another
manifestation of the same defect as an unrelated issue, producing a long tail
of one-finding review rounds. Flat inline publication also encourages fixes to
individual symptoms even when several findings share one mechanism.

Review latency is already measured in minutes. Delaying dispatch to debounce
new commits would make feedback slower, and the existing superseding behavior
already prevents stale jobs from publishing. Review quality improvements must
therefore live in the review data, validation, and publication pipeline rather
than assumptions about the developer's coding harness.

## Decision

Persist root-cause themes as pull-request-scoped records independent of any one
review job. Each review round records a theme observation and links confirmed
findings to themes. Themes remain available to later coordinators after their
linked findings are resolved, and a later manifestation reopens the theme and
increments its recurrence count.

Require every coordinator-confirmed finding to include structured verification
evidence: reachable preconditions, a concrete execution path, a specific
consequence, the changed behavior that introduced the defect, and a behavioral
regression test. Record whether the finding is a new change, a recurrence, a
regression caused by an attempted fix, or a previously missed manifestation.
The server validates evidence completeness and derives recurrence defaults from
durable history rather than trusting model labels alone.

Give the coordinator bounded resolved and unresolved finding history, durable
theme history, and existing external inline review comments. This lets a
second isolated manifestation establish a theme even when the first finding
was resolved before any theme existed. Equivalent human or third-party findings are
rejected as duplicates; nearby comments that describe a different consequence
do not suppress a finding.

Publish one GitHub inline comment for multiple publishable findings that share
one unambiguous root-cause theme. The primary comment contains the root cause,
structural recommendation, and all manifestations. Other manifestations remain
first-class findings in Trouve and are marked as represented by the theme.
The web applications expose themes, recurrence, origin, and verification
evidence.

Do not add a debounce window or another model pass. Jobs continue to start
immediately and use the existing superseding rules.

## Consequences

Review history can distinguish new defects, missed manifestations, fix
regressions, and true recurrence across the full lifetime of a pull request.
Maintainers receive fewer repetitive GitHub threads while retaining every
confirmed manifestation and its fix prompt in Trouve. Reviewers must provide
stronger, testable evidence, which may reject plausible but underspecified
findings.

The protocol gains additive review fields and enum values and therefore moves
to version 7.7 under exact-version compatibility. Storage gains theme,
observation, and finding-theme tables plus evidence and origin columns.
Fetching external inline comments adds one bounded GitHub API request before
coordination, but does not add model latency.

Theme identity is coordinator-assisted and validated against records for the
same pull request. Incorrectly merging distinct mechanisms would hide useful
inline comments, so publication grouping is limited to findings with exactly
one shared theme.

## Alternatives rejected

- Debounce reviews after pushes: it directly increases feedback latency and
  duplicates the purpose of superseding stale work.
- Depend on the fix author's harness to run a broad review before pushing:
  Trouve cannot control which editor, agent, or model authors use.
- Keep only unresolved findings as history: resolving the last symptom erases
  the context needed to recognize the next recurrence.
- Add a dedicated theme-sweep model pass: it increases latency and cost; the
  final coordinator already has the appropriate consolidation role.
- Publish every symptom as an inline comment: it preserves noise and encourages
  point fixes instead of a complete root-cause repair.
