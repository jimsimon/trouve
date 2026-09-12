import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import { errorMessage, Flash, ReviewElement, targetChecked, targetValue } from "./element";
import {
  sanitizeModelOptions,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
} from "./model-settings";
import { routingModeLabel } from "./presentation";
import { selectValue } from "./select-value";
import { modelOptionsSetting, pageHeader, statusPill, thinkingSetting } from "./shared-views";
import { html } from "./template";
import type {
  Dashboard,
  Model,
  ModelOptions,
  Repository,
  ReviewerOverride,
  ReviewerProfile,
} from "./types";

/** The `#/repositories` screen. Rendered inside the app shell's `<section>`. */
export class RepositoriesPage extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    dashboard: { attribute: false },
    models: { attribute: false },
    onChanged: { attribute: false },
    showAll: { state: true },
    query: { state: true },
  };

  api!: ReviewApi;
  dashboard!: Dashboard;
  models: Model[] = [];
  onChanged: () => void = () => {};

  private showAll = false;
  private query = "";

  protected override render(): TemplateResult {
    const dashboard = this.dashboard;
    const repositories = dashboard.repositories.filter(
      (repository) =>
        (this.showAll || repository.mode !== "off") &&
        repository.repository.toLowerCase().includes(this.query.toLowerCase()),
    );
    return html`${pageHeader({
        eyebrow: "Configuration",
        title: "Repositories",
        description:
          "Configured repositories are shown by default. Discovery remains available without cluttering the working set.",
      })}
      <section class="panel">
        <div class="filters">
          <label class="grow">
            Search
            <input
              type="search"
              .value=${live(this.query)}
              placeholder="owner/repository"
              @input=${(event: Event) => {
                this.query = targetValue(event);
              }}
            />
          </label>
          <label class="checkbox">
            <input
              type="checkbox"
              .checked=${live(this.showAll)}
              @change=${(event: Event) => {
                this.showAll = targetChecked(event);
              }}
            />
            Show all discovered repositories
          </label>
        </div>
        <p class="muted">Showing ${repositories.length} of ${dashboard.repositories.length} repositories.</p>
        <div class="repository-list">
          ${repeat(
            repositories,
            (repository) => repository.repository,
            (repository) => html`<trouve-code-review-repository-editor
              .api=${this.api}
              .repository=${repository}
              .reviewers=${dashboard.reviewers}
              .models=${this.models}
              .onSaved=${this.onChanged}
            ></trouve-code-review-repository-editor>`,
          )}
        </div>
      </section>`;
  }
}

const modelOptions = (models: Model[]): TemplateResult[] =>
  models.map(
    (model) => html`<option value=${model.id}>${model.display_name} · ${model.id}</option>`,
  );

