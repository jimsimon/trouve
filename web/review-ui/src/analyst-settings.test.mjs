import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");
const api = readFileSync(new URL("./api.ts", import.meta.url), "utf8");

test("repositories and jobs model the implementation-analyst settings", () => {
  assert.match(types, /analyst_model\?: string;/u);
  assert.match(types, /analyst_thinking_level\?: string;/u);
  assert.match(types, /"router" \| "analyst" \| "reviewer" \| "coordinator"/u);
  assert.match(api, /analyst_model: repository\.analyst_model \|\| null,/u);
  assert.match(api, /analyst_thinking_level: repository\.analyst_thinking_level \|\| null,/u);
});

test("the settings form offers analyst model and thinking pickers", () => {
  assert.match(source, /PR analyst model/u);
  assert.match(source, /PR analyst thinking/u);
  assert.match(source, /analyst_model: analystModel,/u);
  // Changing the fallback model keeps an incompatible analyst level from
  // being silently persisted, matching router behavior.
  assert.match(
    source,
    /analyst_thinking_level: compatibleThinking\(\s*draft\.analyst_thinking_level,\s*selectedAnalystModel,\s*\)/u,
  );
});

test("job details surface the analyst task and its configuration", () => {
  assert.match(source, /<dt>PR analyst model<\/dt>/u);
  assert.match(source, /<dt>PR analyst thinking<\/dt>/u);
  assert.match(source, /task\.role === "analyst"/u);
  assert.match(source, /"PR analyst"/u);
  // The settings explain the pass like other model settings do.
  assert.match(source, /derives what the\s+PR actually builds/u);
  assert.match(source, /advisory only/u);
  assert.match(source, /Full-branch analysis/u);
});
