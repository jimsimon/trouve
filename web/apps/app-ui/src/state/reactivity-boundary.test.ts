import { readFileSync, readdirSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const sourceRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const sourceFiles = (directory: string): readonly string[] =>
  readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return entry.isFile() && path.endsWith(".ts") ? [path] : [];
  });

const directImports = (specifier: string): readonly string[] =>
  sourceFiles(sourceRoot)
    .filter((path) => {
      const source = readFileSync(path, "utf8");
      return new RegExp(
        `(?:from\\s*|import\\s*)["']${specifier.replaceAll("/", "\\/")}["']`,
        "u",
      ).test(source);
    })
    .map((path) => relative(sourceRoot, path).replaceAll("\\", "/"))
    .sort();

describe("owned reactivity boundary", () => {
  it("reaches signals only through the ui-foundation adapter", () => {
    expect(directImports("@lit-labs/signals")).toEqual([]);
    expect(directImports("signal-polyfill")).toEqual([]);
    expect(directImports("@trouve-ai/ui-foundation/reactivity").length).toBeGreaterThan(0);
  });
});
