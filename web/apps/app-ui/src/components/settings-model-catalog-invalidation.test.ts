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

/** The `try { … }` block of a member, so assertions cannot be satisfied by a catch/finally arm. */
const tryBlock = (body: string): string => {
  const start = body.indexOf("try {");
  const end = body.indexOf("} catch", start);
  if (start < 0 || end < 0) throw new Error("member has no try/catch");
  return body.slice(start, end);
};

/** Assert `later` occurs after `earlier` inside `text`. */
const expectOrdered = (text: string, earlier: string, later: string): void => {
  const first = text.indexOf(earlier);
  const second = text.indexOf(later);
  expect(first, `${earlier} present`).toBeGreaterThanOrEqual(0);
  expect(second, `${later} present`).toBeGreaterThan(first);
};

// The model catalog is a session-long stale-while-revalidate cache. Any
// settings action that changes which models can run (a saved/removed
// provider, a completed sign-in, an installed/removed vendor runtime) must
// force it to refetch, or the New Session picker keeps the pre-change roster.
//
// The unit tier has no DOM, so these are source contracts in the style of
// new-thread-setup.test.ts: each mutation path is checked on its own body,
// and the invalidation must sit after the awaited mutation inside the success
// arm rather than merely somewhere in the file.
describe("settings actions invalidate the model catalog", () => {
  const providerSettings = source("provider-settings.ts");
  const cliSettings = source("cli-settings.ts");

  it("provider settings reload through a forced catalog refresh", () => {
    expect(member(providerSettings, "#reloadAfterChange"))
      .toContain('services.modelCatalog.refresh("force")');
  });

  it.each([
    ["#saveSubscription", "await save;"],
    ["#saveApiProvider", "await save;"],
    ["#deleteProvider", "await services.protocol.deleteProvider(providerId);"],
  ])("%s reloads only after the awaited mutation succeeds", (name, mutation) => {
    const success = tryBlock(member(providerSettings, name));
    expectOrdered(success, mutation, "await this.#reloadAfterChange();");
  });

  it("#applyLoginStatus reloads only for a successful sign-in", () => {
    expect(member(providerSettings, "#applyLoginStatus"))
      .toContain("if (success) void this.#reloadAfterChange();");
  });

  it("cli settings invalidate through a forced catalog refresh", () => {
    expect(member(cliSettings, "#invalidateModelCatalog"))
      .toContain('modelCatalog.refresh("force")');
  });

  it("#poll invalidates only once a terminal install status is observed", () => {
    const body = member(cliSettings, "#poll");
    const branch = body.slice(body.indexOf("if (terminalStatusObserved) {"));
    expect(branch).not.toBe("");
    expect(branch.slice(0, branch.indexOf("try {"))).toContain("this.#invalidateModelCatalog();");
    // Exactly one call, and it lives in that branch rather than the poll preamble.
    expect(body.match(/this\.#invalidateModelCatalog\(\)/g)).toHaveLength(1);
  });

  it("#uninstall invalidates only after the uninstall request succeeds", () => {
    const success = tryBlock(member(cliSettings, "#uninstall"));
    expectOrdered(success, "await protocol.uninstallCli(cli.id);", "this.#invalidateModelCatalog();");
  });
});
