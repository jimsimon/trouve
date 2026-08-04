import { describe, expect, it } from "vitest";

import {
  COMPOSER_MAX_HEIGHT,
  COMPOSER_MIN_HEIGHT,
  composerTextareaLayout,
  isComposerCompositionKey,
} from "./composer-input-model.js";

describe("composer input model", () => {
  it("autogrows between the desktop minimum and maximum", () => {
    expect(composerTextareaLayout(12)).toEqual({
      height: COMPOSER_MIN_HEIGHT,
      overflowY: "hidden",
    });
    expect(composerTextareaLayout(96)).toEqual({ height: 96, overflowY: "hidden" });
    expect(composerTextareaLayout(240)).toEqual({
      height: COMPOSER_MAX_HEIGHT,
      overflowY: "auto",
    });
    expect(composerTextareaLayout(Number.NaN)).toEqual({
      height: COMPOSER_MIN_HEIGHT,
      overflowY: "hidden",
    });
  });

  it("recognizes composition, Process, and legacy IME commit key events", () => {
    expect(isComposerCompositionKey({ key: "Enter", isComposing: true })).toBe(true);
    expect(isComposerCompositionKey({ key: "Enter", compositionActive: true })).toBe(true);
    expect(isComposerCompositionKey({ key: "Process" })).toBe(true);
    expect(isComposerCompositionKey({ key: "Enter", keyCode: 229 })).toBe(true);
    expect(isComposerCompositionKey({ key: "Enter", keyCode: 13 })).toBe(false);
  });
});
