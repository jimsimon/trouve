import { describe, expect, it } from "vitest";

import { liveTurnDurationMs, turnMetadataText } from "./turn-metadata.js";

describe("turn metadata", () => {
  it("derives a bounded live duration from the turn start timestamp", () => {
    expect(liveTurnDurationMs(
      "2026-08-01T12:00:00Z",
      Date.parse("2026-08-01T12:01:05Z"),
    )).toBe(65_000);
    expect(liveTurnDurationMs(
      "2026-08-01T12:00:05Z",
      Date.parse("2026-08-01T12:00:00Z"),
    )).toBe(0);
    expect(liveTurnDurationMs("not-a-date", 0)).toBeUndefined();
  });

  it("shows elapsed time immediately and adds live token usage when available", () => {
    expect(turnMetadataText(undefined, 65_000)).toBe("1m 05s");
    expect(turnMetadataText(
      { input_tokens: 1_234, output_tokens: 56, cost_usd: 0.125 },
      65_000,
    )).toBe("1234 in / 56 out tokens · $0.1250 · 1m 05s");
  });
});
