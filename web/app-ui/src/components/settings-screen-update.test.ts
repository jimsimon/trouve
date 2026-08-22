import { describe, expect, it } from "vitest";

import type { DesktopUpdateState } from "../services/host-client.js";
import {
  desktopUpdateCanRetryInstall,
  desktopUpdateConfirmsInstallAction,
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

  it("keeps polling quickly through stale install status, then backs off progressively", () => {
    const available = {
      ...updateState("Version 4.1.0 is ready to install.", "4.1.0"),
      phase: "available",
    } satisfies DesktopUpdateState;
    expect(desktopUpdatePollIntervalMs(available, 0)).toBe(500);
    expect(desktopUpdatePollIntervalMs(available, 1)).toBe(1_000);
    expect(desktopUpdatePollIntervalMs(available, 20)).toBe(30_000);
  });

  it("requires status evidence from the accepted install before ending reconciliation", () => {
    const retryError = updateState(
      "Update installation failed: archive download stopped",
      "4.1.0",
    );
    expect(desktopUpdateConfirmsInstallAction(retryError, { ...retryError })).toBe(false);
    expect(desktopUpdateConfirmsInstallAction(retryError, {
      ...retryError,
      message: "Update installation failed: checksum mismatch",
    })).toBe(true);
    expect(desktopUpdateConfirmsInstallAction(retryError, {
      ...retryError,
      message: "Installing version 4.1.0…",
      phase: "installing",
    })).toBe(true);
  });
});
