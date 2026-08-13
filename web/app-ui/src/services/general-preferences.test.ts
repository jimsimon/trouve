import { describe, expect, it, vi } from "vitest";

import {
  browserGeneralPreferenceStorage,
  DEFAULT_GENERAL_PREFERENCES,
  GeneralPreferencesController,
  normalizeGeneralPreferences,
} from "./general-preferences.js";

describe("general frontend preferences", () => {
  it("normalizes untrusted state against the product default", () => {
    expect(normalizeGeneralPreferences({ preventSleepWhileRunning: false })).toEqual({
      preventSleepWhileRunning: false,
    });
    expect(normalizeGeneralPreferences({
      preventSleepWhileRunning: "yes" as unknown as boolean,
    })).toEqual(DEFAULT_GENERAL_PREFERENCES);
  });

  it("round-trips browser storage safely", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserGeneralPreferenceStorage(storage);
    adapter.save({ preventSleepWhileRunning: false });
    expect(adapter.load()).toEqual({ preventSleepWhileRunning: false });
    values.set("trouve.general.v1", "null");
    expect(adapter.load()).toBeUndefined();
  });

  it("publishes immutable updates and persists them", () => {
    const storage = { load: () => undefined, save: vi.fn() };
    const controller = new GeneralPreferencesController(storage);
    const next = controller.update({ preventSleepWhileRunning: false });
    expect(Object.isFrozen(next)).toBe(true);
    expect(storage.save).toHaveBeenCalledWith(next);
  });
});
