# 0043 — Explicit and sticky model route selection

Status: Accepted (2026-08).

## Context

ADR 0042 introduced provider-neutral model selection and bounded failover, but
its picker catalog replaced hosted provider-qualified choices with a bare model
id. That hid the existing hard-pin behavior and made provider-specific usage
details inaccessible from the model picker. Re-ranking every new turn could
also move a conversation between healthy providers merely because reported
headroom changed, forcing vendor backends to replay or digest history.

This decision partially supersedes ADR 0042's picker-identifier and
concrete-choice policy. Its route eligibility, handoff safety, and bounded
failover rules remain in force.

## Decision

- `/v1/model-routes` exposes both `auto/<model>` entries and concrete
  `provider/<model>` entries. An automatic entry contains every compatible
  hosted API or vendor-agent route; each concrete entry contains exactly one
  route. Bare neutral ids remain accepted as a compatibility alias but are no
  longer emitted by the picker catalog.
- A concrete selection is a hard pin and never crosses providers. Automatic
  selections may cross API-provider and vendor-agent adapter boundaries under
  ADR 0042's safe-handoff rules.
- The first successful route for an automatic selection becomes that thread's
  durable affinity. Later turns keep it ahead of provider preference, capacity
  headroom, and global learned-success ordering. An open circuit or explicitly
  exhausted subscription makes it ineligible; an attempt failure can fail over,
  and the next successful route replaces the affinity.
- Affinity belongs to a thread rather than its containing session because each
  thread owns an independent transcript and vendor resume state. Changing the
  thread's model clears its affinity atomically.
- Provider-specific picker entries retain their provider's subscription usage
  annotation. Automatic entries may summarize the best currently reported
  route, but that summary does not override a healthy thread affinity.
- The built-in `local` provider and user-configured local OpenAI-compatible
  endpoints remain concrete choices. They do not merge with hosted automatic
  routes merely because a local model has the same name.

## Consequences

- A shared hosted model appears as `auto/gpt-5.6-sol` alongside entries such as
  `openai/gpt-5.6-sol`, `codex/gpt-5.6-sol`, and `cursor/gpt-5.6-sol` when those
  routes are available.
- Automatic routing avoids repeated cross-provider context replay during a
  healthy conversation while retaining bounded recovery from quota,
  authentication, and availability failures.
- Provider pins remain predictable: an error is reported for that selected
  provider rather than silently spending quota elsewhere.
- Existing stored bare model ids continue to run and clients map them to the
  corresponding `auto/` picker row.

## Alternatives rejected

- Re-ranking every turn maximizes instantaneous headroom but wastes context and
  vendor-session continuity.
- Hiding concrete choices prevents users from enforcing cost, privacy, or
  account policy and removes provider-specific allowance visibility.
- Session-wide affinity makes unrelated threads compete for one route even
  though their transcripts and vendor sessions are independent.
