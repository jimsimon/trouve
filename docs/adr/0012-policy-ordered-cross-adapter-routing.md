# Policy-ordered cross-adapter routing with circuit breakers

Status: Accepted (2026-07)

## Context

Provider-neutral routing originally kept live failover within one execution
adapter. That avoided translating in-flight state, but meant an API route
could not hand work to Codex, Cursor, or Claude Code even when they exposed
the same model. Capacity-only ranking also gave every API route unknown health;
with many configured providers, deterministic tie-breaking could repeatedly
hammer a long prefix of broken credentials, exhausted quotas, or unavailable
endpoints before reaching a working route.

Users also need intentional policy. A live allowance percentage is useful
when available, but it cannot express preferences based on trust, cost,
privacy, or an organization's provider agreement.

## Decision

Native chat providers and vendor-agent backends participate in one ordered
route sequence. Each attempt reports a common completed, cancelled, or failed
outcome. On a safe failover, the next route resumes from the persisted
transcript and shared session worktree. Provider-qualified model ids remain
hard pins and never enter automatic routing.

Configuration may contain an ordered provider preference prefix. Healthy
listed providers follow that order; unlisted providers remain eligible and
use reported subscription health, recent success, then stable ids as
tie-breakers. Reported exhaustion and an open circuit override preference.

Concrete provider/model failures persist in SQLite. Capacity, authentication,
and availability failures receive class-specific capped exponential cooldowns;
a successful turn closes the circuit and becomes a sticky hint for routes
whose capacity is otherwise unknown. Editing or deleting a provider clears
its learned failures. Open circuits are omitted from a turn, and a turn tries
at most four fresh routes. If every route is cooling down or reports exhausted
capacity, the turn fails fast with a retry horizon instead of probing them.

Retry safety remains conservative. A native model stream cannot execute tools,
so any native provider error can hand off after persisting partial text.
A vendor backend's non-capacity error can hand off only before it starts a
tool; after that, the outcome may be ambiguous. Positively classified capacity
errors may still hand off after tool activity, with open audit cards closed as
aborted and the worktree treated as authoritative.

## Consequences

- API routes and vendor-agent backends can replace one another within a turn.
- Users can control the normal provider order without disabling automatic
  recovery or making omitted providers unreachable.
- A cold configuration may require more than one turn to discover a working
  route, but no single turn fans out unboundedly and known failures are not
  retried every turn or after a restart.
- Provider-native reasoning, caches, approvals, and vendor session state do
  not cross adapters. Continuations receive a bounded transcript digest and
  inspect the shared worktree when necessary.
- Circuit state is operational history, not protocol state; route selections
  and failovers remain auditable through `model.route_selected` events.

## Alternatives rejected

- Racing identical prompts across providers wastes quota and can duplicate
  backend side effects.
- Unlimited sequential probing eventually finds a cold route but creates
  unpredictable latency, cost, and provider traffic.
- Making preference a hard allow-list removes resilience when preferred
  providers are exhausted.
- Retrying every vendor error after tool activity risks duplicating writes and
  commands whose first outcome is unknown.
