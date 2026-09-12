import { describe, expect, it } from "vitest";

import { cleanJsxText, html } from "./template.js";

describe("JSX whitespace template tag", () => {
  it("cleanJsxText matches the JSX compiler's text rules", () => {
    expect(cleanJsxText("\n      ")).toBe("");
    expect(cleanJsxText("\n        Found by ")).toBe("Found by ");
    expect(cleanJsxText("Open findings\n")).toBe("Open findings");
    expect(cleanJsxText("\n  first line\n  second line\n")).toBe("first line second line");
    expect(cleanJsxText(" \u00b7 ")).toBe(" \u00b7 ");
    expect(cleanJsxText(" ")).toBe(" ");
    expect(cleanJsxText("a\tb")).toBe("a b");
  });

  it("drops indentation between elements and around expressions", () => {
    const name = "Security reviewer";
    const result = html`<article>
      <header><strong>${name}</strong></header>
      <small>
        Found by ${name}
      </small>
    </article>`;
    expect([...result.strings]).toEqual([
      "<article><header><strong>",
      "</strong></header><small>Found by ",
      "</small></article>",
    ]);
    expect(result.values).toEqual([name, name]);
  });

  it("keeps explicit spaces and attribute markup untouched", () => {
    const result = html`<a class="brand  wide"
      href=${"#/overview"} title='a > b'>
      <b>${1}:</b> ${2} of${" "}${3}
    </a>`;
    expect([...result.strings]).toEqual([
      '<a class="brand  wide"\n      href=',
      " title='a > b'><b>",
      ":</b> ",
      " of",
      "",
      "</a>",
    ]);
  });

  it("returns a stable strings array per call site so Lit can cache the template", () => {
    const render = (value: number) => html`<p>
      ${value}
    </p>`;
    const first = render(1);
    const second = render(2);
    expect(first.strings).toBe(second.strings);
    expect([...first.strings]).toEqual(["<p>", "</p>"]);
  });
});
