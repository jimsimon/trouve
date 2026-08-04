import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { InspectionFileTreeModel } from "./inspection-file-tree.js";

describe("InspectionFileTreeModel", () => {
  it("sorts and flattens only lazily loaded expanded directories", () => {
    const tree = new InspectionFileTreeModel();
    tree.beginLoading(".");
    expect(tree.directory(".").status).toBe("loading");
    expect(tree.rows).toEqual([]);

    tree.resolveDirectory(".", [
      { name: "z.txt", is_dir: false },
      { name: "src", is_dir: true },
      { name: "docs", is_dir: true },
      { name: "A.txt", is_dir: false },
    ]);
    expect(tree.rows.map((row) => row.path)).toEqual([
      "docs",
      "src",
      "A.txt",
      "z.txt",
    ]);
    expect(tree.activePath).toBe("docs");

    tree.expand("src");
    expect(tree.directory("src").status).toBe("unloaded");
    expect(tree.rows.map((row) => row.path)).not.toContain("src/main.ts");
    tree.beginLoading("src");
    tree.resolveDirectory("src", [
      { name: "main.ts", is_dir: false },
      { name: "components", is_dir: true },
    ]);

    expect(tree.rows.map((row) => [row.path, row.depth, row.level])).toEqual([
      ["docs", 0, 1],
      ["src", 0, 1],
      ["src/components", 1, 2],
      ["src/main.ts", 1, 2],
      ["A.txt", 0, 1],
      ["z.txt", 0, 1],
    ]);
    expect(tree.row("src/main.ts")).toMatchObject({
      parentPath: "src",
      positionInSet: 2,
      setSize: 2,
    });
    tree.collapse("src");
    tree.expand("src");
    expect(tree.needsLoad("src")).toBe(false);
    expect(tree.rows.map((row) => row.path)).toContain("src/main.ts");
  });

  it("exposes deterministic unloaded, loading, error, and empty states", () => {
    const tree = new InspectionFileTreeModel();
    expect(tree.directory(".")).toMatchObject({ status: "unloaded", entries: [] });

    tree.beginLoading(".");
    expect(tree.loading).toBe(true);
    tree.failDirectory(".");
    expect(tree.directory(".")).toMatchObject({ status: "error", entries: [] });
    expect(tree.needsLoad(".")).toBe(true);

    tree.beginLoading(".");
    tree.resolveDirectory(".", []);
    expect(tree.loading).toBe(false);
    expect(tree.directory(".")).toMatchObject({ status: "loaded", entries: [] });
    expect(tree.needsLoad(".")).toBe(false);
  });

  it("maps the standard visible-tree navigation and activation keys", () => {
    const tree = new InspectionFileTreeModel();
    tree.resolveDirectory(".", [
      { name: "src", is_dir: true },
      { name: "README.md", is_dir: false },
    ]);
    tree.expand("src");
    tree.resolveDirectory("src", [
      { name: "lib", is_dir: true },
      { name: "main.ts", is_dir: false },
    ]);

    expect(tree.actionForKey("ArrowDown", "src")).toEqual({
      kind: "focus",
      path: "src/lib",
    });
    expect(tree.actionForKey("ArrowRight", "src")).toEqual({
      kind: "focus",
      path: "src/lib",
    });
    expect(tree.actionForKey("ArrowRight", "src/lib")).toEqual({
      kind: "expand",
      path: "src/lib",
    });
    expect(tree.actionForKey("ArrowLeft", "src/main.ts")).toEqual({
      kind: "focus",
      path: "src",
    });
    expect(tree.actionForKey("ArrowLeft", "src")).toEqual({
      kind: "collapse",
      path: "src",
    });
    expect(tree.actionForKey("Home", "README.md")).toEqual({
      kind: "focus",
      path: "src",
    });
    expect(tree.actionForKey("End", "src")).toEqual({
      kind: "focus",
      path: "README.md",
    });
    expect(tree.actionForKey("Enter", "src/main.ts")).toEqual({
      kind: "activate",
      path: "src/main.ts",
    });
    expect(tree.actionForKey(" ", "src/main.ts")).toEqual({
      kind: "activate",
      path: "src/main.ts",
    });
    expect(tree.actionForKey("ArrowUp", "src")).toEqual({
      kind: "focus",
      path: "src",
    });
    expect(tree.actionForKey("ArrowRight", "README.md")).toBeUndefined();
    expect(tree.actionForKey("PageDown", "src")).toBeUndefined();
  });

  it("recovers focus to a collapsed or surviving ancestor after tree changes", () => {
    const tree = new InspectionFileTreeModel();
    tree.resolveDirectory(".", [
      { name: "src", is_dir: true },
      { name: "README.md", is_dir: false },
    ]);
    tree.expand("src");
    tree.resolveDirectory("src", [{ name: "main.ts", is_dir: false }]);
    expect(tree.setActive("src/main.ts")).toBe(true);

    tree.collapse("src");
    expect(tree.activePath).toBe("src");

    tree.expand("src");
    tree.resolveDirectory("src", [{ name: "main.ts", is_dir: false }]);
    tree.setActive("src/main.ts");
    tree.resolveDirectory("src", []);
    expect(tree.activePath).toBe("src");

    tree.reset("src/deep/missing.ts");
    tree.resolveDirectory(".", [
      { name: "src", is_dir: true },
      { name: "README.md", is_dir: false },
    ]);
    expect(tree.activePath).toBe("src");

    tree.reset("src/main.ts");
    tree.resolveDirectory(".", [{ name: "README.md", is_dir: false }]);
    expect(tree.activePath).toBe("README.md");
  });
});

