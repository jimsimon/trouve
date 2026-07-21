# 0021 — Policy-ordered cross-adapter model routing

Status: Accepted (2026-07).

## Context

ADR 0016 and ADR 0020 make models.dev the canonical metadata and option
catalog for public models while live providers and vendor CLIs determine
account availability. The model picker still exposed provider-qualified ids,
which fixed a thread to one API account or vendor subscription even when
several routes could run the same catalog-normalized model.

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
  remains the provider-qualified compatibility catalog.
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
- Each attempt reports a common completed, cancelled, or failed result.
  Native provider errors can safely hand off because a model stream cannot
  execute tools itself. A vendor backend's non-capacity error can hand off
  only before tool activity; a positively classified capacity error may hand
  off after tool activity once open tool cards are closed as aborted.
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
  events. Turn capacity remains globally bounded and provider-specific
  capacity is acquired separately for each attempted route.

models.dev currently normalizes metadata, option schemas, and public ids but
does not expose a universal cross-vendor alias graph. The identity function
therefore remains deliberately conservative; future reviewed aliases can
extend grouping without changing the picker or turn protocol.

## Consequences

- A picker normally shows a bare hosted model id such as `gpt-5.6-sol`, while
  local and transport-owned entries remain visibly qualified, such as
  `local/qwen2.5-coder:7b` or `cursor/default`.
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
