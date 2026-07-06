# Integrations: MCP, skills, GitHub

Status: Phase 5 (first slice implemented; follow-ups tracked below).

## MCP servers

External tools reach the agent through MCP (Model Context Protocol)
servers, spoken over stdio JSON-RPC by `trouve-core/src/mcp.rs`.

### Configuration

Standard `mcpServers` shape, discovered from two places (workspace wins on
name collision):

- `<config>/mcp.json` — user-global servers
- `<workspace>/.agents/.mcp.json` — per-repo servers (committed, dogfooded)

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

### First-use approval

MCP tools are always treated as mutating, and the permission layer requires
approval for the first use of each server per session **even in yolo mode**
(`allow_key` = `mcp:<server>`; a plain Approve unlocks the server for the
rest of the session). Rationale: MCP servers are external code and a
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
