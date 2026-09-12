import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";

import type { ReviewApi } from "./api";
import { errorMessage, Flash, ReviewElement, targetValue } from "./element";
import {
  defaultThinkingSelection,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
} from "./model-settings";
import "./provider-settings-card";
import {
  MAX_PARALLEL_REVIEWS,
  reviewSettingsFromMinutes,
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  timeoutMinutes,
} from "./review-settings";
import { selectValue } from "./select-value";
import { health, pageHeader, panelTitle, thinkingSetting } from "./shared-views";
import { html } from "./template";
import type {
  CodeReviewSettings,
  GithubAppStatus,
  Model,
  PersonaInfo,
  ProvidersResponse,
} from "./types";

/** The `#/settings` screen body. */
export function settingsPage({
  api,
  app,
  providers,
  reviewSettings,
  models,
  reviewPersonaInfo,
  onChanged,
}: {
  api: ReviewApi;
  app: GithubAppStatus;
  providers: ProvidersResponse | null;
  reviewSettings: CodeReviewSettings | null;
  models: Model[];
  reviewPersonaInfo: PersonaInfo | undefined;
  onChanged: () => void;
}): TemplateResult {
  return html`${pageHeader({
      eyebrow: "Administration",
      title: "Settings",
      description:
        "Review execution defaults, GitHub App health, and model-provider authentication.",
    })}
    <trouve-code-review-persona-settings
      .api=${api}
      .personaInfo=${reviewPersonaInfo}
      .models=${models}
      .globalModel=${providers?.default_model}
      .globalThinking=${providers?.default_thinking_level}
      .onChanged=${onChanged}
    ></trouve-code-review-persona-settings>
    <trouve-code-review-execution-settings
      .api=${api}
      .settings=${reviewSettings}
      .onChanged=${onChanged}
    ></trouve-code-review-execution-settings>
    <div class="settings-grid">
      <trouve-code-review-github-app-settings
        .api=${api}
        .app=${app}
        .onChanged=${onChanged}
      ></trouve-code-review-github-app-settings>
      <trouve-code-review-provider-settings
        .api=${api}
        .providers=${providers}
        .models=${models}
        .onChanged=${onChanged}
      ></trouve-code-review-provider-settings>
    </div>`;
}

