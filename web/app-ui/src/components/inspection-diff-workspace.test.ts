import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("inspection diff workspace contract", () => {
  it("renders a diff-only ARIA file tree with one selected diff viewer", () => {
    const source = readFileSync(
      new URL("./inspection-workspace.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain('id="session-diff-file-tree"');
    expect(source).toContain('aria-label="Changed files"');
    expect(source).toContain('role="treeitem"');
    expect(source).toContain("fileTreeDirectoriesForPaths(");
    expect(source).toContain("#navigateDiffFileTree");
    expect(source).toContain("#activateDiffFileTreeRow");
    expect(source).toContain("Copy raw diff for ${selectedFile.path}");
    expect(source).toContain("<trouve-diff-view");
    expect(source).toContain(".original=${file.original}");
    expect(source).toContain(".modified=${file.modified}");
    expect(source).not.toContain('class="unified-diff-list"');
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
