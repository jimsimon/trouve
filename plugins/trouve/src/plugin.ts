// Plugin exposing trouve code search as native tools in OpenCode and
// Kilo Code (whose plugin runtime is identical to OpenCode's).
//
// Unlike the standalone tool file (src/agents/opencode-tool.ts, one CLI
// process per call), this plugin keeps a single `trouve` server process
// alive for the whole session and speaks its newline-delimited JSON-RPC
// protocol directly. That preserves the server's in-process index cache:
// repeat queries — including against remote git URLs — skip index reload
// entirely.
//
// Requires the `trouve` binary on PATH (cargo install trouve).
import { tool, type Plugin, type PluginModule } from "@opencode-ai/plugin"

import pkg from "../package.json"

const PROTOCOL_VERSION = "2024-11-05"
const CONTENT_TYPES = ["code", "docs", "config", "all"] as const
type ContentType = (typeof CONTENT_TYPES)[number]

interface Pending {
  resolve(value: unknown): void
  reject(error: Error): void
}

/// The initialize handshake involves no indexing and must be quick.
const INIT_TIMEOUT_MS = 30_000
/// A tool call may include a cold index build of a very large (or remote)
/// repo; anything beyond this is treated as a hung server.
const CALL_TIMEOUT_MS = 10 * 60 * 1000

/** Minimal client for trouve's newline-delimited JSON-RPC stdio server. */
class TrouveServer {
  private proc: ReturnType<typeof Bun.spawn> | null = null
  private pending = new Map<number, Pending>()
  private nextId = 1
  private starting: Promise<void> | null = null
  private stderrTail = ""

  constructor(private content: ContentType[]) {}

  async callTool(name: string, args: Record<string, unknown>): Promise<string> {
    await this.ensureStarted()
    const result = (await this.request(
      "tools/call",
      { name, arguments: args },
      CALL_TIMEOUT_MS,
    )) as {
      content?: Array<{ type: string; text?: string }>
    }
    const text = result.content
      ?.map((part) => (part.type === "text" ? (part.text ?? "") : ""))
      .join("")
    return text?.trim() || "trouve returned no output."
  }

  private ensureStarted(): Promise<void> {
    // `proc` is set before the initialize handshake completes, so an
    // in-flight startup must win over the proc check: a concurrent caller
    // would otherwise send tools/call before the server is initialized.
    if (this.starting) return this.starting
    if (this.proc) return Promise.resolve()
    this.starting = this.start().finally(() => {
      this.starting = null
    })
    return this.starting
  }

  private async start(): Promise<void> {
    const argv = ["trouve"]
    if (!(this.content.length === 1 && this.content[0] === "code")) {
      argv.push("--content", ...this.content)
    }
    const proc = Bun.spawn(argv, {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "pipe",
    })
    this.proc = proc
    proc.exited.then(() => this.onExit())
    this.readLoop(proc.stdout).catch(() => this.onExit())
    this.readStderr(proc.stderr).catch(() => {})
    await this.request(
      "initialize",
      {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: {},
        clientInfo: { name: pkg.name, version: pkg.version },
      },
      INIT_TIMEOUT_MS,
    )
    this.write({ jsonrpc: "2.0", method: "notifications/initialized" })
  }

  /// Keep the tail of stderr so a crash can be diagnosed from the tool
  /// output instead of vanishing silently.
  private async readStderr(stderr: ReadableStream<Uint8Array>): Promise<void> {
    const decoder = new TextDecoder()
    for await (const chunk of stderr) {
      this.stderrTail = (this.stderrTail + decoder.decode(chunk, { stream: true })).slice(-2000)
    }
  }

  private onExit(): void {
    this.proc = null
    const pending = [...this.pending.values()]
    this.pending.clear()
    const detail = this.stderrTail.trim()
    const message = detail
      ? `trouve server exited unexpectedly: ${detail}`
      : "trouve server exited unexpectedly"
    for (const p of pending) p.reject(new Error(message))
  }

  private async readLoop(stdout: ReadableStream<Uint8Array>): Promise<void> {
    const decoder = new TextDecoder()
    let buffer = ""
    for await (const chunk of stdout) {
      buffer += decoder.decode(chunk, { stream: true })
      let newline: number
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline).trim()
        buffer = buffer.slice(newline + 1)
        if (line) this.onMessage(line)
      }
    }
  }

  private onMessage(line: string): void {
    let message: { id?: number; result?: unknown; error?: { message?: string } }
    try {
      message = JSON.parse(line)
    } catch {
      return
    }
    if (typeof message.id !== "number") return
    const pending = this.pending.get(message.id)
    if (!pending) return
    this.pending.delete(message.id)
    if (message.error) {
      pending.reject(new Error(message.error.message ?? "trouve server error"))
    } else {
      pending.resolve(message.result)
    }
  }

  private request(method: string, params: unknown, timeoutMs: number): Promise<unknown> {
    const id = this.nextId++
    const promise = new Promise((resolve, reject) => {
      // A server that stalls without exiting would otherwise hang the
      // agent turn forever: fail the request and kill the process so the
      // next call starts fresh.
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) {
          this.proc?.kill()
          reject(
            new Error(
              `trouve server did not respond to ${method} within ${timeoutMs / 1000}s ` +
                "and was restarted. Index builds are incremental; retrying resumes.",
            ),
          )
        }
      }, timeoutMs)
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer)
          resolve(value)
        },
        reject: (error) => {
          clearTimeout(timer)
          reject(error)
        },
      })
    })
    this.write({ jsonrpc: "2.0", id, method, params })
    return promise
  }

  private write(message: unknown): void {
    const stdin = this.proc?.stdin as { write(data: string): void; flush(): void } | undefined
    if (!stdin) throw new Error("trouve server is not running")
    stdin.write(JSON.stringify(message) + "\n")
    stdin.flush()
  }
}

