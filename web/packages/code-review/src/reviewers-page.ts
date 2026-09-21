import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import { errorMessage, Flash, ReviewElement, targetValue } from "./element";
import {
  defaultThinkingSelection,
  modelForSelection,
  modelSelectionValue,
  sanitizeModelOptions,
  thinkingOptions,
} from "./model-settings";
import { selectValue } from "./select-value";
import {
  modelCatalogStatus,
  modelChoices,
  modelOptionsSetting,
  pageHeader,
  thinkingSetting,
} from "./shared-views";
import { html } from "./template";
import type { Model, ModelOptions, ReviewerProfile } from "./types";

/** The `#/reviewers` screen body: one editor per persona plus the creation form. */
export function reviewersPage({
  api,
  reviewers,
  models,
  modelsLoaded,
  modelsError,
  defaultModel,
  onChanged,
}: {
  api: ReviewApi;
  reviewers: ReviewerProfile[];
  models: Model[];
  modelsLoaded: boolean;
  modelsError: string;
  defaultModel: string | undefined;
  onChanged: () => void;
}): TemplateResult {
  return html`${pageHeader({
      eyebrow: "Review policy",
      title: "Reviewer personas",
      description:
        "Focused personas run concurrently and retain separate model, duration, and issue statistics.",
    })}
    ${modelCatalogStatus(modelsLoaded, modelsError)}
    <div class="reviewer-grid">
      ${repeat(
        reviewers,
        (reviewer) => reviewer.id,
        (reviewer) => html`<trouve-code-review-reviewer-editor
          .api=${api}
          .reviewer=${reviewer}
          .models=${models}
          .modelsLoaded=${modelsLoaded}
          .defaultModel=${defaultModel}
          .onChanged=${onChanged}
        ></trouve-code-review-reviewer-editor>`,
      )}
      <trouve-code-review-reviewer-editor
        .api=${api}
        .reviewer=${undefined}
        .models=${models}
        .modelsLoaded=${modelsLoaded}
        .defaultModel=${defaultModel}
        .onChanged=${onChanged}
      ></trouve-code-review-reviewer-editor>
    </div>`;
}

const emptyReviewer = (): ReviewerProfile => ({
  id: "",
  name: "",
  prompt: "",
  built_in: false,
});

