import { describe, expect, it } from "vitest";

import {
  TRANSCRIPT_TRUNCATION_NOTICE,
  isSpawnOutputToolCall,
  presentToolCall,
  presentToolDetail,
  runningActivityLabel,
  toolExecutionMetadata,
  toolDetailText,
  toolDisplayName,
  toolLabel,
} from "./tool-presentation.js";

describe("tool presentation", () => {
  it("uses truncation copy that covers omitted transcript messages and matches", () => {
    expect(TRANSCRIPT_TRUNCATION_NOTICE)
      .toBe("Additional transcript results were omitted.");
  });

  it("humanizes native, vendor, and MCP identifiers", () => {
    expect(toolDisplayName("search")).toBe("Code Search");
    expect(toolDisplayName("find_related")).toBe("Find Related");
    expect(toolDisplayName("WebSearch")).toBe("Web Search");
    expect(toolDisplayName("mcp__jira__create_issue")).toBe("jira: Create Issue");
    expect(toolDisplayName("mcp__trouve__search")).toBe("Code Search");
  });

  it("puts commands and bounded queries in the collapsed title", () => {
    expect(toolLabel("Bash", { command: "wc -l  bench.rs\n" })).toBe("Bash: wc -l bench.rs");
    expect(toolLabel("search", { query: "markdown renderer" })).toBe("Code Search: markdown renderer");
    expect(toolLabel("find_related", { file_path: "src/render.rs" }))
      .toBe("Find Related: src/render.rs");
    expect(toolLabel("Bash", { command: "é".repeat(100) })).toBe(`Bash: ${"é".repeat(29)}…`);
  });

  it("unwraps Codex MCP calls for the same title contract", () => {
    expect(toolLabel("mcpToolCall", {
      tool: "mcp__trouve__search",
      arguments: { query: "settings panel" },
    })).toBe("Code Search: settings panel");
  });

  it("shows commands for qualified external MCP shell tools", () => {
    expect(toolLabel("mcp__remote__execute", { command: "git status --short" }))
      .toBe("remote: Shell: git status --short");
    expect(toolLabel("mcpToolCall", {
      serverName: "ops",
      toolName: "bash",
      arguments: { command: "uptime" },
    })).toBe("ops: Bash: uptime");
  });

  it("retains the MCP namespace of generic provider wrappers", () => {
    expect(toolLabel("mcpToolCall", {
      serverName: "github",
      toolName: "search",
      arguments: { query: "issue 42" },
    })).toBe("github: Code Search: issue 42");
    expect(toolLabel("dynamicToolCall", {
      server: "linear",
      tool: "read_file",
      arguments: { path: "ENG-42" },
    })).toBe("linear: Read File: ENG-42");
    expect(toolLabel("mcpToolCall", {
      server: "external",
      tool: "mcp__trouve__search",
      arguments: { query: "spoofed" },
    })).toBe("external: Code Search: spoofed");

    expect(presentToolCall("mcpToolCall", {
      server: "linear",
      tool: "read_file",
      arguments: { path: "ENG-42" },
    })).toMatchObject({
      title: "linear: Read File: ENG-42",
      filePath: "",
    });
    expect(presentToolDetail("dynamicToolCall", {
      server: "github",
      tool: "search",
      arguments: { query: "issue 42" },
    }, {
      results: [{ file_path: "not-a-code-result" }],
    })).toMatchObject({ kind: "structured" });
  });

  it("presents read targets and inclusive line ranges", () => {
    expect(presentToolCall("read_file", {
      path: "src/main.rs",
      offset: 100,
      limit: 50,
    })).toMatchObject({
      title: "Read",
      subject: "main.rs",
      filePath: "src/main.rs",
      lineFrom: 100,
      lineTo: 149,
      meta: "L100-149",
    });
  });

  it("presents plain and hashline reads as source excerpts", () => {
    expect(presentToolDetail("read_file", {
      path: "src/main.rs",
      offset: 40,
      limit: 2,
    }, {
      content: "pub fn main() {}\n// tail\n",
      lines_read: 2,
      total_lines: 90,
      truncated: true,
    })).toEqual({
      kind: "source",
      inputs: [],
      source: {
        path: "src/main.rs",
        content: "pub fn main() {}\n// tail",
        startLine: 40,
        totalLines: 90,
        truncated: true,
      },
    });
    expect(presentToolDetail("mcpToolCall", {
      tool: "mcp__trouve__read_file",
      arguments: { path: "src/main.rs", format: "hashline" },
    }, {
      content: [{
        type: "text",
        text: JSON.stringify({
          content: "[src/main.rs#abc123]\n8:fn answer() -> u32 {\n9:  42\n10:}\n",
          format: "hashline",
          snapshot: "abc123",
          truncated: false,
          lines_read: 3,
          total_lines: 10,
        }),
      }],
    })).toMatchObject({
      kind: "source",
      source: {
        content: "fn answer() -> u32 {\n  42\n}",
        startLine: 8,
      },
    });
    expect(presentToolDetail("Read", {
      file_path: "src/native.ts",
      offset: 7,
    }, "const native = true;\n")).toMatchObject({
      kind: "source",
      inputs: [],
      source: {
        path: "src/native.ts",
        content: "const native = true;",
        startLine: 7,
      },
    });
  });

  it("splits semantic-search inputs from ranked result snippets", () => {
    const detail = presentToolDetail("search", {
      query: "thread virtualizer",
      top_k: 2,
      max_snippet_lines: 12,
    }, JSON.stringify({
      query: "thread virtualizer",
      results: [{
        file_path: "src/thread-screen.ts",
        start_line: 10,
        end_line: 20,
        score: 0.75,
        content: "class ThreadScreen {}",
      }],
    }));
    expect(detail).toEqual({
      kind: "search",
      inputs: [
        { label: "Query", value: "thread virtualizer" },
        { label: "Repository", value: ".", code: true },
        { label: "Results", value: "2" },
        { label: "Snippet lines", value: "12" },
      ],
      results: [{
        path: "src/thread-screen.ts",
        startLine: 10,
        endLine: 20,
        score: 0.75,
        content: "class ThreadScreen {}",
      }],
      truncated: false,
    });
  });

  it("uses bounded tables and streams for grep, paths, and shell results", () => {
    expect(presentToolDetail("grep", { pattern: "needle" }, {
      matches: [{ path: "src/lib.rs", line: 7, text: "let needle = true;" }],
      truncated: false,
    })).toMatchObject({
      kind: "matches",
      matches: [{ path: "src/lib.rs", line: 7, text: "let needle = true;" }],
    });
    expect(presentToolDetail("glob", { pattern: "**/*.rs" }, {
      files: ["src/lib.rs", "src/main.rs"],
      truncated: false,
    })).toMatchObject({ kind: "paths", paths: ["src/lib.rs", "src/main.rs"] });
    expect(presentToolDetail("shell", { command: "cargo test" }, {
      exit_code: 1,
      stdout: "running tests",
      stderr: "one test failed",
      truncated: false,
    })).toMatchObject({
      kind: "command",
      stdout: "running tests",
      stderr: "one test failed",
    });
    expect(presentToolDetail("shell", {
      command: "vite",
      run_in_background: true,
    }, {
      job_id: "bg-1",
      pid: 42,
      note: "running in background",
    })).toMatchObject({
      kind: "command",
      inputs: [
        { label: "Command", value: "vite", code: true },
        { label: "Job", value: "bg-1", code: true },
        { label: "Process", value: "42" },
        { label: "Background", value: "true" },
        { label: "Note", value: "running in background" },
      ],
    });
  });

  it("presents transcript matches and generic MCP content without JSON wrappers", () => {
    expect(presentToolDetail("search_transcript", { query: "checkpoint", scope: "session" }, {
      query: "checkpoint",
      scope: "session",
      matches: [{
        thread_id: "th_1",
        turn: 12,
        role: "assistant",
        ts: "2026-08-10T00:00:00Z",
        snippet: "…checkpoint complete…",
      }],
      truncated: false,
    })).toMatchObject({
      kind: "transcript",
      matches: [{ threadId: "th_1", turn: 12, role: "assistant" }],
    });
    expect(presentToolDetail("mcp__example__lookup", { id: "42" }, {
      content: [{ type: "text", text: "Found the requested record." }],
      isError: false,
    })).toEqual({
      kind: "structured",
      inputs: [{ label: "Id", value: "42" }],
      resultText: "Found the requested record.",
      error: false,
    });
  });

  it("caps complete turn transcripts and reports local truncation", () => {
    const messages = Array.from({ length: 101 }, (_, index) => ({
      role: "assistant",
      content: `message ${index}`,
    }));
    const detail = presentToolDetail("search_transcript", { turn: 4 }, {
      messages,
      truncated: false,
    });
    expect(detail).toMatchObject({
      kind: "transcript",
      truncated: true,
    });
    expect(detail.kind === "transcript" ? detail.messages : []).toHaveLength(100);
    expect(detail.kind === "transcript" ? detail.messages.at(-1) : undefined)
      .toEqual({ label: "Assistant", value: "message 99" });

    expect(presentToolDetail("search_transcript", { turn: 4 }, {
      messages: messages.slice(0, 1),
      truncated: true,
    })).toMatchObject({ kind: "transcript", truncated: true });

    const matches = Array.from({ length: 101 }, (_, index) => ({
      thread_id: "th_1",
      turn: index,
      role: "assistant",
      snippet: `match ${index}`,
    }));
    const matchDetail = presentToolDetail("search_transcript", { query: "match" }, {
      matches,
      truncated: false,
    });
    expect(matchDetail).toMatchObject({ kind: "transcript", truncated: true });
    expect(matchDetail.kind === "transcript" ? matchDetail.matches : []).toHaveLength(100);
  });

  it("keeps third-party MCP basename collisions in generic presentation", () => {
    expect(presentToolDetail("mcp__example__search", { query: "record" }, {
      results: [{ file_path: "should-not-be-code.ts" }],
    })).toMatchObject({ kind: "structured" });
    expect(isSpawnOutputToolCall("mcp__example__spawn_output", {})).toBe(false);
  });

  it("presents pageable Git diffs without exposing the result envelope", () => {
    expect(presentToolDetail("git_diff", {
      base: "main",
      path: "src/lib.rs",
      limit: 4_096,
    }, {
      diff: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
      offset: 0,
      next_offset: 101,
      total_bytes: 202,
      truncated: true,
    })).toEqual({
      kind: "diff",
      inputs: [
        { label: "Base", value: "main", code: true },
        { label: "Path", value: "src/lib.rs", code: true },
        { label: "Byte offset", value: "0" },
        { label: "Byte limit", value: "4096" },
      ],
      diff: "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
      truncated: true,
      nextOffset: 101,
      totalBytes: 202,
    });
  });

  it("builds bounded inline diffs and addition/deletion badges", () => {
    const presentation = presentToolCall("Edit", {
      file_path: "src/lib.rs",
      old_string: "fn a() {}\nfn b() {}",
      new_string: "fn a() {}\nfn b2() {}\nfn c() {}",
      _line: 8,
    });
    expect(presentation).toMatchObject({
      title: "Edit",
      subject: "lib.rs",
      filePath: "src/lib.rs",
      additions: 2,
      deletions: 1,
    });
    expect(presentation.diff).toEqual([
      { kind: "context", oldNumber: 8, newNumber: 8, text: "fn a() {}" },
      { kind: "delete", oldNumber: 9, newNumber: 0, text: "fn b() {}" },
      { kind: "add", oldNumber: 0, newNumber: 9, text: "fn b2() {}" },
      { kind: "add", oldNumber: 0, newNumber: 10, text: "fn c() {}" },
    ]);
  });

  it("presents hashline edits as file edits with compact payload additions", () => {
    const presentation = presentToolCall("hashline_edit", {
      input: "[src/lib.rs#A1B2C3D4E5F6]\nPUT 8.=9:\n+fn replacement() {}\n",
    });
    expect(presentation).toMatchObject({
      title: "Edit",
      subject: "lib.rs",
      filePath: "src/lib.rs",
      additions: 1,
    });
  });

  it("summarizes multi-file hashline edits without linking only the first file", () => {
    const presentation = presentToolCall("hashline_edit", {
      input: "[src/lib.rs#A1B2C3D4E5F6]\nPUT 8:\n+one\n[src/main.rs#010203040506]\nPUT 9:\n+two\n",
    });
    expect(presentation).toMatchObject({
      title: "Edit",
      subject: "2 files",
      filePath: "",
      additions: 2,
    });
  });

  it("summarizes multi-file apply-patch fallbacks", () => {
    const presentation = presentToolCall("apply_patch_fallback", {
      input: "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Add File: src/new.rs\n+created\n*** End Patch",
    });
    expect(presentation).toMatchObject({
      title: "Edit",
      subject: "2 files",
      filePath: "",
      additions: 2,
      deletions: 1,
    });
  });

  it("uses result-backed todo state for the card summary", () => {
    expect(presentToolCall("todo_write", { todos: [{ status: "pending", content: "old" }] }, {
      todos: [
        { status: "completed", content: "Audit" },
        { status: "in_progress", content: "Port tool cards" },
      ],
    })).toMatchObject({
      title: "TODOs",
      subject: "Port tool cards",
      meta: "1/2",
      todos: [
        { status: "completed", icon: "check", content: "Audit" },
        { status: "in_progress", icon: "play", content: "Port tool cards" },
      ],
    });
  });

  it("keeps third-party todo wrappers as ordinary tool cards", () => {
    expect(presentToolCall("mcpToolCall", {
      server: "linear",
      tool: "todo_write",
      arguments: {
        todos: [{ status: "completed", content: "Close issue" }],
      },
    })).toMatchObject({
      title: "linear: Todo Write",
      todos: [],
    });
  });

  it("recognizes native and wrapped child-agent collection calls", () => {
    expect(isSpawnOutputToolCall("spawn_output", {})).toBe(true);
    expect(isSpawnOutputToolCall("mcp__trouve__spawn_output", {})).toBe(true);
    expect(isSpawnOutputToolCall("mcpToolCall", {
      tool: "mcp__trouve__spawn_output",
      arguments: { thread_id: "th_child" },
    })).toBe(true);
    expect(isSpawnOutputToolCall("spawn_thread", {})).toBe(false);
  });

  it("formats human-readable bounded tool detail without JSON noise", () => {
    expect(toolDetailText({
      command: "cargo test",
      cwd: null,
      nested: { query: "parity" },
    }, [{ type: "text", text: "42 tests" }])).toBe(
      "command: cargo test\nnested:\n  query: parity\n── result ──\n42 tests",
    );
    expect([...toolDetailText({ body: "x".repeat(5_000) })].length).toBe(4_001);
    const unicodeDetail = toolDetailText({ body: "🙂".repeat(2_000) });
    expect(new TextEncoder().encode(unicodeDetail).byteLength).toBeLessThanOrEqual(4_003);
    expect(unicodeDetail).not.toContain("�");
  });

  it("surfaces provider or event-derived duration without redundant exit metadata", () => {
    expect(toolExecutionMetadata({ exit_code: 3, elapsed_ms: 65_432 }, 9_000))
      .toBe("1m 05s");
    expect(toolExecutionMetadata({ metadata: { exitCode: 0 } }, 812))
      .toBe("812ms");
    expect(toolExecutionMetadata({ exit_code: 0, duration_ms: 0 }, 50))
      .toBe("50ms");
    expect(toolExecutionMetadata({ exit_code: 0, duration_ms: 0 }))
      .toBe("");
    expect(toolExecutionMetadata({}, 0)).toBe("<1ms");
    expect(toolExecutionMetadata({ code: 404 })).toBe("");
    expect(toolExecutionMetadata(null)).toBe("");
  });

  it("uses a transient label only when no durable activity node is active", () => {
    expect(runningActivityLabel([], true)).toBe("Reasoning");
    expect(runningActivityLabel([
      { kind: "tool", tool: "WebSearch", args: {}, status: "running" },
      { kind: "turn-status", state: { kind: "running" } },
      { kind: "tool", tool: "read_file", args: { path: "README.md" }, status: "running" },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
      { kind: "tool", tool: "mcp__github__create_issue", args: {}, status: "running" },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
      {
        kind: "tool",
        tool: "mcpToolCall",
        args: { serverName: "github", toolName: "create_issue" },
        status: "running",
      },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
      { kind: "thinking", complete: false },
    ], true)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
      { kind: "progress", content: "Checking tests", complete: false },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
      { kind: "compaction", state: { kind: "running" } },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "tool", tool: "Bash", args: {}, status: "running" },
      { kind: "turn-status", state: { kind: "running" } },
    ], false)).toBe("Progress");
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
    ], false)).toBe("Progress");
  });
});
