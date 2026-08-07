import { describe, expect, it } from "vitest";

import {
  composerContextUsage,
  formatSessionUsage,
  formatTokenCount,
} from "./composer-usage.js";

describe("composer usage presentation", () => {
  it("prefers the live provider window and includes cached input", () => {
    expect(composerContextUsage({
      input_tokens: 40_000,
      cached_input_tokens: 10_000,
      context_window: 100_000,
    }, 200_000, false)).toEqual({
      fill: 0.5,
      percent: 50,
      usedTokens: 50_000,
      windowTokens: 100_000,
      unavailable: false,
      compacting: false,
      label: "Context: 50000 / 100000 tokens (50%)",
    });
  });

  it("prefers an explicit current-context measurement over aggregate counters", () => {
    expect(composerContextUsage({
      input_tokens: 140_000,
      cached_input_tokens: 120_000,
      context_input_tokens: 150_000,
      context_window: 300_000,
    }, undefined, false).percent).toBe(50);
  });

  it("does not double-count cached input in legacy Codex usage", () => {
    const context = composerContextUsage({
      input_tokens: 90_606,
      cached_input_tokens: 80_640,
      context_window: 258_400,
    }, undefined, false, true);
    expect(context.usedTokens).toBe(90_606);
    expect(context.percent).toBe(35);
  });

  it("falls back to the catalog window and clamps a full dial", () => {
    const context = composerContextUsage({ input_tokens: 250_000 }, 200_000, true);
    expect(context.fill).toBe(1);
    expect(context.percent).toBe(100);
    expect(context.compacting).toBe(true);
    expect(context.label).toContain("250000 / 200000");
  });

  it("treats an explicitly reported zero window as authoritative", () => {
    const context = composerContextUsage({
      input_tokens: 500,
      context_window: 0,
    }, 200_000, false);
    expect(context.unavailable).toBe(true);
    expect(context.windowTokens).toBeUndefined();
  });

  it("explains when automatic compaction cannot determine a window", () => {
    const context = composerContextUsage({ input_tokens: 1234 }, undefined, false);
    expect(context.unavailable).toBe(true);
    expect(context.windowTokens).toBeUndefined();
    expect(context.label).toContain("1234 tokens");
    expect(context.label).toContain("Automatic compaction is disabled");
  });

  it("formats aggregate token and billed-cost summaries like the desktop app", () => {
    expect(formatTokenCount(999)).toBe("999");
    expect(formatTokenCount(1_200)).toBe("1.2k");
    expect(formatTokenCount(1_250_000)).toBe("1.3M");
    expect(formatSessionUsage({
      turns: 2,
      input_tokens: 12_500,
      output_tokens: 340,
      cached_input_tokens: 5_000,
      cost_usd: 0.0231,
    })).toBe("12.5k in / 340 out · $0.0231");
  });
});
