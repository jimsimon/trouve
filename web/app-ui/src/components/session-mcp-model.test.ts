import { describe, expect, it } from "vitest";

import {
  parseMcpCommandLine,
  parseMcpConfigJson,
  sessionMcpCommandLine,
  sessionMcpEnvironmentLines,
  sessionMcpHealthLabel,
} from "./session-mcp-model.js";

const server = {
  name: "docs",
  scope: "branch",
  command: "docs server",
  args: ["--root", "path with spaces", "it's-here"],
  env: { TOKEN: "${TOKEN}", ALPHA: "safe" },
  health: "ok",
  detail: "5 tools",
};

describe("session MCP presentation", () => {
  it("renders command arguments unambiguously without executing them", () => {
    expect(sessionMcpCommandLine(server)).toBe(
      "'docs server' --root 'path with spaces' 'it'\\''s-here'",
    );
  });

  it("round-trips quoted command lines for the settings form", () => {
    expect(parseMcpCommandLine(sessionMcpCommandLine(server))).toEqual({
      command: "docs server",
      args: ["--root", "path with spaces", "it's-here"],
    });
    expect(parseMcpCommandLine(`runner --flag=one "two words" ''`)).toEqual({
      command: "runner",
      args: ["--flag=one", "two words", ""],
    });
    expect(parseMcpCommandLine("runner 'unfinished")).toBeUndefined();
  });

  it("keeps deterministic environment and health labels", () => {
    expect(sessionMcpEnvironmentLines(server)).toEqual([
      "ALPHA=safe",
      "TOKEN=${TOKEN}",
    ]);
    expect(sessionMcpHealthLabel("disabled")).toContain("higher-priority");
  });

  it("imports standard and VS Code MCP JSON after validating every entry", () => {
    expect(parseMcpConfigJson(JSON.stringify({
      mcpServers: {
        docs: { command: "npx", args: ["-y", "docs-mcp"], env: { TOKEN: "${TOKEN}" } },
      },
    }))).toEqual([{
      name: "docs",
      command: "npx",
      args: ["-y", "docs-mcp"],
      env: { TOKEN: "${TOKEN}" },
      enabled: true,
    }]);
    expect(parseMcpConfigJson(JSON.stringify({
      servers: { local: { type: "stdio", command: "local-mcp" } },
    }))).toEqual([{
      name: "local",
      command: "local-mcp",
      args: [],
      env: {},
      enabled: true,
    }]);
    expect(parseMcpConfigJson(JSON.stringify({
      mcpServers: { paused: { command: "paused-mcp", disabled: true } },
    }))).toEqual([{
      name: "paused",
      command: "paused-mcp",
      args: [],
      env: {},
      enabled: false,
    }]);
  });

  it("rejects malformed or unsupported MCP configs before import", () => {
    expect(() => parseMcpConfigJson("{"))
      .toThrow("not valid JSON");
    expect(() => parseMcpConfigJson(JSON.stringify({ mcpServers: {} })))
      .toThrow("does not contain any servers");
    expect(() => parseMcpConfigJson(JSON.stringify({
      mcpServers: { remote: { url: "https://example.test/mcp" } },
    }))).toThrow("only stdio servers");
    expect(() => parseMcpConfigJson(JSON.stringify({
      mcpServers: { bad: { command: "runner", args: [1] } },
    }))).toThrow("array of strings");
  });
});
