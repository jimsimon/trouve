# 0052 — Policy-ordered cross-adapter model routing

Status: Accepted (2026-08).

## Context

ADR 0016 and ADR 0020 make models.dev the canonical metadata and option
catalog for public models while live providers and vendor CLIs determine
account availability. The model picker still exposed provider-qualified ids,
which fixed a thread to one API account or vendor subscription even when
several routes could run the same catalog-normalized model.
This decision extends ADRs 0016, 0020, and 0042. Provider-governed turn
admission remains authoritative for every attempted concrete route.

Provider capacity is uneven and only some vendor backends report allowance
windows. With many configured routes, blindly probing in stable order can
also spend most of every turn retrying broken credentials, exhausted quotas,
or unavailable endpoints. Provider choice additionally reflects user policy
such as trust, cost, privacy, and contractual preference, none of which can
be inferred from an allowance percentage.

Native chat providers and vendor-agent backends have different execution
loops. A handoff therefore needs a durable boundary that does not attempt to
translate provider-private live state or duplicate ambiguous side effects.

## Decision

- `/v1/model-routes` is the client model-picker catalog. Public hosted routes
  with the same safe, catalog-normalized execution id share a provider-neutral
  id. Local and loopback routes, transport-owned choices such as `default`,
  and namespaced ids retain a provider-qualified picker id. `/v1/models`
  exposes the same selector ids in the legacy `ModelInfo` shape without route
  details.
- Provider-qualified selections are explicit hard pins. Provider-neutral
  selections resolve at turn time across both API providers and vendor-agent
  backends.
- Configuration stores an ordered provider preference prefix. Healthy listed
  providers follow that order; omitted providers remain eligible and are
  ordered by reported subscription headroom, learned success, and stable ids.
  Reported exhaustion and open circuits override preference.
- Concrete provider/model failures persist in SQLite. Capacity,
  authentication, and availability failures receive class-specific capped
  exponential cooldowns. Editing or deleting a provider clears its learned
  failures. A turn tries at most four fresh routes and fails fast when all
  routes are cooling down or report exhausted capacity.
- Each attempt reports a common completed, cancelled, or failed result. A
  failure may hand off only before a mutating or unknown side effect begins;
  completed read-only tool activity remains replay-safe across adapters. Once
  mutation may have started, the failure is terminal even when it is classified
  as capacity exhaustion. Cancellation is terminal without changing route
  health.
- The persisted transcript, event log, and shared session worktree are the
  cross-adapter handoff boundary. A continuation receives the transcript or a
  bounded digest and is told to inspect current state rather than repeat work.
  Provider-private reasoning state, caches, and live approvals are not
  translated.
- Mode instructions, read/write permission, tool availability, attachments,
  and portable model settings survive a handoff. The routed option schema
  exposes only properties supported identically by every route; catalog
  thinking levels use the canonical `thinking_level` key and are translated
  to each route's native option immediately before execution.
- Initial choices and failovers are persisted as `model.route_selected`
  events. Each attempted route passes through ADR 0042's provider-governed
  cooldown admission independently; automatic routing skips routes already
  cooling down rather than waiting behind each one in preference order.

models.dev currently normalizes metadata, option schemas, and public ids but
does not expose a universal cross-vendor alias graph. The identity function
therefore remains deliberately conservative: direct catalog records use their
canonical model id, and a trouve-owned serving-surface record participates only
when its reviewed `base_model` explicitly points to that public model. Overlay
records without a public base remain concrete-only. Future reviewed aliases can
extend grouping without changing the picker or turn protocol.

## Consequences

- The original picker policy showed a bare hosted model id such as
  `gpt-5.6-sol`, while local and transport-owned entries remained visibly
  qualified. ADR 0053 supersedes that identifier and concrete-choice policy by
  emitting `auto/<model>` alongside concrete `provider/<model>` choices.
- API routes and vendor-agent backends can replace one another within a turn
  without weakening permission or tool policy.
- A cold configuration may need more than one turn to discover a working
  route, but one turn never fans out without bound and known failures are not
  retried on every turn or after a restart.
- Opaque provider state does not cross the boundary. A continuation can spend
  tokens re-establishing context or inspecting the worktree.
- Route health is operational history rather than client state; the event log
  remains the user-visible audit source.

## Alternatives rejected

- Racing identical prompts across providers wastes quota and can duplicate
  backend side effects.
- Unlimited sequential probing creates unpredictable latency, cost, and
  provider traffic.
- Making provider preference a hard allow-list removes recovery when a
  preferred provider is exhausted.
- Restricting handoff to one adapter leaves usable API or subscription
  capacity stranded behind an implementation boundary.
- Retrying every vendor error after tool activity can duplicate commands or
  writes whose first outcome is unknown.
