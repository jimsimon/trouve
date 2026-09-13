import { type TemplateResult } from "lit";

import { ReviewElement } from "./element";
import { html } from "./template";

type CopyState = "idle" | "copied" | "failed";

/** Copies `text` to the clipboard and reports the outcome on the button for 1.5s. */
export class CopyButton extends ReviewElement {
  static override properties = {
    text: { attribute: false },
    label: { attribute: false },
    copyState: { state: true },
  };

  text = "";
  label = "Copy prompt";
  private copyState: CopyState = "idle";

  private async copy(): Promise<void> {
    try {
      if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable");
      await navigator.clipboard.writeText(this.text);
      this.copyState = "copied";
    } catch {
      this.copyState = "failed";
    }
    window.setTimeout(() => {
      this.copyState = "idle";
    }, 1_500);
  }

  protected override render(): TemplateResult {
    return html`<button
      class="ghost compact"
      type="button"
      @click=${() => void this.copy()}
      ?disabled=${!this.text}
    >
      ${this.copyState === "copied"
        ? "Copied"
        : this.copyState === "failed"
          ? "Copy failed"
          : this.label}
    </button>`;
  }
}

customElements.define("trouve-code-review-copy-button", CopyButton);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-copy-button": CopyButton;
  }
}
