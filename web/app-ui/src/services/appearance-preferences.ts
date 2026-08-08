import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export const APPEARANCE_FONT_SIZES = [11, 12, 13, 14, 15, 16, 18] as const;
export const DEFAULT_APPEARANCE_PREFERENCES: AppearancePreferences = Object.freeze({
  fontFamily: "",
  fontSize: 13,
  reduceMotion: false,
});

const STORAGE_KEY = "trouve.appearance.v1";
export const MAX_APPEARANCE_FONT_FAMILY_LENGTH = 256;
const UNSAFE_FONT_FAMILY = /[\u0000-\u001f\u007f-\u009f;{}]/u;

export interface AppearancePreferences {
  /** Empty means the platform's existing UI font stack. */
  readonly fontFamily: string;
  readonly fontSize: number;
  readonly reduceMotion: boolean;
}

export interface AppearancePreferenceStorage {
  load(): AppearancePreferences | undefined;
  save(preferences: AppearancePreferences): void;
}

export const isAppearanceFontSize = (value: number): boolean =>
  APPEARANCE_FONT_SIZES.includes(value as (typeof APPEARANCE_FONT_SIZES)[number]);

export const isAppearanceFontFamily = (value: string): boolean =>
  value.length <= MAX_APPEARANCE_FONT_FAMILY_LENGTH && !UNSAFE_FONT_FAMILY.test(value);

/** A selected family is inserted into `font-family` as one escaped CSS name. */
export const appearanceFontFamilyCssValue = (value: string): string =>
  `"${value.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;

export const normalizeAppearancePreferences = (
  value: Partial<AppearancePreferences>,
  fallback: AppearancePreferences = DEFAULT_APPEARANCE_PREFERENCES,
): AppearancePreferences => {
  const requestedFamily = typeof value.fontFamily === "string"
    ? value.fontFamily.trim()
    : fallback.fontFamily;
  const fontFamily = isAppearanceFontFamily(requestedFamily)
    ? requestedFamily
    : fallback.fontFamily;
  const fontSize = typeof value.fontSize === "number" && isAppearanceFontSize(value.fontSize)
    ? value.fontSize
    : fallback.fontSize;
  const reduceMotion = typeof value.reduceMotion === "boolean"
    ? value.reduceMotion
    : fallback.reduceMotion;
  return Object.freeze({ fontFamily, fontSize, reduceMotion });
};

export const browserAppearancePreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): AppearancePreferenceStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw === null) return undefined;
      const parsed: unknown = JSON.parse(raw);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return undefined;
      }
      return normalizeAppearancePreferences(parsed as Partial<AppearancePreferences>);
    } catch {
      return undefined;
    }
  },
  save: (preferences) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      // Restricted or full browser storage must not block an in-memory change.
    }
  },
});

export class AppearancePreferencesController {
  readonly #storage: AppearancePreferenceStorage | undefined;
  readonly #current = createSignal<AppearancePreferences>(DEFAULT_APPEARANCE_PREFERENCES);
  readonly current: ReadonlySignal<AppearancePreferences> = this.#current;

  constructor(storage?: AppearancePreferenceStorage) {
    this.#storage = storage;
    this.#current.set(normalizeAppearancePreferences(
      storage?.load() ?? DEFAULT_APPEARANCE_PREFERENCES,
    ));
  }

  replace(value: Partial<AppearancePreferences>, persist = true): AppearancePreferences {
    const next = normalizeAppearancePreferences(value, this.#current.get());
    this.#current.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  update(patch: Partial<AppearancePreferences>): AppearancePreferences {
    return this.replace({ ...this.#current.get(), ...patch });
  }
}

export const createBrowserAppearancePreferencesController = (
  persistLocally: boolean,
): AppearancePreferencesController => {
  if (!persistLocally) return new AppearancePreferencesController();
  try {
    return new AppearancePreferencesController(
      browserAppearancePreferenceStorage(globalThis.localStorage),
    );
  } catch {
    return new AppearancePreferencesController();
  }
};

export type AppearancePreferencesSignal = ReadonlySignal<AppearancePreferences>;
