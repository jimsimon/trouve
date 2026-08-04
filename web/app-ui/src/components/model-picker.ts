import { html, LitElement, nothing, type PropertyValues } from "lit";

import type { ProtocolModelInfo } from "../services/protocol-client.js";
import {
  filteredModelIndices,
  type ModelHealthPresentation,
} from "./model-health.js";

export interface ModelPickedDetail {
  readonly modelId: string;
}

let nextModelPickerId = 0;

export class TrouveModelPicker extends LitElement {
  static override properties = {
    models: { attribute: false },
    health: { attribute: false },
    value: { type: String },
    disabled: { type: Boolean, reflect: true },
    placement: { type: String },
    accessibleLabel: { type: String, attribute: "accessible-label" },
    placeholder: { type: String },
    emptyLabel: { type: String, attribute: "empty-label" },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  models: readonly ProtocolModelInfo[] = [];
  health: readonly (ModelHealthPresentation | undefined)[] = [];
  value = "";
  disabled = false;
  placement: "up" | "down" = "up";
  accessibleLabel = "Model";
  placeholder = "Select model…";
  emptyLabel = "";
  #open = false;
  #query = "";
  #activeMatch = 0;
  readonly #listId = `model-picker-list-${++nextModelPickerId}`;

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has("disabled") && this.disabled) this.#open = false;
    if (changed.has("models") || changed.has("value")) {
      const selected = this.#matches().findIndex(
        (index) => (index === -1 ? "" : this.models[index]?.id) === this.value,
      );
      this.#activeMatch = Math.max(0, selected);
    }
  }

  override render() {
    const selectedIndex = this.models.findIndex((model) => model.id === this.value);
    const selected = this.models[selectedIndex];
    const selectedHealth = this.health[selectedIndex];
    const matches = this.#matches();
    const activeIndex = matches[this.#activeMatch];
    return html`
      <span
        class="model-picker ${this.#open ? "open" : ""} placement-${this.placement}"
        @focusout=${this.#focusLeft}
      >
        <button
          class="model-picker-trigger"
          type="button"
          role="combobox"
          aria-label=${this.accessibleLabel}
          aria-haspopup="listbox"
          aria-expanded=${this.#open ? "true" : "false"}
          aria-controls=${this.#listId}
          ?disabled=${this.disabled}
          title=${selectedHealth?.detail ?? selected?.id ?? ""}
          @click=${this.#toggle}
          @keydown=${this.#triggerKeydown}
        >
          <span>${selected?.display_name ?? (this.value || this.placeholder)}</span>
          ${selectedHealth === undefined
            ? nothing
            : html`<span class="model-health-dot tone-${selectedHealth.tone}" aria-hidden="true"></span>`}
          <span aria-hidden="true">${this.placement === "up" ? "▴" : "▾"}</span>
        </button>
        ${this.#open
          ? html`<span class="model-picker-popup">
              <input
                type="search"
                role="searchbox"
                aria-label="Search models"
                aria-controls=${this.#listId}
                aria-activedescendant=${activeIndex === undefined ? nothing : `${this.#listId}-${activeIndex}`}
                placeholder="Search models…"
                .value=${this.#query}
                @input=${this.#queryChanged}
                @keydown=${this.#searchKeydown}
              />
              <span id=${this.#listId} class="model-picker-options" role="listbox" aria-label="Models">
                ${matches.length === 0
                  ? html`<span class="model-picker-empty">No matches</span>`
                  : matches.map((index, matchIndex) => {
                      const model = index === -1 ? undefined : this.models[index];
                      const health = this.health[index];
                      if (index !== -1 && model === undefined) return nothing;
                      const modelId = model?.id ?? "";
                      const label = model?.display_name ?? this.emptyLabel;
                      return html`<button
                        id=${`${this.#listId}-${index}`}
                        type="button"
                        role="option"
                        aria-selected=${modelId === this.value ? "true" : "false"}
                        class=${matchIndex === this.#activeMatch ? "active" : ""}
                        title=${health?.detail ?? modelId}
                        @pointerenter=${() => {
                          this.#activeMatch = matchIndex;
                          this.requestUpdate();
                        }}
                        @click=${() => this.#pick(index)}
                      >
                        <span>${label}</span>
                        ${health === undefined
                          ? nothing
                          : html`<small class=${`tone-${health.tone}`}>
                              <span class=${`model-health-dot tone-${health.tone}`} aria-hidden="true"></span>
                              ${health.summary}
                            </small>`}
                      </button>`;
                    })}
              </span>
            </span>`
          : nothing}
      </span>
    `;
  }

  #matches(): readonly number[] {
    const matches = filteredModelIndices(this.models, this.#query);
    if (this.emptyLabel === "") return matches;
    const query = this.#query.trim().toLocaleLowerCase();
    const includeEmpty = query === "" || this.emptyLabel.toLocaleLowerCase().includes(query);
    return includeEmpty ? [-1, ...matches].slice(0, 100) : matches;
  }

  readonly #toggle = (): void => {
    if (this.disabled) return;
    this.#open = !this.#open;
    this.#query = "";
    this.#activeMatch = Math.max(0, this.#matches().findIndex((index) =>
      (index === -1 ? "" : this.models[index]?.id) === this.value));
    this.requestUpdate();
    if (this.#open) {
      void this.updateComplete.then(() => this.querySelector<HTMLInputElement>(".model-picker-popup input")?.focus());
    }
  };

  readonly #triggerKeydown = (event: KeyboardEvent): void => {
    if (!["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) return;
    event.preventDefault();
    if (!this.#open) this.#toggle();
  };

  readonly #queryChanged = (event: Event): void => {
    this.#query = (event.currentTarget as HTMLInputElement).value;
    this.#activeMatch = 0;
    this.requestUpdate();
  };

  readonly #searchKeydown = (event: KeyboardEvent): void => {
    const matches = this.#matches();
    if (event.key === "Escape") {
      event.preventDefault();
      this.#closeAndFocus();
      return;
    }
    if (event.key === "Enter") {
      const index = matches[this.#activeMatch];
      if (index === undefined) return;
      event.preventDefault();
      this.#pick(index);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    if (matches.length === 0) return;
    const delta = event.key === "ArrowDown" ? 1 : -1;
    this.#activeMatch = (this.#activeMatch + delta + matches.length) % matches.length;
    this.requestUpdate();
    void this.updateComplete.then(() => {
      const index = matches[this.#activeMatch];
      if (index === undefined) return;
      this.querySelector<HTMLElement>(`#${this.#listId}-${index}`)?.scrollIntoView({ block: "nearest" });
    });
  };

  #pick(index: number): void {
    const modelId = index === -1 ? "" : this.models[index]?.id;
    if (modelId === undefined || this.disabled) return;
    this.#open = false;
    this.#query = "";
    this.dispatchEvent(new CustomEvent<ModelPickedDetail>("trouve-model-picked", {
      detail: { modelId },
      bubbles: true,
      composed: true,
    }));
    this.requestUpdate();
    void this.updateComplete.then(() => this.querySelector<HTMLButtonElement>(".model-picker-trigger")?.focus());
  }

  readonly #focusLeft = (): void => {
    queueMicrotask(() => {
      if (!this.#open || this.contains(globalThis.document?.activeElement ?? null)) return;
      this.#open = false;
      this.requestUpdate();
    });
  };

  #closeAndFocus(): void {
    this.#open = false;
    this.requestUpdate();
    void this.updateComplete.then(() => this.querySelector<HTMLButtonElement>(".model-picker-trigger")?.focus());
  }
}

customElements.define("trouve-model-picker", TrouveModelPicker);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-model-picker": TrouveModelPicker;
  }
}
