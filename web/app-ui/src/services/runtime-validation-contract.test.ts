import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const read = (relativePath: string): string =>
  readFileSync(new URL(relativePath, import.meta.url), "utf8");

describe("CSP-safe runtime validation", () => {
  it("keeps generated validators synchronized with the OpenAPI snapshots", () => {
    execFileSync(
      process.execPath,
      [
        fileURLToPath(
          new URL("../../scripts/generate-runtime-validators.mjs", import.meta.url),
        ),
        "--check",
      ],
      { stdio: "pipe" },
    );
  });

  it("does not compile schemas or evaluate source in runtime clients", () => {
    for (const relativePath of ["./protocol-client.ts", "./host-client.ts"]) {
      const source = read(relativePath);
      expect(source).not.toMatch(/\bajv\b/iu);
      expect(source).not.toMatch(/\.compile\s*\(/u);
      expect(source).not.toMatch(/\bFunction\s*\(/u);
      expect(source).not.toMatch(/\beval\s*\(/u);
    }
  });

  it("emits standalone ESM without CSP-hostile runtime helpers", () => {
    for (const relativePath of [
      "../generated/protocol-validators.ts",
      "../generated/host-validators.ts",
    ]) {
      const source = read(relativePath);
      expect(source).not.toMatch(/\brequire\s*\(/u);
      expect(source).not.toMatch(/\bFunction\s*\(/u);
      expect(source).not.toMatch(/\beval\s*\(/u);
    }
  });
});
