import { describe, expect, it } from "vitest";

import { ConfirmedWriteTracker } from "./confirmed-write-tracker.js";

describe("ConfirmedWriteTracker", () => {
  it("rolls overlapping failures back to the loaded host snapshot", () => {
    const tracker = new ConfirmedWriteTracker<string>();
    tracker.load("host");
    const first = tracker.begin();
    const second = tracker.begin();

    expect(tracker.fail(first).current).toBe(false);
    expect(tracker.fail(second)).toEqual({ current: true, confirmed: "host" });
  });

  it("retains an earlier confirmed write when a newer write fails", () => {
    const tracker = new ConfirmedWriteTracker<string>();
    tracker.load("loaded");
    const first = tracker.begin();
    const second = tracker.begin();

    expect(tracker.succeed(first, "saved first")).toBe(false);
    expect(tracker.fail(second)).toEqual({
      current: true,
      confirmed: "saved first",
    });
  });
});
