# trouve Codex plugin

Packages [trouve](https://github.com/jimsimon/trouve) code search for
[OpenAI Codex](https://developers.openai.com/codex): the trouve MCP server
(tools `search` and `find_related`) plus a `trouve-search` workflow skill,
installable as one unit.

## Install

1. Install the `trouve` binary (`cargo install trouve`, or download a
   [release binary](https://github.com/jimsimon/trouve/releases)).
2. Add this repository as a marketplace source and install:

```bash
codex plugin marketplace add 'https://github.com/jimsimon/trouve.git' --ref main
codex plugin install trouve --source trouve
```

New Codex sessions will load the plugin; tools appear as
`mcp__trouve__search` and `mcp__trouve__find_related`.

## What you get

- **MCP server** — `search` and `find_related` tools over any local path or
  https:// git URL, with incremental branch- and worktree-aware indexing.
- **Skill** — `trouve-search` workflow guidance teaching the agent to
  search by intent first and navigate directly to results instead of
  grepping.

## Alternative: trouve install

`trouve install` configures the same integrations by writing directly to
`~/.codex/` (an `[mcp_servers.trouve]` entry in `config.toml`, an AGENTS.md
instructions block, and a sub-agent file). Use one route or the other, not
both.

## License

MIT, same as trouve.