/** Edits one persona (or creates a new one when `reviewer` is undefined). */
export class ReviewerEditor extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    reviewer: { attribute: false },
    models: { attribute: false },
    modelsLoaded: { attribute: false },
    defaultModel: { attribute: false },
    onChanged: { attribute: false },
    draft: { state: true },
    busy: { state: true },
  };

  api!: ReviewApi;
  reviewer: ReviewerProfile | undefined = undefined;
  models: Model[] = [];
  modelsLoaded = false;
  defaultModel: string | undefined = undefined;
  onChanged: () => void = () => {};

  private draft: ReviewerProfile = emptyReviewer();
  private busy = false;
  private readonly message = new Flash(this);
  private readonly draftEffect = this.effect();

  protected override willUpdate(): void {
    if (!this.hasUpdated) this.draft = this.reviewer ?? emptyReviewer();
  }

  protected override updated(): void {
    this.draftEffect.run([JSON.stringify(this.reviewer ?? null)], () => {
      this.draft = this.reviewer ?? emptyReviewer();
    });
  }

  private async submit(event: Event): Promise<void> {
    event.preventDefault();
    this.busy = true;
    try {
      await this.api.saveReviewer(this.draft);
      this.message.flash("Saved");
      if (!this.reviewer) this.draft = emptyReviewer();
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  private async removeReviewer(reviewer: ReviewerProfile): Promise<void> {
    if (!window.confirm(`Delete ${reviewer.name}?`)) return;
    await this.api.deleteReviewer(reviewer.id);
    this.onChanged();
  }

  protected override render(): TemplateResult {
    const reviewer = this.reviewer;
    const models = this.models;
    const modelsLoaded = this.modelsLoaded;
    const defaultModel = this.defaultModel;
    const draft = this.draft;
    const busy = this.busy;
    const message = this.message.message;
    const reviewerModel = modelForSelection(models, draft.model || defaultModel);
    const reviewerThinking = thinkingOptions(reviewerModel);
    // Like the repository editor: drop options the newly selected model does
    // not advertise, but keep stored options when the effective model is not
    // in the loaded catalog so the server can re-validate them on save.
    const compatibleOptions = (
      configured: ModelOptions | undefined,
      model: Model | undefined,
    ): ModelOptions | undefined => {
      if (!configured || !Object.keys(configured).length) return undefined;
      if (!model) return configured;
      const sanitized = sanitizeModelOptions(model, configured);
      return Object.keys(sanitized).length ? sanitized : undefined;
    };
    return html`<form class="panel reviewer-editor" @submit=${(event: Event) => void this.submit(event)}>
      <header>
        <div>
          <p class="eyebrow">${reviewer?.built_in ? "Built in" : reviewer ? "Custom" : "New persona"}</p>
          <h2>${reviewer?.name || "Create reviewer"}</h2>
        </div>
      </header>
      <label>
        Name
        <input
          .value=${live(draft.name)}
          @input=${(event: Event) => {
            this.draft = { ...this.draft, name: targetValue(event) };
          }}
          required
        />
      </label>
      <label>
        Focus prompt
        <textarea
          rows="7"
          .value=${live(draft.prompt)}
          @input=${(event: Event) => {
            this.draft = { ...this.draft, prompt: targetValue(event) };
          }}
          required
        ></textarea>
      </label>
      <label>
        Default model
        <select
          ?disabled=${!modelsLoaded}
          @change=${(event: Event) => {
            const model = targetValue(event) || undefined;
            const effectiveModel = modelForSelection(models, model || defaultModel);
            this.draft = {
              ...this.draft,
              model,
              default_thinking_level:
                defaultThinkingSelection(
                  effectiveModel,
                  this.draft.default_thinking_level,
                ) || undefined,
              model_options: compatibleOptions(this.draft.model_options, effectiveModel),
            };
          }}
        >
          <option value="">Inherit repository/system</option>
          ${modelChoices(models, draft.model)}
          ${selectValue(modelSelectionValue(models, draft.model))}
        </select>
        <small>Sets this persona's reusable model. Repository-specific persona overrides take precedence; otherwise Inherit uses the repository coordinator/fallback model.</small>
      </label>
      <label>
        ${reviewerThinking.budget ? "Thinking budget (tokens)" : "Thinking level"}
        ${thinkingSetting({
          options: reviewerThinking,
          value: draft.default_thinking_level ?? "",
          disabled: !modelsLoaded,
          inheritLabel: "Inherit default",
          onChange: (value) => {
            this.draft = {
              ...this.draft,
              default_thinking_level: value || undefined,
            };
          },
        })}
        <small>Sets this persona's reusable reasoning default. Repository-specific overrides take precedence; otherwise Inherit follows the Review persona default.</small>
      </label>
      ${modelOptionsSetting({
        model: reviewerModel,
        options: draft.model_options,
        disabled: !modelsLoaded,
        onChange: (options) => {
          this.draft = {
            ...this.draft,
            model_options: Object.keys(options).length ? options : undefined,
          };
        },
      })}
      <div class="action-row">
        <button type="submit" ?disabled=${busy}>
          ${busy ? "Saving…" : reviewer ? "Save persona" : "Create persona"}
        </button>
        ${reviewer && !reviewer.built_in
          ? html`<button
              class="danger ghost"
              type="button"
              @click=${() => void this.removeReviewer(reviewer)}
            >
              Delete
            </button>`
          : nothing}
        ${message ? html`<span role="status">${message}</span>` : nothing}
      </div>
    </form>`;
  }
}

customElements.define("trouve-code-review-reviewer-editor", ReviewerEditor);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-reviewer-editor": ReviewerEditor;
  }
}
