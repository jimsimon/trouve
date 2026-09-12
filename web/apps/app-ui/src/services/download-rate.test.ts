import { describe, expect, it } from "vitest";

import { DownloadRateTracker, formatDownloadRate } from "./download-rate.js";

describe("download rate parity", () => {
  it("waits for two useful samples and exponentially smooths later intervals", () => {
    let now = 0;
    const tracker = new DownloadRateTracker(() => now);
    expect(tracker.update("model", 0)).toBeUndefined();
    now = 1_000;
    expect(tracker.update("model", 1_048_576)).toBe(1_048_576);
    now = 2_000;
    expect(tracker.update("model", 3_145_728)).toBeCloseTo(1_468_006.4);
  });

  it("ignores duplicate near-simultaneous renders and resets a restarted transfer", () => {
    let now = 0;
    const tracker = new DownloadRateTracker(() => now);
    tracker.update("cli", 100);
    now = 1_000;
    expect(tracker.update("cli", 1_100)).toBe(1_000);
    now = 1_050;
    expect(tracker.update("cli", 1_100)).toBe(1_000);
    now = 2_000;
    expect(tracker.update("cli", 10)).toBeUndefined();
  });

  it("uses approachable binary transfer units", () => {
    expect(formatDownloadRate(undefined)).toBe("");
    expect(formatDownloadRate(512)).toBe("512 B/s");
    expect(formatDownloadRate(1_536)).toBe("2 kB/s");
    expect(formatDownloadRate(1_500_000)).toBe("1.5 MB/s");
  });
});
