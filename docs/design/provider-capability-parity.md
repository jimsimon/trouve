# Provider capability consistency

ADR 0043 makes provider choice an inference and transport choice, not a
choice of agent product. This document is the implementation contract for
that decision: execution dialects may differ, but the visible harness does
not.

## User-visible contract

Every provider presents the same resolved Trouve surface for a given thread,
regardless of whether inference is reached through an API, a local runtime,
or a subscription CLI:

- the same slash completion catalog and explicit command dispatch;
- the same skills and current skill contents;
- the same mode, workspace rules, and tool policy;
- the same canonical operation families, permission UI, audit lifecycle,
  result presentation, and client rendering;
- the same user-configured MCP tools, reached only through `ToolExecutor`;
- the same Trouve-owned interaction and subagent primitives.

The model-facing spelling and raw result of a core tool may vary by provider.
Subscription models retain the native dialect their harness trains and
optimizes. The adapter must map that lifecycle to a canonical Trouve
operation before it reaches clients.

## Replace, retain, or omit

| Capability | Native API / local | Claude subscription CLI | Codex subscription CLI | Cursor CLI |
| --- | --- | --- | --- | --- |
| Authentication and inference transport | Retain provider | Retain vendor | Retain vendor | Retain vendor |
| Replay and compaction | Trouve loop | Retain vendor; refresh Trouve instructions each turn | Retain vendor; refresh Trouve instructions each turn | Retain vendor; refresh Trouve instructions each turn |
| Mode, rules, and skill catalog | Trouve | Trouve; vendor sources isolated | Trouve; isolated vendor home | Trouve catalog; vendor catalog events ignored |
| Slash completions and explicit skill invocation | Trouve | Trouve; vendor slash commands disabled | Trouve; vendor plugins/skills absent from isolated home | Trouve UI and dispatcher |
| File, shell, lexical search, web, and image tools | `ToolExecutor` | Claude native tools | Codex native tools | Cursor native ACP tools |
| Semantic search, skills, questions, todos, transcript search, and subagents | Trouve loop | Supplemental Trouve MCP | Supplemental Trouve MCP | Supplemental Trouve MCP |
| User MCP | `ToolExecutor` | Trouve bridge → `ToolExecutor` | Trouve bridge → `ToolExecutor` | Trouve bridge → `ToolExecutor` |
| Permission policy and audit | Trouve | Vendor approval transport → Trouve; bridged tools gate in `ToolExecutor` | App-server approval RPC → Trouve; bridged tools gate in `ToolExecutor` | ACP approval RPC → Trouve; bridged tools gate in `ToolExecutor` |
| Hooks, memories, goals, apps/plugins, vendor agents | Not provider-defined | Disabled or isolated | Disabled or isolated | Never published as Trouve capabilities |
| Tool events and rendering | Canonical Trouve events | Native lifecycle normalized; bridge echoes ignored | Native lifecycle normalized; bridge echoes ignored | Native lifecycle normalized; bridge echoes ignored |

There is no strict/full-replacement switch and no authoritative versus
compatibility label. All subscription adapters follow the optimized-native
contract.

## Optimized CLI requirements

A CLI adapter is supported only when all of these assertions hold:

1. The vendor's core execution tools remain available in their native
   model-facing dialect.
2. Trouve's mode-filtered supplemental capabilities are mounted through the
   internal, thread-scoped MCP endpoint.
3. User MCP servers are resolved by Trouve and never mounted directly into
   the vendor process.
4. Ambient vendor commands, skills, plugins, hooks, MCP servers, memories,
   and agent workflows are isolated or disabled wherever the vendor exposes
   a supported control.
5. The vendor process receives current Trouve instructions on every new or
   resumed turn. Vendor command catalogs are ignored.
6. Native tool starts, output, completion, and approval requests map to one
   canonical Trouve call lifecycle. The bridge's vendor-side echo is ignored
   because `ToolExecutor` already emitted that lifecycle.
7. Native writes outside the session worktree are denied before approval
   wherever the vendor protocol exposes their target.
8. The adapter has conformance fixtures for its supported native tool shapes,
   approval protocol, isolated product surface, and supplemental bridge.

Claude uses no ambient settings sources, disables vendor slash
commands/skills/workflows/agents/connectors/hooks, explicitly selects its
native file/shell/search/web tools, and mounts only Trouve's strict
command-line MCP configuration. The required controls are version-probed
before a turn starts. Codex runs from an isolated temporary `CODEX_HOME`
containing only `auth.json`, starts app-server with strict config, disables
apps/plugins/hooks/memories/multi-agent features, and retains its native
execution environment. Cursor keeps its ACP-native execution and permission
transport; catalog events from Cursor do not reach clients.

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
   the supplemental subset through the CLI bridge while the CLI retains its
   core execution tools.
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

The built-in layer is enabled by default. Settings → Skills controls the
global `builtin_skills_enabled` option. Disabling it removes compiled-in
skills from every provider's model prompt, slash catalog, explicit invocation,
and `load_skill` lookup while preserving user and workspace skills. Existing
thread catalogs are republished immediately; turns already in progress may
retain built-in instructions injected when they started.

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

## Model-quality policy

Subscription adapters deliberately preserve the vendor-native core tool
dialect because model training and harness optimizations are part of tool
quality. Evaluate upgrades on the same task corpus and track tool-selection
accuracy, malformed arguments, retries, task completion, latency, and token
use. A vendor schema change must update its canonical event fixtures before
release. Trouve does not offer a full-replacement fallback; a broken adapter
fails its conformance checks instead of silently changing the product
surface.
