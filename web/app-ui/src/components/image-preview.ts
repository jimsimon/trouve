import { css, html, LitElement, nothing, type PropertyValues } from "lit";

import { fontAwesomeIcon } from "./font-awesome-icon.js";

/**
 * A media thumbnail shared by durable and in-progress attachment chips.
 * Images open in a gallery scoped to their nearest attachment list. Videos
 * request external playback so the desktop app can use the system player.
 */
export class TrouveImagePreview extends LitElement {
  static override properties = {
    source: { type: String },
    name: { type: String },
    mime: { type: String },
    video: { type: Boolean, reflect: true },
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
    .image-preview-trigger img,
    .image-preview-trigger video {
      width: 100%;
      height: 100%;
      display: block;
      object-fit: cover;
      background: var(--trouve-code-bg);
    }
    .image-preview-trigger video { pointer-events: none; }
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
      grid-template-columns: minmax(0, 1fr) auto auto;
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
    .image-preview-counter {
      color: var(--trouve-text-dim);
      font-size: 11px;
      font-variant-numeric: tabular-nums;
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
      position: relative;
      max-width: calc(100vw - 34px);
      max-height: calc(100dvh - 72px);
      display: grid;
      place-items: center;
      overflow: auto;
      background: var(--trouve-code-bg);
    }
    .image-preview-navigation {
      position: absolute;
      inset: 0;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 8px;
      pointer-events: none;
    }
    .image-preview-navigation button {
      width: 36px;
      height: 36px;
      display: grid;
      place-items: center;
      border: 1px solid rgba(255, 255, 255, .28);
      border-radius: 50%;
      padding: 0;
      color: white;
      background: rgba(0, 0, 0, .58);
      box-shadow: 0 2px 10px rgba(0, 0, 0, .32);
      cursor: pointer;
      pointer-events: auto;
    }
    .image-preview-navigation button:hover { background: rgba(0, 0, 0, .76); }
    .image-preview-navigation button:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
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
  mime = "";
  video = false;
  lazy = false;
  #viewerOpen = false;
  #gallery: readonly { readonly source: string; readonly name: string }[] = [];
  #galleryIndex = 0;
  #returnFocus: HTMLElement | undefined;
  #videoPreviewSource = "";
  #videoObserver: IntersectionObserver | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    if (!this.hasUpdated) return;
    this.#configureVideoPreview();
    this.requestUpdate();
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    super.willUpdate(changed);
    if (
      changed.has("source")
      || changed.has("mime")
      || changed.has("video")
      || changed.has("lazy")
    ) {
      this.#configureVideoPreview();
    }
  }

  override disconnectedCallback(): void {
    this.#releaseVideoPreview();
    super.disconnectedCallback();
  }

