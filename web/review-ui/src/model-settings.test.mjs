import assert from "node:assert/strict";
import test from "node:test";

import {
  changeModelOption,
  defaultThinkingSelection,
  modelCatalogStatusMessage,
  modelForSelection,
  modelSelectionValue,
  supplementalModelSelection,
  modelOptionControls,
  modelOptionSummaries,
  modelOptionTextValue,
  sanitizeModelOptions,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
} from "./model-settings.ts";

test("model catalog status keeps refresh failures visible after initial load", () => {
  assert.equal(
    modelCatalogStatusMessage(true, "refresh failed"),
    "Model choices may be stale: refresh failed",
  );
  assert.equal(modelCatalogStatusMessage(true, ""), undefined);
  assert.match(modelCatalogStatusMessage(false, "") ?? "", /still loading/u);
});

test("automatic aliases and provider pins resolve to their exact picker rows", () => {
  const route = (provider_id) => ({ provider_id, provider_model: "gpt-5.6-sol" });
  const models = [
    { id: "auto/gpt-5.6-sol", routes: [route("openai"), route("codex")] },
    { id: "codex/gpt-5.6-sol", routes: [route("codex")] },
    { id: "openai/gpt-5.6-sol", routes: [route("openai")] },
  ];

  assert.equal(modelForSelection(models, "gpt-5.6-sol"), models[0]);
  assert.equal(modelForSelection(models, "auto/gpt-5.6-sol"), models[0]);
  assert.equal(modelForSelection(models, "codex/gpt-5.6-sol"), models[1]);
  assert.equal(modelForSelection(models, "cursor/gpt-5.6-sol"), undefined);
  assert.equal(modelSelectionValue(models, "gpt-5.6-sol"), "auto/gpt-5.6-sol");
  assert.equal(modelSelectionValue(models, "openai/gpt-5.6-sol"), "openai/gpt-5.6-sol");
  assert.equal(modelSelectionValue(models, "unknown/model"), "unknown/model");
  assert.equal(supplementalModelSelection(models, "openai/gpt-5.6-sol"), undefined);
  assert.deepEqual(supplementalModelSelection(models, "unknown/model"), {
    value: "unknown/model",
    kind: "unavailable",
  });
  assert.equal(supplementalModelSelection(models, "gpt-5.6-sol"), undefined);
});

test("a provider pin remains visible against an older automatic-only catalog", () => {
  const models = [{
    id: "auto/gpt-5.6-sol",
    routes: [{ provider_id: "codex", provider_model: "gpt-5.6-sol" }],
  }];
  assert.deepEqual(supplementalModelSelection(models, "codex/gpt-5.6-sol"), {
    value: "codex/gpt-5.6-sol",
    kind: "pinned",
  });
});

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

test("model option controls exclude thinking keys and unsupported properties", () => {
  const controls = modelOptionControls(codexModel, { fast: true, temperature: 0.5 });
  assert.deepEqual(controls.map((control) => control.key), [
    "fast",
    "service_tier",
    "temperature",
  ]);
  assert.deepEqual(controls[0], {
    kind: "boolean",
    key: "fast",
    label: "Fast",
    description: "Run faster with increased credit usage",
    selected: true,
    defaultValue: false,
  });
  assert.equal(controls[1].kind, "choice");
  assert.equal(controls[1].selectedIndex, -1);
  assert.deepEqual(controls[1].choices.map((choice) => choice.label), ["Flex", "Priority"]);
  assert.equal(controls[2].kind, "text");
  assert.equal(controls[2].text, "0.5");
  assert.equal(controls[2].hint, "0 – 2");
  assert.deepEqual(modelOptionControls({ id: "anthropic/claude" }), []);
  assert.deepEqual(modelOptionControls(undefined, { fast: true }), []);
});

test("sanitizing options drops keys the model does not advertise", () => {
  assert.deepEqual(
    sanitizeModelOptions(codexModel, {
      fast: true,
      reasoning_effort: "high",
      service_tier: "gold",
      temperature: 9,
      unknown: "x",
    }),
    { fast: true },
  );
  assert.deepEqual(sanitizeModelOptions({ id: "anthropic/claude" }, { fast: true }), {});
  assert.deepEqual(sanitizeModelOptions(codexModel, undefined), {});
});

test("changing and parsing options keeps the map scalar-only", () => {
  assert.deepEqual(changeModelOption(undefined, "fast", true), { fast: true });
  assert.deepEqual(changeModelOption({ fast: true, temperature: 1 }, "fast", undefined), {
    temperature: 1,
  });
  const [, , temperature] = modelOptionControls(codexModel);
  assert.equal(modelOptionTextValue(temperature, " 1.5 "), 1.5);
  assert.equal(modelOptionTextValue(temperature, ""), undefined);
  assert.equal(modelOptionTextValue(temperature, "3"), null);
  assert.equal(modelOptionTextValue(temperature, "abc"), null);
});

test("option summaries humanize snapshotted values", () => {
  assert.deepEqual(
    modelOptionSummaries({ fast: true, thinking_level: "high", service_tier: "flex" }),
    [
      { key: "fast", label: "Fast", value: "On" },
      { key: "service_tier", label: "Service tier", value: "Flex" },
    ],
  );
  assert.deepEqual(modelOptionSummaries(undefined), []);
});

test("thinking options follow the model-advertised schema key", () => {
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

  assert.deepEqual(thinkingOptions(model), {
    values: ["low", "medium", "high", "xhigh"],
    defaultValue: "medium",
  });
  assert.equal(defaultThinkingSelection(model), "medium");
  assert.equal(defaultThinkingSelection(model, "high"), "high");
  assert.equal(defaultThinkingSelection(model, "unsupported"), "medium");
});

test("models without an advertised thinking enum have no thinking selector", () => {
  assert.deepEqual(thinkingOptions({
    id: "openai/plain",
    options_schema: { properties: { temperature: { type: "number" } } },
  }), { values: [] });
  assert.equal(defaultThinkingSelection(undefined, "high"), "");
});

test("enum-backed thinking without a declared default selects its first value", () => {
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

  assert.equal(defaultThinkingSelection(model), "low");
});

test("fixed thinking budgets follow advertised numeric bounds", () => {
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
  assert.deepEqual(thinkingOptions(model), {
    values: [],
    defaultValue: "4096",
    budget: { minimum: 1024, maximum: 32768 },
  });
  assert.equal(thinkingSelectionIsValid(model, "16384"), true);
  assert.equal(thinkingSelectionIsValid(model, "512"), false);
  assert.equal(defaultThinkingSelection(model), "4096");
});

test("thinking labels make provider tokens readable", () => {
  assert.equal(thinkingLevelLabel("xhigh"), "Extra High");
  assert.equal(thinkingLevelLabel("ultra"), "Ultra");
  assert.equal(thinkingLevelLabel("vendor-special"), "vendor-special");
});
