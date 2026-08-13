import { css, html, LitElement, nothing } from "lit";

import {
  type ModelOptionChangeDetail,
  type ModelOptionControl,
  type ModelOptionValue,
} from "./model-option-controls.js";

export const MODEL_OPTION_CHANGED_EVENT = "trouve-model-option-changed" as const;
export type ModelOptionChangedEvent = CustomEvent<ModelOptionChangeDetail>;

export class TrouveModelOptionsEditor extends LitElement {
  static override properties = {
    controls: { attribute: false },
    disabled: { type: Boolean, reflect: true },
    compact: { type: Boolean, reflect: true },
  };

  static override styles = css`
    :host {
      min-width: 0;
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 10px;
      color: var(--trouve-text);
      font: inherit;
    }
    :host([compact]) {
      display: flex;
      align-items: end;
      flex: none;
      gap: 8px;
    }
    * { box-sizing: border-box; }
    label, .boolean-option {
      min-width: 0;
      display: grid;
      align-content: start;
      gap: 3px;
      color: var(--trouve-text-mid);
      font-size: 10px;
      font-weight: 600;
    }
    :host([compact]) label, :host([compact]) .boolean-option {
      width: 118px;
      flex: none;
      gap: 2px;
      font-weight: 400;
    }
    .option-label {
      overflow: hidden;
      color: var(--trouve-text-faint);
      text-overflow: ellipsis;
      white-space: nowrap;
      line-height: 1.2;
    }
    select, input, button {
      width: 100%;
      min-width: 0;
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 7px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
      font-size: 12px;
    }
    :host([compact]) select, :host([compact]) input, :host([compact]) button {
      border-color: var(--trouve-border);
    }
    button { cursor: pointer; }
    button[aria-pressed="true"] {
      border-color: var(--trouve-accent);
      color: var(--trouve-accent-tint);
      background: var(--trouve-accent-bg);
    }
    select:disabled, input:disabled, button:disabled {
      cursor: not-allowed;
      opacity: .56;
    }
    select:focus-visible, input:focus-visible, button:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    small {
      color: var(--trouve-text-dim);
      font-size: 9px;
      font-weight: 400;
    }
    :host([compact]) small {
      position: absolute;
      width: 1px;
      height: 1px;
      overflow: hidden;
      clip-path: inset(50%);
      white-space: nowrap;
    }
    @media (max-width: 560px) {
      :host(:not([compact])) { grid-template-columns: 1fr; }
    }
  `;

  controls: readonly ModelOptionControl[] = [];
  disabled = false;
  compact = false;

  override render() {
    return this.controls.map((control, index) => {
      const descriptionId = control.description === ""
        ? nothing
        : `model-option-description-${index}`;
      if (control.kind === "choice") {
        return html`
          <label title=${this.compact ? control.description : nothing}>
            <span class="option-label">${control.label}</span>
            <select
              aria-label=${control.label}
              aria-describedby=${descriptionId}
              ?disabled=${this.disabled}
              @change=${(event: Event) => {
                const select = event.currentTarget as HTMLSelectElement;
                const choiceIndex = Number(select.selectedOptions[0]?.dataset["choiceIndex"]);
                const choice = Number.isInteger(choiceIndex)
                  ? control.choices[choiceIndex]
                  : undefined;
                if (choice !== undefined) this.#emit(control.key, choice.value);
              }}
            >
              ${control.selectedIndex < 0
                ? html`<option value="" .selected=${true}>Select…</option>`
                : nothing}
              ${control.choices.map((choice, index) =>
                html`<option
                  value=${String(choice.value)}
                  data-choice-index=${String(index)}
                  .selected=${index === control.selectedIndex}
                >${choice.label}</option>`
              )}
            </select>
            ${control.description === ""
              ? nothing
              : html`<small id=${descriptionId}>${control.description}</small>`}
          </label>
        `;
      }
      if (control.kind === "boolean") {
        return html`
          <div class="boolean-option" title=${this.compact ? control.description : nothing}>
            <span class="option-label">${control.label}</span>
            <button
              type="button"
              aria-label=${control.label}
              aria-describedby=${descriptionId}
              aria-pressed=${control.selected ? "true" : "false"}
              ?disabled=${this.disabled}
              @click=${() => this.#emit(control.key, !control.selected)}
            >${control.selected ? "On" : "Off"}</button>
            ${control.description === ""
              ? nothing
              : html`<small id=${descriptionId}>${control.description}</small>`}
          </div>
        `;
      }
      return html`
        <label title=${this.compact ? control.description : nothing}>
          <span class="option-label">${control.label}</span>
          <input
            aria-label=${control.label}
            aria-describedby=${descriptionId}
            type=${control.scalarType === "string" ? "text" : "number"}
            step=${control.scalarType === "integer" ? "1" : "any"}
            min=${control.minimum ?? nothing}
            max=${control.maximum ?? nothing}
            placeholder=${control.hint}
            .value=${control.text}
            ?disabled=${this.disabled}
            @input=${(event: Event) =>
              (event.currentTarget as HTMLInputElement).setCustomValidity("")}
            @change=${(event: Event) => {
              const input = event.currentTarget as HTMLInputElement;
              const raw = input.value.trim();
              if (raw === "") {
                this.#emit(control.key, undefined);
                return;
              }
              if (control.scalarType === "string") {
                this.#emit(control.key, raw);
                return;
              }
              const value = Number(raw);
              const valid =
                Number.isFinite(value)
                && (control.scalarType !== "integer" || Number.isInteger(value))
                && (control.minimum === undefined || value >= control.minimum)
                && (control.maximum === undefined || value <= control.maximum);
              if (valid) {
                input.setCustomValidity("");
                this.#emit(control.key, value);
                return;
              }
              input.value = control.text;
              input.setCustomValidity(`Enter a valid ${control.scalarType} ${control.hint}.`);
              input.reportValidity();
              input.setCustomValidity("");
            }}
          />
          ${control.description === ""
            ? nothing
            : html`<small id=${descriptionId}>${control.description}</small>`}
        </label>
      `;
    });
  }

  #emit(key: string, value: ModelOptionValue | undefined): void {
    this.dispatchEvent(new CustomEvent<ModelOptionChangeDetail>(
      MODEL_OPTION_CHANGED_EVENT,
      {
        detail: { key, value },
        bubbles: true,
        composed: true,
      },
    ));
  }
}

customElements.define("trouve-model-options-editor", TrouveModelOptionsEditor);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-model-options-editor": TrouveModelOptionsEditor;
  }

  interface HTMLElementEventMap {
    "trouve-model-option-changed": ModelOptionChangedEvent;
  }
}
