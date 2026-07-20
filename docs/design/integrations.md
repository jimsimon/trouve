# Integrations: MCP, skills, GitHub

Status: Phase 5 (first slice implemented; follow-ups tracked below).

## MCP servers

External tools reach the agent through MCP (Model Context Protocol)
servers, spoken over stdio JSON-RPC by `trouve-core/src/mcp.rs`.

### Configuration

Standard `mcpServers` shape, discovered from three layers (later wins on
name collision):

- `<config>/mcp.json` — app-wide servers, applied to every workspace
  (labeled "app-wide" in the UI; scope string `user` on the wire)
- `<workspace>/.agents/.mcp.json` — per-repo servers (committed, dogfooded)
- `<session worktree>/.agents/.mcp.json` — the branch's own file, so a
  session sees whatever its checked-out branch declares

```json
{
  "mcpServers": {
    "jira": {
      "command": "jira-mcp",
      "args": ["--stdio"],
      "env": { "JIRA_TOKEN": "${JIRA_TOKEN}" }
    }
  }
}
```

`${VAR}` references in `env` values expand from the process environment at
spawn time, so secrets never live in the committed file.

A higher layer can *remove* an inherited server with a tombstone entry —
`"jira": { "disabled": true }` — since the merge is additive by name and
omitting a server does not unconfigure it.

The app's right panel has an "MCP" tab showing the effective merged config
for the open session (`GET /v1/sessions/{id}/mcp-servers`): each entry is
tagged with the layer whose definition won (app-wide / workspace / branch),
and tombstoned entries stay listed, dimmed, with the disabling layer named.

### Execution model

- Discovered tools surface as `mcp__<server>__<tool>` through the normal
  `ToolExecutor` chokepoint (invariant 3) — the agent loop cannot tell an
  MCP tool from a built-in.
- Connections are lazy (first `specs()`/call per worktree+server) and
  reused; a failing server logs a warning and is skipped rather than
  blocking turns.
- The transport implements exactly `initialize`, `tools/list`, and
  `tools/call`. The `rmcp` SDK can replace it behind the same `McpManager`
  surface if resources/prompts/sampling are needed later.
- Vendor agent backends get the same servers: each backend turn carries the
  merged, env-expanded list (`BackendTurn.mcp_servers`), handed to Claude
  via `--mcp-config`, to Codex via per-thread `config.mcp_servers`
  overrides, and to Cursor via ACP `mcpServers` on `session/new` /
  `session/load`. The name `trouve` is reserved for the tool bridge.

### First-use approval

MCP tools are always treated as mutating, and the permission layer requires
approval for the first use of each server per session in non-read-only ask and
allow-list modes (`allow_key` = `mcp:<server>`; a plain Approve unlocks the
server for the rest of the session). Read-only modes never reach approval:
the always-mutating classification hits the read-only denial first, so MCP
calls are denied outright. Yolo skips all approval prompts, including MCP.
Rationale for the ask/allow-list gate: MCP servers are external code and a
prompt-injection channel — see the risk register in the plan.

## Skills

A skill is a directory with a `SKILL.md` (optional `---` front matter with
`name:` / `description:`). Discovery (`trouve-core/src/skills.rs`):

- `<config>/skills/*/SKILL.md` — user-global
- `<workspace>/.agents/skills/*/SKILL.md` — per-repo (wins on collision)

Skills are advertised in the system prompt as a name + description + path
list; the agent reads the SKILL.md with its normal `read_file` tool when
relevant. Skill bodies are never inlined into the prompt, so many skills
cost almost nothing. This repo dogfoods the mechanism (`.agents/skills/`).

## GitHub PRs

`trouve-core/src/github.rs` (octocrab) + protocol endpoints:

- `GET  /v1/sessions/{id}/pr` — the open PR for the session branch, with
  check runs and review states attached
- `POST /v1/sessions/{id}/pr` — push the session branch to `origin` and
  open a PR (title/body/base/draft)
- `POST /v1/sessions/{id}/pr/merge` — merge with `merge` / `squash` /
  `rebase`

Repo identity comes from parsing the worktree's `origin` remote; the token
comes from `GITHUB_TOKEN`/`GH_TOKEN` or the secret store (`trouve auth
set-key github`). The desktop app exposes create/status on the Diff tab.

### Follow-ups (tracked, not yet implemented)

- Review threads (read + reply) and requesting reviewers
- Merge queues and review-thread resolution via the GraphQL API
- PR comments flowing back into a session thread as context
- GitLab with the same protocol surface
- JIRA / Slack / Discord / wikis via MCP servers (config mechanism already
  works; needs curated server configs + docs)
