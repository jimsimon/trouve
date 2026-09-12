import { describe, expect, it } from "vitest";

import { liveAgentActivity } from "./agent-activity.js";

describe("live agent activity", () => {
  it("uses the current render time instead of a stale parent timestamp", () => {
    const startedAt = "2026-07-31T16:00:00.000Z";
    const startedMs = Date.parse(startedAt);
    expect(liveAgentActivity({
      items: [],
      turnRunning: true,
      thinking: false,
      compacting: false,
      turnModels: new Map([[3, "codex/gpt-5.6-sol"]]),
      turnStartedAt: new Map([[3, startedAt]]),
      nowMs: startedMs + 180_000,
    }, startedMs + 1_000)?.label).toBe("Starting gpt-5.6-sol…");
  });
});
