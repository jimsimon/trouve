// Trouve custom tools for OpenCode. Installed by `trouve install` as
// ~/.config/opencode/tools/trouve.ts; the filename prefixes the exports, so
// these surface to the model as `trouve_search` and `trouve_find_related`.
// Requires the `trouve` binary on PATH (cargo install trouve).
import { tool } from "@opencode-ai/plugin"

const REPO = tool.schema
  .string()
  .optional()
  .describe(
    "Local directory path or https:// git URL to search. Defaults to the project root. " +
      "The index is built on first use and cached; updates are incremental.",
  )

const TOP_K = tool.schema
  .number()
  .int()
  .min(1)
  .optional()
  .describe("Number of results to return (default 5).")

const MAX_SNIPPET_LINES = tool.schema
  .number()
  .int()
  .min(0)
  .optional()
  .describe(
    "Lines of source per result. Default (10): signature + first body lines, enough to " +
      "confirm the location. 0: file path and line range only. Larger values include " +
      "more, up to the full chunk.",
  )

const CONTENT = tool.schema
  .enum(["code", "docs", "config", "all"])
  .optional()
  .describe(
    "What to search: code (default), docs (documentation and prose), config (yaml/toml/etc.), " +
      "or all.",
  )

/// Generous ceiling: a cold first index of a very large (or remote) repo
/// can take minutes; anything beyond this is treated as hung.
const TIMEOUT_MS = 10 * 60 * 1000

function spawnTrouve(args: string[]) {
  return Bun.spawn(["trouve", ...args], {
    stdin: "ignore",
    stdout: "pipe",
    stderr: "pipe",
  })
}

async function trouve(args: string[]): Promise<string> {
  let proc: ReturnType<typeof spawnTrouve>
  try {
    proc = spawnTrouve(args)
  } catch (error) {
    return (
      `trouve failed: ${error}. Is the trouve binary on PATH? ` +
      "Install it with `cargo install trouve` or download a release binary from GitHub."
    )
  }
  let timedOut = false
  let escalation: ReturnType<typeof setTimeout> | undefined
  const timer = setTimeout(() => {
    timedOut = true
    proc.kill()
    // Escalate if the process ignores SIGTERM, so the timeout actually
    // guarantees termination.
    escalation = setTimeout(() => proc.kill("SIGKILL"), 5000)
  }, TIMEOUT_MS)
  try {
    const [stdout, stderr, exitCode] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ])
    if (timedOut) {
      return (
        `trouve timed out after ${TIMEOUT_MS / 60000} minutes and was killed. ` +
        "Index builds are incremental, so retrying will resume where it left off."
      )
    }
    if (exitCode !== 0) {
      const detail = stderr.trim()
      return detail ? `trouve failed: ${detail}` : `trouve exited with code ${exitCode}`
    }
    return stdout.trim()
  } catch (error) {
    // Stream reads or exited can reject after a forced kill; surface it
    // like every other failure path instead of throwing.
    return `trouve failed: ${error}`
  } finally {
    clearTimeout(timer)
    clearTimeout(escalation)
  }
}

interface CommonArgs {
  repo?: string
  top_k?: number
  max_snippet_lines?: number
  content?: "code" | "docs" | "config" | "all"
}

/// Flags shared by both tools: repo (defaulting to the worktree), result
/// count, snippet budget, and content selection.
function commonArgs(args: CommonArgs, context: { worktree: string }): string[] {
  const argv = [
    args.repo ?? context.worktree,
    "--top-k",
    String(args.top_k ?? 5),
    "--max-snippet-lines",
    String(args.max_snippet_lines ?? 10),
  ]
  if (args.content && args.content !== "code") argv.push("--content", args.content)
  return argv
}

export const search = tool({
  description:
    "Search a codebase once with a focused query describing what the code does or its name. " +
    "Write queries using function/class names or behaviour descriptions, not error messages. " +
    "Returns file paths and line numbers — navigate directly there, do not grep for the same " +
    "content again.",
  args: {
    query: tool.schema.string().describe("Natural language or code query."),
    repo: REPO,
    top_k: TOP_K,
    max_snippet_lines: MAX_SNIPPET_LINES,
    content: CONTENT,
  },
  async execute(args, context) {
    return trouve(["search", args.query, ...commonArgs(args, context)])
  },
})

export const find_related = tool({
  description:
    "Find code similar to a known location. Useful for discovering all implementations of an " +
    "interface, all callers of a function, or all tests for a class. Pass `file_path` and " +
    "`line` from a prior trouve_search result.",
  args: {
    file_path: tool.schema
      .string()
      .describe("Path to the file as shown in a search result."),
    line: tool.schema.number().int().min(1).describe("Line number (1-indexed)."),
    repo: REPO,
    top_k: TOP_K,
    max_snippet_lines: MAX_SNIPPET_LINES,
    content: CONTENT,
  },
  async execute(args, context) {
    return trouve(["find-related", args.file_path, String(args.line), ...commonArgs(args, context)])
  },
})
