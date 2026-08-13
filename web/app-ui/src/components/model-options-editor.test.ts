import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./model-options-editor.ts", import.meta.url), "utf8");

describe("model options editor", () => {
  it("associates every advertised description with its control", () => {
    expect(source).toContain("const descriptionId = control.description");
    expect(source).toContain("aria-describedby=${descriptionId}");
    expect(source).toContain("<small id=${descriptionId}>");
    expect(source).not.toContain(":host([compact]) small { display: none; }");
  });

  it("reverts invalid numeric edits instead of leaving unsaved text visible", () => {
    expect(source).toContain("input.value = control.text;");
    expect(source).toContain("input.setCustomValidity(");
    expect(source).toContain("input.reportValidity();");
  });
});
