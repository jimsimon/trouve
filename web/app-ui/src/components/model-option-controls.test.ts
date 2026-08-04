import { describe, expect, it } from "vitest";

import type { ProtocolModelInfo } from "../services/protocol-client.js";
import { modelOptionControls, modelOptionLabel } from "./model-option-controls.js";

const model = (properties: Record<string, unknown>): ProtocolModelInfo => ({
  id: "provider/model",
  display_name: "Model",
  context_window: 128_000,
  supports_tools: true,
  options_schema: { type: "object", properties },
});

describe("model option controls", () => {
  it("derives thinking, context, and fast from the advertised schema", () => {
    expect(modelOptionControls(model({
      reasoning_effort: {
        type: "string",
        enum: ["low", "high", "xhigh"],
        default: "high",
      },
      context: { type: "string", enum: ["300k", "1m"], default: "300k" },
      fast: { type: "boolean", default: true },
    }), {
      reasoning_effort: "xhigh",
      context: "1m",
      fast: false,
    })).toEqual({
      thinking: {
        key: "reasoning_effort",
        values: ["low", "high", "xhigh"],
        selected: "xhigh",
      },
      context: { key: "context", values: ["300k", "1m"], selected: "1m" },
      fast: { key: "fast", selected: false },
    });
  });

  it("uses valid schema defaults and the legacy thinking key", () => {
    const controls = modelOptionControls(model({
      effort: { enum: ["low", "high"], default: "high" },
      context: { enum: ["300k", "1m"], default: "1m" },
      fast: { type: "boolean", default: true },
    }), { thinking_level: "low" });
    expect(controls.thinking?.selected).toBe("low");
    expect(controls.context?.selected).toBe("1m");
    expect(controls.fast?.selected).toBe(true);
  });

  it("ignores malformed and single-choice controls", () => {
    expect(modelOptionControls(model({
      thinking_level: { enum: ["high"] },
      context: { enum: ["300k", 1] },
    }), {})).toEqual({});
  });

  it("uses the same human labels as the desktop controls", () => {
    expect(modelOptionLabel("xhigh")).toBe("Extra High");
    expect(modelOptionLabel("custom")).toBe("custom");
  });
});
