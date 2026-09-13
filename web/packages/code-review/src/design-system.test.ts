import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { FONT_AWESOME_CODEPOINTS } from "@trouve-ai/ui-foundation/font-awesome-icon";

import { sections } from "./route";
import { chartFill } from "./stats-page";

const sourceRoot = fileURLToPath(new URL(".", import.meta.url));
const read = (name: string): string => readFileSync(`${sourceRoot}${name}`, "utf8");

const styles = read("styles.css");
const transcriptStyles = readFileSync(
  new URL("../../transcript/src/styles/transcript.css", import.meta.url),
  "utf8",
);
const foundationStyles = ["tokens.css", "themes.css"]
  .map((name) => readFileSync(new URL(`../../ui-foundation/src/styles/${name}`, import.meta.url), "utf8"))
  .join("\n");
const components = readdirSync(sourceRoot)
  .filter((name) => name.endsWith(".ts") && !name.endsWith(".test.ts"))
  .map((name) => read(name))
  .join("\n");

const stripComments = (css: string): string => css.replaceAll(/\/\*[\s\S]*?\*\//gu, "");

const selectorList = (css: string): readonly string[] =>
  stripComments(css)
    .split("}")
    .flatMap((rule) => rule.split("{")[0]?.split(",") ?? [])
    .map((selector) => selector.trim())
    .filter((selector) => selector !== "" && !selector.startsWith("@"));

/** Every class named anywhere in the stylesheet. */
const classes = (css: string): Set<string> =>
  new Set(selectorList(css).flatMap((selector) => selector.match(/\.[a-zA-Z][\w-]*/gu) ?? []));

/** Classes that some rule targets on their own (`.warning`, `.warning:hover`),
 * i.e. without an element, sibling class, or ancestor to scope them. */
const bareClasses = (css: string): Set<string> =>
  new Set(
    selectorList(css).flatMap((selector) => {
      const match = /^(\.[a-zA-Z][\w-]*)(?::[\w-]+(?:\([^)]*\))?)*$/u.exec(selector);
      return match?.[1] === undefined ? [] : [match[1]];
    }),
  );

describe("code-review design system", () => {
  it("draws every color from the shared --trouve-* tokens", () => {
    const declarations = stripComments(styles);
    expect(declarations).not.toMatch(/#[0-9a-fA-F]{3,8}\b/u);
    expect(declarations).not.toMatch(/\brgba?\(/u);
    expect(declarations).not.toMatch(/\bhsla?\(/u);
    expect(declarations).toMatch(/var\(--trouve-win-bg\)/u);
    expect(declarations).toMatch(/var\(--trouve-font-sans\)/u);
    expect(declarations).toMatch(/var\(--trouve-navigation-width\)/u);
    const defined = new Set([...foundationStyles.matchAll(/(--trouve-[\w-]+)\s*:/gu)].map(([, name]) => name));
    const referenced = new Set([...declarations.matchAll(/var\((--trouve-[\w-]+)/gu)].map(([, name]) => name));
    expect([...referenced].filter((name) => !defined.has(name))).toEqual([]);
    expect(referenced.size).toBeGreaterThan(30);
  });

  it("does not define its own root/body/theme rules; the host owns them", () => {
    const rules = stripComments(styles);
    expect(rules).not.toMatch(/^\s*(?::root|html|body)\b[^{]*\{/mu);
    expect(rules).not.toMatch(/prefers-color-scheme/u);
    expect(rules).not.toContain("data-theme");
  });

  it("never targets a transcript class without scoping it", () => {
    // Both sheets load into one scope. A bare `.failed {}` here would restyle
    // the transcript's `.turn-rail-marker.failed`; compound selectors cannot.
    const shared = classes(transcriptStyles);
    const collisions = [...bareClasses(styles)].filter((selector) => shared.has(selector));
    expect(collisions).toEqual([]);
    expect(bareClasses(styles).has(".activity-group")).toBe(false);
    expect(classes(styles).has(".activity-group")).toBe(false);
  });

  it("uses the shared Font Awesome subset for navigation", () => {
    for (const { icon } of sections) {
      expect(FONT_AWESOME_CODEPOINTS).toHaveProperty(icon);
    }
    expect(read("app.ts")).toContain("fontAwesomeIcon(icon)");
    expect(components).not.toMatch(/[\u25eb\u25c9\u2318\u25ce\u2197\u2699\u00d7]/u);
    expect(read("shared-views.ts")).toContain('fontAwesomeIcon("arrow-up-right-from-square")');
  });

  it("uses the shared visually-hidden utility instead of a local sr-only class", () => {
    expect(components).not.toContain("sr-only");
    expect(styles).not.toContain("sr-only");
  });

  it("exposes the theme picker only when a host owns theming", () => {
    const app = read("app.ts");
    expect(app).toContain("themePreference");
    expect(app).toContain("THEME_NAMES.map(");
    expect(app).toContain('"trouve-code-review-theme-change"');
    expect(app).not.toContain("localStorage");
    expect(app).not.toContain("matchMedia");
  });

  it("resolves chart colors from theme tokens at draw time", () => {
    const stats = read("stats-page.ts");
    expect(stats).not.toMatch(/color: "#[0-9a-fA-F]{6}"/u);
    expect(stats).toContain('"--trouve-ok"');
    expect(stats).toContain("getComputedStyle(this)");
    expect(stats).toContain('attributeFilter: ["data-theme"]');
    expect(chartFill("#7fd18a")).toBe("#7fd18a22");
    expect(chartFill("#abc")).toBe("#aabbcc22");
    expect(chartFill("rgb(1 2 3)")).toBe("color-mix(in srgb, rgb(1 2 3) 13%, transparent)");
  });
});
