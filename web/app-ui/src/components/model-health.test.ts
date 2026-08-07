import { describe, expect, it } from "vitest";

import {
  boundedSubscriptionUsage,
  filteredModelIndices,
  modelHealthPresentation,
  subscriptionUsageTone,
} from "./model-health.js";

describe("model health presentation", () => {
  it("matches the highest-window summary and warning tones", () => {
    const health = modelHealthPresentation({
      provider_id: "codex",
      status: "ok",
      plan: "pro",
      windows: [
        { label: "5-hour", used_percent: 42, resets: "resets in 1h" },
        { label: "Weekly", used_percent: 76, resets: "resets Monday" },
      ],
      credits: "",
      note: "",
    });
    expect(health.summary).toBe("Pro · 76% used");
    expect(health.tone).toBe("warning");
    expect(health.detail).toContain("Weekly: 76% used · resets Monday");
  });

  it("distinguishes API billing from login-required health", () => {
    expect(modelHealthPresentation({
      provider_id: "cursor",
      status: "unsupported",
      plan: "",
      windows: [],
      credits: "",
      note: "usage-billed via API key",
    }).summary).toBe("API billed");
    expect(modelHealthPresentation({
      provider_id: "claude-code",
      status: "unavailable",
      plan: "",
      windows: [],
      credits: "",
      note: "subscription usage needs a login",
    })).toMatchObject({ summary: "login required", tone: "error" });
  });

  it("uses the healthy, warning, and exhausted meter thresholds", () => {
    expect(subscriptionUsageTone(0)).toBe("ok");
    expect(subscriptionUsageTone(69)).toBe("ok");
    expect(subscriptionUsageTone(70)).toBe("warning");
    expect(subscriptionUsageTone(89)).toBe("warning");
    expect(subscriptionUsageTone(90)).toBe("error");
    expect(boundedSubscriptionUsage(-20)).toBe(0);
    expect(boundedSubscriptionUsage(120)).toBe(100);
  });

  it("ranks prefix, contained, and subsequence model matches with a DOM bound", () => {
    const models = [
      { id: "openai/gpt-5", display_name: "GPT-5" },
      { id: "anthropic/claude-sonnet", display_name: "Claude Sonnet" },
      { id: "local/granite", display_name: "Granite" },
    ] as never;
    expect(filteredModelIndices(models, "cla", 2)).toEqual([1, 2]);
    expect(filteredModelIndices(models, "g5", 2)).toEqual([0]);
    expect(filteredModelIndices(models, "", 2)).toEqual([0, 1]);
  });
});
