import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const read = (file: string): string => readFileSync(new URL(file, import.meta.url), "utf8");

const repositoryEditor = read("./repositories-page.ts");
const jobDetail = read("./job-detail.ts");
const types = read("./types.ts");
const api = read("./api.ts");

describe("implementation-analyst settings", () => {
  it("repositories and jobs model the implementation-analyst settings", () => {
    expect(types).toMatch(/analyst_model\?: string \| undefined;/u);
    expect(types).toMatch(/analyst_thinking_level\?: string \| undefined;/u);
    expect(types).toMatch(/"router" \| "analyst" \| "reviewer" \| "coordinator"/u);
    expect(api).toMatch(/analyst_model: repository\.analyst_model \|\| null,/u);
    expect(api).toMatch(
      /analyst_thinking_level: repository\.analyst_thinking_level \|\| null,/u,
    );
  });

  it("the settings form offers analyst model and thinking pickers", () => {
    expect(repositoryEditor).toMatch(/Change analyst model/u);
    expect(repositoryEditor).toMatch(/Change analyst thinking/u);
    expect(repositoryEditor).toMatch(/analyst_model: analystModel,/u);
    // Changing the fallback model keeps an incompatible analyst level from
    // being silently persisted, matching router behavior.
    expect(repositoryEditor).toMatch(
      /analyst_thinking_level: compatibleThinking\(\s*draft\.analyst_thinking_level,\s*selectedAnalystModel,\s*\)/u,
    );
  });

  it("job details surface the analyst task and its configuration", () => {
    expect(jobDetail).toMatch(/<dt>Change analyst model<\/dt>/u);
    expect(jobDetail).toMatch(/<dt>Change analyst thinking<\/dt>/u);
    expect(jobDetail).toMatch(/task\.role === "analyst"/u);
    expect(jobDetail).toMatch(/"Change analyst"/u);
    // The settings explain the pass like other model settings do.
    expect(repositoryEditor).toMatch(/derives what the\s+PR actually builds/u);
    expect(repositoryEditor).toMatch(/advisory only/u);
    expect(jobDetail).toMatch(/Full-branch analysis/u);
  });
});
