import { render } from "@lit-labs/ssr";
import { html, noChange, type TemplateResult } from "lit";
import { type ElementPart, PartType } from "lit/directive.js";
import { describe, expect, it } from "vitest";

import { fontAwesomeIcon, ICON_STYLE } from "./font-awesome-icon.js";
import { inlineStyle, InlineStyleDirective } from "./inline-style.js";

const fakeElementPart = (): { part: ElementPart; element: { style: { cssText: string } } } => {
  const element = { style: { cssText: "" } };
  return { part: { type: PartType.ELEMENT, element } as unknown as ElementPart, element };
};

const ssr = (template: TemplateResult): string => {
  let out = "";
  for (const chunk of render(template as TemplateResult<1>)) out += chunk;
  return out;
};

describe("inlineStyle", () => {
  it("writes per-element CSS through the CSSOM and only when it changes", () => {
    const { part, element } = fakeElementPart();
    const directive = new InlineStyleDirective({ type: PartType.ELEMENT } as never);
    expect(directive.update(part, ["width: 4px"])).toBe(noChange);
    expect(element.style.cssText).toBe("width: 4px");
    element.style.cssText = "width: 4px; color: red"; // e.g. a browser normalising the value
    directive.update(part, ["width: 4px"]);
    expect(element.style.cssText).toBe("width: 4px; color: red");
    directive.update(part, ["width: 9px"]);
    expect(element.style.cssText).toBe("width: 9px");
  });

  it("is an element part, so templates never carry a style attribute", () => {
    expect(() => new InlineStyleDirective({ type: PartType.ATTRIBUTE, name: "style" } as never)).toThrow(/element part/u);
    const template = html`<span ${inlineStyle("width: 4px")}></span>`;
    expect(template.strings.join("")).not.toContain("style=");
    expect(ssr(template)).not.toContain("style=");
  });

  it("styles Font Awesome glyphs the same way, so icons survive a style-src 'self' policy", () => {
    expect(ICON_STYLE).toContain("font-family:'Font Awesome 7 Free'");
    expect(ICON_STYLE).toContain("font-weight:900");
    const icon = fontAwesomeIcon("gear");
    expect(icon.strings.join("")).not.toContain("style=");
    expect(ssr(icon)).not.toContain("style=");
    expect(ssr(icon)).toContain('data-font-awesome-icon="gear"');
  });
});
