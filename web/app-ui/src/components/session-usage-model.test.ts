import { describe, expect, it } from "vitest";

import {
  localMemoryUtilization,
  sessionUsagePanelKind,
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
});
