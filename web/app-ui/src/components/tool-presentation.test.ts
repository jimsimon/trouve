import { describe, expect, it } from "vitest";

import {
  presentToolCall,
  runningActivityLabel,
  toolExecutionMetadata,
  toolDetailText,
  toolDisplayName,
  toolLabel,
} from "./tool-presentation.js";

describe("tool presentation", () => {
  it("humanizes native, vendor, and MCP identifiers", () => {
    expect(toolDisplayName("search")).toBe("Code Search");
    expect(toolDisplayName("find_related")).toBe("Find Related");
    expect(toolDisplayName("WebSearch")).toBe("Web Search");
    expect(toolDisplayName("mcp__jira__create_issue")).toBe("jira: Create Issue");
    expect(toolDisplayName("mcp__trouve__search")).toBe("Code Search");
  });

  it("puts commands and bounded queries in the collapsed title", () => {
    expect(toolLabel("Bash", { command: "wc -l  bench.rs\n" })).toBe("Bash (wc -l bench.rs)");
    expect(toolLabel("search", { query: "markdown renderer" })).toBe("Code Search markdown renderer");
    expect(toolLabel("Bash", { command: "é".repeat(100) })).toBe(`Bash (${"é".repeat(29)}…)`);
  });

  it("unwraps Codex MCP calls for the same title contract", () => {
    expect(toolLabel("mcpToolCall", {
      tool: "mcp__trouve__search",
      arguments: { query: "settings panel" },
    })).toBe("Code Search settings panel");
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

  it("uses result-backed todo state for the card summary", () => {
    expect(presentToolCall("todo_write", { todos: [{ status: "pending", content: "old" }] }, {
      todos: [
        { status: "completed", content: "Audit" },
        { status: "in_progress", content: "Port tool cards" },
      ],
    })).toMatchObject({
      title: "Todos",
      subject: "Port tool cards",
      meta: "1/2",
      todos: [
        { status: "completed", icon: "check", content: "Audit" },
        { status: "in_progress", icon: "play", content: "Port tool cards" },
      ],
    });
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
    expect(runningActivityLabel([], true)).toBe("Thinking…");
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
      { kind: "compaction", state: { kind: "running" } },
    ], false)).toBeUndefined();
    expect(runningActivityLabel([
      { kind: "tool", tool: "Bash", args: {}, status: "running" },
      { kind: "turn-status", state: { kind: "running" } },
    ], false)).toBe("Processing…");
    expect(runningActivityLabel([
      { kind: "turn-status", state: { kind: "running" } },
    ], false)).toBe("Processing…");
  });
});
