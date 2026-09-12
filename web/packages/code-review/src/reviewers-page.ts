import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import { errorMessage, Flash, ReviewElement, targetValue } from "./element";
import { defaultThinkingSelection, thinkingOptions } from "./model-settings";
import { selectValue } from "./select-value";
import { pageHeader, thinkingSetting } from "./shared-views";
import { html } from "./template";
import type { Model, ReviewerProfile } from "./types";

/** The `#/reviewers` screen body: one editor per persona plus the creation form. */
export function reviewersPage({
  api,
  reviewers,
  models,
  defaultModel,
  onChanged,
}: {
  api: ReviewApi;
  reviewers: ReviewerProfile[];
  models: Model[];
  defaultModel: string | undefined;
  onChanged: () => void;
}): TemplateResult {
  return html`${pageHeader({
      eyebrow: "Review policy",
      title: "Reviewer personas",
      description:
        "Focused personas run concurrently and retain separate model, duration, and issue statistics.",
    })}
    <div class="reviewer-grid">
      ${repeat(
        reviewers,
        (reviewer) => reviewer.id,
        (reviewer) => html`<trouve-code-review-reviewer-editor
          .api=${api}
          .reviewer=${reviewer}
          .models=${models}
          .defaultModel=${defaultModel}
          .onChanged=${onChanged}
        ></trouve-code-review-reviewer-editor>`,
      )}
      <trouve-code-review-reviewer-editor
        .api=${api}
        .reviewer=${undefined}
        .models=${models}
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
    defaultModel: { attribute: false },
    onChanged: { attribute: false },
    draft: { state: true },
    busy: { state: true },
  };

  api!: ReviewApi;
  reviewer: ReviewerProfile | undefined = undefined;
  models: Model[] = [];
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
    const defaultModel = this.defaultModel;
    const draft = this.draft;
    const busy = this.busy;
    const message = this.message.message;
    const reviewerModel = models.find(
      (model) => model.id === (draft.model || defaultModel),
    );
    const reviewerThinking = thinkingOptions(reviewerModel);
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
          @change=${(event: Event) => {
            const model = targetValue(event) || undefined;
            this.draft = {
              ...this.draft,
              model,
              default_thinking_level:
                defaultThinkingSelection(
                  models.find((candidate) => candidate.id === (model || defaultModel)),
                  this.draft.default_thinking_level,
                ) || undefined,
            };
          }}
        >
          <option value="">Inherit repository/system</option>
          ${models.map(
            (model) => html`<option value=${model.id}>${model.display_name} · ${model.id}</option>`,
          )}
          ${selectValue(draft.model ?? "")}
        </select>
        <small>Sets this persona's reusable model. Repository-specific persona overrides take precedence; otherwise Inherit uses the repository coordinator/fallback model.</small>
      </label>
      <label>
        ${reviewerThinking.budget ? "Thinking budget (tokens)" : "Thinking level"}
        ${thinkingSetting({
          options: reviewerThinking,
          value: draft.default_thinking_level ?? "",
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
