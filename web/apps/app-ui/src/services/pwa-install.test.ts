import { describe, expect, it, vi } from "vitest";

import {
  isStandalonePwa,
  requestPwaInstall,
  type PwaInstallPromptEvent,
} from "./pwa-install.js";

const promptEvent = (
  outcome: "accepted" | "dismissed",
  prompt = vi.fn(async () => undefined),
): PwaInstallPromptEvent => ({
  prompt,
  userChoice: Promise.resolve({ outcome }),
} as unknown as PwaInstallPromptEvent);

describe("PWA installation", () => {
  it("reports the browser choice after one user-initiated prompt", async () => {
    const event = promptEvent("accepted");

    await expect(requestPwaInstall(event)).resolves.toBe("accepted");
    expect(event.prompt).toHaveBeenCalledTimes(1);
  });

  it("keeps dismissal distinct from browser failure", async () => {
    await expect(requestPwaInstall(promptEvent("dismissed"))).resolves.toBe("dismissed");
    await expect(requestPwaInstall(promptEvent(
      "accepted",
      vi.fn(async () => { throw new Error("not allowed"); }),
    ))).resolves.toBe("failed");
  });

  it("recognizes standard and Safari standalone modes", () => {
    expect(isStandalonePwa({
      matchMedia: () => ({ matches: true }),
      navigator: {} as Navigator,
    })).toBe(true);
    expect(isStandalonePwa({
      matchMedia: () => ({ matches: false }),
      navigator: { standalone: true } as Navigator & { standalone: boolean },
    })).toBe(true);
    expect(isStandalonePwa({
      matchMedia: () => ({ matches: false }),
      navigator: {} as Navigator,
    })).toBe(false);
  });
});
