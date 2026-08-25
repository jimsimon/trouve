# ADR 0043: Cursor SDK Bridge transport

Status: Accepted (2026-08)

## Context

Trouve originally drove Cursor through `cursor-agent acp`. ACP required a
separate CLI login, could not suppress every vendor-native tool, and exposed
no same-run steering. Cursor's supported Agent SDK now ships a standalone
Bridge with a versioned `sdk.v1` Connect contract, API-key authentication,
durable local agents, streaming, cancellation, model options, and host-owned
custom-tool callbacks. Local qualification proved those capabilities and
showed that a strict `mcp` allow-list removes native filesystem and shell
tools even where Cursor's native sandbox is unavailable.

Maintaining ACP beside the SDK would preserve two authentication paths, two
session models, and two tool-confinement contracts for one provider. It would
also leave subscription health coupled to neither transport cleanly: health
already uses the SDK API-key exchange directly.

Cursor agents are durable in a thread-scoped SQLite store, while the Bridge
itself can serve multiple agent lifecycle calls before shutdown. Reusing a
Bridge removes repeated startup work, but its custom-tool callback registration
is process-wide. A global process or long-lived callback registration would mix
credentials, tools, and worktrees from unrelated turns.

## Decision

- Cursor turns use the standalone Cursor SDK Bridge over its `sdk.v1`
  Connect/JSON protocol. There is no ACP fallback.
- Trouve keeps at most one reusable Bridge process per thread. It is tied to
  that thread's worktree and durable state directory and is never shared with
  another thread. A fresh callback server, bearer, effective tool catalog, and
  internal MCP ticket are registered for each turn.
- After a successful run, Trouve closes the SDK agent and clears the callback
  registration before keeping the Bridge warm. Cancellation, consumer loss,
  protocol failure, child exit, or ambiguous cleanup quarantines the process;
  it is terminated and reaped instead of reused.
- Each configured backend retains at most three idle Bridges, reaps processes
  idle for five minutes, and checks once per minute. The cap remains soft while
  all retained processes are actively borrowed so unrelated threads preserve
  concurrency. Backend reload, runtime update, and uninstall discard the pool.
- Cursor receives only the SDK's `mcp` capability. Trouve projects the
  thread-scoped internal MCP tool catalog into SDK custom-tool definitions and
  proxies callbacks back through that MCP endpoint. Cursor-native filesystem,
  shell, task, web, and editing tools are not exposed; every effect therefore
  remains behind `ToolExecutor` and the per-session execution lane. Callback
  ids are mandatory: an identical retry replays its cached result, while id
  reuse with different input fails closed, so transport retries cannot repeat
  a side effect.
- Cursor SDK user/service API keys are stored through the existing provider
  secret boundary and passed explicitly in agent options. The same configured
  key powers the independent subscription-health exchange. Cursor CLI login
  files are never read.
- The managed `cursor-sdk-bridge` release replaces `cursor-agent` in the
  existing managed-binary lifecycle. The legacy `/v1/clis` routes remain a
  compatibility surface but are presented as managed agent runtimes. New
  Cursor configs use `cursor-sdk`; `cursor-cli` is accepted only as a runtime
  compatibility alias and also selects the SDK adapter. Its obsolete explicit
  `cursor-agent` command is ignored.
- Cursor steering remains disabled until the SDK exposes a supported same-run
  injection operation. Cancellation and later resume are not represented as
  steering.

## Consequences

- Cursor, Claude Code, and Codex retain vendor-owned loops while presenting
  the same Trouve-owned tool, permission, event, and worktree contract.
- Cursor setup requires one API key and one managed Agent SDK runtime; the CLI
  binary and CLI subscription login are no longer dependencies.
- Consecutive turns in one thread avoid Bridge startup while durable state
  preserves cold-process resume after idle eviction, crash, update, or restart.
  Warm processes consume bounded memory between turns, and callback credentials
  retain turn lifetime.
- The Bridge protocol is versioned, but release and callback compatibility
  still require conformance and soak coverage when Cursor publishes updates.
  Subscription health additionally retains its documented risk of using an
  undocumented dashboard RPC.

## Alternatives rejected

- **Keep ACP as an automatic fallback.** This would make authentication,
  confinement, and behavior depend on which binary happened to be installed.
- **Expose Cursor-native tools.** This bypasses `ToolExecutor` and reintroduces
  divergent permission and mutation behavior.
- **Run one permanent Bridge daemon.** Its process-wide callback slot would
  serialize or mix unrelated worktrees, credentials, and tool scopes.
- **Start a fresh Bridge for every turn.** This simplifies cleanup but
  repeatedly pays measurable process and SDK initialization cost.
- **Keep one callback server for the process lifetime.** This widens callback
  bearer and MCP-ticket lifetime beyond the turn that authorized them.
