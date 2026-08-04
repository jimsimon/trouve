import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("inspection diff workspace contract", () => {
  it("keeps source mappings, disclosure, and file-scoped copy wired together", () => {
    const source = readFileSync(
      new URL("./inspection-workspace.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain('class="unified-diff-list"');
    expect(source).toContain('aria-expanded=${expanded');
    expect(source).toContain("diffFileActionForKey(");
    expect(source).toContain("Copy raw diff for ${file.path}");
    expect(source).toContain('class=${`unified-diff-row ${row.kind}`}');
    expect(source).toContain('row.oldNumber ?? ""');
    expect(source).toContain('row.newNumber ?? ""');
  });

  it("does not present generic restore failures as authoritative boundaries", () => {
    const source = readFileSync(
      new URL("./inspection-workspace.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain("Availability could not be determined");
    expect(source).not.toContain("may already be at its earliest checkpoint");
    expect(source).not.toContain("may already be at its latest checkpoint");
  });
});
