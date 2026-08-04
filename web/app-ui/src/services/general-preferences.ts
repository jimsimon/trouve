import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export interface GeneralPreferences {
  readonly preventSleepWhileRunning: boolean;
}

export const DEFAULT_GENERAL_PREFERENCES: GeneralPreferences = Object.freeze({
  preventSleepWhileRunning: true,
});

const STORAGE_KEY = "trouve.general.v1";

export interface GeneralPreferenceStorage {
  load(): GeneralPreferences | undefined;
  save(preferences: GeneralPreferences): void;
}

export const normalizeGeneralPreferences = (
  value: Partial<GeneralPreferences>,
  fallback: GeneralPreferences = DEFAULT_GENERAL_PREFERENCES,
): GeneralPreferences => Object.freeze({
  preventSleepWhileRunning:
    typeof value.preventSleepWhileRunning === "boolean"
      ? value.preventSleepWhileRunning
      : fallback.preventSleepWhileRunning,
});

export const browserGeneralPreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): GeneralPreferenceStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw === null) return undefined;
      const parsed: unknown = JSON.parse(raw);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return undefined;
      }
      return normalizeGeneralPreferences(parsed as Partial<GeneralPreferences>);
    } catch {
      return undefined;
    }
  },
  save: (preferences) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      // Preference remains effective for this frontend lifetime.
    }
  },
});

export class GeneralPreferencesController {
  readonly #storage: GeneralPreferenceStorage | undefined;
  readonly current = createSignal<GeneralPreferences>(DEFAULT_GENERAL_PREFERENCES);

  constructor(storage?: GeneralPreferenceStorage) {
    this.#storage = storage;
    this.current.set(storage?.load() ?? DEFAULT_GENERAL_PREFERENCES);
  }

  replace(value: Partial<GeneralPreferences>, persist = true): GeneralPreferences {
    const next = normalizeGeneralPreferences(value, this.current.get());
    this.current.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  update(patch: Partial<GeneralPreferences>): GeneralPreferences {
    return this.replace({ ...this.current.get(), ...patch });
  }
}

export const createBrowserGeneralPreferencesController = (
  persistLocally = true,
) => {
  if (!persistLocally) return new GeneralPreferencesController();
  try {
    return new GeneralPreferencesController(
      browserGeneralPreferenceStorage(globalThis.localStorage),
    );
  } catch {
    return new GeneralPreferencesController();
  }
};

export type GeneralPreferencesSignal = ReadonlySignal<GeneralPreferences>;
