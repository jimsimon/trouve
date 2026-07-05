# @trouve-ai/search-plugin

One plugin package for four agent harnesses:

- **OpenCode** and **Kilo Code** — native `trouve_search` and
  `trouve_find_related` tools (npm install).
- **Claude Code** and **Codex** — MCP server, workflow skill, sub-agent
  (Claude), and session-start index warming (git marketplace install).

Depends on **`@trouve-ai/search-core`** for the native binary.

## OpenCode

```json
{ "plugin": ["@trouve-ai/search-plugin"] }
```

## Kilo Code

```bash
kilo plugin @trouve-ai/search-plugin --global
```

Options:

```json
{ "plugin": [["@trouve-ai/search-plugin", { "content": "all", "warm": true }]] }
```

## Claude Code

```text
/plugin marketplace add jimsimon/trouve
/plugin install trouve-search@trouve
```

Installs the trouve-search MCP server (tools surface as
`mcp__trouve-search__search` and `mcp__trouve-search__find_related`), the
`trouve-search` sub-agent, the workflow skill, and a `SessionStart` hook that
warms the project index in the background.

## Codex

```bash
codex plugin marketplace add 'https://github.com/jimsimon/trouve.git' --ref main
codex plugin add trouve-search@trouve
```

## MCP / npx

Run the MCP stdio server without a harness plugin:

```bash
npx -y @trouve-ai/search-core
```

See [INSTALL.md](../../INSTALL.md) for manual MCP setup and the native
OpenCode tool-file alternative.

## Development

The `npm/` directory is an npm workspace, so install from there:

```bash
cd npm
npm install     # links @trouve-ai/search-core from ../search-core
npm run typecheck
```

## License

MIT, same as trouve-search.
