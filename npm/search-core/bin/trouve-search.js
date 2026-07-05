#!/usr/bin/env node
// Launch the native trouve-search binary (MCP stdio server by default).
import { spawnSync } from "node:child_process"
import { resolveBinaryPath } from "../src/platform.js"

const bin = resolveBinaryPath()
const result = spawnSync(bin, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env,
})
if (result.error) {
  const msg = result.error.message
  console.error(
    /ENOENT|executable|not.*found/i.test(msg)
      ? "trouve-search: binary not found. Reinstall @trouve-ai/search-core or install " +
          "trouve-search with `cargo install trouve-search`."
      : `trouve-search: ${msg}`,
  )
  process.exit(1)
}
process.exit(result.status ?? 1)
