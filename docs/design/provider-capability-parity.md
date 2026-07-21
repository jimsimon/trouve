# Provider capability parity

ADR 0012 makes provider choice an inference and transport choice, not a
choice of agent product. This document is the implementation contract for
that decision.

## User-visible contract

An **authoritative** provider must present the same resolved Trouve surface
for a given thread, regardless of whether inference is reached through an
API, a local runtime, or a subscription CLI:

- the same slash completion catalog and explicit command dispatch;
- the same skills and current skill contents;
- the same mode, workspace rules, and tool policy;
- the same canonical operations, permission decisions, audit events, tool
  call identifiers, result content, and client rendering;
- the same user-configured MCP tools, reached only through `ToolExecutor`;
- the same Trouve-owned interaction and subagent primitives.

The model-facing spelling of a tool may eventually vary by provider dialect
when evaluations show that a model performs better with familiar names or
argument shapes. A dialect is valid only when it maps losslessly to one
canonical operation before permission checking, execution, event logging,
or rendering.

## Replace, retain, or omit

| Capability | Native API / local | Claude subscription CLI | Codex subscription CLI | Cursor CLI |
| --- | --- | --- | --- | --- |
| Authentication and inference transport | Retain provider | Retain vendor | Retain vendor | Retain vendor |
| Replay and compaction | Trouve loop | Retain vendor; refresh Trouve instructions each turn | Retain vendor; refresh Trouve instructions each turn | Retain vendor; refresh Trouve instructions each turn |
| Mode, rules, and skill catalog | Trouve | Replace with Trouve | Replace with Trouve | Add Trouve; vendor surface may remain |
| Slash completions and explicit skill invocation | Trouve | Replace with Trouve; disable vendor slash commands | Replace with Trouve; isolate vendor home/features | Replace in Trouve UI; vendor agent commands are ignored |
| File, shell, search, web, image, and MCP tools | `ToolExecutor` | Replace through thread-scoped Trouve MCP | Replace through thread-scoped Trouve MCP | Add Trouve MCP; native ACP tools cannot yet be suppressed |
| Permission policy and audit | Trouve | `ToolExecutor` | `ToolExecutor` | Trouve for bridged calls; ACP approval path for remaining native calls |
| Hooks, memories, goals, apps/plugins, browser/computer use | Trouve replacement when one exists; otherwise omit | Omit vendor copies | Disable vendor copies | Vendor copies may remain |
| Subagents | Trouve spawn operations, filtered by mode | Replace vendor `Task` | Disable vendor multi-agent | Vendor agents may coexist |
| Tool events and rendering | Canonical Trouve events | Canonical bridge events; duplicate vendor lifecycle ignored | Canonical bridge events; duplicate vendor lifecycle ignored | Compatibility events normalized to canonical names where possible |

Cursor therefore reports `capability_mode: "compatibility"`. All native
providers report `authoritative`. Claude and Codex report `authoritative`
when their full bridge is enabled (the default), and `compatibility` after
an explicit `tool_bridge = false` opt-out.

## Authoritative CLI requirements

A CLI adapter can enter authoritative mode only when all of these assertions
hold:

1. Trouve's full, mode-filtered tool set is mounted through the internal,
   thread-scoped MCP endpoint.
2. User MCP servers are resolved by Trouve and never mounted directly into
   the vendor process.
3. Vendor execution tools and customizations are disabled or isolated. An
   unsupported suppression option must make the turn fail, not fall back to
   the vendor surface.
4. The vendor process receives current Trouve instructions on every new or
   resumed turn.
5. Vendor command catalogs are ignored; only
   `thread.command_catalog_updated` reaches clients.
6. A newly authoritative configuration uses a fresh vendor-session namespace
   so a context created with native tools is never resumed.
7. Any observed native tool or vendor approval event fails the turn. MCP
   lifecycle echoes are discarded because `ToolExecutor` already emitted the
   canonical card and result.
8. Ambient vendor config, MCP servers, skills, plugins, and hooks cannot enter
   the process except for the minimum credential material needed to use the
   subscription and non-overridable organization-managed policy.

