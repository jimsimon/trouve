import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");

test("review data and CLI status do not rely on manual refresh controls", () => {
  assert.doesNotMatch(source, /Reconcile now/u);
  assert.doesNotMatch(source, />\s*Refresh\s*</u);
  assert.match(source, /DASHBOARD_FALLBACK_REFRESH_MS/u);
  assert.match(source, /CLI_IDLE_REFRESH_MS/u);
  assert.match(source, /Retrying automatically\./u);
});

test("persona retry labels describe the actual terminal state", () => {
  assert.ok(
    source.includes(
      '`Retry full review after ${group.name} ${group.status}`',
    ),
  );
  assert.match(source, /group\.persona \? "Retry all" : "Retry"/u);
});
