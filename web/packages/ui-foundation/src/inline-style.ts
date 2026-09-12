import { noChange } from "lit";
import { Directive, directive, type ElementPart, type PartInfo, PartType } from "lit/directive.js";

/**
 * Apply per-element inline CSS through the CSSOM instead of a `style`
 * attribute. Attribute-borne inline styles are refused by hosts whose
 * Content-Security-Policy has `style-src 'self'` (the self-hosted review site);
 * `element.style.cssText` is not subject to that directive. Use as an element
 * part: `<span ${inlineStyle("display:inline-block")}>`.
 */
export class InlineStyleDirective extends Directive {
  #applied: string | undefined;

  constructor(partInfo: PartInfo) {
    super(partInfo);
    if (partInfo.type !== PartType.ELEMENT) {
      throw new Error("inlineStyle() must be used as an element part: <el ${inlineStyle(css)}>");
    }
  }

  override render(_css: string): typeof noChange {
    return noChange;
  }

  override update(part: ElementPart, [css]: [string]): typeof noChange {
    if (this.#applied !== css) {
      (part.element as HTMLElement).style.cssText = css;
      this.#applied = css;
    }
    return noChange;
  }
}

export const inlineStyle = directive(InlineStyleDirective);
