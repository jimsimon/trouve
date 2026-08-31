import { describe, expect, it } from "vitest";

import { threadModeSettingRequest } from "./thread-settings-model.js";

describe("thread setting transitions", () => {
  const modes = [
    { id: "code", default_model: "openai/gpt-5.6" },
    { id: "plan", default_model: "openai/gpt-5.6" },
    { id: "review", default_model: "anthropic/claude-opus-4.1" },
    { id: "custom", default_model: null },
  ];

  it("keeps model options when the next mode uses the current model", () => {
    expect(threadModeSettingRequest(modes, "plan", "openai/gpt-5.6")).toEqual({
      mode: "plan",
      model: "openai/gpt-5.6",
    });
  });

  it("clears model options atomically when a mode changes the model", () => {
    expect(threadModeSettingRequest(modes, "review", "openai/gpt-5.6")).toEqual({
      mode: "review",
      model: "anthropic/claude-opus-4.1",
      model_options: {},
    });
  });

  it("keeps the current model and options when a mode has no default", () => {
    expect(threadModeSettingRequest(modes, "custom", "openai/gpt-5.6")).toEqual({
      mode: "custom",
    });
  });
});
