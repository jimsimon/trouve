# Installing trouve into your coding agent

Every integration needs the `trouve` binary on `PATH`:

```bash
cargo install trouve
# or download a release binary from GitHub Releases
```

There are three ways to wire trouve into an agent. Pick **one per agent** —
they expose the same tools, so combining them shows the model duplicates.

1. [Plugin](#1-plugin) — OpenCode, Kilo Code, Claude Code, Codex.
2. [Native tool file](#2-native-tool-file-opencode) — OpenCode, without a
   server process.
3. [MCP server entry](#3-mcp-server-entry) — everything else (Cursor,
   Gemini CLI, Copilot, VS Code, Windsurf, Zed, …).

The CLI itself needs no setup and always works as a fallback (`trouve
search "query" ./repo`); every route shares the same on-disk index store,
so mixing CLI use with any of the above costs nothing.

## 1. Plugin

Plugins are versioned in lockstep with the crate, install/uninstall as one
unit, and are the only routes with session-start index warming. Details and
options are in the [plugin README](plugins/trouve/README.md).

- **[OpenCode](https://opencode.ai)** — add to your opencode config:

  ```json
  { "plugin": ["trouve-plugin"] }
  ```

- **[Kilo Code](https://kilo.ai)** — `kilo plugin trouve-plugin --global`
  (Kilo CLI and VS Code extension).
- **[Claude Code](https://code.claude.com)** —
  `/plugin marketplace add jimsimon/trouve` then
  `/plugin install trouve@trouve`.
- **[Codex](https://developers.openai.com/codex)** —
  `codex plugin marketplace add 'https://github.com/jimsimon/trouve.git' --ref main`
  then `codex plugin add trouve@trouve`.

## 2. Native tool file (OpenCode)

A single custom-tool file exposing `trouve_search` and `trouve_find_related`
as native OpenCode tools — no MCP server process, no JSON config edits.
Unlike the OpenCode plugin it runs one CLI process per call (no in-session
remote-index cache and no session-start warming), but it also needs no npm
package.

Copy [`src/agents/opencode-tool.ts`](src/agents/opencode-tool.ts) to
`~/.config/opencode/tools/trouve.ts`:

```bash
mkdir -p ~/.config/opencode/tools
curl -fsSL https://raw.githubusercontent.com/jimsimon/trouve/main/src/agents/opencode-tool.ts \
  -o ~/.config/opencode/tools/trouve.ts
```

The filename prefixes the exports, so the tools surface as `trouve_search`
and `trouve_find_related`. To uninstall, delete the file.

## 3. MCP server entry

`trouve` with no subcommand runs an MCP stdio server exposing two tools,
`search` and `find_related` (most harnesses prefix them as
`mcp__trouve__search` / `mcp__trouve__find_related`). The generic shape,
whatever your agent's config syntax:

- command: `trouve`
- args: none (or `--content all` to index docs and config too)
- transport: stdio

Per-agent config locations and snippets (all paths relative to `~`):

### Claude Code — `~/.claude.json`

```bash
claude mcp add --scope user trouve -- trouve
```

or add under `"mcpServers"`:

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

### Cursor — `~/.cursor/mcp.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

### Gemini CLI — `~/.gemini/settings.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

### Kiro — `~/.kiro/settings/mcp.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

### OpenCode — `~/.config/opencode/opencode.json` (or `.jsonc`)

```json
{ "mcp": { "trouve": { "command": ["trouve"], "type": "local", "enabled": true } } }
```

(Prefer the [plugin](#1-plugin) or the [tool file](#2-native-tool-file-opencode)
for OpenCode; use only one.)

### GitHub Copilot — `~/.copilot/mcp-config.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [] } } }
```

### Codex — `~/.codex/config.toml`

```toml
[mcp_servers.trouve]
command = "trouve"
args = []
```

### VS Code — user `mcp.json`

`~/.config/Code/User/mcp.json` on Linux,
`~/Library/Application Support/Code/User/mcp.json` on macOS,
`%APPDATA%\Code\User\mcp.json` on Windows. Note the key is `servers`:

```json
{ "servers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

### Windsurf — `~/.codeium/windsurf/mcp_config.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [] } } }
```

### Zed — `~/.config/zed/settings.json`

```json
{ "context_servers": { "trouve": { "source": "custom", "command": "trouve", "args": [] } } }
```

### Reasonix — `~/.reasonix/config.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [] } } }
```

### Pi — `~/.pi/agent/mcp.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [] } } }
```

### Command Code — `~/.commandcode/mcp.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [] } } }
```

### Antigravity — `~/.gemini/config/mcp_config.json`

```json
{ "mcpServers": { "trouve": { "command": "trouve", "args": [], "type": "stdio" } } }
```

To uninstall, remove the `trouve` entry from the same file.

## Optional: sub-agent definitions

Ready-made `trouve-search` sub-agent files (search specialists that use the
trouve CLI via the agent's shell tool) live in
[`src/agents/`](src/agents/). Copy the one matching your agent to where it
loads sub-agents from:

| Agent | Source file | Destination |
| --- | --- | --- |
| Claude Code | `claude.md` | `~/.claude/agents/trouve-search.md` |
| Cursor | `cursor.md` | `~/.cursor/agents/trouve-search.md` |
| Gemini CLI | `gemini.md` | `~/.gemini/agents/trouve-search.md` |
| Kiro | `kiro.md` | `~/.kiro/agents/trouve-search.md` |
| OpenCode | `opencode.md` | `~/.config/opencode/agents/trouve-search.md` |
| GitHub Copilot | `copilot.md` | `~/.copilot/agents/trouve-search.agent.md` |
| Codex | `codex.toml` | `~/.codex/agents/trouve-search.toml` |
| Reasonix | `reasonix.md` | `~/.reasonix/skills/trouve-search.md` |
| Pi | `pi.md` | `~/.pi/agents/trouve-search.md` |
| Command Code | `commandcode.md` | `~/.commandcode/agents/trouve-search.md` |
| Antigravity | `antigravity.md` | `~/.gemini/config/skills/trouve-search/SKILL.md` |

To steer the main agent (not just a sub-agent) toward trouve, you can also
add a short note to its instructions file (`~/.claude/CLAUDE.md`,
`~/.gemini/GEMINI.md`, `~/.config/opencode/AGENTS.md`, …) along the lines
of: *"Use the trouve `search` tool to find code by intent instead of
grep; navigate directly to the returned file and line."*
