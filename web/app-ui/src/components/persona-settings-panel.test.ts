import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(
  new URL("./persona-settings-panel.ts", import.meta.url),
  "utf8",
);

describe("persona settings thinking controls", () => {
  it("keeps global and persona thinking fields visible for every selected model", () => {
    expect(source).toContain("<span>Global default thinking level</span>");
    expect(source).toContain("<span>Default thinking level</span>");
    expect(source).toContain("<option>Not supported</option>");
  });

  it("derives enum levels and fixed budgets from the shared model schema", () => {
    expect(source).toContain("thinkingOption(");
    expect(source).toContain("thinkingSelectionIsValid(");
    expect(source).toContain("Global default thinking budget (tokens)");
    expect(source).toContain("Default thinking budget (tokens)");
    expect(source).toContain('type="number"');
  });
});
