import { css, html, LitElement, nothing } from "lit";

import { fontAwesomeIcon } from "./font-awesome-icon.js";

/**
 * A thumbnail that opens the complete, uncropped attachment in a modal image
 * viewer. The control is shared by durable and in-progress attachment chips so
 * their interaction stays consistent across every composer surface.
 */
export class TrouveImagePreview extends LitElement {
  static override properties = {
    source: { type: String },
    name: { type: String },
    lazy: { type: Boolean },
  };

  static override styles = css`
    :host {
      width: 64px;
      height: 48px;
      display: block;
      min-width: 0;
      min-height: 0;
    }
    * { box-sizing: border-box; }
    button { color: inherit; font: inherit; }
    .image-preview-trigger {
      position: relative;
      width: 64px;
      height: 48px;
      display: block;
      overflow: hidden;
      border: 0;
      border-radius: 3px;
      padding: 0;
      color: var(--trouve-text-hi);
      background: var(--trouve-code-bg);
      cursor: zoom-in;
    }
    .image-preview-trigger:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    .image-preview-trigger img {
      width: 100%;
      height: 100%;
      display: block;
      object-fit: cover;
      background: var(--trouve-code-bg);
    }
    .image-preview-affordance {
      position: absolute;
      inset: 0;
      display: grid;
      place-items: center;
      color: white;
      background: rgba(0, 0, 0, .46);
      opacity: 0;
      transition: opacity 100ms ease-out;
      pointer-events: none;
    }
    .image-preview-trigger:hover .image-preview-affordance,
    .image-preview-trigger:focus-visible .image-preview-affordance { opacity: 1; }
    dialog {
      max-width: calc(100vw - 32px);
      max-height: calc(100dvh - 32px);
      margin: auto;
      overflow: hidden;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius, 7px);
      padding: 0;
      color: var(--trouve-text);
      background: var(--trouve-popup-bg);
      box-shadow: 0 14px 48px var(--trouve-scrim);
    }
    dialog::backdrop { background: var(--trouve-scrim); }
    figure {
      min-width: min(280px, calc(100vw - 32px));
      max-width: calc(100vw - 32px);
      max-height: calc(100dvh - 32px);
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      margin: 0;
    }
    figcaption {
      min-width: 0;
      min-height: 38px;
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: center;
      gap: 12px;
      border-bottom: 1px solid var(--trouve-border);
      padding: 6px 7px 6px 11px;
      color: var(--trouve-text-mid);
      background: var(--trouve-header-bg);
    }
    figcaption strong {
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .image-preview-close {
      width: 28px;
      height: 28px;
      display: grid;
      place-items: center;
      border: 0;
      border-radius: var(--trouve-radius-sm, 4px);
      padding: 0;
      color: var(--trouve-text-dim);
      background: transparent;
      cursor: pointer;
    }
    .image-preview-close:hover { color: var(--trouve-text-hi); background: var(--trouve-hover-bg); }
    .image-preview-close:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    .image-preview-viewport {
      max-width: calc(100vw - 34px);
      max-height: calc(100dvh - 72px);
      display: grid;
      place-items: center;
      overflow: auto;
      background: var(--trouve-code-bg);
    }
    .image-preview-full {
      width: auto;
      height: auto;
      max-width: calc(100vw - 34px);
      max-height: calc(100dvh - 72px);
      display: block;
      object-fit: contain;
    }
    @media (prefers-reduced-motion: reduce) {
      .image-preview-affordance { transition: none; }
    }
  `;

  source = "";
  name = "";
  lazy = false;
  #viewerOpen = false;
  #returnFocus: HTMLElement | undefined;

  override render() {
    const label = this.name.trim() === "" ? "image attachment" : this.name;
    return html`
      <button
        class="image-preview-trigger"
        type="button"
        aria-label=${`View full-size image: ${label}`}
        title=${`View full-size image: ${label}`}
        @click=${this.#openViewer}
      >
        <img
          src=${this.source}
          alt=${`Preview of ${label}`}
          loading=${this.lazy ? "lazy" : nothing}
          decoding="async"
        />
        <span class="image-preview-affordance" aria-hidden="true">
          ${fontAwesomeIcon("magnifying-glass")}
        </span>
      </button>
      <dialog
        aria-label=${`Full-size preview of ${label}`}
        @cancel=${this.#cancelViewer}
        @close=${this.#viewerClosed}
        @click=${this.#closeFromBackdrop}
      >
        ${this.#viewerOpen
          ? html`
              <figure>
                <figcaption>
                  <strong title=${label}>${label}</strong>
                  <button
                    class="image-preview-close"
                    type="button"
                    aria-label="Close image preview"
                    title="Close"
                    @click=${this.#closeViewer}
                  >${fontAwesomeIcon("xmark")}</button>
                </figcaption>
                <div class="image-preview-viewport">
                  <img
                    class="image-preview-full"
                    src=${this.source}
                    alt=${`Full-size preview of ${label}`}
                    decoding="async"
                  />
                </div>
              </figure>
            `
          : nothing}
      </dialog>
    `;
  }

  readonly #openViewer = (event: Event): void => {
    if (this.source === "" || this.#viewerOpen) return;
    this.#returnFocus = event.currentTarget instanceof HTMLElement
      ? event.currentTarget
      : undefined;
    this.#viewerOpen = true;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (!this.#viewerOpen) return;
      const dialog = this.renderRoot.querySelector<HTMLDialogElement>("dialog");
      if (dialog === null || dialog.open) return;
      try {
        dialog.showModal();
      } catch {
        try {
          dialog.show();
        } catch {
          this.#finishClose(false);
          return;
        }
      }
      dialog.querySelector<HTMLButtonElement>(".image-preview-close")?.focus();
    });
  };

  readonly #closeViewer = (): void => {
    const dialog = this.renderRoot.querySelector<HTMLDialogElement>("dialog");
    if (dialog?.open === true) dialog.close();
    else this.#finishClose(true);
  };

  readonly #cancelViewer = (event: Event): void => {
    event.preventDefault();
    this.#closeViewer();
  };

  readonly #viewerClosed = (): void => {
    this.#finishClose(true);
  };

  readonly #closeFromBackdrop = (event: MouseEvent): void => {
    if (event.target === event.currentTarget) this.#closeViewer();
  };

  #finishClose(restoreFocus: boolean): void {
    const returnFocus = this.#returnFocus;
    this.#viewerOpen = false;
    this.#returnFocus = undefined;
    this.requestUpdate();
    if (restoreFocus && returnFocus?.isConnected === true) returnFocus.focus();
  }
}

customElements.define("trouve-image-preview", TrouveImagePreview);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-image-preview": TrouveImagePreview;
  }
}
