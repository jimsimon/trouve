# Agent backend conformance and transport qualification

Status: active contract for shipping transports and replacement candidates.

## Product contract

Threads have one Trouve experience regardless of the selected model. The
frontend consumes the same durable event log, permission flow, worktree,
attachments, commands, todos, usage, cancellation, and resume semantics for
every model. Transport ownership is an internal implementation detail and is
not exposed as a user-facing mode or badge.

There are two internal execution shapes:

- API providers stream model items into Trouve's agent loop. Trouve supplies
  the prompt, tools, iteration, context replay, and `ToolExecutor` calls.
- Vendor-agent adapters ask the vendor harness to run a turn, then normalize
  its sanctioned protocol into `BackendEvent`. Mutation-capable tools still
  cross Trouve's `ToolExecutor` boundary, normally through the full MCP bridge.

Both shapes converge on the protocol event taxonomy before any client sees
them. They do not need identical private control flow; they must have the same
observable behavior and safety invariants.

## Conformance layers

| Layer | Purpose | Automated evidence |
| --- | --- | --- |
| Provider transport | Preserve native request/stream semantics without changing the agent loop | Provider unit tests, including typed tool calls, reasoning replay, usage, and truncated streams |
| Vendor translation | Convert each sanctioned vendor protocol to the complete `BackendEvent` vocabulary | `crates/trouve-agents/tests/adapters.rs` stub-process tests |
| Cross-path behavior | Ensure API-loop and vendor-agent turns fold to the same visible thread result | `crates/trouve-server/tests/backend_conformance.rs` |
| Engine safety | Keep permissions, approvals, cancellation, worktree serialization, resume, and checkpoints authoritative | `crates/trouve-server/tests/e2e_api.rs` and core tests |
| Live qualification | Detect vendor protocol/auth/billing changes that fixtures cannot prove | Manually gated matrix below, run against the exact candidate release before rollout |

The cross-path suite compares folded protocol behavior, not adapter-specific
event boundaries. For example, a provider may stream five text deltas while a
vendor emits one; the final assistant content and lifecycle boundaries must
still agree.

## Required behavior matrix

A shipping adapter, or a replacement transport for one, must pass every
applicable row.

| Area | Required result |
| --- | --- |
| Instructions and modes | The current data-driven persona reaches the model; plan/review turns remain read-only |
| Text and reasoning | Streaming text, reasoning summaries, and completion boundaries fold without duplicates or stranded activity |
| Tools | The effective Trouve tool schema is available and every side effect reaches `ToolExecutor` |
| Permissions | read-only denies mutation; ask/allow-list produce one Trouve approval; yolo does not prompt |
| External MCP | User/workspace/worktree servers preserve merge order, environment expansion, and first-use policy |
| Interaction | questions, commands/skills, todos, attachments, steering, and subagents degrade only through an explicit internal capability check |
| Sessions | a second turn resumes; model A → B → A resumes A; cold process restart does not lose instructions |
| Cancellation | cancel is acknowledged, pending tools terminate, and a replacement turn cannot overlap stale vendor work |
| Failure | malformed frames, process exit, timeout, and partial streams end the turn once and leave no pending UI state |
| Usage | token/context and subscription-health data use the common protocol projection |
| Durability | events, checkpoints, diffs, and worktree mutations remain reconstructable from Trouve's sources of truth |

## Cursor SDK Bridge qualification

The shipping Cursor integration uses the published Agent SDK Bridge. It
provides a versioned `sdk.v1` Connect/protobuf contract, resumable agents,
streaming, cancellation, explicit built-in tool selection, and host-owned
custom-tool callbacks.

Run the live baseline from the repository root with a Cursor user or service
API key in the environment:

```sh
CURSOR_API_KEY=... node scripts/qualify_cursor_sdk_bridge.mjs
```

Exercise the complete shipping path—including managed runtime installation,
the secured Trouve server, a real session worktree, approval-gated
`ToolExecutor` callbacks, warm-process reuse, the durable thread view,
uninstall, and credential-persistence checks—with:

```sh
TROUVE_E2E=1 CURSOR_API_KEY=... \
  cargo test -p trouve-server --test cursor_sdk_live \
    cursor_sdk_shipping_path_installs_tools_resumes_and_cleans_up -- \
    --ignored --exact --nocapture
```

Run the broader promotion probe only when several paid turns are acceptable:

```sh
CURSOR_API_KEY=... node scripts/qualify_cursor_sdk_bridge_full.mjs
```

Run only the direct, non-billable subscription-health qualification (no Bridge
process) with:

```sh
CURSOR_API_KEY=... node scripts/qualify_cursor_sdk_bridge_full.mjs --health-only
```

No mode prints the key. `--health-only` performs only the direct HTTPS
exchange/dashboard sequence and starts no Bridge process. The
Bridge probes download and SHA-256-verify the pinned standalone release (or
accept `--bridge PATH`), use an isolated temporary state store, remove ambient
setting sources, and confine built-ins to the required MCP capability group.
The baseline performs two paid SDK turns; the broader probe performs several.
Both fail before downloading anything when `CURSOR_API_KEY` is absent. The SDK
deliberately uses API-key authentication rather than a separate CLI login.
Cursor's native sandbox is disabled in this baseline because the standalone
runtime does not support it on every host. Confinement instead follows the
pinned SDK's published `AgentOptions.tools` allow-list contract. Trouve reviews
the complete public `ToolName` vocabulary for v1.0.28, sends only `mcp`, and
explicitly denies every other known native tool as defense in depth. Before any
paid turn, each probe first creates and closes (without running) an agent that
selects the real native `shell` tool, proving that identifier is recognized by
the exact Bridge release. It then submits an unknown built-in name and requires
a `ConnectRpcError` whose structured code is `invalid_argument` and whose
detail names that exact probe; a generic transport or authentication failure
cannot pass. The shipping agent is created only with `mcp` allowed and with
`shell` plus every other known native tool explicitly denied. Stream tool
telemetry is corroborating evidence only:
when present it must contain no filesystem, shell, task, or other native
capability. Cursor may label a custom callback as either the generic `mcp`
capability or its exact custom-tool name; the probe accepts only those exact
spellings, correlates the call id with `CallCustomTool`, and rejects every
additional call id or tool name. Model compliance with a prompt is never
treated as the sole confinement evidence. Each paid baseline turn and the
first full-qualification turn also ask for the recognized native `shell` tool
while the exact shipping options are active; the stream must still contain
only the requested host callback. That negative exercise corroborates the
pinned allow-list contract and its deterministic validation.

