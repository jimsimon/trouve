import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import type {
  ProtocolCodeReviewRepository,
  ProtocolGithubAppStatus,
  ProtocolModelInfo,
  ProtocolReviewerProfile,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  repositoryDraft,
  repositoryKey,
  repositoryUpdateRequest,
  sanitizeGithubAppStatus,
  type CodeReviewMode,
  type CodeReviewRoutingMode,
  type RepositoryDraft,
  type ReviewerOverrideDraft,
  type ReviewerPromptMode,
} from "./code-review-configuration-model.js";

const CONFIGURATION_RETRY_MS = 5_000;

export interface CodeReviewConfigurationChangeDetail {
  readonly kind: "github-app" | "repository" | "reviewer" | "reviewer-delete";
  readonly id?: string;
}

const formatTimestamp = (value: string | null | undefined): string => {
  if (value === undefined || value === null || value === "") return "Not reported";
  const timestamp = new Date(value);
  return Number.isNaN(timestamp.valueOf())
    ? "Not reported"
    : new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(timestamp);
};

const setMembership = (
  values: readonly string[],
  id: string,
  selected: boolean,
): readonly string[] => selected
  ? values.includes(id) ? values : [...values, id]
  : values.filter((value) => value !== id);

const emptyOverride = (reviewerId: string): ReviewerOverrideDraft => ({
  reviewerId,
  model: "",
  thinkingLevel: "",
  promptMode: "inherit",
  prompt: "",
});

