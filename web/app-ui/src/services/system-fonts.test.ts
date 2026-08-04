import { describe, expect, it, vi } from "vitest";

import {
  normalizeSystemFontFamilies,
  queryBrowserSystemFontFamilies,
} from "./system-fonts.js";

describe("system font discovery", () => {
  it("normalizes installed families into safe sorted unique selector options", () => {
    expect(normalizeSystemFontFamilies([
      "Zed Sans",
      " Noto Sans ",
      "Alpha Sans",
      "M+ 1m (UI)",
      "Noto Sans",
      ".Hidden Font",
      "Unsafe; Font",
      "Line\nBreak",
      42,
    ])).toEqual(["Alpha Sans", "M+ 1m (UI)", "Noto Sans", "Zed Sans"]);
  });

  it("reads browser-local font records when the Local Font Access API is available", async () => {
    const scope = {
      queryLocalFonts: vi.fn(async () => [
        { family: "Fira Sans", fullName: "Fira Sans Regular" },
        { family: "Atkinson Hyperlegible" },
        { family: "Fira Sans", style: "Italic" },
      ]),
    };
    await expect(queryBrowserSystemFontFamilies(scope)).resolves.toEqual([
      "Atkinson Hyperlegible",
      "Fira Sans",
    ]);
    expect(scope.queryLocalFonts).toHaveBeenCalledOnce();
  });

  it("falls back without throwing when browser enumeration is absent or denied", async () => {
    await expect(queryBrowserSystemFontFamilies({})).resolves.toEqual([]);
    await expect(queryBrowserSystemFontFamilies({
      queryLocalFonts: vi.fn(async () => {
        throw new DOMException("denied", "NotAllowedError");
      }),
    })).resolves.toEqual([]);
  });
});
