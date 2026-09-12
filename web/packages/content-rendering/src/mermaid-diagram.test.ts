import { describe, expect, it } from "vitest";

import { renderMermaid } from "./mermaid-diagram.js";

describe("renderMermaid", () => {
  it("renders a flowchart to themed SVG with escaped labels", async () => {
    const svg = await renderMermaid('graph TD\n  a["<b>&amp; one</b>"] --> b[two]');
    expect(svg).toBeDefined();
    expect(svg).toContain("<svg");
    expect(svg).toContain("var(--trouve-text)");
    expect(svg).toContain("two");
    expect(svg).not.toContain("<b>");
    expect(svg).not.toContain("<script");
  });

  it("returns the cached SVG for a repeated source", async () => {
    const source = "graph LR\n  x --> y";
    const first = await renderMermaid(source);
    const second = await renderMermaid(source);
    expect(first).toBeDefined();
    expect(second).toBe(first);
  });

  it("reports unsupported diagram text so the code block is kept", async () => {
    await expect(renderMermaid("not a diagram")).resolves.toBeUndefined();
  });
});
