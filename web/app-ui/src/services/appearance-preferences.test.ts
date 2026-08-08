import { describe, expect, it, vi } from "vitest";

import {
  AppearancePreferencesController,
  appearanceFontFamilyCssValue,
  browserAppearancePreferenceStorage,
  DEFAULT_APPEARANCE_PREFERENCES,
  normalizeAppearancePreferences,
} from "./appearance-preferences.js";

describe("appearance preferences", () => {
  it("normalizes supported values and rejects CSS-shaped font input", () => {
    expect(normalizeAppearancePreferences({
      fontFamily: "  Noto Sans  ",
      fontSize: 16,
      reduceMotion: true,
    })).toEqual({ fontFamily: "Noto Sans", fontSize: 16, reduceMotion: true });

    expect(normalizeAppearancePreferences({
      fontFamily: "M+ 1m (UI)",
    })).toMatchObject({ fontFamily: "M+ 1m (UI)" });

    expect(normalizeAppearancePreferences({
      fontFamily: "sans-serif; color: red",
      fontSize: 17,
    })).toEqual(DEFAULT_APPEARANCE_PREFERENCES);
  });

  it("escapes an installed family as one CSS font-family name", () => {
    expect(appearanceFontFamilyCssValue('A "Quoted" \\ Font')).toBe(
      '"A \\"Quoted\\" \\\\ Font"',
    );
  });

  it("loads and saves browser preferences without trusting malformed state", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserAppearancePreferenceStorage(storage);
    expect(adapter.load()).toBeUndefined();
    adapter.save({ fontFamily: "Fira Sans", fontSize: 14, reduceMotion: true });
    expect(adapter.load()).toEqual({
      fontFamily: "Fira Sans",
      fontSize: 14,
      reduceMotion: true,
    });

    values.set("trouve.appearance.v1", "not-json");
    expect(adapter.load()).toBeUndefined();
  });

  it("publishes immutable updates and delegates persistence", () => {
    const storage = {
      load: () => ({ fontFamily: "", fontSize: 12, reduceMotion: false }),
      save: vi.fn(),
    };
    const controller = new AppearancePreferencesController(storage);
    const next = controller.update({ reduceMotion: true });
    expect(controller.current.get()).toEqual({
      fontFamily: "",
      fontSize: 12,
      reduceMotion: true,
    });
    expect(Object.isFrozen(next)).toBe(true);
    expect(storage.save).toHaveBeenCalledWith(next);
  });
});
