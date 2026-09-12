import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const sourceRoot = fileURLToPath(new URL(".", import.meta.url));

const source = readdirSync(sourceRoot)
  .filter((name) => name.endsWith(".ts") && !name.endsWith(".test.ts"))
  .map((name) => readFileSync(`${sourceRoot}${name}`, "utf8"))
  .join("\n");

const read = (file: string): string => readFileSync(new URL(file, import.meta.url), "utf8");
const jobDetail = read("./job-detail.ts");
const app = read("./app.ts");
const repositoriesPage = read("./repositories-page.ts");
const reviewersPage = read("./reviewers-page.ts");
const settingsPage = read("./settings-page.ts");
const sharedViews = read("./shared-views.ts");

const between = (text: string, start: string, end: string): string =>
  text.slice(text.indexOf(start), text.indexOf(end));

describe("automatic refresh", () => {
  it("review data and CLI status do not rely on manual refresh controls", () => {
    expect(source).not.toMatch(/Reconcile now/u);
    expect(source).not.toMatch(/>\s*Refresh\s*</u);
    expect(source).toMatch(/DASHBOARD_FALLBACK_REFRESH_MS/u);
    expect(source).toMatch(/CLI_IDLE_REFRESH_MS/u);
    expect(source).toMatch(/Retrying automatically\./u);
  });

  it("persona retry labels describe the actual terminal state", () => {
    expect(jobDetail).toContain("`Retry full review after ${group.name} ${group.status}`");
    expect(jobDetail).toMatch(/persona \? "Retry all" : "Retry"/u);
  });

  it("model discovery does not block unrelated repository and persona saves", () => {
    expect(app).toMatch(/void this\.loadModelRoutes\(\);/u);
    const configurationLoader = between(
      app,
      "private loadConfiguration()",
      "private get isConfigurationRoute",
    );
    expect(configurationLoader).toMatch(/void this\.loadStaticModels\(\);/u);
    expect(configurationLoader).not.toMatch(/Promise\.allSettled\(\[\s*api\.getModels\(\)/u);
    expect(configurationLoader).not.toMatch(/getModels\(\)/u);
    // The static list only paints while live discovery has not replaced it,
    // and a static failure never erases a catalog that already loaded.
    const staticLoader = between(app, "private loadStaticModels()", "private loadConfiguration()");
    expect(staticLoader).toMatch(
      /current\.loaded \? current : \{ \.\.\.current, models, loaded: true \}/u,
    );
    expect(staticLoader).not.toMatch(/current\.loaded \? current : \{ \.\.\.current, error \}/u);
    // Pickers wait for the catalog; the forms' save buttons do not.
    const repositoryEditor = between(
      repositoriesPage,
      "export class RepositoryEditor",
      'customElements.define("trouve-code-review-repositories"',
    );
    expect(repositoryEditor).toMatch(/\?disabled=\$\{busy \|\| reviewerPolicyInvalid \|\| reviewModelInvalid\}/u);
    expect(repositoryEditor).not.toMatch(/\?disabled=\$\{busy \|\| !modelsLoaded/u);
    const reviewerEditor = between(
      reviewersPage,
      "export class ReviewerEditor",
      'customElements.define("trouve-code-review-reviewer-editor"',
    );
    expect(reviewerEditor).toMatch(/<button type="submit" \?disabled=\$\{busy\}>/u);
    expect(reviewerEditor).not.toMatch(/\?disabled=\$\{busy \|\| !modelsLoaded\}>/u);
  });

  it("model discovery retries independently on configuration routes", () => {
    expect(app).toMatch(/private staticModelError = "";/u);
    expect(app).toMatch(
      /if \(!isConfigurationRoute \|\| !staticModelError\) return;[\s\S]*?void this\.loadStaticModels\(\);/u,
    );
    expect(app).toMatch(
      /if \(!isConfigurationRoute \|\| !modelRouteError\) return;[\s\S]*?void this\.loadModelRoutes\(\);/u,
    );
    expect(app).not.toMatch(/\bneedsConfiguration\b/u);
    // Every configuration page shows the same combined catalog state.
    expect(app).toMatch(
      /const modelsError = this\.modelCatalog\.error \|\| this\.staticModelError;/u,
    );
    expect([...app.matchAll(/\bmodelsError\b/gu)]).toHaveLength(5);
    expect(sharedViews).toMatch(
      /const modelCatalogStatus = modelCatalogStatusMessage\(modelsLoaded, modelsError\);/u,
    );
    expect(sharedViews).toMatch(/role="status">\$\{modelCatalogStatus\}/u);
    for (const page of [repositoriesPage, reviewersPage, settingsPage]) {
      expect(page).toMatch(/\$\{modelCatalogStatus\((this\.)?modelsLoaded, (this\.)?modelsError\)\}/u);
    }
  });
});
