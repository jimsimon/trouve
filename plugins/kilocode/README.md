# kilocode-trouve

[Kilo Code](https://kilo.ai) plugin exposing
[trouve](https://github.com/jimsimon/trouve) code search as native tools:
`trouve_search` and `trouve_find_related`. Works in both the Kilo CLI and
the VS Code extension.

The plugin keeps one `trouve` server process alive for the whole Kilo
session and speaks its JSON-RPC stdio protocol directly, so repeat queries —
including against remote git URLs — reuse the server's in-process index
cache. No MCP configuration is needed.

## Install

1. Install the `trouve` binary (`cargo install trouve`, or download a
   [release binary](https://github.com/jimsimon/trouve/releases)).
2. Install the plugin:

```bash
kilo plugin kilocode-trouve --global
```

Or add it to your Kilo config (`~/.config/kilo/kilo.json` or a per-project
`kilo.json`) yourself:

```json
{
  "plugin": ["kilocode-trouve"]
}
```

To index more than code, pass options:

```json
{
  "plugin": [["kilocode-trouve", { "content": "all" }]]
}
```

`content` accepts `"code"` (default), `"docs"`, `"config"`, `"all"`, or an
array of those.

## Tools

- `trouve_search` — search with a natural-language or code query. Arguments:
  `query` (required), `repo` (defaults to the project root; local path or
  https:// git URL), `top_k` (default 5), `max_snippet_lines` (default 10).
- `trouve_find_related` — find code similar to a `file_path` + `line` from a
  prior search result. Same optional arguments.

## Development

```bash
npm install
npm run typecheck
```

## License

MIT, same as trouve.
