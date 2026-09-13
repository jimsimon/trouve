import { describe, expect, it } from "vitest";

import { parseUnifiedDiff, selectedDiffIndexAfterRefresh } from "./diff-parser.js";

describe("parseUnifiedDiff", () => {
  it("reconstructs aligned before/after excerpts and per-file counts", () => {
    const files = parseUnifiedDiff(`diff --git a/src/a.ts b/src/a.ts
--- a/src/a.ts
+++ b/src/a.ts
@@ -1,3 +1,3 @@
 keep
-old
+new
 tail
diff --git a/asset.bin b/asset.bin
Binary files a/asset.bin and b/asset.bin differ
`);

    expect(files).toEqual([
      {
        path: "src/a.ts",
        raw: `diff --git a/src/a.ts b/src/a.ts
--- a/src/a.ts
+++ b/src/a.ts
@@ -1,3 +1,3 @@
 keep
-old
+new
 tail
`,
        original: "keep\nold\ntail",
        modified: "keep\nnew\ntail",
        originalLineNumbers: [1, 2, 3],
        modifiedLineNumbers: [1, 2, 3],
        additions: 1,
        deletions: 1,
        binary: false,
        rows: [
          { kind: "hunk", oldNumber: null, newNumber: null, text: "@@ -1,3 +1,3 @@" },
          { kind: "context", oldNumber: 1, newNumber: 1, text: "keep" },
          { kind: "delete", oldNumber: 2, newNumber: null, text: "old" },
          { kind: "add", oldNumber: null, newNumber: 2, text: "new" },
          { kind: "context", oldNumber: 3, newNumber: 3, text: "tail" },
        ],
      },
      {
        path: "asset.bin",
        raw: `diff --git a/asset.bin b/asset.bin
Binary files a/asset.bin and b/asset.bin differ
`,
        original: "[Binary file changed]",
        modified: "[Binary file changed]",
        originalLineNumbers: [null],
        modifiedLineNumbers: [null],
        additions: 0,
        deletions: 0,
        binary: true,
        rows: [
          { kind: "context", oldNumber: null, newNumber: null, text: "Binary file changed" },
        ],
      },
    ]);
  });

  it("keeps disjoint hunks visibly separated", () => {
    const [file] = parseUnifiedDiff(`diff --git a/a b/a
@@ -1 +1 @@
-one
+ONE
@@ -10 +10 @@
-ten
+TEN
`);
    expect(file?.original).toBe("one\n⋯\nten");
    expect(file?.modified).toBe("ONE\n⋯\nTEN");
    expect(file?.originalLineNumbers).toEqual([1, null, 10]);
    expect(file?.modifiedLineNumbers).toEqual([1, null, 10]);
    expect(file?.rows.filter((row) => row.kind === "hunk").map((row) => row.text)).toEqual([
      "@@ -1 +1 @@",
      "@@ -10 +10 @@",
    ]);
  });

  it("treats hunk additions beginning with plus signs as content", () => {
    const [file] = parseUnifiedDiff(`diff --git a/a b/a
--- a/a
+++ b/a
@@ -1 +1 @@
-old
+++ new
`);

    expect(file?.path).toBe("a");
    expect(file?.modified).toBe("++ new");
    expect(file?.additions).toBe(1);
  });

  it("tracks source positions independently through additions and deletions", () => {
    const [file] = parseUnifiedDiff(`diff --git a/a b/a
@@ -20,4 +30,5 @@
 context
-old one
-old two
+new one
+new two
+new three
 tail
`);

    expect(file?.originalLineNumbers).toEqual([20, 21, 22, 23]);
    expect(file?.modifiedLineNumbers).toEqual([30, 31, 32, 33, 34]);
    expect(file?.raw).toContain("@@ -20,4 +30,5 @@");
  });

  it("keeps the selected path stable across live refreshes and clamps removed selections", () => {
    const current = [{ path: "a.ts" }, { path: "b.ts" }, { path: "c.ts" }];
    expect(selectedDiffIndexAfterRefresh(current, 1, [
      { path: "new.ts" },
      { path: "b.ts" },
      { path: "a.ts" },
    ])).toBe(1);
    expect(selectedDiffIndexAfterRefresh(current, 2, [{ path: "a.ts" }])).toBe(0);
    expect(selectedDiffIndexAfterRefresh(current, 1, [])).toBe(0);
  });
});
