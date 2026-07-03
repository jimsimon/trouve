# opencode-trouve

[OpenCode](https://opencode.ai) plugin exposing [trouve](https://github.com/jimsimon/trouve)
code search as native tools: `trouve_search` and `trouve_find_related`.

The plugin keeps one `trouve` server process alive for the whole OpenCode
session and speaks its JSON-RPC stdio protocol directly, so repeat queries —
including against remote git URLs — reuse the server's in-process index
cache. No MCP configuration is needed.

## Install

1. Install the `trouve` binary (`cargo install trouve`, or download a
   [release binary](https://github.com/jimsimon/trouve/releases)).
2. Add the plugin to your OpenCode config (`~/.config/opencode/opencode.json`
   or per-project `opencode.json`):

```json
{
  "plugin": ["opencode-trouve"]
}
```

To index more than code, pass options:

```json
{
  "plugin": [["opencode-trouve", { "content": "all" }]]
}
```

`content` accepts `"code"` (default), `"docs"`, `"config"`, `"all"`, or an
array of those.

## Alternative: standalone tool file

If you prefer zero plugin configuration, `trouve install` writes a
self-contained custom-tool file to `~/.config/opencode/tools/trouve.ts`
that shells out to the CLI per call. Use one or the other, not both —
otherwise the model sees duplicate trouve tools.

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
