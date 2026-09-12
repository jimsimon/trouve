import { nothing, type TemplateResult } from "lit";
import { createRef, ref } from "lit/directives/ref.js";

import { ReviewElement } from "./element";
import { boundReviewOutput } from "./review-output";
import { html } from "./template";

/**
 * One retained output stream. While the task is running and the reader has
 * not scrolled away from the tail, new output keeps the tail pinned.
 */
export class OutputBlock extends ReviewElement {
  static override properties = {
    heading: { attribute: false },
    value: { attribute: false },
    followTail: { attribute: false },
  };

  heading = "";
  value = "";
  followTail = false;

  private pinned = true;
  private readonly pre = createRef<HTMLPreElement>();
  private readonly scrollEffect = this.effect();

  protected override updated(): void {
    const followTail = this.followTail;
    const boundedValue = boundReviewOutput(this.value);
    this.scrollEffect.run([followTail, boundedValue], () => {
      if (!followTail || !this.pinned || document.visibilityState !== "visible") {
        return;
      }
      const frame = window.requestAnimationFrame(() => {
        const element = this.pre.value;
        if (element && this.pinned) element.scrollTop = element.scrollHeight;
      });
      return () => window.cancelAnimationFrame(frame);
    });
  }

  private readonly onScroll = (event: Event): void => {
    const element = event.currentTarget as HTMLPreElement;
    this.pinned = element.scrollHeight - element.scrollTop - element.clientHeight <= 16;
  };

  protected override render(): TemplateResult | typeof nothing {
    const boundedValue = boundReviewOutput(this.value);
    if (!boundedValue) return nothing;
    return html`<section class="output-block">
      <h3>${this.heading}</h3>
      <pre ${ref(this.pre)} tabindex="0" aria-busy=${this.followTail} @scroll=${this.onScroll}>${boundedValue}</pre>
    </section>`;
  }
}

customElements.define("trouve-code-review-output-block", OutputBlock);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-output-block": OutputBlock;
  }
}
