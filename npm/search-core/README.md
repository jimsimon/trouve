# @trouve-ai/search-core

Native `trouve-search` binary distribution and MCP launcher for Node.

```bash
npm i -g @trouve-ai/search-core
trouve-search search "auth flow" ./repo
npx -y @trouve-ai/search-core   # MCP stdio server
```

Platform-specific binaries (`@trouve-ai/search-linux-x64-gnu`, etc.) install
automatically via optional dependencies. Requires Node 18+ (Bun also works).

Exports `resolveBinaryPath()` for other packages (e.g. the harness plugin).
