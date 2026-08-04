import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export interface NotificationPreferences {
  /** Master switch. When disabled, every event-specific preference is gated. */
  readonly enabled: boolean;
  readonly onFinish: boolean;
  readonly onFail: boolean;
  readonly onAttention: boolean;
  readonly sound: boolean;
}

export const DEFAULT_NOTIFICATION_PREFERENCES: NotificationPreferences = Object.freeze({
  enabled: true,
  onFinish: true,
  onFail: true,
  onAttention: true,
  sound: false,
});

const STORAGE_KEY = "trouve.notifications.v1";

export interface NotificationPreferenceStorage {
  load(): NotificationPreferences | undefined;
  save(preferences: NotificationPreferences): void;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** Normalize untrusted persisted/host state without allowing truthy strings
 * or partial corruption to silently change the established Slint defaults. */
export const normalizeNotificationPreferences = (
  value: Partial<NotificationPreferences>,
  fallback: NotificationPreferences = DEFAULT_NOTIFICATION_PREFERENCES,
): NotificationPreferences => Object.freeze({
  enabled: typeof value.enabled === "boolean" ? value.enabled : fallback.enabled,
  onFinish: typeof value.onFinish === "boolean" ? value.onFinish : fallback.onFinish,
  onFail: typeof value.onFail === "boolean" ? value.onFail : fallback.onFail,
  onAttention:
    typeof value.onAttention === "boolean" ? value.onAttention : fallback.onAttention,
  sound: typeof value.sound === "boolean" ? value.sound : fallback.sound,
});

export const browserNotificationPreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): NotificationPreferenceStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw === null) return undefined;
      const parsed: unknown = JSON.parse(raw);
      if (!isRecord(parsed)) return undefined;
      return normalizeNotificationPreferences(parsed);
    } catch {
      return undefined;
    }
  },
  save: (preferences) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      // Storage can be restricted or full. The in-memory preference remains
      // authoritative for the current frontend lifetime.
    }
  },
});

export class NotificationPreferencesController {
  readonly #storage: NotificationPreferenceStorage | undefined;
  readonly current = createSignal<NotificationPreferences>(
    DEFAULT_NOTIFICATION_PREFERENCES,
  );

  constructor(storage?: NotificationPreferenceStorage) {
    this.#storage = storage;
    this.current.set(storage?.load() ?? DEFAULT_NOTIFICATION_PREFERENCES);
  }

  replace(
    value: Partial<NotificationPreferences>,
    persist = true,
  ): NotificationPreferences {
    const next = normalizeNotificationPreferences(value, this.current.get());
    this.current.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  update(patch: Partial<NotificationPreferences>): NotificationPreferences {
    return this.replace({ ...this.current.get(), ...patch });
  }
}

export const createBrowserNotificationPreferencesController = (
  persistLocally = true,
) => {
  if (!persistLocally) return new NotificationPreferencesController();
  try {
    return new NotificationPreferencesController(
      browserNotificationPreferenceStorage(globalThis.localStorage),
    );
  } catch {
    return new NotificationPreferencesController();
  }
};

export type NotificationPreferencesSignal = ReadonlySignal<NotificationPreferences>;