  override render() {
    const label = this.name.trim() === "" ? "image attachment" : this.name;
    const current = this.#gallery[this.#galleryIndex] ?? {
      source: this.source,
      name: label,
    };
    const multiple = this.#gallery.length > 1;
    return html`
      <button
        class="image-preview-trigger"
        type="button"
        aria-label=${this.video
          ? `Open video in external player: ${label}`
          : `View full-size image: ${label}`}
        title=${this.video
          ? `Open video in external player: ${label}`
          : `View full-size image: ${label}`}
        @click=${this.#openViewer}
      >
        ${this.video
          ? html`<video
              src=${this.#videoPreviewSource === "" ? nothing : this.#videoPreviewSource}
              preload="metadata"
              muted
              playsinline
              aria-hidden="true"
            ></video>`
          : html`<img
              src=${this.source}
              alt=${`Preview of ${label}`}
              loading=${this.lazy ? "lazy" : nothing}
              decoding="async"
            />`}
        <span class="image-preview-affordance" aria-hidden="true">
          ${fontAwesomeIcon(this.video ? "play" : "magnifying-glass")}
        </span>
      </button>
      ${this.video
        ? nothing
        : html`<dialog
            aria-label=${`Full-size preview of ${current.name}`}
            @cancel=${this.#cancelViewer}
            @close=${this.#viewerClosed}
            @click=${this.#closeFromBackdrop}
            @keydown=${this.#viewerKeydown}
          >
            ${this.#viewerOpen
              ? html`
                  <figure>
                    <figcaption>
                      <strong title=${current.name}>${current.name}</strong>
                      ${multiple
                        ? html`<span class="image-preview-counter" aria-live="polite">
                            ${this.#galleryIndex + 1} of ${this.#gallery.length}
                          </span>`
                        : nothing}
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
                        src=${current.source}
                        alt=${`Full-size preview of ${current.name}`}
                        decoding="async"
                      />
                      ${multiple
                        ? html`<nav class="image-preview-navigation" aria-label="Image gallery">
                            <button
                              type="button"
                              aria-label="Previous image"
                              title="Previous image"
                              @click=${this.#previousImage}
                            >${fontAwesomeIcon("arrow-left")}</button>
                            <button
                              type="button"
                              aria-label="Next image"
                              title="Next image"
                              @click=${this.#nextImage}
                            >${fontAwesomeIcon("arrow-right")}</button>
                          </nav>`
                        : nothing}
                    </div>
                  </figure>
                `
              : nothing}
          </dialog>`}
    `;
  }

  readonly #openViewer = (event: Event): void => {
    if (this.source === "" || this.#viewerOpen) return;
    if (this.video) {
      this.dispatchEvent(new CustomEvent("trouve-open-video", {
        detail: {
          source: this.source,
          name: this.name,
          mime: this.mime,
        },
        bubbles: true,
        composed: true,
      }));
      return;
    }
    this.#returnFocus = event.currentTarget instanceof HTMLElement
      ? event.currentTarget
      : undefined;
    const gallery = this.#imageGallery();
    this.#gallery = gallery.items;
    this.#galleryIndex = gallery.index;
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

  readonly #previousImage = (): void => this.#moveGallery(-1);
  readonly #nextImage = (): void => this.#moveGallery(1);

  readonly #viewerKeydown = (event: KeyboardEvent): void => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    this.#moveGallery(event.key === "ArrowLeft" ? -1 : 1);
  };

  #moveGallery(delta: number): void {
    if (this.#gallery.length < 2) return;
    this.#galleryIndex = (
      this.#galleryIndex + delta + this.#gallery.length
    ) % this.#gallery.length;
    this.requestUpdate();
  }

  #imageGallery(): {
    readonly items: readonly { readonly source: string; readonly name: string }[];
    readonly index: number;
  } {
    const attachmentList = this.closest(".attachment-list");
    const previews = attachmentList === null
      ? [this]
      : [...attachmentList.querySelectorAll<TrouveImagePreview>("trouve-image-preview")];
    const images = previews.filter((preview) => !preview.video && preview.source !== "");
    return {
      items: images.map((preview) => ({
        source: preview.source,
        name: preview.name.trim() === "" ? "image attachment" : preview.name,
      })),
      index: Math.max(0, images.indexOf(this)),
    };
  }

  #configureVideoPreview(): void {
    this.#releaseVideoPreview();
    if (!this.video || this.source === "") return;
    if (this.lazy && "IntersectionObserver" in globalThis) {
      const observer = new IntersectionObserver((entries) => {
        if (this.#videoObserver !== observer) return;
        if (!entries.some((entry) => entry.isIntersecting)) return;
        observer.disconnect();
        this.#videoObserver = undefined;
        this.#videoPreviewSource = this.source;
        this.requestUpdate();
      }, { rootMargin: "200px" });
      this.#videoObserver = observer;
      observer.observe(this);
      return;
    }
    this.#videoPreviewSource = this.source;
  }

  #releaseVideoPreview(): void {
    this.#videoObserver?.disconnect();
    this.#videoObserver = undefined;
    this.#videoPreviewSource = "";
  }

  #finishClose(restoreFocus: boolean): void {
    const returnFocus = this.#returnFocus;
    this.#viewerOpen = false;
    this.#gallery = [];
    this.#galleryIndex = 0;
    this.#returnFocus = undefined;
    this.requestUpdate();
    if (restoreFocus && returnFocus?.isConnected === true) returnFocus.focus();
  }
}

customElements.define("trouve-image-preview", TrouveImagePreview);

declare global {
  interface HTMLElementEventMap {
    "trouve-open-video": CustomEvent<{
      readonly source: string;
      readonly name: string;
      readonly mime: string;
    }>;
  }

  interface HTMLElementTagNameMap {
    "trouve-image-preview": TrouveImagePreview;
  }
}
