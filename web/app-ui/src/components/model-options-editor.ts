import { css, html, LitElement, nothing } from "lit";
import { live } from "lit/directives/live.js";

import { jsonNumberValueToken } from "../services/protocol-json.js";
import {
  type ModelOptionChangeDetail,
  type ModelOptionControl,
  modelOptionScalarValue,
  modelOptionTextValue,
  type TextModelOptionControl,
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
    label, .boolean-option, .text-option {
      min-width: 0;
      display: grid;
      align-content: start;
      gap: 3px;
      color: var(--trouve-text-mid);
      font-size: 10px;
      font-weight: 600;
    }
    :host([compact]) label, :host([compact]) .boolean-option,
    :host([compact]) .text-option {
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
    .text-option-control {
      min-width: 0;
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 4px;
    }
    .reset-option {
      width: 30px;
      padding-inline: 4px;
    }
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
  #committedText = new WeakMap<HTMLInputElement, string>();

  override render() {
    return this.controls.map((control, index) => {
      const descriptionId = control.description === ""
        ? nothing
        : `model-option-description-${index}`;
      if (control.kind === "choice") {
        const overridden = control.overridden ?? true;
        return html`
          <label title=${this.compact ? control.description : nothing}>
            <span class="option-label">${control.label}</span>
            <select
              aria-label=${control.label}
              aria-describedby=${descriptionId}
              ?disabled=${this.disabled}
              @change=${(event: Event) => {
                const select = event.currentTarget as HTMLSelectElement;
                const selected = select.selectedOptions[0];
                if (selected?.dataset["modelDefault"] === "true") {
                  this.#emit({ key: control.key, value: undefined });
                  return;
                }
                const choiceIndex = Number(selected?.dataset["choiceIndex"]);
                const choice = Number.isInteger(choiceIndex)
                  ? control.choices[choiceIndex]
                  : undefined;
                if (choice !== undefined) {
                  this.#emit({ key: control.key, value: choice.value });
                }
              }}
            >
              <option
                value=""
                data-model-default="true"
                .selected=${live(!overridden)}
              >
                Model default${control.selectedIndex < 0
                  ? ""
                  : ` · ${control.choices[control.selectedIndex]?.label ?? ""}`}
              </option>
              ${control.choices.map((choice, index) =>
                html`<option
                  value=${String(modelOptionScalarValue(choice.value))}
                  data-choice-index=${String(index)}
                  .selected=${live(overridden && index === control.selectedIndex)}
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
        const overridden = control.overridden ?? true;
        return html`
          <div class="boolean-option" title=${this.compact ? control.description : nothing}>
            <span class="option-label">${control.label}</span>
            <select
              aria-label=${control.label}
              aria-describedby=${descriptionId}
              .value=${live(overridden ? String(control.selected) : "")}
              ?disabled=${this.disabled}
              @change=${(event: Event) => {
                const select = event.currentTarget as HTMLSelectElement;
                this.#emit({
                  key: control.key,
                  value: select.value === "" ? undefined : select.value === "true",
                });
              }}
            >
              <option value="" data-model-default="true">
                Model default${control.selected === undefined
                  ? ""
                  : ` · ${control.selected ? "On" : "Off"}`}
              </option>
              <option value="true">On</option>
              <option value="false">Off</option>
            </select>
            ${control.description === ""
              ? nothing
              : html`<small id=${descriptionId}>${control.description}</small>`}
          </div>
        `;
      }
      return html`
        <div class="text-option" title=${this.compact ? control.description : nothing}>
          <span class="option-label">${control.label}</span>
          <div class="text-option-control">
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
              @input=${(event: Event) => {
                const input = event.currentTarget as HTMLInputElement;
                input.setCustomValidity("");
                this.#committedText.delete(input);
              }}
              @keydown=${(event: KeyboardEvent) => {
                if (event.key !== "Enter") return;
                event.preventDefault();
                this.#commitText(control, event.currentTarget as HTMLInputElement);
              }}
              @change=${(event: Event) =>
                this.#commitText(control, event.currentTarget as HTMLInputElement)}
            />
            ${control.overridden
              ? html`<button
                  class="reset-option"
                  type="button"
                  aria-label=${`Use model default for ${control.label}`}
                  ?disabled=${this.disabled}
                  @click=${(event: Event) => {
                    ((event.target as HTMLElement)
                      .previousElementSibling as HTMLInputElement).focus();
                    this.#emit({ key: control.key, value: undefined });
                  }}
                >↺</button>`
              : nothing}
          </div>
          ${control.description === ""
            ? nothing
            : html`<small id=${descriptionId}>${control.description}</small>`}
        </div>
      `;
    });
  }

  #commitText(control: TextModelOptionControl, input: HTMLInputElement): void {
    const raw = control.scalarType === "string" ? input.value : input.value.trim();
    const parsed = modelOptionTextValue(control, raw);
    if (parsed === null) {
      input.setCustomValidity(`Enter a valid ${control.scalarType} ${control.hint}.`);
      input.reportValidity();
      return;
    }
    input.setCustomValidity("");
    const committed = typeof parsed === "object"
      ? jsonNumberValueToken(parsed.value)
      : String(parsed ?? "");
    if (this.#committedText.get(input) === committed) return;
    this.#committedText.set(input, committed);
    this.#emit({ key: control.key, value: parsed });
  }

  #emit(detail: ModelOptionChangeDetail): void {
    this.dispatchEvent(new CustomEvent<ModelOptionChangeDetail>(
      MODEL_OPTION_CHANGED_EVENT,
      {
        detail,
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
