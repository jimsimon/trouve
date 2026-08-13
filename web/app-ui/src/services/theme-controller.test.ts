import { describe, expect, it, vi } from "vitest";

import {
  browserThemePreferenceStorage,
  ThemeController,
  type ThemePreference,
} from "./theme-controller.js";
import { readSignal } from "../state/reactivity.js";

describe("ThemeController", () => {
  it("tracks the system preference without replacing explicit accessible themes", () => {
    let listener: ((event: { matches: boolean }) => void) | undefined;
    const controller = new ThemeController({
      darkQuery: {
        matches: false,
        addEventListener: (_type, next) => {
          listener = next;
        },
      },
    });
    expect(readSignal(controller.theme)).toBe("light");
    listener?.({ matches: true });
    expect(readSignal(controller.theme)).toBe("dark");
    controller.setPreference("colorblind-light");
    listener?.({ matches: true });
    expect(readSignal(controller.theme)).toBe("colorblind-light");
  });

  it("persists only a validated nonsecret preference", () => {
    let value: string | null = "not-a-theme";
    const storage = browserThemePreferenceStorage({
      getItem: () => value,
      setItem: vi.fn((_key: string, next: string) => {
        value = next;
      }),
    });
    expect(storage.load()).toBeUndefined();
    const preference: ThemePreference = "high-contrast-dark";
    storage.save(preference);
    expect(storage.load()).toBe(preference);
  });
});
