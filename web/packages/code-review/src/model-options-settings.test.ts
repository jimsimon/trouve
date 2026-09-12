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
      // Untouched maps are omitted, while an intentional clear serializes {}.
      expect(api).toMatch(
        new RegExp(
          `changedModelOptions\\.${role}[\\s\\S]*${role}_model_options: repository\\.${role}_model_options \\?\\? \\{\\}`,
          "u",
        ),
      );
    }
    expect(api).toMatch(/changedModelOptions: RepositoryModelOptionChanges = \{\}/u);
    expect(types).toMatch(/model_options\?: ModelOptions \| undefined;\n  prompt_mode:/u);
  });

  it("the repository editor only re-sends option maps the user touched", () => {
    expect(repositoryEditor).toMatch(/await this\.api\.saveRepository\(next, modelOptionChanges\)/u);
    // A coordinator change cascades into every role that inherits the model.
    expect(repositoryEditor).toMatch(
      /this\.markModelOptionsChanged\("coordinator", "router", "analyst"\)/u,
    );
    for (const role of ["coordinator", "router", "analyst"]) {
      expect(repositoryEditor).toMatch(
        new RegExp(`this\\.markModelOptionsChanged\\("${role}"\\)`, "u"),
      );
    }
    // Disabling reviews is reversible and must not clear dormant options.
    expect(repositoryEditor).toMatch(
      /\{ \.\.\.repository, mode: "off" \},\s*"Reviews disabled",\s*\{\},/u,
    );
    // A fresh server snapshot resets the dirty set along with the draft.
    expect(repositoryEditor).toMatch(
      /this\.draft = this\.repository;\s*this\.changedModelOptions = \{\};/u,
    );
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
    // Every role resolves automatic aliases and preserves pinned routes.
    for (const role of ["router", "analyst"]) {
      expect(repositoryEditor).toMatch(
        new RegExp(
          `const effective(Router|Analyst)Model = modelForSelection\\(models, draft\\.${role}_model \\|\\| draft\\.model\\)`,
          "u",
        ),
      );
      expect(repositoryEditor).toMatch(
        new RegExp(`\\$\\{modelChoices\\(models, draft\\.${role}_model\\)\\}`, "u"),
      );
      expect(repositoryEditor).toMatch(
        new RegExp(
          `\\$\\{selectValue\\(modelSelectionValue\\(models, draft\\.${role}_model\\)\\)\\}`,
          "u",
        ),
      );
    }
    expect(repositoryEditor).toMatch(
      /const effectiveCoordinatorModel = modelForSelection\(models, draft\.model\)/u,
    );
    expect(repositoryEditor).not.toMatch(/models\.find\(/u);
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