/** One collapsible repository configuration form with its own unsaved draft. */
export class RepositoryEditor extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    repository: { attribute: false },
    reviewers: { attribute: false },
    models: { attribute: false },
    onSaved: { attribute: false },
    draft: { state: true },
    busy: { state: true },
  };

  api!: ReviewApi;
  repository!: Repository;
  reviewers: ReviewerProfile[] = [];
  models: Model[] = [];
  onSaved: () => void = () => {};

  private draft!: Repository;
  private busy = false;
  private readonly message = new Flash(this);
  private readonly draftEffect = this.effect();

  protected override willUpdate(): void {
    if (!this.hasUpdated) this.draft = this.repository;
  }

  protected override updated(): void {
    this.draftEffect.run([JSON.stringify(this.repository)], () => {
      this.draft = this.repository;
    });
  }

  private async persistRepository(
    next: Repository,
    successMessage = "Saved",
  ): Promise<void> {
    this.busy = true;
    try {
      await this.api.saveRepository(next);
      this.message.flash(successMessage);
      this.onSaved();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  private togglePersona(id: string): void {
    const current = this.draft;
    if (current.routing_mode === "automatic") return;
    if (current.routing_mode === "manual") {
      this.draft = {
        ...current,
        reviewer_ids: current.reviewer_ids.includes(id)
          ? current.reviewer_ids.filter((reviewer) => reviewer !== id)
          : [...current.reviewer_ids, id],
      };
      return;
    }
    const included = current.included_reviewer_ids ?? [];
    this.draft = {
      ...current,
      included_reviewer_ids: included.includes(id)
        ? included.filter((reviewer) => reviewer !== id)
        : [...included, id],
    };
  }

  private updateReviewerOverride(id: string, patch: Partial<ReviewerOverride>): void {
    const current = this.draft;
    const overrides = current.reviewer_overrides ?? [];
    const existing = overrides.find((item) => item.reviewer_id === id) ?? {
      reviewer_id: id,
      prompt_mode: "inherit" as const,
      prompt: "",
    };
    const updated = { ...existing, ...patch };
    const retained = overrides.filter((item) => item.reviewer_id !== id);
    if (
      updated.model ||
      updated.thinking_level ||
      Object.keys(updated.model_options ?? {}).length > 0 ||
      updated.prompt_mode !== "inherit" ||
      updated.prompt
    ) {
      retained.push(updated);
    }
    this.draft = { ...current, reviewer_overrides: retained };
  }

  protected override render(): TemplateResult {
    const repository = this.repository;
    const reviewers = this.reviewers;
    const models = this.models;
    const draft = this.draft;
    const busy = this.busy;
    const message = this.message.message;
    const effectiveCoordinatorModel = models.find((model) => model.id === draft.model);
    const coordinatorThinking = thinkingOptions(effectiveCoordinatorModel);
    const effectiveRouterModel = models.find(
      (model) => model.id === (draft.router_model || draft.model),
    );
    const routerThinking = thinkingOptions(effectiveRouterModel);
    const effectiveAnalystModel = models.find(
      (model) => model.id === (draft.analyst_model || draft.model),
    );
    const analystThinking = thinkingOptions(effectiveAnalystModel);
    const compatibleThinking = (
      configured: string | undefined,
      model: Model | undefined,
    ): string | undefined => {
      if (!configured || !model) return configured;
      return thinkingSelectionIsValid(model, configured) ? configured : undefined;
    };
    const compatibleOptions = (
      configured: ModelOptions | undefined,
      model: Model | undefined,
    ): ModelOptions | undefined => {
      if (!configured || !Object.keys(configured).length) return undefined;
      // Like compatibleThinking, keep the stored options when the effective
      // model is not in the loaded catalog; the server re-validates on save.
      if (!model) return configured;
      const sanitized = sanitizeModelOptions(model, configured);
      return Object.keys(sanitized).length ? sanitized : undefined;
    };
    const reviewerPolicyInvalid =
      draft.mode !== "off" &&
      (draft.routing_mode === "manual"
        ? draft.reviewer_ids.length === 0
        : reviewers.length === 0);
    const reviewModelInvalid = draft.mode !== "off" && !draft.model;
    const semanticRouterConfigEnabled = draft.routing_mode !== "manual";
    const semanticRouterRequirement =
      "Choose Additive or Automatic persona selection to configure it.";
    return html`<details class="repository-editor">
      <summary>
        <span>
          <strong>${repository.repository}</strong>
          <small>${repository.private ? "private" : "public"} · installation ${repository.installation_id} · ${routingModeLabel(repository.routing_mode)} persona selection</small>
        </span>
        ${statusPill(repository.mode === "off" ? "disabled" : repository.mode)}
      </summary>
      <form
        @submit=${(event: Event) => {
          event.preventDefault();
          void this.persistRepository(this.draft);
        }}
      >
        <div class="form-grid">
          <label>
            Review mode
            <select
              @change=${(event: Event) => {
                this.draft = { ...this.draft, mode: targetValue(event) as Repository["mode"] };
              }}
            >
              <option value="off">Off</option>
              <option value="manual">Manual requests</option>
              <option value="automatic">Automatic</option>
              ${selectValue(draft.mode)}
            </select>
            <small>Off prevents reviews. Manual runs only when requested; Automatic reviews eligible pull request updates.</small>
          </label>
          <label>
            Persona selection mode
            <select
              @change=${(event: Event) => {
                const routingMode = targetValue(event) as Repository["routing_mode"];
                this.draft = {
                  ...this.draft,
                  routing_mode: routingMode,
                  semantic_routing: routingMode !== "manual",
                  included_reviewer_ids:
                    routingMode === "automatic" ? [] : this.draft.included_reviewer_ids,
                  excluded_reviewer_ids: [],
                };
              }}
            >
              <option value="manual">Manual</option>
              <option value="additive">Additive</option>
              <option value="automatic">Automatic</option>
              ${selectValue(draft.routing_mode)}
            </select>
            <small>${draft.routing_mode === "manual"
                ? "Runs exactly the personas enabled below; semantic routing is disabled."
                : draft.routing_mode === "additive"
                  ? "Always runs the baseline and enabled core personas, then optionally adds personas using semantic triage."
                  : "Lets semantic triage select personas from the complete catalog."}</small>
          </label>
          <label>
            Coordinator and fallback model
            <select
              @change=${(event: Event) => {
                const draft = this.draft;
                const model = targetValue(event) || undefined;
                const selectedCoordinatorModel = models.find(
                  (candidate) => candidate.id === model,
                );
                const selectedRouterModel = models.find(
                  (candidate) => candidate.id === (draft.router_model || model),
                );
                const selectedAnalystModel = models.find(
                  (candidate) => candidate.id === (draft.analyst_model || model),
                );
                this.draft = {
                  ...draft,
                  model,
                  coordinator_thinking_level: compatibleThinking(
                    draft.coordinator_thinking_level,
                    selectedCoordinatorModel,
                  ),
                  router_thinking_level: compatibleThinking(
                    draft.router_thinking_level,
                    selectedRouterModel,
                  ),
                  analyst_thinking_level: compatibleThinking(
                    draft.analyst_thinking_level,
                    selectedAnalystModel,
                  ),
                  coordinator_model_options: compatibleOptions(
                    draft.coordinator_model_options,
                    selectedCoordinatorModel,
                  ),
                  router_model_options: compatibleOptions(
                    draft.router_model_options,
                    selectedRouterModel,
                  ),
                  analyst_model_options: compatibleOptions(
                    draft.analyst_model_options,
                    selectedAnalystModel,
                  ),
                  reviewer_overrides: (draft.reviewer_overrides ?? []).map((override) => {
                    const profile = reviewers.find(
                      (reviewer) => reviewer.id === override.reviewer_id,
                    );
                    const selectedReviewerModel = models.find(
                      (candidate) =>
                        candidate.id === (override.model || profile?.model || model),
                    );
                    return {
                      ...override,
                      thinking_level: compatibleThinking(
                        override.thinking_level,
                        selectedReviewerModel,
                      ),
                      model_options: compatibleOptions(
                        override.model_options,
                        selectedReviewerModel,
                      ),
                    };
                  }),
                };
              }}
            >
              <option value="">Select a model</option>
              ${modelOptions(models)}
              ${selectValue(draft.model ?? "")}
            </select>
            <small>${models.length
                ? "Runs the final coordinator that validates and combines findings. It is also the fallback for personas without their own model."
                : "No models are currently available. Configure or sign in to a model provider first."}</small>
          </label>
          <label>
            ${coordinatorThinking.budget
              ? "Coordinator thinking budget (tokens)"
              : "Coordinator thinking"}
            ${thinkingSetting({
              options: coordinatorThinking,
              value: draft.coordinator_thinking_level ?? "",
              inheritLabel: "Inherit review persona",
              onChange: (value) => {
                this.draft = {
                  ...this.draft,
                  coordinator_thinking_level: value || undefined,
                };
              },
            })}
            <small>Controls reasoning for the final coordinator. Inherit review persona uses the default configured in Review persona settings.</small>
          </label>
          ${modelOptionsSetting({
            model: effectiveCoordinatorModel,
            options: draft.coordinator_model_options,
            scope: "Coordinator",
            onChange: (options) => {
              this.draft = { ...this.draft, coordinator_model_options: options };
            },
          })}
          <label class=${semanticRouterConfigEnabled ? nothing : "field-disabled"}>
            Semantic router model
            <select
              ?disabled=${!semanticRouterConfigEnabled}
              @change=${(event: Event) => {
                const draft = this.draft;
                const routerModel = targetValue(event) || undefined;
                const selectedRouterModel = models.find(
                  (candidate) => candidate.id === (routerModel || draft.model),
                );
                this.draft = {
                  ...draft,
                  router_model: routerModel,
                  router_thinking_level: compatibleThinking(
                    draft.router_thinking_level,
                    selectedRouterModel,
                  ),
                  router_model_options: compatibleOptions(
                    draft.router_model_options,
                    selectedRouterModel,
                  ),
                };
              }}
            >
              <option value="">Inherit coordinator/fallback model</option>
              ${modelOptions(models)}
              ${selectValue(draft.router_model ?? "")}
            </select>
            <small>Runs the lightweight, read-only triage pass that may add relevant personas.${!semanticRouterConfigEnabled ? ` ${semanticRouterRequirement}` : nothing}</small>
          </label>
          <label class=${semanticRouterConfigEnabled ? nothing : "field-disabled"}>
            ${routerThinking.budget
              ? "Semantic router thinking budget (tokens)"
              : "Semantic router thinking"}
            ${thinkingSetting({
              options: routerThinking,
              value: draft.router_thinking_level ?? "",
              inheritLabel: "Inherit review default",
              disabled: !semanticRouterConfigEnabled,
              onChange: (value) => {
                this.draft = {
                  ...this.draft,
                  router_thinking_level: value || undefined,
                };
              },
            })}
            <small>Controls reasoning for semantic triage. Inherit review default follows the Review mode setting.${!semanticRouterConfigEnabled ? ` ${semanticRouterRequirement}` : nothing}</small>
          </label>
          ${modelOptionsSetting({
            model: effectiveRouterModel,
            options: draft.router_model_options,
            scope: "Semantic router",
            disabled: !semanticRouterConfigEnabled,
            onChange: (options) => {
              this.draft = { ...this.draft, router_model_options: options };
            },
          })}
          <label>
            Change analyst model
            <select
              @change=${(event: Event) => {
                const draft = this.draft;
                const analystModel = targetValue(event) || undefined;
                const selectedAnalystModel = models.find(
                  (candidate) => candidate.id === (analystModel || draft.model),
                );
                this.draft = {
                  ...draft,
                  analyst_model: analystModel,
                  analyst_thinking_level: compatibleThinking(
                    draft.analyst_thinking_level,
                    selectedAnalystModel,
                  ),
                  analyst_model_options: compatibleOptions(
                    draft.analyst_model_options,
                    selectedAnalystModel,
                  ),
                };
              }}
            >
              <option value="">Inherit coordinator/fallback model</option>
              ${modelOptions(models)}
              ${selectValue(draft.analyst_model ?? "")}
            </select>
            <small>Once per review round, reads the full pull-request branch diff and derives what the PR actually builds. The final review editor uses the result as whole-PR context and as the observed counterpoint to the author's stated intent. It never sees the PR title or description, and its output is advisory only — never evidence for or against a finding.</small>
          </label>
          <label>
            ${analystThinking.budget
              ? "Change analyst thinking budget (tokens)"
              : "Change analyst thinking"}
            ${thinkingSetting({
              options: analystThinking,
              value: draft.analyst_thinking_level ?? "",
              inheritLabel: "Inherit review default",
              onChange: (value) => {
                this.draft = {
                  ...this.draft,
                  analyst_thinking_level: value || undefined,
                };
              },
            })}
            <small>Controls reasoning for the PR analysis pass. Inherit review default follows the Review mode setting.</small>
          </label>
          ${modelOptionsSetting({
            model: effectiveAnalystModel,
            options: draft.analyst_model_options,
            scope: "Change analyst",
            onChange: (options) => {
              this.draft = { ...this.draft, analyst_model_options: options };
            },
          })}
        </div>
        <label>
          Repository instructions
          <textarea
            rows="4"
            .value=${live(draft.prompt)}
            @input=${(event: Event) => {
              this.draft = { ...this.draft, prompt: targetValue(event) };
            }}
          ></textarea>
          <small>Adds repository-specific guidance to the instructions used for this repository's reviews.</small>
        </label>
        <fieldset>
          <legend>Persona selection</legend>
          <label
            class="checkbox semantic-routing ${draft.routing_mode !== "additive" ? "field-disabled" : ""}"
          >
            <input
              type="checkbox"
              .checked=${live(
                draft.routing_mode === "automatic" ||
                  (draft.routing_mode === "additive" && draft.semantic_routing),
              )}
              ?disabled=${draft.routing_mode !== "additive"}
              @change=${(event: Event) => {
                this.draft = { ...this.draft, semantic_routing: targetChecked(event) };
              }}
            />
            <span>
              <strong>Semantic triage</strong>
              <small>${draft.routing_mode === "automatic"
                  ? "Required in Automatic mode and solely decides which personas run for each batch."
                  : draft.routing_mode === "additive"
                    ? "Run one lightweight, read-only routing pass per batch. It may add relevant personas but cannot remove baseline or enabled core personas."
                    : "Semantic triage is off in Manual mode; only the checked personas run."}</small>
            </span>
          </label>
          <div class="routing-personas">
            ${repeat(
              reviewers,
              (reviewer) => reviewer.id,
              (reviewer) => {
                const checked =
                  draft.routing_mode === "manual"
                    ? draft.reviewer_ids.includes(reviewer.id)
                    : draft.routing_mode === "additive"
                      ? (draft.included_reviewer_ids ?? []).includes(reviewer.id)
                      : false;
                const disabled = draft.routing_mode === "automatic";
                return html`<label class="routing-persona checkbox ${disabled ? "field-disabled" : ""}">
                  <input
                    type="checkbox"
                    .checked=${live(checked)}
                    ?disabled=${disabled}
                    @change=${() => this.togglePersona(reviewer.id)}
                  />
                  <span>
                    <strong>${reviewer.name}</strong>
                    <small>${draft.routing_mode === "manual"
                        ? "Runs exactly when enabled"
                        : draft.routing_mode === "additive"
                          ? "Always runs when enabled"
                          : "Selected automatically"} · ${reviewer.model || "inherits review model"}</small>
                  </span>
                </label>`;
              },
            )}
          </div>
        </fieldset>
        <fieldset>
          <legend>Persona models and thinking</legend>
          <p class="field-help">Tune a persona for this repository without changing its reusable defaults. Model overrides take precedence over the persona and coordinator fallback; thinking overrides take precedence over the persona and Review persona default.</p>
          <div class="persona-execution-grid">
            ${repeat(
              reviewers,
              (reviewer) => reviewer.id,
              (reviewer) => {
                const override = (draft.reviewer_overrides ?? []).find(
                  (item) => item.reviewer_id === reviewer.id,
                );
                const effectiveModelId = override?.model || reviewer.model || draft.model;
                const effectiveReviewerModel = models.find(
                  (model) => model.id === effectiveModelId,
                );
                const reviewerThinking = thinkingOptions(effectiveReviewerModel);
                return html`<div class="persona-execution">
                  <header>
                    <strong>${reviewer.name}</strong>
                    <small>${effectiveModelId || "No model selected"}</small>
                  </header>
                  <label>
                    Model
                    <select
                      @change=${(event: Event) => {
                        const model = targetValue(event) || undefined;
                        const selectedModel = models.find(
                          (candidate) =>
                            candidate.id === (model || reviewer.model || this.draft.model),
                        );
                        this.updateReviewerOverride(reviewer.id, {
                          model,
                          thinking_level: compatibleThinking(
                            override?.thinking_level,
                            selectedModel,
                          ),
                          model_options: compatibleOptions(
                            override?.model_options,
                            selectedModel,
                          ),
                        });
                      }}
                    >
                      <option value="">Inherit · ${reviewer.model || draft.model || "no model"}</option>
                      ${modelOptions(models)}
                      ${selectValue(override?.model ?? "")}
                    </select>
                    <small>Overrides the model for this persona in this repository only.</small>
                  </label>
                  <label>
                    ${reviewerThinking.budget ? "Thinking budget (tokens)" : "Thinking"}
                    ${thinkingSetting({
                      options: reviewerThinking,
                      value: override?.thinking_level ?? "",
                      inheritLabel: reviewer.default_thinking_level
                        ? `Inherit · ${thinkingLevelLabel(reviewer.default_thinking_level)}`
                        : "Inherit persona/review persona",
                      onChange: (value) =>
                        this.updateReviewerOverride(reviewer.id, {
                          thinking_level: value || undefined,
                        }),
                    })}
                    <small>Overrides this persona's reasoning setting for this repository only.</small>
                  </label>
                  ${modelOptionsSetting({
                    model: effectiveReviewerModel,
                    options: override?.model_options,
                    onChange: (options) =>
                      this.updateReviewerOverride(reviewer.id, {
                        model_options: Object.keys(options).length ? options : undefined,
                      }),
                  })}
                </div>`;
              },
            )}
          </div>
        </fieldset>
        <div class="action-row">
          <button
            type="submit"
            ?disabled=${busy || reviewerPolicyInvalid || reviewModelInvalid}
          >
            ${busy ? "Saving…" : "Save repository"}
          </button>
          ${reviewModelInvalid
            ? html`<span class="error-text">Select a review model before enabling reviews.</span>`
            : nothing}
          ${repository.mode !== "off"
            ? html`<button
                class="danger ghost"
                type="button"
                ?disabled=${busy}
                @click=${() =>
                  void this.persistRepository(
                    { ...repository, mode: "off" },
                    "Reviews disabled",
                  )}
              >
                Disable reviews
              </button>`
            : nothing}
          ${message ? html`<span role="status">${message}</span>` : nothing}
        </div>
      </form>
    </details>`;
  }
}

customElements.define("trouve-code-review-repositories", RepositoriesPage);
customElements.define("trouve-code-review-repository-editor", RepositoryEditor);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-repositories": RepositoriesPage;
    "trouve-code-review-repository-editor": RepositoryEditor;
  }
}
