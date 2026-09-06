import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = (name: string): string =>
  readFileSync(new URL(`./${name}`, import.meta.url), "utf8");

/** Body of one class member, from its signature to the next member at class indentation. */
const member = (component: string, name: string): string => {
  const start = component.search(new RegExp(`^  (?:async |readonly )?${name}\\b`, "m"));
  if (start < 0) throw new Error(`member ${name} not found`);
  const rest = component.slice(start);
  const next = rest.slice(1).search(/^  (?:async |readonly |static |override )*#?\w+(?:\(|\s*=)/m);
  return next < 0 ? rest : rest.slice(0, next + 1);
};

// The model catalog is a session-long stale-while-revalidate cache. Any
// settings action that changes which models can run (a saved/removed
// provider, a completed sign-in, an installed/removed vendor runtime) must
// force it to refetch, or the New Session picker keeps the pre-change roster.
describe("settings actions invalidate the model catalog", () => {
  const providerSettings = source("provider-settings.ts");
  const cliSettings = source("cli-settings.ts");

  it("provider settings reload through a forced catalog refresh", () => {
    expect(member(providerSettings, "#reloadAfterChange"))
      .toContain('services.modelCatalog.refresh("force")');
  });

  it.each([
    "#saveSubscription",
    "#saveApiProvider",
    "#deleteProvider",
    "#applyLoginStatus",
  ])("%s reloads after the mutation succeeds", (name) => {
    expect(member(providerSettings, name)).toContain("this.#reloadAfterChange()");
  });

  it("cli settings invalidate through a forced catalog refresh", () => {
    expect(member(cliSettings, "#invalidateModelCatalog"))
      .toContain('modelCatalog.refresh("force")');
  });

  it.each(["#poll", "#uninstall"])("%s invalidates the catalog", (name) => {
    expect(member(cliSettings, name)).toContain("this.#invalidateModelCatalog()");
  });
});