Today Claude satisfies these requirements with no user/project/local settings
sources, a session-level all-hooks disable, explicit
memory/skill/workflow/subagent/connector disables, an empty built-in tool set,
a strict command-line MCP config, and a defensive disallow list. Its monolithic
safe mode is intentionally not used because that also suppresses the Trouve MCP
server. Anthropic-managed organization policy has higher precedence than these
session controls; restrictions remain in force, and deployments with managed
executable hooks cannot claim the literal one-chokepoint guarantee until those
hooks are disabled by the administrator. Codex uses an isolated temporary
`CODEX_HOME`, copies only `auth.json`, starts app-server with strict config,
disables vendor features, and starts authoritative threads with no execution
environments. Both adapters retain a runtime escape detector in the engine.
Claude authoritative turns additionally require a certified CLI version and
probe the required command-line controls before starting; unsupported versions
fail instead of falling back to native tools.

## Capability resolution

For every turn, Trouve resolves capabilities in this order:

1. Load the thread mode and its allowed-tool policy.
2. Discover global and workspace skills, with workspace definitions winning
   by stable skill name.
3. Merge built-in tools and trusted user MCP tools in `LocalToolExecutor`.
4. Add engine-owned interactions (`ask_question`, transcript search, and
   mode-permitted spawn operations).
5. Merge Trouve's typed action commands, the generic `/skill` prompt
   command, and user-invocable direct skill aliases; publish that catalog to
   the thread event log.
6. Send the resulting system context directly to a native provider, or mount
   the resulting operation set through the CLI bridge.
7. Include a revision of the mode-filtered tool schemas in the bridge mount,
   so vendor-side MCP caches are invalidated when policy or user MCP tools
   change.

Skill contents are loaded by name through `load_skill`. Models never receive
host-absolute skill paths, and a symlinked `SKILL.md` that leaves its declared
root is rejected.

## Trouve command catalog

Commands have provider-independent dispatch semantics. `action` commands go
to `POST /v1/threads/{id}/commands`, never enter a model transcript, and
persist their output as `thread.command_executed`. `prompt` commands go
through the normal message endpoint and start a model turn.

The first command wave covers the everyday control surface:

| Command | Dispatch | Purpose |
| --- | --- | --- |
| `/help [command]` | action | Discover the resolved catalog. |
| `/status` | action | Inspect the current provider, model, mode, permissions, and activity. |
| `/skills [name]` | action | List or inspect resolved skills and provenance. |
| `/skill <name> [request]` | prompt | Invoke a skill even when its direct name collides with a core command. |
| `/mode [id]` | action | List, inspect, or change modes. |
| `/model [provider/model]` | action | List, inspect, or change models. |
| `/permissions [ask\|allow-list\|yolo]` | action | Inspect or change permission policy. |
| `/undo`, `/redo` | action | Navigate session checkpoints. |
| `/cancel` | action | Interrupt the active turn. |
| `/new` | action | Create and select a same-session thread. |

The second wave exposes deeper harness state without asking a model to
describe it:

| Command | Purpose |
| --- | --- |
| `/tools` | Resolved, mode-filtered Trouve tool catalog. |
| `/mcp` | MCP servers resolved by Trouve for the session. |
| `/usage` | Accumulated thread token and cost data. |
| `/diff` | Session diff against its base revision. |
| `/files` | Worktree path inventory. |
| `/queue` | Pending thread prompts. |
| `/instructions` | Effective mode, user, workspace, and skill instructions. |
| `/rename <title>` | Rename the current session. |
| `/terminal` | Reveal and attach the integrated terminal. |

Core command names are reserved. A skill with a colliding name remains
available through `/skill <name>` but does not create a duplicate completion.
`/compact` remains deferred because CLI and native providers do not yet share
a useful user-facing compaction contract. A future `/clear` should alias
`/new` rather than introduce a second transcript-reset meaning.

## Native skill inventory and Trouve baseline

Inventory date: 2026-07-20. Vendor catalogs are moving targets; this is a
selection record, not an instruction to mirror every vendor release.

### Codex

Codex uses the Agent Skills `SKILL.md` convention with progressive loading
and explicit or description-based invocation. The system skill pack present
in the inspected Codex 0.142.5 installation was:

