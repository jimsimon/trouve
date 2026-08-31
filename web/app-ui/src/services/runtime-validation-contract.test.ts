import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { automation } from "../generated/protocol-validators.js";

const read = (relativePath: string): string =>
  readFileSync(new URL(relativePath, import.meta.url), "utf8");

describe("CSP-safe runtime validation", () => {
  it("keeps generated validators synchronized with the OpenAPI snapshots", () => {
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

  it("accepts only scalar values in generated model-option maps", () => {
    const record = {
      id: "automation-1",
      name: "Daily review",
      prompt: "Review open work",
      workspace_id: "workspace-1",
      model_options: { fast: true, effort: "high", temperature: 0.25 },
      schedule: { kind: "daily" },
      enabled: true,
      created_at: "2026-08-22T09:00:00Z",
    };
    expect(automation(record)).toBe(true);
    for (const value of [null, ["high"], { nested: true }]) {
      expect(automation({ ...record, model_options: { invalid: value } })).toBe(false);
    }
  });
});
