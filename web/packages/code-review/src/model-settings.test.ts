import { describe, expect, it } from "vitest";

import {
  changeModelOption,
  defaultThinkingSelection,
  modelOptionControls,
  modelOptionSummaries,
  modelOptionTextValue,
  sanitizeModelOptions,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
  type TextModelOptionControl,
} from "./model-settings.js";

describe("model thinking settings", () => {
  it("thinking options follow the model-advertised schema key", () => {
    const model = {
      id: "codex/gpt-5.4",
      options_schema: {
        properties: {
          reasoning_effort: {
            type: "string",
            enum: ["low", "medium", "high", "xhigh"],
            default: "medium",
          },
        },
      },
    };

    expect(thinkingOptions(model)).toEqual({
      values: ["low", "medium", "high", "xhigh"],
      defaultValue: "medium",
    });
    expect(defaultThinkingSelection(model)).toBe("medium");
    expect(defaultThinkingSelection(model, "high")).toBe("high");
    expect(defaultThinkingSelection(model, "unsupported")).toBe("medium");
  });

  it("models without an advertised thinking enum have no thinking selector", () => {
    expect(
      thinkingOptions({
        id: "openai/plain",
        options_schema: { properties: { temperature: { type: "number" } } },
      }),
    ).toEqual({ values: [] });
    expect(defaultThinkingSelection(undefined, "high")).toBe("");
  });

  it("enum-backed thinking without a declared default selects its first value", () => {
    const model = {
      id: "provider/enum-thinking",
      options_schema: {
        properties: {
          reasoning_effort: {
            type: "string",
            enum: ["low", "high"],
          },
        },
      },
    };

    expect(defaultThinkingSelection(model)).toBe("low");
  });

  it("fixed thinking budgets follow advertised numeric bounds", () => {
    const model = {
      id: "anthropic/claude-haiku-4-5",
      options_schema: {
        properties: {
          thinking_budget_tokens: {
            type: "integer",
            minimum: 1024,
            maximum: 32768,
            default: 4096,
          },
        },
      },
    };
    expect(thinkingOptions(model)).toEqual({
      values: [],
      defaultValue: "4096",
      budget: { minimum: 1024, maximum: 32768 },
    });
    expect(thinkingSelectionIsValid(model, "16384")).toBe(true);
    expect(thinkingSelectionIsValid(model, "512")).toBe(false);
    expect(defaultThinkingSelection(model)).toBe("4096");
  });

  it("thinking labels make provider tokens readable", () => {
    expect(thinkingLevelLabel("xhigh")).toBe("Extra High");
    expect(thinkingLevelLabel("ultra")).toBe("Ultra");
    expect(thinkingLevelLabel("vendor-special")).toBe("vendor-special");
  });
});

describe("model options", () => {
  const codexModel = {
    id: "codex/gpt-5.5",
    options_schema: {
      properties: {
        reasoning_effort: { type: "string", enum: ["low", "high"], default: "low" },
        thinking_budget_tokens: { type: "integer", minimum: 1024 },
        fast: {
          type: "boolean",
          default: false,
          description: "Run faster with increased credit usage",
        },
        service_tier: { type: "string", enum: ["flex", "priority"] },
        temperature: { type: "number", minimum: 0, maximum: 2 },
        locked: { type: "string", readOnly: true },
        fixed: { const: "x" },
        nested: { type: "object" },
      },
    },
  };

  it("model option controls exclude thinking keys and unsupported properties", () => {
    const controls = modelOptionControls(codexModel, { fast: true, temperature: 0.5 });
    expect(controls.map((control) => control.key)).toEqual([
      "fast",
      "service_tier",
      "temperature",
    ]);
    expect(controls[0]).toEqual({
      kind: "boolean",
      key: "fast",
      label: "Fast",
      description: "Run faster with increased credit usage",
      selected: true,
      defaultValue: false,
    });
    const tier = controls[1];
    expect(tier?.kind).toBe("choice");
    if (tier?.kind !== "choice") throw new Error("expected a choice control");
    expect(tier.selectedIndex).toBe(-1);
    expect(tier.choices.map((choice) => choice.label)).toEqual(["Flex", "Priority"]);
    const temperature = controls[2];
    expect(temperature?.kind).toBe("text");
    if (temperature?.kind !== "text") throw new Error("expected a text control");
    expect(temperature.text).toBe("0.5");
    expect(temperature.hint).toBe("0 – 2");
    expect(modelOptionControls({ id: "anthropic/claude" })).toEqual([]);
    expect(modelOptionControls(undefined, { fast: true })).toEqual([]);
  });

  it("sanitizing options drops keys the model does not advertise", () => {
    expect(
      sanitizeModelOptions(codexModel, {
        fast: true,
        reasoning_effort: "high",
        service_tier: "gold",
        temperature: 9,
        unknown: "x",
      }),
    ).toEqual({ fast: true });
    expect(sanitizeModelOptions({ id: "anthropic/claude" }, { fast: true })).toEqual({});
    expect(sanitizeModelOptions(codexModel, undefined)).toEqual({});
  });

  it("changing and parsing options keeps the map scalar-only", () => {
    expect(changeModelOption(undefined, "fast", true)).toEqual({ fast: true });
    expect(changeModelOption({ fast: true, temperature: 1 }, "fast", undefined)).toEqual({
      temperature: 1,
    });
    const temperature = modelOptionControls(codexModel)[2] as TextModelOptionControl;
    expect(modelOptionTextValue(temperature, " 1.5 ")).toBe(1.5);
    expect(modelOptionTextValue(temperature, "")).toBeUndefined();
    expect(modelOptionTextValue(temperature, "3")).toBeNull();
    expect(modelOptionTextValue(temperature, "abc")).toBeNull();
  });

  it("option summaries humanize snapshotted values", () => {
    expect(
      modelOptionSummaries({ fast: true, thinking_level: "high", service_tier: "flex" }),
    ).toEqual([
      { key: "fast", label: "Fast", value: "On" },
      { key: "service_tier", label: "Service tier", value: "Flex" },
    ]);
    expect(modelOptionSummaries(undefined)).toEqual([]);
  });
});
