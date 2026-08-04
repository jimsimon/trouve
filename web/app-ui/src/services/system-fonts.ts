import { isAppearanceFontFamily } from "./appearance-preferences.js";

const MAX_SYSTEM_FONT_FAMILIES = 4_096;

interface LocalFontQueryScope {
  readonly queryLocalFonts?: () => Promise<unknown>;
}

/**
 * Produces the stable, bounded family list used by both native-host and
 * browser font discovery. Font names must also be valid persisted appearance
 * values so every option in the selector can actually be applied.
 */
export const normalizeSystemFontFamilies = (
  values: readonly unknown[],
): readonly string[] => {
  const names = values
    .filter((value): value is string => typeof value === "string")
    .map((value) => value.trim())
    .filter((value) =>
      value !== "" && !value.startsWith(".") && isAppearanceFontFamily(value)
    )
    .sort((left, right) =>
      left.localeCompare(right, undefined, { sensitivity: "base" }) ||
      left.localeCompare(right)
    );
  const unique: string[] = [];
  const seen = new Set<string>();
  for (const name of names) {
    const key = name.toLocaleLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    unique.push(name);
    if (unique.length === MAX_SYSTEM_FONT_FAMILIES) break;
  }
  return Object.freeze(unique);
};

/**
 * Uses the permission-gated Local Font Access API where a PWA/browser offers
 * it. Unsupported browsers and denied permission intentionally return an
 * empty list; the Font selector continues to offer the platform default.
 */
export const queryBrowserSystemFontFamilies = async (
  scope: object = globalThis,
): Promise<readonly string[]> => {
  const queryLocalFonts = (scope as LocalFontQueryScope).queryLocalFonts;
  if (typeof queryLocalFonts !== "function") return Object.freeze([]);
  try {
    const records = await queryLocalFonts.call(scope);
    if (!Array.isArray(records)) return Object.freeze([]);
    return normalizeSystemFontFamilies(records.map((record: unknown) => {
      if (typeof record !== "object" || record === null || Array.isArray(record)) {
        return undefined;
      }
      return (record as { readonly family?: unknown }).family;
    }));
  } catch {
    return Object.freeze([]);
  }
};
