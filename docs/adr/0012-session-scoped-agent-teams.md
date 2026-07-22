# 0012: Session-scoped role-based agent teams

Status: Accepted (2026-07)

## Context

Users want an optional chat shape where several named agents collaborate on a
goal, can mention one another, and remain directly addressable by the user.
Treating each role as a provider-specific subagent would make behavior depend
on the selected backend, lose coordination state on restart, and bypass the
existing event log, prompt queue, permission, and worktree guarantees.

## Decision

A team is a durable `team` session kind. Each member is a persistent trouve
thread with a stable id, handle, role, data-driven agent mode, and model. The
server owns orchestration: canonical team messages, resolved mentions,
deliveries, lifecycle state, and a bounded automatic-turn budget are persisted
and exposed over HTTP plus the session event stream.

Human messages without a mention go to the orchestrator. Explicit mentions
enqueue durable prompts for those members. Agent output is always added to the
shared timeline; explicit teammate mentions enqueue follow-up prompts. Roles do
not add Rust control flow, members do not recursively spawn agents, and all
members share the session worktree under the existing mutation lock.

The client presents teams as a distinct session kind with a shared timeline,
read-only member transcripts, and an optional Agents inspector. Team delivery
queues are server-owned rather than directly editable through the ordinary
thread controls. The client folds events and never coordinates agents locally.

## Consequences

- Coordination survives restarts and works across native and bridged providers.
- Permissions, audit history, cancellation, and worktree serialization remain
  centralized in the existing engine.
- Team conversations can run autonomously, so status controls and a finite turn
  budget are mandatory safeguards.
- Canonical team messages and raw member transcripts are both retained, trading
  some duplicated text for clear attribution and debuggability.
- Initial templates are server-provided data; customizable team composition can
  be added without changing the orchestration model.
