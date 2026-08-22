import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");

test("review jobs distinguish new findings from PR-wide open findings", () => {
  assert.match(types, /open_issue_count\?: number \| null/u);
  assert.match(source, /open across this pull request/u);
  assert.match(source, /A clean incremental result does not resolve findings from earlier rounds/u);
  assert.match(source, /open across pull request/u);
  assert.match(source, /Open status unknown/u);
  assert.match(source, /legacy review predates PR-wide finding snapshots/u);
});

test("final-editor retry includes legacy reviewer tasks without reviewer ids", () => {
  assert.match(source, /filter\(\(task\) => task\.role === "reviewer"\)/u);
  assert.match(source, /task\.reviewer_id \|\| task\.reviewer_name \|\| task\.id/u);
});
