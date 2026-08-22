import { describe, expect, it } from "vitest";

import type { ProtocolModelInfo } from "../services/protocol-client.js";
import {
  changeModelOption,
  modelOptionControls,
  modelOptionLabel,
  modelSelectorLabel,
  sanitizeModelOptions,
} from "./model-option-controls.js";

const model = (properties: Record<string, unknown>): ProtocolModelInfo => ({
  id: "provider/model",
  display_name: "Model",
  context_window: 128_000,
  supports_tools: true,
  options_schema: { type: "object", properties },
});

describe("model option controls", () => {
  it("derives every supported scalar control from the advertised schema", () => {
    const controls = modelOptionControls(model({
      reasoning_effort: {
        type: "string",
        enum: ["low", "high", "xhigh"],
        default: "high",
      },
      context: {
        oneOf: [
          { const: "300k", title: "Standard" },
          { const: "1m", title: "Extended" },
        ],
        default: "300k",
      },
      fast: { type: "boolean", default: true },
      temperature: {
        type: "number",
        title: "Temperature",
        description: "Sampling temperature",
        minimum: 0,
        maximum: 2,
        default: 0.5,
      },
      seed: { type: "integer", default: 7 },
      instructions: { type: ["string", "null"], examples: ["Be concise"] },
    }), {
      reasoning_effort: "xhigh",
      context: "1m",
      fast: false,
      temperature: 0.8,
    });

    expect(controls).toEqual([
      {
        kind: "choice",
        key: "reasoning_effort",
        label: "Reasoning effort",
        description: "",
        overridden: true,
        choices: [
          { label: "Low", value: "low" },
          { label: "High", value: "high" },
          { label: "Extra High", value: "xhigh" },
        ],
        selectedIndex: 2,
      },
      {
        kind: "choice",
        key: "context",
        label: "Context",
        description: "",
        overridden: true,
        choices: [
          { label: "Standard", value: "300k" },
          { label: "Extended", value: "1m" },
        ],
        selectedIndex: 1,
      },
      {
        kind: "boolean",
        key: "fast",
        label: "Fast",
        description: "",
        overridden: true,
        selected: false,
      },
      {
        kind: "text",
        key: "temperature",
        label: "Temperature",
        description: "Sampling temperature",
        overridden: true,
        scalarType: "number",
        text: "0.8",
        hint: "0 – 2",
        minimum: 0,
        maximum: 2,
      },
      {
        kind: "text",
        key: "seed",
        label: "Seed",
        description: "",
        overridden: false,
        scalarType: "integer",
        text: "7",
        hint: "value",
      },
      {
        kind: "text",
        key: "instructions",
        label: "Instructions",
        description: "",
        overridden: false,
        scalarType: "string",
        text: "",
        hint: "Be concise",
      },
    ]);
  });

  it("uses schema defaults for display and translates the legacy thinking key", () => {
    const controls = modelOptionControls(model({
      effort: { enum: ["low", "high"], default: "high" },
      context: { enum: ["300k", "1m"], default: "1m" },
      fast: { type: "boolean", default: true },
    }), { thinking_level: "low" });
    expect(controls).toMatchObject([
      { kind: "choice", selectedIndex: 0, overridden: true },
      { kind: "choice", selectedIndex: 1, overridden: false },
      { kind: "boolean", selected: true, overridden: false },
    ]);
    expect(sanitizeModelOptions(model({
      effort: { enum: ["low", "high"] },
    }), { thinking_level: "high" })).toEqual({ effort: "high" });
    expect(modelOptionControls(model({
      reasoning_effort: { enum: ["low", "high"], default: "low" },
    }), { thinking_level: "high", reasoning_effort: "invalid" })[0]).toMatchObject({
      selectedIndex: 0,
    });
    expect(modelOptionControls(model({
      reasoning_effort: { enum: ["low", "high"], default: "low" },
    }), {}, { thinking_level: "high" })[0]).toMatchObject({
      selectedIndex: 1,
      overridden: false,
    });
    expect(sanitizeModelOptions(model({
      reasoning_effort: { enum: ["low", "high"] },
    }), { thinking_level: "high", reasoning_effort: "invalid" })).toEqual({
      reasoning_effort: "high",
    });
  });

  it("sanitizes values and ignores malformed or non-editable controls", () => {
    expect(modelOptionControls(model({
      thinking_level: { enum: ["high"] },
      context: { enum: ["300k", 1] },
      fixed: { const: "locked" },
      output: { type: "string", readOnly: true },
      nested: { type: "object" },
      broken: { type: "string", enum: ["valid", { nested: true }] },
      incomplete: { type: "string", oneOf: [{ const: "only" }] },
      ambiguous: { type: ["string", "number"] },
      patterned: { type: "string", pattern: "^[a-z]+$" },
      stepped: { type: "number", multipleOf: 0.5 },
      malformed_bound: { type: "number", minimum: "zero" },
    }), {})).toEqual([{
      kind: "choice",
      key: "context",
      label: "Context",
      description: "",
      overridden: false,
      choices: [
        { label: "300K", value: "300k" },
        { label: "1", value: 1 },
      ],
      selectedIndex: -1,
    }]);

    const advertised = model({
      effort: { enum: ["low", "high"] },
      fast: { type: "boolean" },
      temperature: { type: "number", minimum: 0, maximum: 1 },
    });
    expect(sanitizeModelOptions(advertised, {
      effort: "missing",
      fast: true,
      temperature: 2,
      unknown: "value",
    })).toEqual({ fast: true });
  });

  it("applies and removes overrides without retaining a duplicate legacy key", () => {
    expect(changeModelOption(
      { thinking_level: "low", fast: true },
      { key: "reasoning_effort", value: "high" },
    )).toEqual({ reasoning_effort: "high", fast: true });
    expect(changeModelOption(
      { reasoning_effort: "high", fast: true },
      { key: "fast", value: undefined },
    )).toEqual({ reasoning_effort: "high" });
    const budgetModel = model({
      thinking_budget_tokens: {
        type: "integer",
        minimum: 1024,
        maximum: 32768,
      },
    });
    expect(sanitizeModelOptions(budgetModel, {
      thinking_budget_tokens: 8192,
      thinking_level: "16384",
    })).toEqual({ thinking_budget_tokens: 8192 });
    expect(sanitizeModelOptions(budgetModel, {
      thinking_level: "16384",
    })).toEqual({ thinking_budget_tokens: 16384 });
    expect(changeModelOption(
      { thinking_level: "16384" },
      { key: "thinking_budget_tokens", value: 8192 },
    )).toEqual({ thinking_budget_tokens: 8192 });
  });

  it("uses the same human labels as the desktop controls", () => {
    expect(modelOptionLabel("xhigh")).toBe("Extra High");
    expect(modelOptionLabel("ultra")).toBe("Ultra");
    expect(modelOptionLabel("custom")).toBe("custom");
  });

  it("uses provider-qualified ids in every model selector", () => {
    expect(modelSelectorLabel({ id: "codex/gpt-5.6-sol" })).toBe(
      "codex/gpt-5.6-sol",
    );
  });
});
