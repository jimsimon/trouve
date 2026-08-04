import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("composer clipboard attachment parity", () => {
  for (const relative of [
    "../app/trouve-app.ts",
    "./thread-screen.ts",
    "./new-thread-setup.ts",
  ]) {
    it(`prefers text before image/file representations in ${relative}`, () => {
      const source = readFileSync(new URL(relative, import.meta.url), "utf8");
      const pasteStart = source.indexOf("Paste = (event: ClipboardEvent)");
      expect(pasteStart).toBeGreaterThanOrEqual(0);
      const pasteBody = source.slice(pasteStart, pasteStart + 1_200);
      expect(pasteBody.indexOf('types.includes("text/plain")')).toBeGreaterThanOrEqual(0);
      expect(pasteBody.indexOf('types.includes("text/plain")')).toBeLessThan(
        pasteBody.indexOf("clipboardData?.files"),
      );
    });
  }
});
