import { describe, expect, it } from "vitest";

import {
  parseMcpCommandLine,
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
});
