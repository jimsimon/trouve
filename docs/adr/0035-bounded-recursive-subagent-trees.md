# Bounded recursive subagent trees

Status: Accepted (2026-08)

## Context

Provider-native collaboration systems can spawn a child that delegates again.
Trouve previously represented those descendants as durable threads but treated
only direct children as part of the parent's lifecycle. A root turn could
therefore report completion and close its provider stream while a grandchild
was still waiting for capacity. Trouve-owned `spawn_thread` and
`spawn_session` avoided that failure by rejecting all nested delegation, at
the cost of inconsistent capabilities and limited composition.

## Decision

Provider-native and trouve-owned subagents form one durable parent/child tree.
Nested descendants remain independently addressable threads, including when a
`spawn_session` edge crosses into another worktree session.

Delegation is bounded to four levels below the root, four concurrently active
direct children per parent, and sixteen concurrently active descendants per
root tree. Spawn admission is serialized per root tree so parallel tool calls
cannot race those limits. Children inherit the parent's permission posture;
read-only modes may delegate only same-mode `spawn_thread` work and cannot
spawn sessions or escalate into a writing mode.

A provider-native root stream remains open after provisional root completion
until every announced descendant reaches a terminal state. `spawn_output`
treats an active descendant as active subtree work and aggregates subtree
usage. The existing subagent endpoint remains direct-child-only by default;
clients that present whole-tree state opt into its recursive projection.

## Consequences

- Agents can decompose work recursively without losing durable transcripts,
  status, or accounting.
- Capacity-delayed grandchildren can finish instead of being aborted when the
  root provider turn first reports completion.
- Read-only exploration and review agents can fan out safely without gaining a
  mutation path.
- Nested trees consume bounded model capacity and may keep a root turn alive
  longer than its own model response.
- Direct-child callers remain compatible, while tree-aware clients must request
  recursive descendants explicitly.

## Alternatives rejected

- **Disallow all nesting:** provider-native harnesses do not reliably expose a
  pre-spawn policy hook, and rejecting their descendants would discard useful
  work after it has already started.
- **Allow unbounded nesting:** recursive fan-out could exhaust provider and
  local resources or deadlock on model capacity.
- **Flatten every descendant under the root:** this hides delegation ownership
  and makes child completion and usage attribution ambiguous.