Qualification is gated in this order:

1. **Authentication and billing.** Prove the supported user API-key flow is an
   acceptable subscription onboarding experience and is charged to the same
   request pools users expect.
2. **Tool confinement.** Run with built-ins removed and expose Trouve tools
   through SDK custom-tool callbacks. Verify all matrix permission modes and
   image/tool-result shapes.
3. **Lifecycle fidelity.** Verify resume, model options, steering, cancellation,
   process cleanup, usage, and subscription-health equivalents.
4. **Operational cost.** Track startup latency, idle memory, binary update
   policy, protocol compatibility, and failure recovery across Bridge releases.

**Current local evidence (2026-08-24): full transport qualification passed.**
The pinned v1.0.28 archive downloaded, matched its published
checksum, and reported Bridge version 1.0.0 with protocol `sdk.v1`. Missing and
intentionally invalid credentials failed closed without being printed, while a
real user API key authenticated Composer 2 against a 36-model catalog. The
complete run registered 134 custom tools, including 128 schema stress probes,
without exposing the explicitly requested native shell capability.

Seven paid turns proved host allow and deny/error results, input images,
text/structured/image tool results, two genuinely concurrent read callbacks,
cancellation followed by recovery, and a cold Bridge-process resume. The
Bridge accepted a per-send plan-mode request and returned a tool-free plan, but
SDK v1 does not echo the effective mode in run or stream results; qualification
records that limitation instead of treating model behavior as proof of mode.
The report places it under `non_gating_observations` with a `not-attested`
status; it is not a certified SDK capability or a promotion claim. Read-only
safety remains independently enforced by the MCP-only `ToolExecutor` boundary.
Every callback id correlated with the generic `mcp` stream event, all seven
turns reported token usage, and the final run had complete terminal tool
events. Durable `ObserveRun` replay returned opaque offsets and resumed
exclusively after an offset. `GetRun`, `ListRuns`, `GetRunConversation`, and
`ListAgentMessages` also passed. A preceding attempt stopped after one custom
tool lacked a terminal stream event; the clean rerun makes that an intermittent
reliability signal to cover in soak testing rather than a deterministic
contract failure. Isolated Bridge state was removed after each attempt.

One-host measurements were: 115,102,950-byte Bridge binary, 301 ms ready time,
about 144 MB ready RSS, about 265 MB warm RSS, and 308 ms cold restart. These
are qualification observations, not benchmark claims.

Subscription health was separately qualified without the CLI. Sending the raw
user API key directly to `DashboardService` correctly failed with 401; Cursor's
own SDK v1.0.28 showed the required preceding
`POST /auth/exchange_user_api_key` step. The live API-key exchange returned
200, after which `GetCurrentPeriodUsage` and `GetPlanInfo` both returned 200
with the billing cycle, all three plan meters, spend-limit data, and plan name.
Trouve now performs that sequence directly, keeps the exchanged access token
only in memory for the query, and never invokes `cursor-agent` or reads its
credential files.

The same date's shipping-path qualification passed two paid Composer 2.5 turns
through the production Rust adapter. It verified one approval-gated
`write_file` and one `read_file` callback exactly once, worktree confinement,
token usage, durable tool cards, and reuse of both the SDK agent and one warm
Bridge process. The second callback reached a newly registered turn-scoped
server and MCP ticket. The test observed the same private Bridge runtime
directory after both turns, then verified that uninstall removed it. It also
installed and uninstalled the managed runtime through the public HTTP API,
returned live subscription health, and found no API-key bytes under Trouve's
test data directory. The first attempt exposed an adapter bug: the documented
Bridge handshake puts its bearer-token file in the OS temporary directory
rather than the durable state root. The adapter now redirects each child
process's temporary directory to a private, auto-cleaned directory and keeps
fail-closed path validation there; the clean reruns passed.

**Decision: use the SDK Bridge adapter and retire the Cursor CLI transport.**
Cursor steering remains disabled through the existing per-backend
capability; Codex and capable providers keep their steering behavior. The lack
of Cursor's native sandbox is accepted because the explicit MCP-only allow-list
with host-owned callbacks is the confinement boundary. API-key onboarding is
the supported SDK flow and uses the same account request pools. Local
`GetUsage` availability is not a promotion blocker: stream events provide
per-turn token usage, while the direct exchange/dashboard path supplies
provider-wide subscription windows.

The fixture suite, permission/approval integration, and repeated live
qualification remain required whenever the pinned Bridge release changes. See
ADR [0043](../adr/0043-cursor-sdk-bridge-transport.md), plus the official
[Cursor SDK Bridge contract](https://cursor.com/docs/sdk/bridge).

## Rollout rule

Transport selection stays inside provider/backend construction. No protocol
field, settings badge, or thread mode tells users which loop or bridge ran.
Rollout uses an internal guarded configuration, conformance comparison, and
automatic fallback where safe; after qualification, the better transport can
replace the old one without changing the thread experience.
