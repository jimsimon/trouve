import { describe, expect, it } from "vitest";

import { fontAwesomeIcon } from "./font-awesome-icon.js";

describe("fontAwesomeIcon", () => {
  it("renders a locally bundled Font Awesome Free glyph as decorative by default", () => {
    const icon = fontAwesomeIcon("gear");
    expect(icon.strings.join("<value>")).toContain("data-font-awesome-icon=");
    expect(icon.values).toContain("gear");
    expect(icon.values).toContain("true");
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
});
