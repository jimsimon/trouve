import assert from "node:assert/strict";
import test from "node:test";

import {
  defaultThinkingSelection,
  modelForSelection,
  modelSelectionValue,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
} from "./model-settings.ts";

test("provider-qualified selections resolve through neutral model routes", () => {
  const models = [{
    id: "gpt-5.6-sol",
    routes: [
      { provider_id: "openai", provider_model: "gpt-5.6-sol" },
      { provider_id: "codex", provider_model: "gpt-5.6-sol" },
    ],
  }];

  assert.equal(modelForSelection(models, "gpt-5.6-sol"), models[0]);
  assert.equal(modelForSelection(models, "codex/gpt-5.6-sol"), models[0]);
  assert.equal(modelForSelection(models, "cursor/gpt-5.6-sol"), undefined);
  assert.equal(modelSelectionValue(models, "openai/gpt-5.6-sol"), "gpt-5.6-sol");
  assert.equal(modelSelectionValue(models, "unknown/model"), "unknown/model");
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
