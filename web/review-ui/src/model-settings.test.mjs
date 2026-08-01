import assert from "node:assert/strict";
import test from "node:test";

import {
  defaultThinkingSelection,
  modelForSelection,
  modelSelectionValue,
  supplementalModelSelection,
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
  assert.equal(modelSelectionValue("openai/gpt-5.6-sol"), "openai/gpt-5.6-sol");
  assert.equal(modelSelectionValue("unknown/model"), "unknown/model");
  assert.deepEqual(supplementalModelSelection(models, "openai/gpt-5.6-sol"), {
    value: "openai/gpt-5.6-sol",
    kind: "pinned",
  });
  assert.deepEqual(supplementalModelSelection(models, "unknown/model"), {
    value: "unknown/model",
    kind: "unavailable",
  });
  assert.equal(supplementalModelSelection(models, "gpt-5.6-sol"), undefined);
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
  assert.equal(thinkingLevelLabel("vendor-special"), "vendor-special");
});
