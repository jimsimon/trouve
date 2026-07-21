# Provider-neutral model routing with capacity failover

Status: Superseded by 0012

## Context

The model picker exposed provider-qualified ids, even when several configured
providers offered the same underlying model. That made provider capacity a
manual concern and fixed a thread to one subscription. Vendor catalogs also
use slightly different names for equivalent models; a separate catalog can
improve those mappings over time, but execution still needs a stable routing
boundary and an auditable failover policy.

Provider errors are not all safe to retry. Replaying an arbitrary failed
agent attempt can duplicate tool side effects, while a positively identified
quota or rate-limit failure means the selected route cannot finish the work.

## Decision

Provider-qualified model ids remain supported as explicit, pinned routes. A
new provider-neutral catalog groups concrete routes under a stable model id;
new clients select that id and the engine chooses a route at turn time. The
identity-mapping function is isolated so richer catalog aliases can replace
exact-name matching without changing the protocol or execution loop.

Routes with reported subscription usage are ordered by their most constrained
allowance window. Unknown capacity remains eligible. Every choice and switch
is recorded in the thread event log.

Automatic failover is limited to errors positively classified as capacity
exhaustion. A replacement provider resumes from persisted transcript text and
the shared session worktree; completed tool events remain in the audit log.
Authentication, protocol, transport, and arbitrary tool failures remain
terminal, because replaying them could repeat a side effect whose outcome is
unknown.

An in-flight handoff stays within the same execution adapter: native chat
providers can replace native chat providers, and vendor-agent backends can
replace vendor-agent backends. A different adapter may be selected on the next
turn, but crossing that boundary mid-turn would require translating live tool
and approval state rather than merely handing off persisted state.

## Consequences

- Users choose a model independently from a provider when a stable identity is
  available, while provider-qualified ids remain an escape hatch.
- Capacity can be balanced across subscriptions and exhausted routes can hand
  work to another provider offering the same model.
- A resumed provider sees persisted text and worktree state. Tool audit events
  remain visible to the user, but hidden reasoning and provider-native caches
  do not transfer, so a backend may need to inspect state before continuing.
- Mid-turn failover requires another route using the same execution adapter.
- Exact-name routing is deliberately conservative until an external model
  catalog supplies reviewed aliases.
- Capacity detection must prefer false negatives over unsafe retries.

## Alternatives rejected

- Silent retry on every provider error risks duplicate writes and commands.
- Persisting one preferred provider on the thread defeats capacity routing.
- Replacing the provider-qualified catalog would break older clients and
  remove the explicit-route escape hatch.
