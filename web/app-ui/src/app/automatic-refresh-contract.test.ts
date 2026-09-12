import { existsSync, readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const sourceRoot = fileURLToPath(new URL("..", import.meta.url));

const productionSources = (directory: string): readonly string[] =>
  readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = `${directory}/${entry.name}`;
    if (entry.isDirectory()) return productionSources(path);
    if (!entry.isFile() || !path.endsWith(".ts") || path.endsWith(".test.ts")) return [];
    return [path];
  });

describe("automatic data refresh contract", () => {
  it("does not expose manual refresh controls", () => {
    const forbiddenControl =
      /<button\b[\s\S]*?(?:>\s*Refresh(?:ing…)?\s*<|aria-label=(?:"|`)[^"`]*refresh|title=(?:"|`)[^"`]*refresh)[\s\S]*?<\/button>/giu;
    const forbiddenDataRetry =
      /(?:Retry connection|Retry loading|Retry pull requests|Retry terminals?|Retry settings|Reconcile now)/giu;

    for (const path of productionSources(sourceRoot)) {
      const source = readFileSync(path, "utf8");
      expect(source.match(forbiddenControl), path).toEqual(null);
      expect(source.match(forbiddenDataRetry), path).toEqual(null);
    }
  });

  it("does not ship the removed pull-to-refresh gesture", () => {
    expect(existsSync(`${sourceRoot}/services/pull-to-refresh.ts`)).toBe(false);
  });

  it("keeps request deadlines compatible with older system WebViews", () => {
    for (const path of productionSources(sourceRoot)) {
      expect(readFileSync(path, "utf8"), path).not.toContain("AbortSignal.timeout(");
    }
  });
});
