import { describe, expect, it, vi } from "vitest";

import {
  browserNotificationPreferenceStorage,
  DEFAULT_NOTIFICATION_PREFERENCES,
  normalizeNotificationPreferences,
  NotificationPreferencesController,
} from "./notification-preferences.js";

describe("notification preferences", () => {
  it("retains defaults for malformed or missing fields", () => {
    expect(normalizeNotificationPreferences({
      enabled: false,
      onAttention: false,
    })).toEqual({
      enabled: false,
      onFinish: true,
      onFail: true,
      onAttention: false,
      sound: false,
    });

    expect(normalizeNotificationPreferences({
      enabled: "yes" as unknown as boolean,
      sound: 1 as unknown as boolean,
    })).toEqual(DEFAULT_NOTIFICATION_PREFERENCES);
  });

  it("round-trips browser storage and rejects invalid payloads", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserNotificationPreferenceStorage(storage);
    adapter.save({
      enabled: true,
      onFinish: false,
      onFail: true,
      onAttention: false,
      sound: true,
    });
    expect(adapter.load()).toEqual({
      enabled: true,
      onFinish: false,
      onFail: true,
      onAttention: false,
      sound: true,
    });

    values.set("trouve.notifications.v1", "[]");
    expect(adapter.load()).toBeUndefined();
  });

  it("publishes immutable updates and persists them", () => {
    const storage = {
      load: () => undefined,
      save: vi.fn(),
    };
    const controller = new NotificationPreferencesController(storage);
    const next = controller.update({ enabled: false, sound: true });
    expect(Object.isFrozen(next)).toBe(true);
    expect(next.enabled).toBe(false);
    expect(next.sound).toBe(true);
    expect(controller.current.get()).toBe(next);
    expect(storage.save).toHaveBeenCalledWith(next);
  });
});
