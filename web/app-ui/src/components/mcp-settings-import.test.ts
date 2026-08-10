import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(
  new URL("./management-settings-panels.ts", import.meta.url),
  "utf8",
);

describe("MCP settings import", () => {
  it("accepts uploaded or pasted JSON and writes validated servers to one scope", () => {
    expect(source).toContain('fontAwesomeIcon("file-import")');
    expect(source).toContain('accept=".json,application/json"');
    expect(source).toContain("this.#importJson = await file.text()");
    expect(source).toContain("const servers = parseMcpConfigJson(this.#importJson)");
    expect(source).toContain("await protocol.upsertMcpServer(server.name");
    expect(source).toContain("enabled: server.enabled");
    expect(source).toContain('scope === "workspace"');
  });

  it("offers a persistent enablement switch without deleting the definition", () => {
    expect(source).toContain('role="switch"');
    expect(source).toContain("await protocol.setMcpServerEnabled(server.name");
    expect(source).toContain("MCP_CONFIG_CHANGED_EVENT");
  });
});
