import { describe, expect, it } from "vitest";

import {
  latestCompletedTurnDuration,
  localMemoryUtilization,
  sessionUsagePanelKind,
  usageBreakdownRows,
  usageThroughput,
} from "./session-usage-model.js";

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