export class TrouveCodeReviewConfiguration extends withSignalTracking(LitElement) {
  static override styles = css`
    :host { display: block; color: var(--trouve-text); }
    * { box-sizing: border-box; }
    .settings-stack { display: grid; gap: 14px; }
    .section-heading { display: flex; align-items: start; gap: 12px; }
    .section-heading > div { flex: 1; min-width: 0; }
    h2, h3, h4, p { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 16px; }
    h3 { color: var(--trouve-text-hi); font-size: 13px; }
    h4 { color: var(--trouve-text-hi); font-size: 12px; }
    p, small { color: var(--trouve-text-dim); }
    .section-heading p, .settings-card > p, fieldset > p, .subsection > p { margin-top: 4px; }
    .settings-card {
      padding: 14px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .card-list { display: grid; gap: 8px; margin-top: 10px; }
    .summary-grid {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 1px;
      margin-top: 10px;
      padding: 1px;
      background: var(--trouve-rule);
    }
    .summary-grid > div { min-width: 0; padding: 9px 10px; background: var(--trouve-inset-bg); }
    .summary-grid dt { color: var(--trouve-text-dim); font-size: 10px; text-transform: uppercase; letter-spacing: .04em; }
    .summary-grid dd { margin: 3px 0 0; overflow-wrap: anywhere; color: var(--trouve-text-hi); }
    .notice { min-height: 20px; color: var(--trouve-text-dim); }
    .notice.error { color: var(--trouve-err); }
    .model-warning {
      margin-top: 10px;
      padding: 9px 10px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-warn);
      background: var(--trouve-inset-bg);
    }
    .status-pill {
      display: inline-flex;
      align-items: center;
      min-height: 22px;
      padding: 2px 7px;
      border-radius: 999px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 11px;
      white-space: nowrap;
    }
    .status-pill.ready { color: var(--trouve-ok); }
    .status-pill.warning { color: var(--trouve-warn); }
    .status-pill.failed { color: var(--trouve-err); }
    form { display: grid; gap: 12px; margin-top: 12px; }
    .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
    .form-grid.three { grid-template-columns: repeat(3, minmax(0, 1fr)); }
    label { display: grid; gap: 4px; min-width: 0; color: var(--trouve-text-hi); font-weight: 600; }
    label small { font-weight: 400; }
    input, select, textarea, button {
      min-height: 36px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
    }
    input, select, textarea { width: 100%; padding: 6px 8px; font-weight: 400; }
    textarea { min-height: 82px; resize: vertical; }
    textarea.private-key { min-height: 126px; font-family: var(--trouve-font-mono); }
    button { padding: 5px 10px; cursor: pointer; }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.danger { border-color: var(--trouve-err); color: var(--trouve-err-soft); }
    button:disabled, input:disabled, select:disabled, textarea:disabled { cursor: not-allowed; opacity: .56; }
    button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible, summary:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
    .section-row { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
    .section-row > div { min-width: 0; }
    details.item {
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-inset-bg);
    }
    details.item > summary {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      min-height: 42px;
      padding: 9px 10px;
      color: var(--trouve-text-hi);
      cursor: pointer;
      font-weight: 600;
    }
    details.item > form, details.item > .item-body { margin: 0; padding: 0 10px 12px; }
    details.subdetails {
      padding: 0 10px;
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-panel-bg);
    }
    details.subdetails > summary {
      min-height: 38px;
      padding: 8px 0;
      color: var(--trouve-text-hi);
      cursor: pointer;
      font-weight: 600;
    }
    .subsection {
      display: grid;
      gap: 9px;
      padding: 10px;
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-panel-bg);
    }
    fieldset {
      min-width: 0;
      margin: 0;
      padding: 10px;
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
    }
    legend { padding: 0 5px; color: var(--trouve-text-hi); font-weight: 600; }
    .check-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 5px 12px; margin-top: 8px; }
    .check-row { display: flex; align-items: center; gap: 8px; min-height: 34px; font-weight: 400; }
    .check-row input { width: 18px; min-height: 18px; flex: 0 0 auto; accent-color: var(--trouve-accent); }
    .override-list { display: grid; gap: 8px; margin: 8px 0 10px; }
    .override-card {
      display: grid;
      gap: 9px;
      padding: 10px;
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-inset-bg);
    }
    .empty { padding: 9px 0; color: var(--trouve-text-dim); }
    .visually-hidden {
      position: absolute;
      width: 1px;
      height: 1px;
      padding: 0;
      margin: -1px;
      overflow: hidden;
      clip: rect(0, 0, 0, 0);
      white-space: nowrap;
      border: 0;
    }
    @media (max-width: 720px) {
      .summary-grid, .form-grid, .form-grid.three, .check-grid { grid-template-columns: 1fr; }
      .section-heading, .section-row { align-items: stretch; flex-direction: column; }
      .actions { justify-content: stretch; }
      .actions button, button, input, select, textarea, summary { min-height: 44px; }
      details.item > summary { align-items: flex-start; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  #loadedServices: AppServices | undefined;
  #app: ProtocolGithubAppStatus | undefined;
  #appNeedsAttention = false;
  #repositories: readonly ProtocolCodeReviewRepository[] = [];
  #reviewers: readonly ProtocolReviewerProfile[] = [];
  #models: readonly ProtocolModelInfo[] = [];
  #repositoryDrafts = new Map<string, RepositoryDraft>();
  #loading = true;
  #modelsUnavailable = false;
  #busy = "";
  #notice = "";
  #noticeIsError = false;
  #loadGeneration = 0;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;

  #availableModels(): readonly ProtocolModelInfo[] {
    const catalog = this.#services.value?.modelCatalog.current;
    if (catalog === undefined) return this.#models;
    const models = readSignal(catalog);
    return models.length === 0 ? this.#models : models;
  }

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#loadedServices = undefined;
    this.#clearRetry();
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const services = this.#services.value;
    if (services !== undefined && services !== this.#loadedServices) {
      this.#loadedServices = services;
      void this.#load();
    }
  }

  override render() {
    const services = this.#services.value;
    const models = this.#availableModels();
    if (services === undefined || (this.#loading && this.#app === undefined)) {
      return html`<div class="settings-card" role="status">Loading code-review configuration…</div>`;
    }
    if (this.#app === undefined) {
      return html`
        <div class="settings-card" role="alert">
          Code-review configuration could not be loaded. Retrying automatically.
        </div>
      `;
    }

    return html`
      <section class="settings-stack" aria-labelledby="code-review-configuration-title">
        <datalist id="code-review-models">
          ${models.map((model) => html`<option value=${model.id}>${model.display_name}</option>`)}
        </datalist>
        <header class="section-heading">
          <div>
            <h2 id="code-review-configuration-title">Code review</h2>
            <p>Configure the GitHub App, repository policies, and focused personas.</p>
          </div>
        </header>
        <p class=${`notice${this.#noticeIsError ? " error" : ""}`} role=${this.#noticeIsError ? "alert" : "status"} aria-live="polite">
          ${this.#notice}
        </p>
        ${this.#modelsUnavailable
          ? html`<p class="model-warning" role="note">Model choices could not be loaded. Existing model ids remain editable while trouve retries automatically.</p>`
          : nothing}
        ${this.#renderGithubApp(this.#app)}
        ${this.#renderRepositories()}
      </section>
    `;
  }

  #renderGithubApp(app: ProtocolGithubAppStatus) {
    const statusClass = this.#appNeedsAttention
      ? "failed"
      : app.configured ? "ready" : "warning";
    const statusLabel = this.#appNeedsAttention
      ? "Needs attention"
      : app.configured ? "Configured" : "Not configured";
    return html`
      <section class="settings-card" aria-labelledby="github-app-title">
        <div class="section-row">
          <div>
            <h3 id="github-app-title">GitHub App</h3>
            <p>Credentials are write-only. Saving replaces the current App credentials after server validation.</p>
          </div>
          <span class=${`status-pill ${statusClass}`}>${statusLabel}</span>
        </div>
        <dl class="summary-grid">
          <div><dt>App id</dt><dd>${app.app_id ?? "Not set"}</dd></div>
          <div><dt>Bot</dt><dd>${app.bot_login || app.slug || "Not reported"}</dd></div>
          <div><dt>Installations</dt><dd>${app.installation_count ?? 0}</dd></div>
          <div><dt>Checks permission</dt><dd>${app.checks_write_configured ? "Ready" : "Not confirmed"}</dd></div>
          <div><dt>Webhook</dt><dd>${app.webhook_configured ? "Configured" : "Polling only"}</dd></div>
          <div><dt>Re-run actions</dt><dd>${app.check_run_webhook_configured ? "Ready" : "Not confirmed"}</dd></div>
          <div><dt>Last poll</dt><dd>${formatTimestamp(app.last_poll_at)}</dd></div>
          <div><dt>Rate limit remaining</dt><dd>${app.rate_limit_remaining ?? "Not reported"}</dd></div>
          <div><dt>Rate limit reset</dt><dd>${formatTimestamp(app.rate_limit_reset_at)}</dd></div>
        </dl>
        ${this.#appNeedsAttention
          ? html`<p class="model-warning" role="alert">The GitHub App reported a problem. Recheck its permissions and credentials; status updates automatically.</p>`
          : nothing}
        <form @submit=${(event: SubmitEvent) => void this.#configureGithubApp(event)}>
          <div class="form-grid">
            <label>
              App id
              <input name="app_id" type="number" min="1" step="1" required inputmode="numeric" .value=${app.app_id === null || app.app_id === undefined ? "" : String(app.app_id)} />
            </label>
            <label>
              Webhook secret
              <input name="webhook_secret" type="password" autocomplete="new-password" spellcheck="false" placeholder="Leave blank to disable webhooks" />
              <small>Blank disables webhook verification and uses reconciliation polling only.</small>
            </label>
          </div>
          <label>
            RSA private key (PEM)
            <textarea class="private-key" name="private_key_pem" required autocomplete="off" autocapitalize="off" spellcheck="false" placeholder="-----BEGIN RSA PRIVATE KEY-----"></textarea>
            <small>Paste the complete key. It is cleared from this form immediately when submitted and is never returned.</small>
          </label>
          <div class="actions">
            <button class="primary" type="submit" ?disabled=${this.#busy !== ""}>${app.configured ? "Replace App credentials" : "Configure GitHub App"}</button>
          </div>
        </form>
      </section>
    `;
  }

  #renderRepositories() {
    return html`
      <section class="settings-card" aria-labelledby="review-repositories-title">
        <h3 id="review-repositories-title">Repositories</h3>
        <p>Choose review triggers, coordinator and router models, reviewer routing, and repository-specific instructions.</p>
        <div class="card-list">
          ${this.#repositories.length === 0
            ? html`<div class="empty">No repositories are visible to the configured GitHub App.</div>`
            : this.#repositories.map((repository) => this.#renderRepository(repository))}
        </div>
      </section>
    `;
  }

  #renderRepository(repository: ProtocolCodeReviewRepository) {
    const key = repositoryKey(repository);
    const draft = this.#repositoryDrafts.get(key) ?? repositoryDraft(repository);
    const repositoryBusy = this.#busy === `repository:${key}`;
    return html`
      <details class="item">
        <summary>
          <span>${repository.repository}${repository.private ? " · Private" : ""}</span>
          <span class=${`status-pill ${draft.mode === "automatic" ? "ready" : draft.mode === "manual" ? "warning" : ""}`}>${draft.mode === "off" ? "Off" : draft.mode === "manual" ? "On request" : "Automatic"}</span>
        </summary>
        <form @submit=${(event: SubmitEvent) => void this.#saveRepository(event, repository)}>
          <div class="form-grid three">
            <label>
              Review trigger
              <select .value=${draft.mode} ?disabled=${this.#busy !== ""} @change=${(event: Event) => this.#patchRepositoryDraft(key, { mode: (event.currentTarget as HTMLSelectElement).value as CodeReviewMode })}>
                <option value="off">Off</option>
                <option value="manual">On request</option>
                <option value="automatic">Every eligible revision</option>
              </select>
            </label>
            <label>
              Coordinator model
              <input list="code-review-models" autocomplete="off" spellcheck="false" .value=${draft.model} .required=${draft.mode !== "off"} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { model: (event.currentTarget as HTMLInputElement).value })} />
              <small>Required while reviews are enabled.</small>
            </label>
            <label>
              Coordinator thinking
              <input autocomplete="off" spellcheck="false" placeholder="Inherit, level, or token budget" .value=${draft.coordinatorThinkingLevel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { coordinatorThinkingLevel: (event.currentTarget as HTMLInputElement).value })} />
            </label>
          </div>

          <label>
            Repository instructions
            <textarea .value=${draft.prompt} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { prompt: (event.currentTarget as HTMLTextAreaElement).value })} placeholder="Extra constraints or context for reviews in this repository"></textarea>
          </label>

          <fieldset>
            <legend>Persona routing</legend>
            <div class="form-grid three">
              <label>
                Selection mode
                <select .value=${draft.routingMode} ?disabled=${this.#busy !== ""} @change=${(event: Event) => this.#setRoutingMode(key, (event.currentTarget as HTMLSelectElement).value as CodeReviewRoutingMode)}>
                  <option value="manual">Selected personas only</option>
                  <option value="additive">Always include + route</option>
                  <option value="automatic">Route all personas</option>
                </select>
              </label>
              <label>
                Router model
                <input list="code-review-models" autocomplete="off" spellcheck="false" placeholder="Inherit coordinator model" .value=${draft.routerModel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { routerModel: (event.currentTarget as HTMLInputElement).value })} />
              </label>
              <label>
                Router thinking
                <input autocomplete="off" spellcheck="false" placeholder="Inherit, level, or token budget" .value=${draft.routerThinkingLevel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { routerThinkingLevel: (event.currentTarget as HTMLInputElement).value })} />
              </label>
              <label>
                PR analyst model
                <input list="code-review-models" autocomplete="off" spellcheck="false" placeholder="Inherit coordinator model" .value=${draft.analystModel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { analystModel: (event.currentTarget as HTMLInputElement).value })} />
                <small>Derives what the PR builds from the full branch diff each round; the final editor uses it as whole-PR context.</small>
              </label>
              <label>
                PR analyst thinking
                <input autocomplete="off" spellcheck="false" placeholder="Inherit, level, or token budget" .value=${draft.analystThinkingLevel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchRepositoryDraft(key, { analystThinkingLevel: (event.currentTarget as HTMLInputElement).value })} />
              </label>
            </div>
            <label class="check-row">
              <input type="checkbox" .checked=${draft.semanticRouting} ?disabled=${this.#busy !== "" || draft.routingMode === "manual"} @change=${(event: Event) => this.#patchRepositoryDraft(key, { semanticRouting: (event.currentTarget as HTMLInputElement).checked })} />
              Allow one read-only semantic routing pass per diff batch
            </label>
          </fieldset>

          ${draft.routingMode === "manual"
            ? this.#renderReviewerSelection(key, "reviewerIds", "Personas run for every requested review", "Select at least one persona while reviews are enabled.", draft.reviewerIds)
            : draft.routingMode === "additive"
              ? html`
                  ${this.#renderReviewerSelection(key, "includedReviewerIds", "Always included personas", "Routing may add other relevant personas.", draft.includedReviewerIds)}
                  ${this.#renderReviewerSelection(key, "excludedReviewerIds", "Excluded from routing", "Keep these personas out of routed review batches.", draft.excludedReviewerIds, false)}
                `
              : this.#renderReviewerSelection(key, "excludedReviewerIds", "Excluded from routing", "All other personas are eligible for routing.", draft.excludedReviewerIds)}

          ${this.#renderReviewerOverrides(key, draft)}
          <div class="actions">
            <button class="primary" type="submit" ?disabled=${this.#busy !== ""}>${repositoryBusy ? "Saving…" : "Save repository"}</button>
          </div>
        </form>
      </details>
    `;
  }

  #renderReviewerSelection(
    key: string,
    field: "reviewerIds" | "includedReviewerIds" | "excludedReviewerIds",
    legend: string,
    description: string,
    selected: readonly string[],
    showEmptyAction = true,
  ) {
    return html`
      <fieldset>
        <legend>${legend}</legend>
        <p>${description}</p>
        ${this.#reviewers.length === 0
          ? html`<div class="empty">
              <span>No personas are available before enabling reviews.</span>
              ${showEmptyAction
                ? html`<button type="button" ?disabled=${this.#busy !== ""} @click=${() => this.#services.value?.router.navigate({ kind: "settings", section: "personas" })}>Open Personas &amp; Models</button>`
                : nothing}
            </div>`
          : html`
              <div class="check-grid">
                ${this.#reviewers.map((reviewer) => html`
                  <label class="check-row">
                    <input type="checkbox" .checked=${selected.includes(reviewer.id)} ?disabled=${this.#busy !== ""} @change=${(event: Event) => this.#toggleRepositoryReviewer(key, field, reviewer.id, (event.currentTarget as HTMLInputElement).checked)} />
                    ${reviewer.name}
                  </label>
                `)}
              </div>
            `}
      </fieldset>
    `;
  }

  #renderReviewerOverrides(key: string, draft: RepositoryDraft) {
    return html`
      <details class="subdetails">
        <summary>Persona-specific overrides</summary>
        <p>Override a persona only for this repository. Resetting an override restores persona and repository inheritance.</p>
        <div class="override-list">
          ${this.#reviewers.length === 0
            ? html`<div class="empty">No personas are available.</div>`
            : this.#reviewers.map((reviewer) => {
                const stored = draft.reviewerOverrides.find((override) => override.reviewerId === reviewer.id);
                const override = stored ?? emptyOverride(reviewer.id);
                return html`
                  <article class="override-card">
                    <div class="section-row">
                      <h4>${reviewer.name}</h4>
                      ${stored === undefined
                        ? html`<span class="status-pill">Inherited</span>`
                        : html`<button type="button" ?disabled=${this.#busy !== ""} @click=${() => this.#resetReviewerOverride(key, reviewer.id)}>Reset override</button>`}
                    </div>
                    <div class="form-grid three">
                      <label>
                        Model
                        <input list="code-review-models" autocomplete="off" spellcheck="false" placeholder="Inherit" .value=${override.model} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchReviewerOverride(key, reviewer.id, { model: (event.currentTarget as HTMLInputElement).value })} />
                      </label>
                      <label>
                        Thinking
                        <input autocomplete="off" spellcheck="false" placeholder="Inherit" .value=${override.thinkingLevel} ?disabled=${this.#busy !== ""} @input=${(event: Event) => this.#patchReviewerOverride(key, reviewer.id, { thinkingLevel: (event.currentTarget as HTMLInputElement).value })} />
                      </label>
                      <label>
                        Prompt behavior
                        <select .value=${override.promptMode} ?disabled=${this.#busy !== ""} @change=${(event: Event) => this.#patchReviewerOverride(key, reviewer.id, { promptMode: (event.currentTarget as HTMLSelectElement).value as ReviewerPromptMode })}>
                          <option value="inherit">Inherit persona prompt</option>
                          <option value="append">Append instructions</option>
                          <option value="replace">Replace persona prompt</option>
                        </select>
                      </label>
                    </div>
                    <label>
                      Prompt override
                      <textarea .value=${override.prompt} ?disabled=${this.#busy !== "" || override.promptMode === "inherit"} @input=${(event: Event) => this.#patchReviewerOverride(key, reviewer.id, { prompt: (event.currentTarget as HTMLTextAreaElement).value })} placeholder=${override.promptMode === "append" ? "Additional instructions" : "Repository-specific persona prompt"}></textarea>
                    </label>
                  </article>
                `;
              })}
        </div>
      </details>
    `;
  }


  async #load(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    this.#clearRetry();
    const generation = ++this.#loadGeneration;
    this.#loading = true;
    this.requestUpdate();

    const modelsPromise: Promise<{ readonly models: readonly ProtocolModelInfo[]; readonly unavailable: boolean }> = services.modelCatalog.refresh("if-stale")
      .then((models) => ({ models, unavailable: false }))
      .catch(() => ({ models: [], unavailable: true }));
    try {
      const [dashboard, modelResult] = await Promise.all([
        services.protocol.codeReviewDashboard(),
        modelsPromise,
      ]);
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      const safeApp = sanitizeGithubAppStatus(dashboard.app);
      this.#app = safeApp.status;
      this.#appNeedsAttention = safeApp.needsAttention;
      this.#repositories = dashboard.repositories;
      this.#reviewers = dashboard.reviewers;
      this.#models = modelResult.models;
      this.#modelsUnavailable = modelResult.unavailable;
      if (modelResult.unavailable) this.#scheduleRetry();
      if (this.#notice === "Code-review configuration could not be loaded. Retrying automatically.") {
        this.#setNotice("", false);
      }
      this.#repositoryDrafts = new Map(dashboard.repositories.map((repository) => [
        repositoryKey(repository),
        repositoryDraft(repository),
      ]));
      this.#loading = false;
    } catch {
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#loading = false;
      this.#setNotice("Code-review configuration could not be loaded. Retrying automatically.", true);
      this.#scheduleRetry();
    }
    this.requestUpdate();
  }

  #scheduleRetry(): void {
    if (!this.isConnected || this.#retryTimer !== undefined) return;
    this.#retryTimer = globalThis.setTimeout(() => {
      this.#retryTimer = undefined;
      if (
        this.#busy !== ""
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) {
        this.#scheduleRetry();
        return;
      }
      void this.#load();
    }, CONFIGURATION_RETRY_MS);
  }

  #clearRetry(): void {
    if (this.#retryTimer === undefined) return;
    globalThis.clearTimeout(this.#retryTimer);
    this.#retryTimer = undefined;
  }

  async #configureGithubApp(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    const appIdControl = form.elements.namedItem("app_id");
    const privateKeyControl = form.elements.namedItem("private_key_pem");
    const webhookControl = form.elements.namedItem("webhook_secret");
    if (
      services === undefined ||
      this.#busy !== "" ||
      !(appIdControl instanceof HTMLInputElement) ||
      !(privateKeyControl instanceof HTMLTextAreaElement) ||
      !(webhookControl instanceof HTMLInputElement)
    ) return;

    const appId = Number(appIdControl.value);
    const privateKey = privateKeyControl.value;
    const webhookSecret = webhookControl.value;
    if (!Number.isSafeInteger(appId) || appId < 1 || privateKey === "") {
      this.#setNotice("Enter a valid App id and complete RSA private key.", true);
      return;
    }
    privateKeyControl.value = "";
    webhookControl.value = "";

    this.#busy = "github-app";
    this.requestUpdate();
    try {
      const result = await services.protocol.configureCodeReviewGithubApp({
        app_id: appId,
        private_key_pem: privateKey,
        webhook_secret: webhookSecret,
      });
      const safeApp = sanitizeGithubAppStatus(result);
      this.#app = safeApp.status;
      this.#appNeedsAttention = safeApp.needsAttention;
      this.#setNotice("GitHub App configuration was saved.", false);
      this.#dispatchChange("github-app");
    } catch {
      this.#setNotice("GitHub App configuration could not be saved. Verify the values and try again.", true);
    } finally {
      privateKeyControl.value = "";
      webhookControl.value = "";
      this.#busy = "";
      this.requestUpdate();
    }
  }

  #patchRepositoryDraft(key: string, patch: Partial<RepositoryDraft>): void {
    const draft = this.#repositoryDrafts.get(key);
    if (draft === undefined) return;
    this.#repositoryDrafts.set(key, { ...draft, ...patch });
    this.requestUpdate();
  }

  #setRoutingMode(key: string, routingMode: CodeReviewRoutingMode): void {
    const draft = this.#repositoryDrafts.get(key);
    if (draft === undefined) return;
    this.#patchRepositoryDraft(key, {
      routingMode,
      semanticRouting: routingMode === "manual"
        ? false
        : draft.routingMode === "manual" ? true : draft.semanticRouting,
    });
  }

  #toggleRepositoryReviewer(
    key: string,
    field: "reviewerIds" | "includedReviewerIds" | "excludedReviewerIds",
    reviewerId: string,
    selected: boolean,
  ): void {
    const draft = this.#repositoryDrafts.get(key);
    if (draft === undefined) return;
    if (field === "reviewerIds") {
      this.#patchRepositoryDraft(key, { reviewerIds: setMembership(draft.reviewerIds, reviewerId, selected) });
      return;
    }
    if (field === "includedReviewerIds") {
      this.#patchRepositoryDraft(key, {
        includedReviewerIds: setMembership(draft.includedReviewerIds, reviewerId, selected),
        ...(selected ? { excludedReviewerIds: setMembership(draft.excludedReviewerIds, reviewerId, false) } : {}),
      });
      return;
    }
    this.#patchRepositoryDraft(key, {
      excludedReviewerIds: setMembership(draft.excludedReviewerIds, reviewerId, selected),
      ...(selected ? { includedReviewerIds: setMembership(draft.includedReviewerIds, reviewerId, false) } : {}),
    });
  }

  #patchReviewerOverride(
    key: string,
    reviewerId: string,
    patch: Partial<ReviewerOverrideDraft>,
  ): void {
    const draft = this.#repositoryDrafts.get(key);
    if (draft === undefined) return;
    const index = draft.reviewerOverrides.findIndex((override) => override.reviewerId === reviewerId);
    const current = index < 0 ? undefined : draft.reviewerOverrides[index];
    const updated: ReviewerOverrideDraft = { ...(current ?? emptyOverride(reviewerId)), ...patch };
    const overrides = index < 0
      ? [...draft.reviewerOverrides, updated]
      : draft.reviewerOverrides.map((override, overrideIndex) => overrideIndex === index ? updated : override);
    this.#patchRepositoryDraft(key, { reviewerOverrides: overrides });
  }

  #resetReviewerOverride(key: string, reviewerId: string): void {
    const draft = this.#repositoryDrafts.get(key);
    if (draft === undefined) return;
    this.#patchRepositoryDraft(key, {
      reviewerOverrides: draft.reviewerOverrides.filter((override) => override.reviewerId !== reviewerId),
    });
  }

  async #saveRepository(event: SubmitEvent, repository: ProtocolCodeReviewRepository): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const key = repositoryKey(repository);
    const draft = this.#repositoryDrafts.get(key);
    if (services === undefined || draft === undefined || this.#busy !== "") return;
    if (draft.mode !== "off" && draft.model.trim() === "") {
      this.#setNotice(`Choose a coordinator model before enabling reviews for ${repository.repository}.`, true);
      return;
    }
    if (draft.mode !== "off" && this.#reviewers.length === 0) {
      this.#setNotice("No personas are available. Create one under Settings → Personas & Models before enabling repository reviews.", true);
      return;
    }
    if (draft.mode !== "off" && draft.routingMode === "manual" && draft.reviewerIds.length === 0) {
      this.#setNotice(`Select at least one persona for ${repository.repository}.`, true);
      return;
    }

    const request = repositoryUpdateRequest(repository, draft);
    this.#busy = `repository:${key}`;
    this.requestUpdate();
    try {
      const updated = await services.protocol.updateCodeReviewRepository(request);
      this.#repositories = this.#repositories.map((current) => repositoryKey(current) === key ? updated : current);
      this.#repositoryDrafts.set(key, repositoryDraft(updated));
      this.#setNotice(`${repository.repository} review configuration was saved.`, false);
      this.#dispatchChange("repository", key);
    } catch {
      this.#setNotice(`Repository review configuration could not be saved. Try again.`, true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }


  #dispatchChange(kind: CodeReviewConfigurationChangeDetail["kind"], id?: string): void {
    this.dispatchEvent(new CustomEvent<CodeReviewConfigurationChangeDetail>(
      "trouve-code-review-configuration-changed",
      {
        detail: { kind, ...(id === undefined ? {} : { id }) },
        bubbles: true,
        composed: true,
      },
    ));
  }

  #setNotice(message: string, error: boolean): void {
    this.#notice = message;
    this.#noticeIsError = error;
    this.requestUpdate();
  }
}

if ("customElements" in globalThis && !customElements.get("trouve-code-review-configuration")) {
  customElements.define("trouve-code-review-configuration", TrouveCodeReviewConfiguration);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-configuration": TrouveCodeReviewConfiguration;
  }

  interface HTMLElementEventMap {
    "trouve-code-review-configuration-changed": CustomEvent<CodeReviewConfigurationChangeDetail>;
  }
}
