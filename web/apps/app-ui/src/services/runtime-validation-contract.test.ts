import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const read = (relativePath: string): string =>
  readFileSync(new URL(relativePath, import.meta.url), "utf8");

describe("CSP-safe desktop host validation", () => {
  it("keeps generated host validators synchronized with the OpenAPI snapshot", () => {
    const result = spawnSync(
      process.execPath,
      [
        fileURLToPath(
          new URL("../../scripts/generate-runtime-validators.mjs", import.meta.url),
        ),
        "--check",
      ],
      { encoding: "utf8" },
    );
    expect(
      result.status,
      [result.stdout, result.stderr].filter(Boolean).join("\n"),
    ).toBe(0);
  });

  it("does not compile schemas or evaluate source in the host client", () => {
    const source = read("./host-client.ts");
    expect(source).not.toMatch(/\bajv\b/iu);
    expect(source).not.toMatch(/\.compile\s*\(/u);
    expect(source).not.toMatch(/\bFunction\s*\(/u);
    expect(source).not.toMatch(/\beval\s*\(/u);
  });

  it("emits standalone ESM without CSP-hostile runtime helpers", () => {
    const source = read("../generated/host-validators.ts");
    expect(source).not.toMatch(/\brequire\s*\(/u);
    expect(source).not.toMatch(/\bFunction\s*\(/u);
    expect(source).not.toMatch(/\beval\s*\(/u);
  });
});
