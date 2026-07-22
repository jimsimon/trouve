# 0012 — Trouve owns the agent capability surface

Status: Accepted (2026-07).

## Context

Trouve can run models through three kinds of provider: direct APIs, local
inference, and subscription-backed vendor CLIs. Direct and local providers
already use Trouve's agent loop and `ToolExecutor`, but CLI providers arrive
with their own tools, slash commands, skills, permission behavior, event
names, and rendering conventions. As a result, changing the selected model
can also change the product the user appears to be using.

An MCP bridge alone is insufficient. It can make Trouve tools callable from
a vendor harness, but unless the vendor capability surface is replaced and
isolated, calls can still bypass `ToolExecutor`. It also leaves vendor slash
commands and skills in the UI, and exposes provider-specific tool schemas to
clients. Conversely, forcing every model to use one unfamiliar schema risks
degrading tool-use quality because models are trained on different native
tool dialects.

Vendor CLIs may still provide useful subscription authentication, context
storage, replay, and compaction. Those implementation details do not need to
be identical when they are not user-visible.

## Decision

- Trouve is the authoritative source of user-visible agent capabilities:
  commands, skills, rules, hooks, goals, apps/plugins, subagent operations,
  tool semantics, permission policy, audit events, and presentation.
- Every executable capability resolves to a canonical Trouve operation and
  is executed through `ToolExecutor` (or an engine-owned interaction
  primitive that cannot perform an external side effect). User-configured
  MCP servers are discovered and invoked by Trouve; they are not mounted
  directly into vendor harnesses.
- Trouve maintains one resolved capability set per turn. A provider adapter
  may project that set into a provider-specific **tool dialect**—names,
  descriptions, and argument schemas optimized for that model—but the
  projection maps back to canonical operation identifiers before policy,
  execution, events, and rendering. Provider dialects are an internal model
  compatibility layer, not a product capability difference.
- Trouve publishes a core-owned command catalog through the protocol.
  Vendor-reported slash commands and skills are not authoritative and are
  not surfaced in new event logs. Every catalog entry declares either prompt
  or action dispatch. Action commands execute through a typed Trouve endpoint
  and persist their output without entering a model session; prompt commands
  are expanded before a prompt enters either a native loop or a vendor
  session. Skills are loaded by stable name through a read-only Trouve tool,
  never by exposing host-absolute paths to a model.
- Vendor capabilities are suppressed only after a Trouve replacement exists.
  Each adapter keeps an explicit inventory whose entries are classified as
  **replace**, **retain as an invisible transport concern**, or **omit**.
  Replay and compaction may be retained when their ownership has no
  user-visible effect.
- A CLI adapter may claim authoritative mode only for a certified vendor
  version and must verify at runtime that forbidden built-ins are absent.
  Certification fails closed: an unknown or unverifiable version cannot be
  presented as parity-capable. Providers without hard isolation may run in
  an explicitly labelled compatibility mode, but cannot silently claim the
  same guarantees.
- Tool-call events use canonical operation identifiers and correlated call
  IDs regardless of provider. Clients render canonical operations; they do
  not carry an expanding set of vendor-name aliases.
- Provider launch environments and configuration are isolated so ambient
  user/vendor instructions, plugins, MCP servers, hooks, and skills cannot
  reintroduce a second capability source.
- Organization-managed policy remains a higher-level governance boundary.
  Trouve does not weaken policy restrictions. A vendor-managed executable
  hook that the CLI does not allow a session to suppress is outside the
  authoritative contract; deployments requiring a literal single execution
  chokepoint must disable such hooks in managed policy.
- A vendor's monolithic safe/bare mode is not sufficient when it also
  disables the explicit Trouve bridge or subscription authentication.
  Adapters use the smallest granular isolation controls that retain both.

## Consequences

- Switching providers changes the model and transport, not prompt
  completion, tool cards, permissions, skill availability, or workflows.
- Provider adapters become more deliberate: each needs a dialect projection,
  capability inventory, isolation strategy, supported-version range, and
  conformance tests.
- Tool schemas need not be byte-identical across models. Semantic parity and
  canonical execution are required; model-facing syntax may differ.
- CLI replay and compaction can remain vendor-owned, reducing migration risk,
  but Trouve must inject the same current rules, mode, and skill catalog on
  every turn where a resumed vendor session might otherwise be stale.
- A newly released CLI can temporarily lose authoritative certification
  until its suppression behavior is tested. This is preferable to silently
  bypassing permissions or changing the user experience.
- Enterprise policy can further restrict a subscription CLI. This is analogous
  to an OS policy restricting a native provider, but executable managed hooks
  are a documented vendor limitation because they cannot be routed through
  `ToolExecutor`.
- Existing `thread.commands_updated` events remain readable for historical
  replay, but a new core-owned catalog event supersedes them for current
  clients and new turns.
- Trouve carries a deliberately small built-in skill baseline. User and
  workspace skills override it by stable name, allowing local policy without
  making the selected provider a second skill source.
- The built-in layer is enabled by default and can be disabled globally in
  Settings → Skills (`builtin_skills_enabled = false` in `config.toml`). The
  switch removes built-ins from prompts, completion catalogs, explicit
  invocation, and `load_skill`; it does not disable user or workspace skills.

## Alternatives rejected

- **Expose every vendor's native experience.** This preserves vendor
  optimization but makes provider selection change the Trouve product.
- **Use one literal tool schema for every model.** Operationally simple, but
  unnecessarily discards model training advantages. Canonical semantics plus
  dialect projections provide consistency without requiring identical
  syntax.
- **Bridge Trouve tools alongside vendor tools.** Duplicate capabilities do
  not establish authority and permit side effects outside `ToolExecutor`.
- **Take over all replay and compaction immediately.** Full loop ownership
  would be cleaner, but subscription CLIs generally expose an agent seam, not
  a raw inference seam. It is not required for the user-visible parity goal.
