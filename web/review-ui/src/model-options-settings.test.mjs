import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
const types = readFileSync(new URL("./types.ts", import.meta.url), "utf8");
const api = readFileSync(new URL("./api.ts", import.meta.url), "utf8");

test("repositories, overrides, and jobs model per-role model options", () => {
  for (const role of ["coordinator", "router", "analyst"]) {
    assert.match(types, new RegExp(`${role}_model_options\\?: ModelOptions;`, "u"));
    // Maps are always sent so an empty map clears stale options server-side.
    assert.match(
      api,
      new RegExp(`${role}_model_options: repository\\.${role}_model_options \\?\\? \\{\\},`, "u"),
    );
  }
  assert.match(types, /model_options\?: ModelOptions;\n  prompt_mode:/u);
});

test("the settings form renders schema-driven option controls per role", () => {
  assert.match(source, /function ModelOptionsSetting\(/u);
  assert.match(source, /scope="Coordinator"/u);
  assert.match(source, /scope="Semantic router"/u);
  assert.match(source, /scope="Change analyst"/u);
  assert.match(source, /options=\{override\?\.model_options\}/u);
  // Changing a model drops options the new model does not advertise.
  for (const role of ["coordinator", "router", "analyst"]) {
    assert.match(
      source,
      new RegExp(
        `${role}_model_options: compatibleOptions\\(\\s*draft\\.${role}_model_options,`,
        "u",
      ),
    );
  }
  assert.match(source, /model_options: compatibleOptions\(\s*override\??\.model_options,/u);
  // The analyst path must resolve automatic aliases and preserve pinned routes
  // just like the pre-existing coordinator and router paths.
  assert.match(
    source,
    /const effectiveAnalystModel = modelForSelection\(models, draft\.analyst_model \|\| draft\.model\)/u,
  );
  assert.match(source, /value=\{modelSelectionValue\(models, draft\.analyst_model\)\}/u);
  assert.match(source, /<ModelOptions models=\{models\} selection=\{draft\.analyst_model\} \/>/u);
  // Overrides with only model options are retained.
  assert.match(source, /Object\.keys\(updated\.model_options \?\? \{\}\)\.length > 0/u);
});

test("job details surface snapshotted model options", () => {
  assert.match(source, /function ModelOptionFacts\(/u);
  assert.match(source, /options=\{job\.coordinator_model_options\}/u);
  assert.match(source, /options=\{job\.router_model_options\}/u);
  assert.match(source, /options=\{job\.analyst_model_options\}/u);
});
