import { describe, expect, it } from "vitest";

import {
  highlightSource,
  highlightSourceGeneric,
  supportsGenericHighlighting,
} from "./source-highlighter.js";

describe("source highlighting parity", () => {
  it("lexes common native-preview languages without adding parser bundles", () => {
    const rust = "pub fn answer() -> u32 { // why\n  42\n}";
    const tokens = highlightSourceGeneric(rust, "rust");
    expect(tokens.some((token) => token.classes === "tok-keyword" && rust.slice(token.from, token.to) === "pub")).toBe(true);
    expect(tokens.some((token) => token.classes === "tok-typeName" && rust.slice(token.from, token.to) === "u32")).toBe(true);
    expect(tokens.some((token) => token.classes === "tok-comment" && rust.slice(token.from, token.to) === "// why")).toBe(true);
    expect(tokens.some((token) => token.classes === "tok-number" && rust.slice(token.from, token.to) === "42")).toBe(true);
  });

  it("handles strings, config keys, markup, and unterminated comments safely", () => {
    const config = 'name = "trouve"\nenabled = true';
    expect(highlightSourceGeneric(config, "toml").map((token) => token.classes)).toEqual([
      "tok-propertyName",
      "tok-string",
      "tok-propertyName",
      "tok-keyword",
    ]);
    const markup = '<main aria-label="App">text</main>';
    expect(highlightSourceGeneric(markup, "html").some(
      (token) => token.classes === "tok-typeName" && markup.slice(token.from, token.to) === "main",
    )).toBe(true);
    expect(highlightSourceGeneric("/* open", "c")).toEqual([
      { from: 0, to: 7, classes: "tok-comment" },
    ]);
  });

  it("keeps JavaScript on Lezer and unknown formats as selectable plain text", async () => {
    expect(supportsGenericHighlighting("python")).toBe(true);
    expect(supportsGenericHighlighting("binary")).toBe(false);
    expect(await highlightSource("const value = 1", "typescript")).not.toEqual([]);
    expect(await highlightSource("opaque", "binary")).toEqual([]);
  });
});