/** Concurrency and timeout form backed by `/config/code-review`. */
export class ReviewExecutionSettings extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    settings: { attribute: false },
    onChanged: { attribute: false },
    maxParallel: { state: true },
    total: { state: true },
    reviewer: { state: true },
    coordinator: { state: true },
    busy: { state: true },
  };

  api!: ReviewApi;
  settings: CodeReviewSettings | null = null;
  onChanged: () => void = () => {};

  private maxParallel = "";
  private total = "";
  private reviewer = "";
  private coordinator = "";
  private busy = false;
  private readonly message = new Flash(this);
  private readonly settingsEffect = this.effect();

  protected override willUpdate(): void {
    if (this.hasUpdated) return;
    const settings = this.settings;
    this.maxParallel = settings ? String(settings.max_parallel_reviews) : "";
    this.total = settings ? timeoutMinutes(settings.total_timeout_seconds) : "";
    this.reviewer = settings ? timeoutMinutes(settings.reviewer_timeout_seconds) : "";
    this.coordinator = settings ? timeoutMinutes(settings.coordinator_timeout_seconds) : "";
  }

  protected override updated(): void {
    const settings = this.settings;
    this.settingsEffect.run(
      [
        settings?.max_parallel_reviews,
        settings?.total_timeout_seconds,
        settings?.reviewer_timeout_seconds,
        settings?.coordinator_timeout_seconds,
      ],
      () => {
        if (!settings) return;
        this.maxParallel = String(settings.max_parallel_reviews);
        this.total = timeoutMinutes(settings.total_timeout_seconds);
        this.reviewer = timeoutMinutes(settings.reviewer_timeout_seconds);
        this.coordinator = timeoutMinutes(settings.coordinator_timeout_seconds);
      },
    );
  }

  private async submit(event: Event): Promise<void> {
    event.preventDefault();
    this.busy = true;
    try {
      await this.api.saveReviewSettings(
        reviewSettingsFromMinutes(this.maxParallel, this.total, this.reviewer, this.coordinator),
      );
      this.message.flash("Review execution settings saved");
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  protected override render(): TemplateResult {
    const busy = this.busy;
    const message = this.message.message;
    return html`<section class="panel settings-card review-timeout-settings">
      ${panelTitle(
        "Review execution",
        "Concurrency and deadlines for unattended review jobs.",
      )}
      ${this.settings
        ? html`<form @submit=${(event: Event) => void this.submit(event)}>
            <div class="form-grid">
              <label>
                Max parallel reviews
                <input
                  type="number"
                  min="1"
                  max=${MAX_PARALLEL_REVIEWS}
                  step="1"
                  required
                  .value=${live(this.maxParallel)}
                  @input=${(event: Event) => {
                    this.maxParallel = targetValue(event);
                  }}
                />
                <small>Concurrent pull-request review jobs. Changes apply immediately.</small>
              </label>
              <label>
                Total review timeout (minutes)
                <input
                  type="number"
                  min=${TIMEOUT_MINUTES_INPUT_MIN}
                  step=${TIMEOUT_MINUTES_INPUT_STEP}
                  required
                  .value=${live(this.total)}
                  @input=${(event: Event) => {
                    this.total = targetValue(event);
                  }}
                />
                <small>Outer deadline covering preparation through publication.</small>
              </label>
              <label>
                Reviewer timeout (minutes)
                <input
                  type="number"
                  min=${TIMEOUT_MINUTES_INPUT_MIN}
                  step=${TIMEOUT_MINUTES_INPUT_STEP}
                  required
                  .value=${live(this.reviewer)}
                  @input=${(event: Event) => {
                    this.reviewer = targetValue(event);
                  }}
                />
                <small>Maximum time for one persona batch, including JSON repair.</small>
              </label>
              <label>
                Final editor timeout (minutes)
                <input
                  type="number"
                  min=${TIMEOUT_MINUTES_INPUT_MIN}
                  step=${TIMEOUT_MINUTES_INPUT_STEP}
                  required
                  .value=${live(this.coordinator)}
                  @input=${(event: Event) => {
                    this.coordinator = targetValue(event);
                  }}
                />
                <small>Maximum time for candidate validation and final selection.</small>
              </label>
            </div>
            <p class="field-help">Higher concurrency increases provider usage and may encounter provider rate limits. The maximum is ${MAX_PARALLEL_REVIEWS}. Environment review variables take precedence over these persisted values.</p>
            <div class="action-row">
              <button type="submit" ?disabled=${busy}>
                ${busy ? "Saving…" : "Save review execution settings"}
              </button>
              ${message ? html`<span role="status">${message}</span>` : nothing}
            </div>
          </form>`
        : html`<p class="muted">Review execution configuration is unavailable.</p>`}
    </section>`;
  }
}

/** Default model and thinking level for the built-in review persona. */
export class ReviewPersonaSettings extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    personaInfo: { attribute: false },
    models: { attribute: false },
    globalModel: { attribute: false },
    globalThinking: { attribute: false },
    onChanged: { attribute: false },
    model: { state: true },
    thinking: { state: true },
    busy: { state: true },
  };

  api!: ReviewApi;
  personaInfo: PersonaInfo | undefined = undefined;
  models: Model[] = [];
  globalModel: string | undefined = undefined;
  globalThinking: string | undefined = undefined;
  onChanged: () => void = () => {};

  private model = "";
  private thinking = "";
  private busy = false;
  private readonly message = new Flash(this);
  private readonly modelEffect = this.effect();
  private readonly thinkingEffect = this.effect();
  private readonly compatibilityEffect = this.effect();

  protected override willUpdate(): void {
    if (this.hasUpdated) return;
    const persona = this.personaInfo?.persona;
    this.model = persona?.default_model ?? "";
    this.thinking = persona?.default_thinking_level ?? "";
  }

  protected override updated(): void {
    const personaInfo = this.personaInfo;
    const persona = personaInfo?.persona;
    // The effective model is the one this render used; the sync effects below
    // may already schedule a newer `model`, which only reaches the
    // compatibility check on the following update, as with the Preact hooks.
    const effectiveModel = this.model || this.globalModel || "";
    const selectedModel = this.models.find((candidate) => candidate.id === effectiveModel);
    this.modelEffect.run([persona?.default_model], () => {
      this.model = persona?.default_model ?? "";
    });
    this.thinkingEffect.run([persona?.default_thinking_level], () => {
      this.thinking = persona?.default_thinking_level ?? "";
    });
    this.compatibilityEffect.run(
      [effectiveModel, selectedModel, personaInfo, personaInfo?.persona.default_thinking_level],
      () => {
        if (!personaInfo || !selectedModel) return;
        // Functional updater: reads the pending value, not the rendered one.
        const current = this.thinking;
        if (!current || thinkingSelectionIsValid(selectedModel, current)) return;
        this.thinking = defaultThinkingSelection(selectedModel);
      },
    );
  }

  private async submit(event: Event, persona: PersonaInfo["persona"]): Promise<void> {
    event.preventDefault();
    this.busy = true;
    try {
      await this.api.savePersona({
        ...persona,
        default_model: this.model || undefined,
        default_thinking_level: this.thinking || undefined,
      });
      this.message.flash("Review persona defaults saved");
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  private async reset(persona: PersonaInfo["persona"]): Promise<void> {
    this.busy = true;
    try {
      await this.api.resetPersona(persona.id);
      this.message.flash("Review persona reset to built-in defaults");
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  protected override render(): TemplateResult {
    const personaInfo = this.personaInfo;
    const persona = personaInfo?.persona;
    const models = this.models;
    const globalModel = this.globalModel;
    const model = this.model;
    const thinking = this.thinking;
    const busy = this.busy;
    const message = this.message.message;
    const effectiveModel = model || globalModel || "";
    const selectedModel = models.find((candidate) => candidate.id === effectiveModel);
    const options = thinkingOptions(selectedModel);
    const inheritedThinking = this.globalThinking
      ? thinkingLevelLabel(this.globalThinking)
      : "model default";
    return html`<section class="panel settings-card review-persona-settings">
      ${panelTitle(
        "Review persona",
        "Defaults for review-persona threads. Repository models still take precedence for automated reviews.",
      )}
      ${persona
        ? html`<form @submit=${(event: Event) => void this.submit(event, persona)}>
            <div class="form-grid">
              <label>
                Default model
                <select
                  @change=${(event: Event) => {
                    const next = targetValue(event);
                    this.model = next;
                    const nextModel = models.find(
                      (candidate) => candidate.id === (next || globalModel),
                    );
                    if (this.thinking && !thinkingSelectionIsValid(nextModel, this.thinking)) {
                      this.thinking = defaultThinkingSelection(nextModel);
                    }
                  }}
                >
                  <option value="">Inherit global${globalModel ? ` · ${globalModel}` : ""}</option>
                  ${models.map(
                    (candidate) => html`<option value=${candidate.id}>${candidate.display_name} · ${candidate.id}</option>`,
                  )}
                  ${selectValue(model)}
                </select>
                <small>Manual review threads inherit this model. Automated jobs continue to use their repository model.</small>
              </label>
              <label>
                ${options.budget ? "Default thinking budget (tokens)" : "Default thinking level"}
                ${thinkingSetting({
                  options,
                  value: thinking,
                  onChange: (value) => {
                    this.thinking = value;
                  },
                  inheritLabel: `Inherit global · ${inheritedThinking}`,
                })}
                <small>Automated coordinators inherit this level; persona-specific settings still take precedence.</small>
              </label>
            </div>
            <div class="action-row">
              <button type="submit" ?disabled=${busy}>
                ${busy ? "Saving…" : "Save review persona"}
              </button>
              ${personaInfo?.origin === "customized"
                ? html`<button
                    class="ghost"
                    type="button"
                    ?disabled=${busy}
                    @click=${() => void this.reset(persona)}
                  >
                    Reset built-in defaults
                  </button>`
                : nothing}
              ${message ? html`<span role="status">${message}</span>` : nothing}
            </div>
          </form>`
        : html`<p class="muted">Review persona configuration is unavailable.</p>`}
    </section>`;
  }
}

/** GitHub App health plus the credential form. */
export class GithubAppSettings extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    app: { attribute: false },
    onChanged: { attribute: false },
    busy: { state: true },
  };

  api!: ReviewApi;
  app!: GithubAppStatus;
  onChanged: () => void = () => {};

  private busy = false;
  private readonly message = new Flash(this);

  private async submit(event: Event): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    this.busy = true;
    try {
      await this.api.configureApp({
        app_id: Number(data.get("app_id")),
        private_key_pem: String(data.get("private_key_pem")),
        webhook_secret: String(data.get("webhook_secret")),
      });
      form.reset();
      this.message.flash("GitHub App saved");
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.busy = false;
    }
  }

  protected override render(): TemplateResult {
    const app = this.app;
    const busy = this.busy;
    const message = this.message.message;
    return html`<section class="panel settings-card">
      ${panelTitle(
        "GitHub App",
        "Credentials are validated before the saved secret is replaced.",
      )}
      <div class="health-list">
        ${health({
          ok: app.configured,
          label: "App credentials",
          detail: app.bot_login || "Not configured",
        })}
        ${health({
          ok: app.checks_write_configured,
          label: "Checks permission",
          detail: "Read and write required to show a PR check",
        })}
        ${health({
          ok: app.contents_write_configured === true,
          label: "Contents permission",
          detail: "Read and write required for GitHub to let the bot resolve finding threads",
        })}
        ${health({
          ok: app.webhook_configured,
          label: "Webhook secret",
          detail: "Optional with polling; secures webhook delivery",
        })}
        ${health({
          ok: app.check_run_webhook_configured,
          optional: true,
          label: "check_run webhook",
          detail: "Optional; enables GitHub Re-run actions",
        })}
      </div>
      <form @submit=${(event: Event) => void this.submit(event)}>
        <label>
          App ID
          <input name="app_id" inputmode="numeric" .defaultValue=${String(app.app_id ?? "")} required />
        </label>
        <label>
          Private key PEM
          <textarea name="private_key_pem" rows="8" required placeholder="-----BEGIN RSA PRIVATE KEY-----"></textarea>
        </label>
        <label>
          Webhook secret
          <input name="webhook_secret" type="password" autocomplete="new-password" />
          <small>Use the same random secret in GitHub App → Webhook. Leave empty for polling-only operation.</small>
        </label>
        <div class="action-row">
          <button type="submit" ?disabled=${busy}>${busy ? "Validating…" : "Save GitHub App"}</button>
          ${message ? html`<span role="status">${message}</span>` : nothing}
        </div>
      </form>
    </section>`;
  }
}

customElements.define("trouve-code-review-execution-settings", ReviewExecutionSettings);
customElements.define("trouve-code-review-persona-settings", ReviewPersonaSettings);
customElements.define("trouve-code-review-github-app-settings", GithubAppSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-execution-settings": ReviewExecutionSettings;
    "trouve-code-review-persona-settings": ReviewPersonaSettings;
    "trouve-code-review-github-app-settings": GithubAppSettings;
  }
}
