import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("trouve-diff-view accessibility contract", () => {
  it("mounts side-specific source gutters and describes excerpt ranges", () => {
    const source = readFileSync(new URL("./diff-view.ts", import.meta.url), "utf8");
    const codeView = readFileSync(new URL("./code-view.ts", import.meta.url), "utf8");

    expect(source).toContain("lineNumbers: this.originalLineNumbers");
    expect(source).toContain("lineNumbers: this.modifiedLineNumbers");
    expect(source).toContain('label: `${this.label}, original`');
    expect(source).toContain('label: `${this.label}, modified`');
    expect(source).toContain('aria-describedby="diff-line-ranges"');
    expect(codeView).toContain('import("@codemirror/search")');
    expect(codeView).toContain("...search.searchKeymap");
    expect(codeView).toContain("search.highlightSelectionMatches()");
  });
});
