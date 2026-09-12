import { describe, expect, it } from "vitest";

import type { ProtocolSubscriptionHealth } from "../services/protocol-client.js";
import {
  collapsedUsageSummary,
  latestCompletedTurnDuration,
  localMemoryUtilization,
  sessionUsagePanelKind,
  usageBreakdownRows,
  usagePanelRoute,
  usageThroughput,
} from "./session-usage-model.js";

const health = (
  provider_id: string,
  overrides: Partial<ProtocolSubscriptionHealth> = {},
): ProtocolSubscriptionHealth => ({
  provider_id,
  status: "ok",
  plan: "pro",
  credits: "",
  note: "",
  windows: [{ label: "5h window", used_percent: 10, resets: "" }],
  ...overrides,
});

const route = (overrides: Partial<Parameters<typeof usagePanelRoute>[0]> = {}) =>
  usagePanelRoute({
    model: "auto/claude-fable-5-1",
    turnModels: new Map(),
    threadRoute: undefined,
    candidates: [],
    subscriptions: [],
    ...overrides,
  });

const kind = (overrides: Partial<Parameters<typeof sessionUsagePanelKind>[0]> = {}) =>
  sessionUsagePanelKind({
    placeholder: false,
    sessionId: "se-1",
    threadId: "th-1",
    model: "openai/gpt-5",
    hasSubscriptionHealth: false,
    ...overrides,
  });

describe("session usage panel presentation", () => {
  it("summarizes the most relevant usage for the collapsed footer", () => {
    const summary = (
      overrides: Partial<Parameters<typeof collapsedUsageSummary>[0]> = {},
    ) => collapsedUsageSummary({
      kind: "api",
      loading: false,
      error: "",
      health: undefined,
      sessionCostUsd: undefined,
      localServerStatus: undefined,
      ...overrides,
    });
    expect(summary({ kind: "placeholder" })).toEqual({ text: "No active session", tone: "neutral" });
    expect(summary({
      kind: "subscription",
      health: {
        provider_id: "codex",
        status: "ok",
        plan: "pro",
        credits: "",
        note: "",
        windows: [
          { label: "5h window", used_percent: 12, resets: "resets in 2h" },
          { label: "Weekly", used_percent: 68, resets: "resets in 1d 0h" },
        ],
      },
    })).toEqual({ text: "Pro · 68% used", tone: "ok" });
    expect(summary({ kind: "local", localServerStatus: "running" })).toEqual({ text: "Local · running", tone: "neutral" });
    expect(summary({ sessionCostUsd: 1.234 })).toEqual({ text: "Session · $1.23", tone: "neutral" });
    expect(summary({ loading: true })).toEqual({ text: "Loading usage…", tone: "neutral" });
    expect(summary({ error: "Usage refresh failed." })).toEqual({ text: "Usage refresh failed.", tone: "warning" });
  });

  it("uses placeholder copy for setup and empty-session scopes", () => {
    expect(kind({ placeholder: true })).toBe("placeholder");
    expect(kind({ sessionId: "" })).toBe("placeholder");
    expect(kind({ threadId: "" })).toBe("placeholder");
  });

  it("selects subscription, API, and local presentations from the active model", () => {
    expect(kind({ hasSubscriptionHealth: true })).toBe("subscription");
    expect(kind()).toBe("api");
    expect(kind({ model: "local/qwen-coder", hasSubscriptionHealth: true })).toBe("local");
  });

  it("attributes pinned models to their own provider", () => {
    expect(route({ model: "openai/gpt-5" })).toEqual({
      providerId: "openai",
      providerModel: "gpt-5",
      source: "pinned",
    });
    expect(route({ model: "openai/org/gpt-5" })).toMatchObject({
      providerId: "openai",
      providerModel: "org/gpt-5",
    });
    expect(route({ model: "bare-model" })).toBeUndefined();
  });

  it("attributes automatic models to the latest routed turn, skipping unrouted seeds", () => {
    expect(route({
      turnModels: new Map([
        [1, "codex/gpt-5.6-sol"],
        [2, "claude-code/claude-fable-5-1"],
        [3, "auto/claude-fable-5-1"],
      ]),
      threadRoute: { provider_id: "codex", provider_model: "gpt-5.6-sol" },
    })).toEqual({
      providerId: "claude-code",
      providerModel: "claude-fable-5-1",
      source: "turn",
    });
  });

  it("falls back to the thread's sticky route before any routed turn is known", () => {
    expect(route({
      turnModels: new Map([[1, "auto/claude-fable-5-1"]]),
      threadRoute: { provider_id: "codex", provider_model: "gpt-5.6-sol" },
      candidates: [{ provider_id: "anthropic", provider_model: "claude-fable-5-1" }],
    })).toEqual({
      providerId: "codex",
      providerModel: "gpt-5.6-sol",
      source: "thread",
    });
  });

  it("falls back to the healthiest catalog candidate on a fresh thread", () => {
    const candidates = [
      { provider_id: "anthropic", provider_model: "claude-fable-5-1" },
      { provider_id: "claude-code", provider_model: "claude-fable-5-1" },
      { provider_id: "cursor", provider_model: "claude-fable-5-1" },
    ];
    expect(route({
      candidates,
      subscriptions: [
        health("claude-code", { windows: [{ label: "5h", used_percent: 80, resets: "" }] }),
        health("cursor", { windows: [{ label: "5h", used_percent: 20, resets: "" }] }),
      ],
    })).toEqual({ providerId: "cursor", providerModel: "claude-fable-5-1", source: "candidate" });
    expect(route({
      candidates,
      subscriptions: [health("cursor", { status: "unavailable", windows: [] })],
    })).toMatchObject({ providerId: "anthropic", source: "candidate" });
    expect(route({ candidates: [] })).toBeUndefined();
  });

  it("bounds local memory utilization and derives last-turn throughput", () => {
    expect(localMemoryUtilization(7, 10)).toBe(70);
    expect(localMemoryUtilization(12, 10)).toBe(100);
    expect(localMemoryUtilization(2, 0)).toBe(0);
    expect(usageThroughput(50, 2_000)).toBe(25);
    expect(usageThroughput(50, undefined)).toBeUndefined();
  });

  it("selects the latest duration from a large turn history without variadic arguments", () => {
    const durations = new Map<number, number>();
    for (let turn = 0; turn < 200_000; turn += 1) durations.set(turn, turn * 10);
    durations.set(-1, 999);

    expect(latestCompletedTurnDuration(durations)).toBe(1_999_990);
    expect(latestCompletedTurnDuration(new Map())).toBeUndefined();
  });

  it("breaks usage down by model and adds a total only for multiple models", () => {
    const one = {
      model: "openai/gpt-5",
      turns: 2,
      input_tokens: 10,
      cached_input_tokens: 3,
      output_tokens: 4,
      cost_usd: 0.02,
    };
    expect(usageBreakdownRows({ ...one, models: [one] })).toEqual([
      { ...one, label: "openai/gpt-5", total: false },
    ]);

    const second = { ...one, model: "anthropic/claude", turns: 1 };
    const rows = usageBreakdownRows({ ...one, turns: 3, models: [one, second] });
    expect(rows.map((row) => row.label)).toEqual([
      "openai/gpt-5",
      "anthropic/claude",
      "Total",
    ]);
    expect(rows.at(-1)).toMatchObject({ turns: 3, total: true });
  });
});
