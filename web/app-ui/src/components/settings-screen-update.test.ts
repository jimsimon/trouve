import { describe, expect, it } from "vitest";

import type { DesktopUpdateState } from "../services/host-client.js";
import {
  desktopUpdateCanRetryInstall,
  desktopUpdatePollIntervalMs,
} from "./settings-screen.js";

const updateState = (
  message: string,
  availableVersion: string | undefined,
): DesktopUpdateState => ({
  availableVersion,
  currentVersion: "4.0.0",
  message,
  phase: "error",
  progressPercent: undefined,
});

describe("desktop update retry action", () => {
  it("retries installation after installation or restart failures", () => {
    expect(desktopUpdateCanRetryInstall(updateState(
      "Update installation failed: archive download stopped",
      "4.1.0",
    ))).toBe(true);
    expect(desktopUpdateCanRetryInstall(updateState(
      "Version 4.1.0 is installed, but trouve could not restart: launch failed",
      "4.1.0",
    ))).toBe(true);
  });

  it("checks again after check failures or when no release is retained", () => {
    expect(desktopUpdateCanRetryInstall(updateState(
      "Update check failed: offline",
      "4.1.0",
    ))).toBe(false);
    expect(desktopUpdateCanRetryInstall(updateState(
      "Update installation failed: no release",
      undefined,
    ))).toBe(false);
  });
});

describe("desktop update status polling", () => {
  it("polls active work quickly and settled states at a continuing low frequency", () => {
    expect(desktopUpdatePollIntervalMs({
      ...updateState("Downloading", "4.1.0"),
      phase: "downloading",
    })).toBe(500);
    expect(desktopUpdatePollIntervalMs({
      ...updateState("Up to date", undefined),
      phase: "idle",
    })).toBe(30_000);
  });
});
