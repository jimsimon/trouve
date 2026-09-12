import { noChange } from "lit";
import { Directive, directive, PartType, type ChildPart, type PartInfo } from "lit/directive.js";

/**
 * Controlled `<select>` value. Lit commits an element's property bindings
 * before its child expressions, so `.value` on a select whose options come
 * from an expression has nothing to select on the first render. Placing this
 * directive after the options commits the value once they exist, and keeps
 * the DOM value in sync with state on every render (like Preact's `value`).
 */
class SelectValueDirective extends Directive {
  constructor(partInfo: PartInfo) {
    super(partInfo);
    if (partInfo.type !== PartType.CHILD) {
      throw new Error("selectValue() must be the last child expression of a <select>");
    }
  }

  override render(_value: string): typeof noChange {
    return noChange;
  }

  override update(part: ChildPart, [value]: [string]): typeof noChange {
    const select = part.parentNode;
    if (select instanceof HTMLSelectElement && select.value !== value) {
      select.value = value;
    }
    return noChange;
  }
}

export const selectValue = directive(SelectValueDirective);
