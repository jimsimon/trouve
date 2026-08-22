import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./automations-screen.ts", import.meta.url), "utf8");

const section = (start: string, end: string): string => {
  const startAt = source.indexOf(start);
  const endAt = source.indexOf(end, startAt + start.length);
  expect(startAt, `missing section start: ${start}`).toBeGreaterThanOrEqual(0);
  expect(endAt, `missing section end: ${end}`).toBeGreaterThan(startAt);
  return source.slice(startAt, endAt);
};

describe("automations screen model-option lifecycle", () => {
  it("scopes cached modes to the workspace that loaded them", () => {
    const loadModes = section(
      "async #loadModes(workspaceId: string)",
      "\n  override connectedCallback()",
    );
    expect(source).toContain('#modesWorkspaceId = "";');
    expect(loadModes).toContain("workspaceId !== this.#modesWorkspaceId");
    expect(loadModes).toContain("this.#modes = [];");
    expect(loadModes).toContain("this.#modesWorkspaceId = workspaceId;");
  });

  it("resolves current metadata only when model options need sanitizing", () => {
    const resolve = section(
      "async #modelForMutation(\n    draft: AutomationDraft,",
      "\n  #scheduleLoadRetry()",
    );
    expect(resolve).toContain("Object.keys(draft.modelOptions).length === 0");
    expect(resolve).toContain("services.protocol.personas(draft.workspaceId)");
    expect(resolve).toContain('services.modelCatalog.refresh("if-stale")');
    expect(resolve).toContain("services.protocol.providers()");
    expect(resolve).toContain("modes.some((mode) => mode.id === modeId)");
    expect(resolve).toContain("effective model metadata is unavailable");
    expect(resolve).toContain("No changes were saved.");
  });

  it("snapshots saves and refreshes stored state before enabled-state updates", () => {
    const persist = section(
      "async #persistAutomation()",
      "\n  async #toggleEnabled(",
    );
    const toggle = section(
      "async #toggleEnabled(",
      "\n  async #runNow(",
    );
    expect(persist).toContain("const draft: AutomationDraft = {");
    expect(persist).toContain("modelOptions: { ...this.#draft.modelOptions }");
    expect(persist).toContain("await this.#modelForMutation(draft)");
    expect(persist).toContain("if (model === undefined) return;");
    expect(toggle).toContain("await services.protocol.automations()");
    expect(toggle).toContain("automationEnabledRequest(current, enabled)");
    expect(toggle).not.toContain("#modelForMutation");
  });

  it("preserves model options when a mode change keeps the effective model", () => {
    const modeChanged = section(
      "readonly #modeChanged = (event: Event): void =>",
      "\n  readonly #modelPicked",
    );
    expect(modeChanged).toContain("const previousModel = this.#effectiveAutomationModel");
    expect(modeChanged).toContain("const nextModel = this.#effectiveAutomationModel");
    expect(modeChanged).toContain(
      "nextModel?.id === previousModel?.id ? this.#draft.modelOptions : {}",
    );
  });

  it("preserves model options when the same effective model is reselected", () => {
    const modelPicked = section(
      "readonly #modelPicked =",
      "\n  readonly #modelOptionChanged",
    );
    expect(modelPicked).toContain("const previousModelId =");
    expect(modelPicked).toContain("const nextModelId =");
    expect(modelPicked).toContain(
      "nextModelId === previousModelId ? this.#draft.modelOptions : {}",
    );
  });
});
