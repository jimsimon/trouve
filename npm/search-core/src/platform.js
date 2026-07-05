// Resolve the native trouve-search binary shipped in a platform package.
// Plain ESM JavaScript so it runs under Node (npx) and Bun alike.
import { execSync } from "node:child_process"
import { existsSync } from "node:fs"
import { createRequire } from "node:module"
import { arch, platform } from "node:os"
import { dirname, join } from "node:path"

const require = createRequire(import.meta.url)

/**
 * npm package name for the current OS/arch/libc.
 * @returns {string}
 */
export function platformPackageName() {
  const os = platform()
  const cpu = arch()
  if (cpu !== "x64" && cpu !== "arm64") {
    throw new Error(`Unsupported platform: ${os}-${cpu}`)
  }
  if (os === "linux") {
    const libc = isMusl() ? "musl" : "gnu"
    return `@trouve-ai/search-linux-${cpu}-${libc}`
  }
  if (os === "darwin") {
    return `@trouve-ai/search-darwin-${cpu}`
  }
  if (os === "win32") {
    return `@trouve-ai/search-win32-${cpu}`
  }
  throw new Error(`Unsupported platform: ${os}-${cpu}`)
}

/** @returns {boolean} */
function isMusl() {
  try {
    const out = execSync("ldd /bin/sh 2>/dev/null || true", {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    })
    return out.includes("musl")
  } catch {
    return false
  }
}

/** @returns {string} */
function binaryFileName() {
  return platform() === "win32" ? "trouve-search.exe" : "trouve-search"
}

/**
 * Absolute path to the bundled binary, or the bare binary name for PATH fallback.
 * @returns {string}
 */
export function resolveBinaryPath() {
  const pkg = platformPackageName()
  try {
    const pkgJson = require.resolve(`${pkg}/package.json`)
    const bin = join(dirname(pkgJson), "bin", binaryFileName())
    if (existsSync(bin)) return bin
  } catch {
    // Platform package not installed (e.g. dev checkout without optional deps).
  }
  return binaryFileName()
}
