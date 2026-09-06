import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = (name: string): string =>
  readFileSync(new URL(`./${name}`, import.meta.url), "utf8");

// The model catalog is a session-long stale-while-revalidate cache. Any
// settings action that changes which models can run (a saved/removed
// provider, a completed sign-in, an installed/removed vendor runtime) must
// force it to refetch, or the New Session picker keeps the pre-change roster.
describe("settings actions invalidate the model catalog", () => {
  it("provider settings force a catalog refresh after every mutation", () => {
    const component = source("provider-settings.ts");
    expect(component).toContain('services.modelCatalog.refresh("force")');
    const reloads = component.match(/this\.#reloadAfterChange\(\)/g) ?? [];
    // upsert (subscription + API forms), delete, and successful login
    expect(reloads.length).toBe(4);
  });

  it("cli settings force a catalog refresh when installs settle or uninstall", () => {
    const component = source("cli-settings.ts");
    expect(component).toContain('modelCatalog.refresh("force")');
    const invalidations = component.match(/this\.#invalidateModelCatalog\(\)/g) ?? [];
    expect(invalidations.length).toBe(2);
  });
});
