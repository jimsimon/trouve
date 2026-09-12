import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./inspection-workspace.ts", import.meta.url), "utf8");

describe("inspection file-browser parity wiring", () => {
  it("keeps the file tree collapsible without hiding a requested file", () => {
    expect(source).toContain("#fileTreeCollapsed");
    expect(source).toContain('aria-controls="session-file-tree"');
    expect(source).toContain("this.#fileTreeCollapsed = false");
  });

  it("offers a browser-compatible copy action for file contents", () => {
    expect(source).toContain("#copyFileContents(file: ProtocolFileContent)");
    expect(source).toContain("Copy file contents");
    expect(source).toContain("file.content");
  });
});
