# trouve-plugin

One plugin package exposing [trouve](https://github.com/jimsimon/trouve)
code search across four agent harnesses:

- **OpenCode** and **Kilo Code** — native `trouve_search` and
  `trouve_find_related` tools backed by a persistent trouve server process
  (npm package `trouve-plugin`).
- **Claude Code** — MCP server + `trouve-search` sub-agent + workflow skill
  (this directory doubles as the Claude plugin bundle).
- **Codex** — MCP server + workflow skill (this directory also carries the
  Codex plugin manifest).

All routes require the `trouve` binary on PATH: `cargo install trouve`, or
download a [release binary](https://github.com/jimsimon/trouve/releases).

## OpenCode

Add to `~/.config/opencode/opencode.json` (or a per-project config):

```json
{ "plugin": ["trouve-plugin"] }
```

## Kilo Code

```bash
kilo plugin trouve-plugin --global
```

or add `{ "plugin": ["trouve-plugin"] }` to your Kilo config. Works in both
the Kilo CLI and the VS Code extension.

For OpenCode and Kilo, the plugin keeps one `trouve` server process alive
for the whole session and speaks its JSON-RPC stdio protocol directly, so
repeat queries — including against remote git URLs — reuse the server's
in-process index cache. It also warms the project index in the background
at session start and (throttled) after each idle turn, so the first search
never pays the index build and later searches see the agent's own edits.
To adjust, pass options:

```json
{ "plugin": [["trouve-plugin", { "content": "all", "warm": true }]] }
```

`content` accepts `"code"` (default), `"docs"`, `"config"`, `"all"`, or an
array of those. Set `"warm": false` to disable background index warming.

## Claude Code

```text
/plugin marketplace add jimsimon/trouve
/plugin install trouve@trouve
```

Installs the trouve MCP server (tools surface as `mcp__trouve__search` and
`mcp__trouve__find_related` in Claude Code), the
`trouve-search` sub-agent, the workflow skill, and a `SessionStart` hook
that warms the project index in the background so the first search of a
session is instant (POSIX shells; on Windows the first search builds the
index instead).

## Codex

```bash
codex plugin marketplace add 'https://github.com/jimsimon/trouve.git' --ref main
codex plugin add trouve@trouve
```

Installs the trouve MCP server (tools surface as `mcp__trouve__search` and
`mcp__trouve__find_related`) and the workflow skill.

## Tools (OpenCode / Kilo Code)

- `trouve_search` — search with a natural-language or code query. Arguments:
  `query` (required), `repo` (defaults to the project root; local path or
  https:// git URL), `top_k` (default 5), `max_snippet_lines` (default 10).
- `trouve_find_related` — find code similar to a `file_path` + `line` from a
  prior search result. Same optional arguments.

## Alternative: trouve install

`trouve install` configures agents directly (MCP entries, instruction
blocks, sub-agents) and covers many more harnesses than this plugin. Use
either the plugin or the installer per agent, not both — otherwise the
model sees duplicate trouve tools. The
[Agent integrations](../../README.md#agent-integrations) section of the
main README has a feature grid comparing every route.

## Development

```bash
npm install
npm run typecheck
```

The version in `package.json` and both plugin manifests is kept in lockstep
with the trouve crate version (`scripts/sync_versions.py`, enforced in CI).

## License

MIT, same as trouve.
