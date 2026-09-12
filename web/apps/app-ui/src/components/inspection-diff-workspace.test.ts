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
    expect(source).toContain("Copy raw diff for ${selectedPatch.path}");
    expect(source).toContain("<trouve-diff-view");
    expect(source).toContain(".original=${patch.original}");
    expect(source).toContain(".modified=${patch.modified}");
    expect(source).not.toContain('class="unified-diff-list"');
  });

  it("loads a lightweight manifest and only the selected file patch", () => {
    const source = readFileSync(
      new URL("./inspection-workspace.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain("services.protocol.sessionDiffSummary(sessionId)");
    expect(source).toContain("services.protocol.sessionFileDiff(sessionId, path)");
    expect(source).not.toContain("services.protocol.sessionDiff(sessionId)");
    expect(source).not.toContain("Copy complete diff");
  });

  it("exposes both relative checkpoint directions in the diff pane", () => {
    const source = readFileSync(
      new URL("./inspection-workspace.ts", import.meta.url),
      "utf8",
    );

    expect(source).toContain('aria-label="Undo to the previous checkpoint"');
    expect(source).toContain('aria-label="Redo to the next checkpoint"');
    expect(source).toContain("restoreSessionCheckpoint(sessionId, direction)");
    expect(source).toContain('this.#restoreRelativeCheckpoint("undo")');
    expect(source).toContain('this.#restoreRelativeCheckpoint("redo")');
    expect(source).toContain("this.#checkpointActions.reset()");
    expect(source).toContain("this.#checkpointActions.isCurrent(token)");
  });
});
