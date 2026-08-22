import { describe, expect, it } from "vitest";

import {
  boundedSubscriptionUsage,
  filteredModelIndices,
  modelHealthPresentation,
  modelHealthPresentations,
  subscriptionUsageTone,
} from "./model-health.js";
import type { ProtocolModelInfo } from "../services/protocol-client.js";

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

  it("uses the healthiest concrete route for an automatic model", () => {
    const model = {
      id: "auto/gpt-5.6-sol",
      display_name: "GPT-5.6 Sol",
      context_window: 128_000,
      options_schema: {},
      supports_tools: true,
      routes: [
        { provider_id: "codex", provider_model: "gpt-5.6-sol" },
        { provider_id: "cursor", provider_model: "gpt-5.6-sol" },
      ],
    };
    const presentations = modelHealthPresentations([model], [
      {
        provider_id: "codex",
        status: "unavailable",
        plan: "",
        windows: [],
        credits: "",
        note: "login required",
      },
      {
        provider_id: "cursor",
        status: "ok",
        plan: "pro",
        windows: [{ label: "Monthly", used_percent: 25, resets: "" }],
        credits: "",
        note: "",
      },
    ]);

    expect(presentations[0]).toMatchObject({
      summary: "Pro · 25% used",
      tone: "ok",
    });
    expect(presentations[0]?.detail).toContain("cursor · Pro");
  });

  it("ranks prefix, contained, and subsequence model matches with a DOM bound", () => {
    const model = (id: string, displayName: string): ProtocolModelInfo => ({
      id,
      display_name: displayName,
      context_window: 128_000,
      options_schema: {},
      supports_tools: true,
    });
    const models: readonly ProtocolModelInfo[] = [
      model("openai/gpt-5", "GPT-5"),
      model("anthropic/claude-sonnet", "Claude Sonnet"),
      model("local/granite", "Granite"),
    ];
    expect(filteredModelIndices(models, "cla", 2)).toEqual([1, 2]);
    expect(filteredModelIndices(models, "g5", 2)).toEqual([0]);
    expect(filteredModelIndices(models, "", 2)).toEqual([0, 1]);
  });
});
