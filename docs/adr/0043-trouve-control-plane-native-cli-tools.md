# ADR 0043: Trouve control plane with native CLI tools

Status: Accepted (2026-07).

## Context

ADR 0042 made Trouve the only executable capability source by disabling
vendor tools and replacing them through MCP. That gives one execution
chokepoint, but subscription CLIs expose an agent seam rather than raw model
inference. Their models are optimized for the harness's native tool names,
schemas, result shapes, concurrency, and error-recovery behavior. Replacing
those tools with namespaced custom MCP tools can reduce tool-use quality.

The product requirement is consistent user experience: switching providers
must not change slash completion, skills, permission prompts, tool cards,
audit history, or workflows. Identical internal execution is not itself
user-visible.

## Decision

- Trouve owns the agent **control plane**: commands, skills, modes, rules,
  permission policy, user MCP configuration, canonical operation taxonomy,
  audit events, and presentation are provider-independent.
- Native API and local providers continue to execute tools through
  `ToolExecutor`.
- Subscription CLI adapters retain their model-optimized native file, shell,
  search, edit, and other core execution tools. A native tool is accepted
  only through an explicit adapter mapping that:
  - assigns a canonical Trouve operation and correlated call id;
  - routes vendor approval requests through Trouve's permission UI;
  - confines execution to the session worktree using available vendor
    sandbox and path controls;
  - normalizes arguments, lifecycle events, results, and rendering; and
  - is covered by adapter conformance tests.
- Trouve-only capabilities such as semantic search, skill loading,
  interaction, subagent operations, todo state, and user-configured MCP tools
  are mounted through the thread-scoped Trouve bridge and still execute
  through `ToolExecutor`.
- Vendor slash commands, skills, plugins, hooks, MCP configuration, memories,
  and agent workflows are isolated or disabled where the CLI provides a
  supported control. Vendor-reported catalogs are ignored. A provider that
  cannot suppress an internal feature may retain it as an implementation
  detail, but it never becomes a second Trouve UI or command catalog.
- There is one optimized CLI integration path. Trouve does not expose a
  strict full-tool-replacement mode or a provider capability-mode distinction.
- Vendor-owned replay and compaction remain permitted because they do not
  alter the user-visible capability surface.

## Consequences

- Models keep the tool dialect on which their vendor harness optimizes them,
  while users see the same Trouve command catalog, permission flow, canonical
  tool names, cards, and event history.
- `ToolExecutor` remains the side-effect chokepoint for Trouve's own loop and
  bridged capabilities, but is not the executor for certified native CLI
  tools. ADR 0004's single-executor guarantee therefore applies to native
  API/local turns, not to vendor-agent internals.
- Adapter quality matters more: new vendor tool shapes require normalization
  tests, and missing approval or worktree-boundary guarantees must fail
  closed where the vendor protocol permits.
- Internal tool behavior can still differ slightly across providers. Trouve
  normalizes the product contract, not undocumented implementation details.

## Alternatives rejected

- **Replace every native tool through MCP.** This maximizes internal
  uniformity but discards trained-in harness behavior and makes tool quality
  depend on a custom namespaced schema.
- **Normalize only visual output.** Consistent cards alone leave vendor
  commands, skills, permissions, MCP servers, and audit semantics as competing
  product surfaces.
