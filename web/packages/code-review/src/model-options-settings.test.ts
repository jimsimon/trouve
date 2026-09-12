import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const read = (file: string): string => readFileSync(new URL(file, import.meta.url), "utf8");

const repositoryEditor = read("./repositories-page.ts");
const jobDetail = read("./job-detail.ts");
const sharedViews = read("./shared-views.ts");
const types = read("./types.ts");
const api = read("./api.ts");

describe("model-specific options", () => {
  it("repositories, overrides, and jobs model per-role model options", () => {
    for (const role of ["coordinator", "router", "analyst"]) {
      expect(types).toMatch(new RegExp(`${role}_model_options\\?: ModelOptions`, "u"));
      // Maps are always sent so an empty map clears stale options server-side.
      expect(api).toMatch(
        new RegExp(`${role}_model_options: repository\\.${role}_model_options \\?\\? \\{\\},`, "u"),
      );
    }
    expect(types).toMatch(/model_options\?: ModelOptions \| undefined;\n  prompt_mode:/u);
  });

  it("the settings form renders schema-driven option controls per role", () => {
    expect(sharedViews).toMatch(/export function modelOptionsSetting\(/u);
    expect(repositoryEditor).toMatch(/scope: "Coordinator"/u);
    expect(repositoryEditor).toMatch(/scope: "Semantic router"/u);
    expect(repositoryEditor).toMatch(/scope: "Change analyst"/u);
    expect(repositoryEditor).toMatch(/options: override\?\.model_options,/u);
    // Changing a model drops options the new model does not advertise.
    for (const role of ["coordinator", "router", "analyst"]) {
      expect(repositoryEditor).toMatch(
        new RegExp(
          `${role}_model_options: compatibleOptions\\(\\s*draft\\.${role}_model_options,`,
          "u",
        ),
      );
    }
    expect(repositoryEditor).toMatch(
      /model_options: compatibleOptions\(\s*override\??\.model_options,/u,
    );
    // Overrides with only model options are retained.
    expect(repositoryEditor).toMatch(/Object\.keys\(updated\.model_options \?\? \{\}\)\.length > 0/u);
  });

  it("job details surface snapshotted model options", () => {
    expect(sharedViews).toMatch(/export function modelOptionFacts\(/u);
    expect(jobDetail).toMatch(/modelOptionFacts\("Coordinator", job\.coordinator_model_options\)/u);
    expect(jobDetail).toMatch(/modelOptionFacts\("Router", job\.router_model_options\)/u);
    expect(jobDetail).toMatch(/modelOptionFacts\("Change analyst", job\.analyst_model_options\)/u);
  });
});