function errorText(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  if (/ENOENT|executable|not.*found/i.test(message)) {
    return (
      "trouve failed: the `trouve` binary was not found on PATH. " +
      "Install it with `cargo install trouve` or download a release binary from GitHub."
    )
  }
  return `trouve failed: ${message}`
}

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

/** Minimum interval between background index warms. */
const WARM_INTERVAL_MS = 60_000

/**
 * Fire-and-forget index warm: `trouve stats` builds (or incrementally
 * refreshes) the on-disk index and snapshot for `directory`, so the first
 * real search of a session mmap-loads a warm snapshot instead of paying the
 * build. Failures (e.g. no trouve binary) are silently ignored — the tools
 * themselves report actionable errors when actually called.
 */
function makeWarmer(directory: string | undefined, content: ContentType[]) {
  let last = 0
  return () => {
    if (!directory) return
    const now = Date.now()
    if (now - last < WARM_INTERVAL_MS) return
    last = now
    try {
      const argv = ["trouve", "stats", directory]
      if (!(content.length === 1 && content[0] === "code")) {
        argv.push("--content", ...content)
      }
      Bun.spawn(argv, { stdin: "ignore", stdout: "ignore", stderr: "ignore" })
    } catch {
      // Missing binary: stay silent here; tool calls surface the real error.
    }
  }
}

/**
 * Plugin options (set in opencode/kilo config as `["trouve-plugin", {...}]`):
 * - `content`: what the server indexes — "code" (default), "docs",
 *   "config", "all", or an array of those.
 * - `warm`: build/refresh the project index in the background at session
 *   start and after each idle turn (default true).
 */
export const TrouvePlugin: Plugin = async (input, options) => {
  const opts = options as { content?: string | string[]; warm?: boolean } | undefined
  const requested = opts?.content
  const requestedList = Array.isArray(requested) ? requested : requested ? [requested] : ["code"]
  const invalid = requestedList.filter(
    (c) => !(CONTENT_TYPES as readonly string[]).includes(c),
  )
  if (invalid.length) {
    console.warn(
      `trouve-plugin: ignoring invalid content value(s) ${invalid.join(", ")}; ` +
        `valid values are ${CONTENT_TYPES.join(", ")}.`,
    )
  }
  const content = requestedList.filter((c): c is ContentType =>
    (CONTENT_TYPES as readonly string[]).includes(c),
  )
  const resolved = content.length ? content : (["code"] as ContentType[])
  const server = new TrouveServer(resolved)

  const warm = opts?.warm === false ? () => {} : makeWarmer(input?.worktree, resolved)
  // Warm at plugin load, so the index is ready before the first search;
  // re-warm (throttled) whenever a session goes idle, absorbing any edits
  // the agent made during the turn.
  warm()

  return {
    event: async ({ event }) => {
      if (event.type === "session.idle") warm()
    },
    tool: {
      trouve_search: tool({
        description:
          "Search a codebase once with a focused query describing what the code does or its " +
          "name. Write queries using function/class names or behaviour descriptions, not " +
          "error messages. Returns file paths and line numbers — navigate directly there, " +
          "do not grep for the same content again.",
        args: {
          query: tool.schema.string().describe("Natural language or code query."),
          repo: REPO,
          top_k: TOP_K,
          max_snippet_lines: MAX_SNIPPET_LINES,
        },
        async execute(args, context) {
          try {
            return await server.callTool("search", {
              query: args.query,
              repo: args.repo ?? context.worktree,
              top_k: args.top_k ?? 5,
              max_snippet_lines: args.max_snippet_lines ?? 10,
            })
          } catch (error) {
            return errorText(error)
          }
        },
      }),
      trouve_find_related: tool({
        description:
          "Find code similar to a known location. Useful for discovering all implementations " +
          "of an interface, all callers of a function, or all tests for a class. Pass " +
          "`file_path` and `line` from a prior trouve_search result.",
        args: {
          file_path: tool.schema
            .string()
            .describe("Path to the file as shown in a search result."),
          line: tool.schema.number().int().min(1).describe("Line number (1-indexed)."),
          repo: REPO,
          top_k: TOP_K,
          max_snippet_lines: MAX_SNIPPET_LINES,
        },
        async execute(args, context) {
          try {
            return await server.callTool("find_related", {
              file_path: args.file_path,
              line: args.line,
              repo: args.repo ?? context.worktree,
              top_k: args.top_k ?? 5,
              max_snippet_lines: args.max_snippet_lines ?? 10,
            })
          } catch (error) {
            return errorText(error)
          }
        },
      }),
    },
  }
}

// Module descriptor: the shape both OpenCode's and Kilo Code's loaders
// prefer. The named `TrouvePlugin` export above remains for older loaders
// that invoke plugin function exports directly.
export default { id: "trouve", server: TrouvePlugin } satisfies PluginModule
