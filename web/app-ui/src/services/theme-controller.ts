import {
  createComputed,
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export const THEME_NAMES = [
  "dark",
  "light",
  "high-contrast-dark",
  "colorblind-dark",
  "colorblind-light",
] as const;

export type ThemeName = (typeof THEME_NAMES)[number];
export type ThemePreference = "system" | ThemeName;

export interface ThemePreferenceStorage {
  load(): ThemePreference | undefined;
  save(preference: ThemePreference): void;
}

export interface MediaQueryLike {
  readonly matches: boolean;
  addEventListener?(type: "change", listener: (event: { matches: boolean }) => void): void;
  removeEventListener?(type: "change", listener: (event: { matches: boolean }) => void): void;
}

export const isThemePreference = (value: unknown): value is ThemePreference =>
  value === "system" || THEME_NAMES.includes(value as ThemeName);

export const browserThemePreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): ThemePreferenceStorage => ({
  load: () => {
    try {
      const value = storage.getItem("trouve.theme");
      return isThemePreference(value) ? value : undefined;
    } catch {
      return undefined;
    }
  },
  save: (preference) => {
    try {
      storage.setItem("trouve.theme", preference);
    } catch {
      // A denied or full browser store must not prevent theme changes in memory.
    }
  },
});

export class ThemeController {
  readonly #storage: ThemePreferenceStorage | undefined;
  readonly #darkQuery: MediaQueryLike;
  readonly #systemDark = createSignal(false);
  readonly #onDarkChange = (event: { matches: boolean }) => {
    this.#systemDark.set(event.matches);
  };

  readonly preference = createSignal<ThemePreference>("system");
  readonly theme: ReadonlySignal<ThemeName> = createComputed(() => {
    const preference = this.preference.get();
    if (preference !== "system") return preference;
    return this.#systemDark.get() ? "dark" : "light";
  });

  constructor(options: {
    readonly darkQuery: MediaQueryLike;
    readonly storage?: ThemePreferenceStorage;
  }) {
    this.#darkQuery = options.darkQuery;
    this.#storage = options.storage;
    this.#systemDark.set(options.darkQuery.matches);
    this.preference.set(options.storage?.load() ?? "system");
    options.darkQuery.addEventListener?.("change", this.#onDarkChange);
  }

  setPreference(preference: ThemePreference): void {
    this.preference.set(preference);
    this.#storage?.save(preference);
  }

  dispose(): void {
    this.#darkQuery.removeEventListener?.("change", this.#onDarkChange);
  }
}

export const createBrowserThemeController = (
  persistLocally: boolean,
): ThemeController => {
  const darkQuery = globalThis.matchMedia?.("(prefers-color-scheme: dark)") ?? {
    matches: true,
  };
  let storage: ThemePreferenceStorage | undefined;
  if (persistLocally) {
    try {
      storage = browserThemePreferenceStorage(globalThis.localStorage);
    } catch {
      // Accessing localStorage may itself throw in a restricted browser context.
    }
  }
  return new ThemeController({ darkQuery, ...(storage === undefined ? {} : { storage }) });
};
