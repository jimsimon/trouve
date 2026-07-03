# trouve Claude Code plugin

Packages [trouve](https://github.com/jimsimon/trouve) code search for
[Claude Code](https://code.claude.com): the trouve MCP server (tools
`search` and `find_related`), a `trouve-search` sub-agent, and a workflow
skill — installable as one unit.

## Install

1. Install the `trouve` binary (`cargo install trouve`, or download a
   [release binary](https://github.com/jimsimon/trouve/releases)).
2. In Claude Code:

```
/plugin marketplace add jimsimon/trouve
/plugin install trouve@trouve
```

## What you get

- **MCP server** — `search` and `find_related` tools over any local path or
  https:// git URL, with incremental branch- and worktree-aware indexing.
- **Sub-agent** — `trouve-search`, a dedicated code-search agent that uses
  the trouve CLI.
- **Skill** — workflow guidance teaching the agent to search by intent
  first and navigate directly to results instead of grepping.

## Alternative: trouve install

`trouve install` configures the same integrations by writing directly to
`~/.claude/` (MCP entry, CLAUDE.md instructions block, sub-agent file). Use
one route or the other, not both.

## License

MIT, same as trouve.
