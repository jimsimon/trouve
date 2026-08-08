import { describe, expect, it } from "vitest";

import { nothing } from "lit";

import {
  FONT_AWESOME_CODEPOINTS,
  fontAwesomeIcon,
  type FontAwesomeIconName,
} from "./font-awesome-icon.js";

describe("fontAwesomeIcon", () => {
  it("renders a locally bundled Font Awesome Free glyph as decorative by default", () => {
    const icon = fontAwesomeIcon("gear");
    expect(icon.strings.join("<value>")).toContain("data-font-awesome-icon=");
    expect(icon.values).toContain("gear");
    expect(icon.values).toContain(String.fromCodePoint(FONT_AWESOME_CODEPOINTS.gear));
    const ariaLabelIndex = icon.strings.findIndex((part) => part.includes("aria-label="));
    expect(icon.values[ariaLabelIndex]).toBe(nothing);
  });

  it("supports labelled and animated icons", () => {
    const icon = fontAwesomeIcon("spinner", {
      className: "tool-status-icon",
      label: "Running",
      spin: true,
    });
    expect(icon.values).toContain("trouve-icon trouve-icon-spin tool-status-icon");
    expect(icon.values).toContain("Running");
    expect(icon.values).toContain("img");
  });

  it("uses an explicit fallback glyph for an unknown runtime icon name", () => {
    const icon = fontAwesomeIcon("not-mapped" as FontAwesomeIconName);
    expect(icon.values).toContain("□");
  });
});