describe("files inspection component contract", () => {
  const component = readFileSync(
    new URL("./inspection-workspace.ts", import.meta.url),
    "utf8",
  );
  const styles = readFileSync(
    new URL("../styles/app.css", import.meta.url),
    "utf8",
  );

  it("wires the existing directory/file endpoints alongside the Slint-shaped unified diff", () => {
    expect(component).toContain("services.protocol.sessionFiles(sessionId, path)");
    expect(component).toContain("services.protocol.sessionFile(sessionId, path)");
    expect(component).toContain("<trouve-code-view");
    expect(component).toContain('class="unified-diff-list"');
    expect(component).toContain('class="file-tree-icon"');
  });

  it("renders a roving ARIA tree and localized asynchronous states", () => {
    for (const contract of [
      'role="tree"',
      'role="treeitem"',
      "aria-level=${row.level}",
      "aria-posinset=${row.positionInSet}",
      "aria-setsize=${row.setSize}",
      "aria-expanded=${row.isDirectory",
      "tabindex=${this.#fileTree.activePath === row.path ? 0 : -1}",
      "#navigateFileTree",
      "Loading files…",
      "could not be loaded.",
      "Empty directory",
    ]) {
      expect(component).toContain(contract);
    }
    expect(styles).toContain(".file-tree-item:focus-visible");
    expect(styles).toContain("var(--trouve-accent)");
  });

  it("uses a list-to-viewer Files flow on the mobile PWA", () => {
    expect(component).toContain('const MOBILE_FILES_QUERY = "(max-width: 760px)"');
    expect(component).toContain("globalThis.matchMedia?.(MOBILE_FILES_QUERY).matches === true");
    expect(component).toContain("this.#fileTreeCollapsed = true");
    expect(styles).toContain(".files-inspection:not(.file-tree-collapsed) > .file-view-shell");
    expect(styles).toContain(".files-inspection.file-tree-collapsed > .file-view-shell");
  });

  it("keeps the Slint unified diff as default and exposes the desktop split enhancement", () => {
    expect(component).toContain('#diffMode: DiffMode = "unified"');
    expect(component).toContain('class="diff-mode-additive"');
    expect(component).toContain('class="split-diff-file-picker"');
    expect(component).toContain("<trouve-diff-view");
    expect(component).toContain('@trouve-diff-mode-change=');
    expect(component).toContain("Binary file changed.");
    expect(styles).toContain(".diff-mode-additive { display: none; }");
  });
});