- `imagegen`
- `openai-docs`
- `plugin-creator`
- `skill-creator`
- `skill-installer`

See the [official Codex skills documentation](https://learn.chatgpt.com/docs/build-skills).
The exact installed system pack is distribution-dependent, so Trouve must not
treat it as a portable product API.

### Claude Code

Claude Code's published bundled skill catalog (checked alongside local CLI
2.1.201) included:

- `batch`, `claude-api`, `code-review`, `dataviz`, `debug`, `design-sync`
- `doctor`, `fewer-permission-prompts`, `loop`, `run`
- `run-skill-generator`, `security-review`, `simplify`, `verify`

Claude also exposes built-in command workflows such as security review. See
the official [skills](https://code.claude.com/docs/en/slash-commands) and
[interactive commands](https://code.claude.com/docs/en/commands) references.

### Cursor

Cursor supports Agent Skills in both editor and CLI, but publishes no stable
bundled-skill catalog. The inspected cursor-agent
2026.05.24-dda726e discovered `.cursor/skills`, `.cursor/skills-cursor`,
`.cursor/cloud-skills`, and compatible `.agents/skills`, `.claude/skills`, and
`.codex/skills` roots. Its documentation and release notes describe workflows
and default agents rather than a versioned built-in skill API. Trouve therefore
catalogs the standard and discovery behavior, not an inferred community list. See
Cursor's official [Agent Skills announcement](https://cursor.com/changelog/2-4),
[custom commands](https://docs.cursor.com/en/agent/chat/commands), and
[CLI slash commands](https://docs.cursor.com/en/cli/reference/slash-commands).

### Built into Trouve

Trouve ships the common, provider-neutral workflows that its existing tools
and modes can execute consistently:

| Skill | Invocation policy | Reason to include |
| --- | --- | --- |
| `code-review` | automatic + explicit | Common cross-provider read-only review workflow. |
| `security-review` | automatic + explicit | Provider-neutral trust-boundary and exploit review. |
| `debug` | automatic + explicit | Evidence-first diagnosis and regression verification. |
| `simplify` | explicit only | Useful mutating workflow that should never trigger opportunistically. |
| `verify` | automatic + explicit | Makes evidence and unrun checks consistent across models. |
| `skill-creator` | automatic + explicit | Gives Trouve users one canonical way to author portable skills. |

Built-ins are compiled into `trouve-core`; user skills override them by
stable name, and workspace `.agents/skills` override both. Supported front
matter includes `name`, `description`, `argument-hint`,
`disable-model-invocation`, and `user-invocable`. All selected built-ins are
instruction-only so they work with the current canonical tool set.

The following vendor-native categories are deliberately not built in yet:

- vendor documentation assistants (`openai-docs`, `claude-api`), because
  their content and source integrations need independent update ownership;
- plugin installers/creators, because Trouve does not yet have a canonical
  plugin distribution contract;
- image generation, design synchronization, and data visualization, because
  no provider-neutral generation/design tool is currently guaranteed;
- `batch`, `run`, cloud execution, and generated-run workflows, until Trouve
  owns their lifecycle and resource semantics;
- `doctor` and permission-prompt reduction, which should be deterministic
  status/configuration features rather than model-authored skills;
- `loop`, which should map to Trouve automations/goals instead of creating a
  second model-controlled scheduler.

## Compatibility exit criteria

Cursor can become authoritative when its ACP implementation provides a
negotiated, testable way to suppress every native execution capability and
vendor customization while retaining HTTP MCP. At that point it must pass
the same escape, direct-MCP, command-catalog, permission, and event-parity
tests as Claude and Codex before its protocol metadata changes.

## Model-quality policy

Operational parity is not enough if a model stops using tools reliably.
Authoritative adapters must be evaluated on the same task corpus as their
native vendor harness. Track tool-selection accuracy, malformed arguments,
retries, task completion, latency, and token use. Introduce a provider
dialect only for a measured regression; keep canonical execution and events
unchanged. A compatibility opt-out remains available while a regression is
being investigated, but clients must label it as a mixed surface.
