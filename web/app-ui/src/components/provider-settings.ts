import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import type {
  ProtocolKnownProvider,
  ProtocolLoginStatus,
  ProtocolProviderInfo,
  ProtocolProvidersResponse,
  ProtocolSubscriptionHealth,
  ProtocolUpsertProviderRequest,
} from "../services/protocol-client.js";
import "./cli-settings.js";
import {
  boundedSubscriptionUsage,
  subscriptionUsageTone,
} from "./model-health.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

const PROVIDER_REFRESH_MS = 30_000;
const PROVIDER_RETRY_MS = 5_000;

const CUSTOM_PROVIDER = "__custom__";
const DEFAULT_LOGIN_POLL_MS = 1_000;
const DEFAULT_LOGIN_POLL_ATTEMPTS = 180;
const PRESETS_ERROR = "Provider presets unavailable. Retrying.";
const USAGE_ERROR = "Subscription usage unavailable.";
const PROVIDERS_ERROR = "Provider settings unavailable. Retrying.";
export const validatedHttpsUrl = (value: string): string | undefined => {
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      url.host !== "" &&
      url.username === "" &&
      url.password === "" &&
      !/[\u0000-\u001f\u007f]/u.test(value) &&
      url.href.length <= 8_000
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
};

export interface ProviderFormValues {
  readonly id: string;
  readonly kind: string;
  readonly baseUrl: string;
  readonly apiKey: string;
  readonly fields: Readonly<Record<string, string>>;
}

export interface ProviderSubmission {
  readonly id: string;
  readonly request: ProtocolUpsertProviderRequest;
}

export const normalizedProviderOrder = (
  order: readonly string[],
  configuredIds: readonly string[],
): readonly string[] => {
  const configured = new Set(configuredIds);
  return [
    ...order.filter((id, index) => configured.has(id) && order.indexOf(id) === index),
    ...configuredIds.filter((id) => !order.includes(id)),
  ];
};

export const movedProviderOrder = (
  order: readonly string[],
  configuredIds: readonly string[],
  providerId: string,
  direction: -1 | 1,
  movableIds: readonly string[] = configuredIds,
): readonly string[] => {
  const normalized = [...normalizedProviderOrder(order, configuredIds)];
  const movable = new Set(movableIds);
  const slots = normalized
    .map((id, index) => movable.has(id) ? index : -1)
    .filter((index) => index >= 0);
  const slot = slots.indexOf(normalized.indexOf(providerId));
  const targetSlot = slot + direction;
  if (slot < 0 || targetSlot < 0 || targetSlot >= slots.length) return normalized;
  const index = slots[slot];
  const target = slots[targetSlot];
  if (index === undefined || target === undefined) return normalized;
  const current = normalized[index];
  const replacement = normalized[target];
  if (current === undefined || replacement === undefined) return normalized;
  normalized[index] = replacement;
  normalized[target] = current;
  return normalized;
};

/** Local and loopback adapters never participate in hosted automatic routes. */
export const automaticRoutingProviders = (
  providers: readonly ProtocolProviderInfo[],
): readonly ProtocolProviderInfo[] =>
  providers.filter((provider) => provider.category !== "local");

/** Build the write request without ever putting write-only values in element
 * properties or rendered attributes. Callers should discard the result as
 * soon as the request settles. */
export const providerSubmission = (
  preset: ProtocolKnownProvider | undefined,
  values: ProviderFormValues,
): ProviderSubmission => {
  const id = preset?.id ?? values.id.trim();
  const kind = preset?.kind ?? values.kind.trim();
  const baseUrl = (preset?.base_url ?? values.baseUrl).trim();
  const settings: Record<string, string> = {};
  const secretValues: Record<string, string> = {};

  for (const field of preset?.config_fields ?? []) {
    const value = values.fields[field.id] ?? "";
    if (field.secret === true) {
      if (value !== "") secretValues[field.id] = value;
    } else if (value !== "") {
      settings[field.id] = value.trim();
    }
  }

  return {
    id,
    request: {
      kind,
      ...(baseUrl === "" ? {} : { base_url: baseUrl }),
      ...(values.apiKey === "" ? {} : { api_key: values.apiKey }),
      settings,
      secret_values: secretValues,
      headers: { ...(preset?.headers ?? {}) },
      query_params: { ...(preset?.query_params ?? {}) },
    },
  };
};

export interface LoginPollScheduler {
  readonly set: (callback: () => void, delayMs: number) => unknown;
  readonly clear: (handle: unknown) => void;
}

const loginPollScheduler: LoginPollScheduler = {
  set: (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
  clear: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/** One non-overlapping, bounded login-status loop. Generation checks also
 * suppress a response that arrives after stop()/disconnect. */
export class ProviderLoginPoller {
  #generation = 0;
  #attempts = 0;
  #timer: unknown | undefined;
  #active = false;
  readonly #loadStatus: (providerId: string) => Promise<ProtocolLoginStatus>;
  readonly #scheduler: LoginPollScheduler;
  readonly #intervalMs: number;
  readonly #maxAttempts: number;

  constructor(
    loadStatus: (providerId: string) => Promise<ProtocolLoginStatus>,
    scheduler: LoginPollScheduler = loginPollScheduler,
    intervalMs = DEFAULT_LOGIN_POLL_MS,
    maxAttempts = DEFAULT_LOGIN_POLL_ATTEMPTS,
  ) {
    this.#loadStatus = loadStatus;
    this.#scheduler = scheduler;
    this.#intervalMs = intervalMs;
    this.#maxAttempts = maxAttempts;
  }

  get active(): boolean {
    return this.#active;
  }

  start(
    providerId: string,
    onStatus: (status: ProtocolLoginStatus) => void,
    onExhausted: () => void,
  ): void {
    this.stop();
    this.#active = true;
    this.#attempts = 0;
    const generation = this.#generation;
    this.#schedule(providerId, generation, onStatus, onExhausted);
  }

  stop(): void {
    this.#generation += 1;
    this.#active = false;
    if (this.#timer !== undefined) {
      this.#scheduler.clear(this.#timer);
      this.#timer = undefined;
    }
  }

  #schedule(
    providerId: string,
    generation: number,
    onStatus: (status: ProtocolLoginStatus) => void,
    onExhausted: () => void,
  ): void {
    this.#timer = this.#scheduler.set(() => {
      this.#timer = undefined;
      void this.#poll(providerId, generation, onStatus, onExhausted);
    }, this.#intervalMs);
  }

  async #poll(
    providerId: string,
    generation: number,
    onStatus: (status: ProtocolLoginStatus) => void,
    onExhausted: () => void,
  ): Promise<void> {
    if (!this.#active || generation !== this.#generation) return;
    this.#attempts += 1;
    let status: ProtocolLoginStatus | undefined;
    try {
      status = await this.#loadStatus(providerId);
    } catch {
      // Authentication failures can contain vendor diagnostics. The caller
      // deliberately receives only an exhausted signal, never the raw error.
    }
    if (!this.#active || generation !== this.#generation) return;

    if (status !== undefined) onStatus(status);
    if (status !== undefined && status.status !== "pending") {
      this.#active = false;
      return;
    }
    if (this.#attempts >= this.#maxAttempts) {
      this.#active = false;
      onExhausted();
      return;
    }
    this.#schedule(providerId, generation, onStatus, onExhausted);
  }
}

interface ActiveLogin {
  readonly providerId: string;
  readonly displayName: string;
  readonly authorizationUrl: string;
  readonly userCode: string | undefined;
  readonly phase: "pending" | "success" | "failed";
}

const fieldValues = (
  form: HTMLFormElement,
  preset: ProtocolKnownProvider | undefined,
): Record<string, string> => {
  const result: Record<string, string> = {};
  for (const field of preset?.config_fields ?? []) {
    const control = form.elements.namedItem(`field:${field.id}`);
    result[field.id] = control instanceof HTMLInputElement ? control.value : "";
  }
  return result;
};

const formValue = (form: HTMLFormElement, name: string): string => {
  const control = form.elements.namedItem(name);
  return control instanceof HTMLInputElement || control instanceof HTMLSelectElement
    ? control.value
    : "";
};

const clearWriteOnlyControls = (form: HTMLFormElement): void => {
  for (const input of form.querySelectorAll<HTMLInputElement>('input[type="password"]')) {
    input.value = "";
  }
};

const category = (provider: Pick<ProtocolKnownProvider, "category">): string =>
  provider.category ?? "api";

export class TrouveProviderSettings extends LitElement {
  static override properties = {
    providerCategory: { type: String, attribute: "provider-category" },
    showHeading: { type: Boolean, attribute: "show-heading" },
  };

  static override styles = css`
    :host { display: block; color: var(--trouve-text); }
    * { box-sizing: border-box; }
    .settings-stack { display: grid; gap: 12px; }
    .section-heading { display: flex; align-items: start; gap: 12px; }
    .section-heading > div { flex: 1; min-width: 0; }
    h2, h3, p { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 16px; }
    h3 { color: var(--trouve-text-hi); font-size: 13px; }
    p, small { color: var(--trouve-text-dim); }
    .section-heading p, .settings-card > p { margin-top: 4px; }
    .settings-card {
      padding: 14px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .settings-card.subtle { background: var(--trouve-panel-bg); }
    .provider-description { font-size: 11px; }
    .provider-list-surface {
      height: 150px;
      overflow: auto;
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .provider-list-surface .card-list { gap: 0; margin: 0; }
    .provider-editor { display: grid; gap: 8px; }
    .provider-editor > h3 { color: var(--trouve-text-mid); }
    .card-list { display: grid; gap: 8px; margin-top: 10px; }
    .provider-row, .health-row {
      display: grid;
      grid-template-columns: 110px minmax(0, 1fr) auto auto;
      gap: 8px 12px;
      align-items: center;
      min-height: 34px;
      padding: 3px 6px 3px 10px;
      border: 0;
      border-radius: 0;
      background: transparent;
    }
    .provider-name { min-width: 0; overflow: hidden; color: var(--trouve-text-hi); text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
    .provider-kind { min-width: 0; overflow: hidden; color: var(--trouve-text-dim); text-overflow: ellipsis; white-space: nowrap; font-size: 12px; }
    .provider-auth { color: var(--trouve-text-mid); font-size: 11px; white-space: nowrap; }
    .provider-auth.ready { color: var(--trouve-ok); }
    .provider-auth.warning { color: var(--trouve-warn); }
    .provider-copy { min-width: 0; }
    .provider-copy strong, .provider-copy small { display: block; overflow-wrap: anywhere; }
    .provider-copy small { margin-top: 2px; }
    .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
    form { display: grid; gap: 8px; margin-top: 0; }
    .provider-picker { display: block; }
    .provider-picker > span { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); }
    .provider-save-row { display: flex; align-items: end; justify-content: flex-end; gap: 8px; }
    .provider-save-row input { min-width: 0; flex: 1; }
    .routing-priority { display: grid; gap: 8px; }
    .routing-priority > p { font-size: 11px; }
    .priority-list { display: grid; gap: 4px; }
    .priority-row { display: grid; grid-template-columns: 24px minmax(0, 1fr) auto; align-items: center; gap: 8px; }
    .priority-rank { color: var(--trouve-text-dim); font-size: 11px; text-align: right; }
    .priority-row strong { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .subscription-health { display: grid; gap: 12px; }
    .subscription-health > h3 { font-size: 16px; }
    .subscription-health > p { font-size: 11px; }
    .subscription-health .card-list { gap: 12px; margin: 0; }
    .health-row { display: grid; grid-template-columns: minmax(0, 1fr); gap: 8px; padding: 12px; border-radius: var(--trouve-radius); background: var(--trouve-surface); }
    .health-heading { min-width: 0; display: flex; align-items: center; gap: 8px; }
    .health-heading > strong { min-width: 0; flex: 1; overflow-wrap: anywhere; color: var(--trouve-text-hi); font-size: 13px; }
    .health-meta { display: flex; align-items: center; gap: 8px; }
    .health-plan { color: var(--trouve-accent); }
    .health-note { color: var(--trouve-text-dim); font-size: 11px; overflow-wrap: anywhere; }
    .health-note.warning { color: var(--trouve-warn); }
    .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
    label { display: grid; gap: 4px; min-width: 0; color: var(--trouve-text-hi); font-weight: 600; }
    label small { font-weight: 400; }
    input, select, button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
    }
    input, select { width: 100%; padding: 6px 8px; font-weight: 400; }
    button { padding: 5px 10px; cursor: pointer; }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.danger { border-color: var(--trouve-err); color: var(--trouve-err-soft); }
    button:disabled, input:disabled, select:disabled { cursor: not-allowed; opacity: .56; }
    button:focus-visible, input:focus-visible, select:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    .status-pill {
      display: inline-flex;
      align-items: center;
      min-height: 20px;
      padding: 2px 7px;
      border-radius: 999px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 11px;
    }
    .status-pill.ready { color: var(--trouve-ok); }
    .status-pill.warning { color: var(--trouve-warn); }
    .status-pill.failed { color: var(--trouve-err); }
    .health-windows { display: grid; gap: 8px; }
    .health-window { min-width: 0; display: grid; gap: 3px; }
    .health-window-copy { min-width: 0; display: flex; align-items: center; gap: 8px; color: var(--trouve-text-mid); font-size: 11px; }
    .health-window-copy > span { min-width: 0; flex: 1; overflow-wrap: anywhere; }
    .health-window-copy > small { flex: none; color: var(--trouve-text-mid); }
    .health-window-copy > small.tone-error { color: var(--trouve-err); }
    .health-meter { height: 6px; overflow: hidden; border-radius: 3px; background: var(--trouve-control-bg); }
    .health-meter-fill { display: block; min-width: 0; height: 100%; border-radius: 3px; background: var(--trouve-ok); }
    .health-meter-fill.nonzero { min-width: 6px; }
    .health-meter-fill.tone-warning { background: var(--trouve-warn); }
    .health-meter-fill.tone-error { background: var(--trouve-err); }
    .login-card { border-color: var(--trouve-accent); background: var(--trouve-accent-veil); }
    .login-code { margin-top: 8px; color: var(--trouve-text-hi); }
    .login-code code { user-select: all; font-family: var(--trouve-font-mono); }
    .notice { min-height: 20px; color: var(--trouve-text-dim); }
    .notice.error { color: var(--trouve-err); }
    .empty { padding: 9px 0; color: var(--trouve-text-dim); }
    .experimental { color: var(--trouve-warn); }
    @media (max-width: 640px) {
      .form-grid { grid-template-columns: 1fr; }
      .provider-row, .health-row { grid-template-columns: 1fr; }
      .actions { justify-content: stretch; }
      .actions button { flex: 1 1 auto; min-height: 44px; }
      input, select, form > button { min-height: 44px; }
      .health-heading { align-items: start; flex-direction: column; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  providerCategory: "subscription" | "api" = "subscription";
  showHeading = true;
  #loadedServices: AppServices | undefined;
  #providers: ProtocolProvidersResponse | undefined;
  #knownProviders: readonly ProtocolKnownProvider[] = [];
  #health: readonly ProtocolSubscriptionHealth[] = [];
  #subscriptionId = "";
  #apiPresetId = "";
  #customProviderId = "";
  #customProviderKind = "openai-compat";
  #customProviderBaseUrl = "";
  #busy = "";
  #confirmDelete = "";
  #notice = "";
  #noticeIsError = false;
  #loading = true;
  #loadGeneration = 0;
  #knownProvidersPending: Promise<readonly ProtocolKnownProvider[]> | undefined;
  #healthPending: Promise<readonly ProtocolSubscriptionHealth[]> | undefined;
  #login: ActiveLogin | undefined;
  #loginPoller: ProviderLoginPoller | undefined;
  #refreshTimer: ReturnType<typeof setInterval> | undefined;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    this.#refreshTimer ??= globalThis.setInterval(() => {
      if (
        this.#loading
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#load(false);
    }, PROVIDER_REFRESH_MS);
  }

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#loadedServices = undefined;
    this.#loginPoller?.stop();
    this.#loginPoller = undefined;
    if (this.#refreshTimer !== undefined) {
      globalThis.clearInterval(this.#refreshTimer);
      this.#refreshTimer = undefined;
    }
    this.#clearRetry();
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const services = this.#services.value;
    if (services !== undefined && services !== this.#loadedServices) {
      this.#loadedServices = services;
      this.#knownProvidersPending = undefined;
      this.#healthPending = undefined;
      void this.#load(true);
    }
  }

  override render() {
    const services = this.#services.value;
    if (services === undefined || (this.#loading && this.#providers === undefined)) {
      return html`<div class="settings-card" role="status">Loading providers…</div>`;
    }

    const allConfigured = this.#providers?.providers ?? [];
    const automaticConfigured = automaticRoutingProviders(allConfigured);
    const subscriptionPresets = this.#knownProviders.filter(
      (provider) => category(provider) === "subscription" || provider.auth === "oauth" || provider.auth === "cli",
    );
    const apiPresets = this.#knownProviders.filter(
      (provider) => category(provider) !== "local" && !subscriptionPresets.includes(provider),
    );
    const selectedSubscription = subscriptionPresets.find(
      (provider) => provider.id === this.#subscriptionId,
    );
    const selectedApi = apiPresets.find((provider) => provider.id === this.#apiPresetId);
    const subscriptionCategory = this.providerCategory !== "api";
    const configured = allConfigured.filter((provider) => {
      const providerIsSubscription = provider.category === "subscription"
        || provider.auth === "oauth"
        || provider.auth === "cli";
      return subscriptionCategory
        ? providerIsSubscription
        : provider.category !== "local" && !providerIsSubscription;
    });
    const description = subscriptionCategory
      ? "Membership-backed providers with plan status, allowance windows, and vendor login or keys."
      : "Usage-billed hosted APIs and custom remote endpoints.";

    return html`
      <section class="settings-stack" aria-labelledby="provider-settings-title">
        ${this.showHeading ? html`
          <header class="section-heading">
            <div>
              <h2 id="provider-settings-title">Providers</h2>
            </div>
          </header>
        ` : nothing}

        <p class="provider-description">${description}</p>

        ${automaticConfigured.length < 2
          ? nothing
          : this.#renderRoutingPriority(automaticConfigured)}

        ${this.#notice === "" ? nothing : html`
          <p class=${`notice${this.#noticeIsError ? " error" : ""}`} role=${this.#noticeIsError ? "alert" : "status"} aria-live="polite">
            ${this.#notice}
          </p>
        `}

        <section class="provider-list-surface" aria-label="Configured providers">
          <div class="card-list">
            ${configured.length === 0
              ? html`<div class="empty">No providers are configured yet.</div>`
              : configured.map((provider) => this.#renderProvider(provider))}
          </div>
        </section>

        ${this.#login === undefined ? nothing : this.#renderLogin(this.#login)}

        ${subscriptionCategory
          ? html`<section class="provider-editor" aria-labelledby="subscription-onboarding-title">
          <h3 id="subscription-onboarding-title">Add or edit a provider</h3>
          <form @submit=${(event: SubmitEvent) => void this.#saveSubscription(event, selectedSubscription)}>
            <label class="provider-picker">
              <span>Provider</span>
              <select
                name="preset"
                required
                @change=${(event: Event) => {
                  this.#subscriptionId = (event.currentTarget as HTMLSelectElement).value;
                  this.requestUpdate();
                }}
              >
                <option value="" ?selected=${this.#subscriptionId === ""}>Choose a subscription…</option>
                ${subscriptionPresets.map((provider) => html`
                  <option value=${provider.id} ?selected=${provider.id === this.#subscriptionId}>${provider.display_name}${provider.experimental === true ? " · Experimental" : ""}</option>
                `)}
              </select>
            </label>
            ${this.#renderPresetFields(selectedSubscription, configured)}
            <div class="provider-save-row">
              ${selectedSubscription?.auth === "api-key" ? html`
                <input name="api_key" aria-label="API key" type="password" autocomplete="new-password" spellcheck="false" placeholder="API key (stored in the OS keychain; leave empty to keep)" />
              ` : nothing}
              <button class="primary" type="submit" ?disabled=${selectedSubscription === undefined || this.#busy !== ""}>
                ${this.#busy === "subscription" ? "Saving…" : "Save provider"}
              </button>
            </div>
          </form>
        </section>

        <trouve-cli-settings></trouve-cli-settings>

        <section class="subscription-health" aria-labelledby="subscription-health-title">
          <h3 id="subscription-health-title">Subscription health</h3>
          <p>How much of each subscription's metered allowance is used. Codex and Claude Code report through their CLIs; Kimi Code uses the subscription key saved above. Cursor's usage comes from an undocumented dashboard endpoint, so it may break or be restricted at any time.</p>
          <div class="card-list">
            ${this.#health.length === 0
              ? html`<div class="empty">No subscription providers configured.</div>`
              : this.#health.map((health) => this.#renderHealth(health))}
          </div>
        </section>`
          : html`<section class="provider-editor" aria-labelledby="api-onboarding-title">
          <h3 id="api-onboarding-title">Add or edit a provider</h3>
          <form @submit=${(event: SubmitEvent) => void this.#saveApiProvider(event, selectedApi)}>
            <label class="provider-picker">
              <span>Preset</span>
              <select
                name="preset"
                @change=${(event: Event) => {
                  this.#apiPresetId = (event.currentTarget as HTMLSelectElement).value;
                  this.requestUpdate();
                }}
              >
                <option value="" ?selected=${this.#apiPresetId === ""}>Choose a preset…</option>
                ${apiPresets.map((provider) => html`
                  <option value=${provider.id} ?selected=${provider.id === this.#apiPresetId}>${provider.display_name}${provider.experimental === true ? " · Experimental" : ""}</option>
                `)}
                <option value=${CUSTOM_PROVIDER} ?selected=${this.#apiPresetId === CUSTOM_PROVIDER}>Custom provider…</option>
              </select>
            </label>
            ${this.#apiPresetId === CUSTOM_PROVIDER ? this.#renderCustomFields() : nothing}
            ${this.#renderPresetFields(selectedApi, configured)}
            ${selectedApi?.experimental === true
              ? html`<p class="experimental" role="note">This integration uses an experimental vendor surface and may change without notice.</p>`
              : nothing}
            <div class="provider-save-row">
              <input name="api_key" aria-label="API key" type="password" autocomplete="new-password" spellcheck="false" placeholder="API key (stored in the OS keychain; leave empty to keep)" />
              <button class="primary" type="submit" ?disabled=${(selectedApi === undefined && this.#apiPresetId !== CUSTOM_PROVIDER) || this.#busy !== ""}>
                ${this.#busy === "api" ? "Saving…" : "Save provider"}
              </button>
            </div>
          </form>
        </section>`}
      </section>
    `;
  }

  #renderProvider(provider: ProtocolProviderInfo) {
    const canLogin = provider.auth === "oauth" || provider.auth === "cli";
    const deleting = this.#confirmDelete === provider.id;
    const credentialLabel = provider.auth === "aws"
      ? "AWS credential chain"
      : provider.auth === "gcp"
        ? "Google credential chain"
        : provider.auth === "oauth"
          ? provider.has_credentials ? "logged in" : "not logged in"
          : provider.auth === "cli"
            ? provider.has_credentials ? "CLI ready" : "CLI not set up"
            : provider.has_credentials ? "credentials" : "no credentials";
    const credentialTone = provider.auth === "aws" || provider.auth === "gcp"
      ? ""
      : provider.has_credentials ? "ready" : "warning";
    return html`
      <article class="provider-row">
        <strong class="provider-name">${provider.id}</strong>
        <span class="provider-kind">${provider.kind}${provider.base_url ? ` · ${provider.base_url}` : ""}${provider.experimental === true ? html` · ${fontAwesomeIcon("triangle-exclamation")} experimental` : ""}</span>
        <span class=${`provider-auth ${credentialTone}`}>${credentialTone === "ready" ? fontAwesomeIcon("check") : nothing}${credentialLabel}</span>
        <div class="actions">
          ${canLogin ? html`
            <button type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#beginLogin(provider.id, provider.id)}>
              ${provider.has_credentials ? "Re-login" : "Log in"}
            </button>
          ` : nothing}
          <button type="button" ?disabled=${this.#busy !== ""} @click=${() => this.#editProvider(provider)}>Edit</button>
          ${deleting
            ? html`
                <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#deleteProvider(provider.id)}>
                  Confirm remove
                </button>
                <button type="button" @click=${() => { this.#confirmDelete = ""; this.requestUpdate(); }}>Cancel</button>
              `
            : html`
                <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => { this.#confirmDelete = provider.id; this.requestUpdate(); }}>
                  Remove
                </button>
              `}
        </div>
      </article>
    `;
  }

  #renderRoutingPriority(configured: readonly ProtocolProviderInfo[]) {
    const ids = configured.map((provider) => provider.id);
    const allIds = this.#providers?.providers.map((provider) => provider.id) ?? ids;
    const order = normalizedProviderOrder(
      this.#providers?.provider_order ?? [],
      allIds,
    ).filter((providerId) => ids.includes(providerId));
    return html`
      <section class="settings-card routing-priority" aria-labelledby="routing-priority-title">
        <h3 id="routing-priority-title">Automatic routing priority</h3>
        <p>Automatic selections prefer the first healthy route and stay there until it fails.</p>
        <div class="priority-list">
          ${order.map((providerId, index) => html`
            <div class="priority-row">
              <span class="priority-rank">${index + 1}</span>
              <strong>${providerId}</strong>
              <div class="actions">
                <button
                  type="button"
                  aria-label=${`Move ${providerId} earlier`}
                  title="Prefer earlier"
                  data-provider-id=${providerId}
                  data-direction="-1"
                  aria-disabled=${this.#busy !== "" ? "true" : "false"}
                  ?disabled=${this.#busy !== "" || index === 0}
                  @click=${() => void this.#moveProvider(providerId, -1)}
                >${fontAwesomeIcon("arrow-up")}</button>
                <button
                  type="button"
                  aria-label=${`Move ${providerId} later`}
                  title="Prefer later"
                  data-provider-id=${providerId}
                  data-direction="1"
                  aria-disabled=${this.#busy !== "" ? "true" : "false"}
                  ?disabled=${this.#busy !== "" || index + 1 === order.length}
                  @click=${() => void this.#moveProvider(providerId, 1)}
                >${fontAwesomeIcon("arrow-down")}</button>
              </div>
            </div>
          `)}
        </div>
      </section>
    `;
  }

  #renderPresetFields(
    preset: ProtocolKnownProvider | undefined,
    configured: readonly ProtocolProviderInfo[],
  ) {
    const fields = preset?.config_fields ?? [];
    if (preset === undefined || fields.length === 0) return nothing;
    const current = configured.find((provider) => provider.id === preset.id);
    return html`
      <div class="form-grid">
        ${fields.map((field) => {
          const writeOnly = field.secret === true;
          const currentValue = writeOnly
            ? ""
            : current?.settings?.[field.id] ?? field.default_value ?? "";
          const required = field.required === true && !(writeOnly && current?.has_credentials === true);
          return html`
            <label>
              ${field.label}
              <input
                name=${`field:${field.id}`}
                type=${writeOnly ? "password" : "text"}
                autocomplete=${writeOnly ? "new-password" : "off"}
                spellcheck="false"
                .value=${currentValue}
                ?required=${required}
              />
              ${field.description || field.env
                ? html`<small>${field.description}${field.env ? ` Server fallback: ${field.env}.` : ""}</small>`
                : nothing}
            </label>
          `;
        })}
      </div>
    `;
  }

  #renderCustomFields() {
    return html`
      <div class="form-grid">
        <label>
          Provider id
          <input name="provider_id" required pattern="[A-Za-z0-9][A-Za-z0-9._-]*" autocomplete="off" spellcheck="false" .value=${this.#customProviderId} @input=${(event: Event) => { this.#customProviderId = (event.currentTarget as HTMLInputElement).value; }} />
        </label>
        <label>
          Transport kind
          <select name="provider_kind" required @change=${(event: Event) => { this.#customProviderKind = (event.currentTarget as HTMLSelectElement).value; }}>
            <option value="openai-compat" ?selected=${this.#customProviderKind === "openai-compat"}>OpenAI compatible</option>
            <option value="anthropic" ?selected=${this.#customProviderKind === "anthropic"}>Anthropic</option>
            <option value="azure-openai">Azure OpenAI</option>
            <option value="amazon-bedrock">Amazon Bedrock</option>
            <option value="google-vertex">Google Vertex</option>
            <option value="google-vertex-anthropic">Vertex Anthropic</option>
          </select>
        </label>
      </div>
      <label>
        Base URL
        <input name="base_url" type="url" autocomplete="url" spellcheck="false" placeholder="https://api.example.com/v1" .value=${this.#customProviderBaseUrl} @input=${(event: Event) => { this.#customProviderBaseUrl = (event.currentTarget as HTMLInputElement).value; }} />
      </label>
    `;
  }

  #editProvider(provider: ProtocolProviderInfo): void {
    const preset = this.#knownProviders.find((candidate) => candidate.id === provider.id);
    const providerIsSubscription = provider.category === "subscription"
      || provider.auth === "oauth"
      || provider.auth === "cli";
    if (providerIsSubscription) {
      this.#subscriptionId = preset?.id ?? provider.id;
    } else if (preset !== undefined) {
      this.#apiPresetId = preset.id;
    } else {
      this.#apiPresetId = CUSTOM_PROVIDER;
      this.#customProviderId = provider.id;
      this.#customProviderKind = provider.kind;
      this.#customProviderBaseUrl = provider.base_url ?? "";
    }
    this.requestUpdate();
  }

  #renderLogin(login: ActiveLogin) {
    return html`
      <aside class="settings-card login-card" aria-labelledby="provider-login-title" aria-live="polite">
        <div class="section-heading">
          <div>
            <h3 id="provider-login-title">Sign in to ${login.displayName}</h3>
            <p>${login.phase === "pending"
              ? "Complete authorization in your browser. This screen will stop checking automatically."
              : login.phase === "success"
                ? "Provider credentials are ready."
                : "Sign-in did not complete. You can start again."}</p>
          </div>
          <span class=${`status-pill ${login.phase === "success" ? "ready" : login.phase === "failed" ? "failed" : "warning"}`}>
            ${login.phase}
          </span>
        </div>
        ${login.phase === "pending" ? html`
          <div class="actions">
            <button class="primary" type="button" @click=${() => this.#openAuthorization(login.authorizationUrl)}>
              Open authorization page
            </button>
            <button type="button" @click=${() => this.#cancelLogin()}>Cancel</button>
          </div>
          ${login.userCode === undefined
            ? nothing
            : html`<p class="login-code">Enter code <code>${login.userCode}</code> on the authorization page.</p>`}
          <form @submit=${(event: SubmitEvent) => void this.#completeLogin(event, login.providerId)}>
            <label>
              Callback URL or authorization code
              <input name="callback" type="password" autocomplete="one-time-code" spellcheck="false" required />
              <small>Only use this when the provider asks you to return a callback URL or code.</small>
            </label>
            <button type="submit" ?disabled=${this.#busy !== ""}>Submit callback</button>
          </form>
        ` : nothing}
      </aside>
    `;
  }

  #renderHealth(health: ProtocolSubscriptionHealth) {
    return html`
      <article class="health-row" aria-label=${`${health.provider_id} subscription health`}>
        <div class="health-heading">
          <strong>${health.provider_id}</strong>
          <div class="health-meta">
            ${health.plan === "" ? nothing : html`<small class="health-plan">${health.plan} plan</small>`}
            ${health.credits === "" ? nothing : html`<small>${health.credits}</small>`}
          </div>
        </div>
        ${health.status === "ok" || health.note === ""
          ? nothing
          : html`<p class=${`health-note ${health.status === "unavailable" ? "warning" : ""}`}>${health.note}</p>`}
        ${health.windows.length === 0 ? nothing : html`
          <div class="health-windows">
            ${health.windows.map((window) => {
              const percent = boundedSubscriptionUsage(window.used_percent);
              const tone = subscriptionUsageTone(percent);
              return html`
                <div class="health-window">
                  <div class="health-window-copy">
                    <span>${window.label}</span>
                    <small class=${tone === "error" ? "tone-error" : ""}>${percent}% used${window.resets ? ` · ${window.resets}` : ""}</small>
                  </div>
                  <div
                    class="health-meter"
                    role="progressbar"
                    aria-label=${`${window.label}: ${percent}% used`}
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow=${String(percent)}
                  >
                    <span
                      class=${`health-meter-fill tone-${tone} ${percent > 0 ? "nonzero" : ""}`}
                      style=${`width:${percent}%`}
                    ></span>
                  </div>
                </div>
              `;
            })}
          </div>
        `}
      </article>
    `;
  }

  async #load(forceHealth = true): Promise<boolean> {
    const services = this.#services.value;
    if (services === undefined) return false;
    this.#clearRetry();
    const generation = ++this.#loadGeneration;
    this.#loading = true;
    this.requestUpdate();
    this.#loadKnownProviders(services);
    this.#loadHealth(services, forceHealth);
    const [providers] = await Promise.allSettled([services.protocol.providers()]);
    if (generation !== this.#loadGeneration || !this.isConnected) return false;
    this.#loading = false;
    if (providers.status === "fulfilled") this.#providers = providers.value;
    if (providers.status === "rejected") {
      this.#setNotice(PROVIDERS_ERROR, true);
      this.#scheduleRetry();
    } else if (this.#notice === PROVIDERS_ERROR) {
      this.#setNotice("", false);
    }
    this.requestUpdate();
    // Provider state is authoritative for mutations. Optional resource loads
    // publish independently and never keep a committed change busy.
    const providerStateLoaded = providers.status === "fulfilled";
    if (providerStateLoaded && this.#busy === "provider-order-sync") {
      this.#busy = "";
      this.#setNotice("Automatic routing priority was reloaded from the server.", false);
      this.requestUpdate();
    }
    return providerStateLoaded;
  }

  #loadKnownProviders(services: AppServices): void {
    if (this.#knownProvidersPending !== undefined) return;
    const request = services.protocol.knownProviders(AbortSignal.timeout(PROVIDER_REFRESH_MS));
    this.#knownProvidersPending = request;
    void request.then((knownProviders) => {
      if (services !== this.#services.value || !this.isConnected) return;
      this.#knownProviders = knownProviders;
      const subscriptions = this.#knownProviders.filter(
        (provider) => category(provider) === "subscription" || provider.auth === "oauth" || provider.auth === "cli",
      );
      if (!subscriptions.some((provider) => provider.id === this.#subscriptionId)) {
        this.#subscriptionId = subscriptions[0]?.id ?? "";
      }
      const subscriptionIds = new Set(subscriptions.map((provider) => provider.id));
      const api = this.#knownProviders.filter(
        (provider) => category(provider) !== "local" && !subscriptionIds.has(provider.id),
      );
      if (!api.some((provider) => provider.id === this.#apiPresetId) && this.#apiPresetId !== CUSTOM_PROVIDER) {
        this.#apiPresetId = api[0]?.id ?? CUSTOM_PROVIDER;
      }
      if (this.#notice === PRESETS_ERROR) {
        this.#setNotice("", false);
      }
      this.requestUpdate();
    }, () => {
      if (services !== this.#services.value || !this.isConnected) return;
      if (
        this.#notice === ""
        || this.#notice === PRESETS_ERROR
      ) this.#setNotice(PRESETS_ERROR, true);
      this.#scheduleRetry();
      this.requestUpdate();
    }).finally(() => {
      if (this.#knownProvidersPending === request) this.#knownProvidersPending = undefined;
    });
  }

  #loadHealth(services: AppServices, force: boolean): void {
    if (this.#healthPending !== undefined) return;
    const request = services.subscriptionHealth.refresh(force ? "force" : "if-stale");
    this.#healthPending = request;
    void request.then((health) => {
      if (services !== this.#services.value || !this.isConnected) return;
      this.#health = health;
      if (this.#notice === USAGE_ERROR) {
        this.#setNotice("", false);
      }
      this.requestUpdate();
    }, () => {
      if (services !== this.#services.value || !this.isConnected) return;
      if (
        this.#notice === ""
        || this.#notice === USAGE_ERROR
      ) this.#setNotice(USAGE_ERROR, false);
      this.#scheduleRetry();
      this.requestUpdate();
    }).then(() => {
      if (this.#healthPending !== request) return;
      this.#healthPending = undefined;
    });
  }

  #scheduleRetry(): void {
    if (!this.isConnected || this.#retryTimer !== undefined) return;
    this.#retryTimer = globalThis.setTimeout(() => {
      this.#retryTimer = undefined;
      if (typeof document !== "undefined" && document.visibilityState === "hidden") {
        this.#scheduleRetry();
        return;
      }
      void this.#load(false);
    }, PROVIDER_RETRY_MS);
  }

  #clearRetry(): void {
    if (this.#retryTimer === undefined) return;
    globalThis.clearTimeout(this.#retryTimer);
    this.#retryTimer = undefined;
  }

  async #saveSubscription(
    event: SubmitEvent,
    preset: ProtocolKnownProvider | undefined,
  ): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    if (services === undefined || preset === undefined || this.#busy !== "") return;
    const submission = providerSubmission(preset, {
      id: preset.id,
      kind: preset.kind,
      baseUrl: preset.base_url ?? "",
      apiKey: formValue(form, "api_key"),
      fields: fieldValues(form, preset),
    });
    this.#busy = "subscription";
    this.#setNotice("Saving provider…", false);
    this.requestUpdate();
    try {
      const save = services.protocol.upsertProvider(submission.id, submission.request);
      clearWriteOnlyControls(form);
      await save;
      this.#setNotice(`${preset.display_name} is configured.`, false);
      await this.#load();
    } catch {
      this.#setNotice("Provider could not be saved. Check the server configuration and try again.", true);
    } finally {
      clearWriteOnlyControls(form);
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #saveApiProvider(
    event: SubmitEvent,
    preset: ProtocolKnownProvider | undefined,
  ): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    if (services === undefined || this.#busy !== "") return;
    const submission = providerSubmission(preset, {
      id: formValue(form, "provider_id"),
      kind: formValue(form, "provider_kind"),
      baseUrl: formValue(form, "base_url"),
      apiKey: formValue(form, "api_key"),
      fields: fieldValues(form, preset),
    });
    if (submission.id === "" || submission.request.kind === "") {
      this.#setNotice("Provider id and transport kind are required.", true);
      return;
    }
    this.#busy = "api";
    this.#setNotice("Saving provider…", false);
    this.requestUpdate();
    try {
      const save = services.protocol.upsertProvider(submission.id, submission.request);
      clearWriteOnlyControls(form);
      await save;
      this.#setNotice(`${submission.id} is configured.`, false);
      await this.#load();
    } catch {
      this.#setNotice("Provider could not be saved. Check the server configuration and try again.", true);
    } finally {
      clearWriteOnlyControls(form);
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #deleteProvider(providerId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "" || this.#confirmDelete !== providerId) return;
    this.#busy = `delete:${providerId}`;
    this.requestUpdate();
    try {
      await services.protocol.deleteProvider(providerId);
      this.#confirmDelete = "";
      this.#setNotice(`${providerId} was removed.`, false);
      await this.#load();
    } catch {
      this.#setNotice("Provider could not be removed. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #moveProvider(providerId: string, direction: -1 | 1): Promise<void> {
    const services = this.#services.value;
    const providers = this.#providers;
    if (services === undefined || providers === undefined || this.#busy !== "") return;
    const automaticIds = automaticRoutingProviders(providers.providers)
      .map((provider) => provider.id);
    const configuredIds = providers.providers.map((provider) => provider.id);
    const expectedProviderIds = providers.provider_order ?? configuredIds;
    const providerIds = movedProviderOrder(
      expectedProviderIds,
      configuredIds,
      providerId,
      direction,
      automaticIds,
    );
    this.#busy = "provider-order";
    this.#setNotice("Saving routing priority…", false);
    this.requestUpdate();
    let orderConfirmed = true;
    try {
      await services.protocol.setProviderOrder({
        expected_provider_ids: [...expectedProviderIds],
        provider_ids: [...providerIds],
      });
      this.#providers = { ...providers, provider_order: [...providerIds] };
      this.requestUpdate();
      const loaded = await this.#load(false);
      if (loaded) this.#setNotice("Routing priority updated.", false);
    } catch {
      // The response may have been lost after the server committed the PUT.
      // Re-read the authoritative order while the controls remain disabled so
      // the next move is never based on an optimistic or stale snapshot.
      const loaded = await this.#load(false);
      orderConfirmed = loaded;
      this.#setNotice(
        loaded
          ? "Routing priority could not be saved."
          : "Routing priority could not be saved or confirmed. Retrying automatically.",
        true,
      );
    } finally {
      this.#busy = orderConfirmed ? "" : "provider-order-sync";
      this.requestUpdate();
      await this.updateComplete;
      this.#restoreRoutingFocus(providerId, direction);
    }
  }

  #restoreRoutingFocus(providerId: string, direction: -1 | 1): void {
    const buttons = [...this.renderRoot.querySelectorAll<HTMLButtonElement>(
      ".routing-priority button[data-provider-id]",
    )].filter((button) => button.dataset["providerId"] === providerId);
    const preferred = buttons.find((button) =>
      button.dataset["direction"] === String(direction) && !button.disabled
    );
    (preferred ?? buttons.find((button) => !button.disabled))?.focus();
  }

  async #beginLogin(providerId: string, displayName: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const ownsBusyState = this.#busy === "";
    if (ownsBusyState) {
      this.#busy = "login-start";
      this.requestUpdate();
    }
    this.#loginPoller?.stop();
    try {
      const started = await services.protocol.startProviderLogin(providerId);
      const authorizationUrl = validatedHttpsUrl(started.verification_url);
      if (authorizationUrl === undefined) {
        this.#login = undefined;
        this.#setNotice("The provider returned an unsafe authorization URL, so it was not opened.", true);
        return;
      }
      this.#login = {
        providerId,
        displayName,
        authorizationUrl,
        userCode: started.user_code ?? undefined,
        phase: "pending",
      };
      this.#setNotice("Authorization started.", false);
      this.#openAuthorization(authorizationUrl);
      this.#loginPoller = new ProviderLoginPoller((id) => services.protocol.providerLoginStatus(id));
      this.#loginPoller.start(
        providerId,
        (status) => this.#applyLoginStatus(status),
        () => {
          if (this.#login?.providerId !== providerId) return;
          this.#login = { ...this.#login, phase: "failed" };
          this.#setNotice("Sign-in timed out. Start the authorization flow again.", true);
          this.requestUpdate();
        },
      );
      this.requestUpdate();
    } catch {
      this.#setNotice("Sign-in could not be started. Try again.", true);
    } finally {
      if (ownsBusyState) {
        this.#busy = "";
        this.requestUpdate();
      }
    }
  }

  #openAuthorization(value: string): void {
    const href = validatedHttpsUrl(value);
    if (href === undefined) {
      this.#setNotice("The authorization URL was rejected because it is not HTTPS.", true);
      return;
    }
    this.dispatchEvent(new CustomEvent("trouve-open-external", {
      detail: { href },
      bubbles: true,
      composed: true,
    }));
  }

  async #completeLogin(event: SubmitEvent, providerId: string): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    const control = form.elements.namedItem("callback");
    if (services === undefined || !(control instanceof HTMLInputElement) || control.value === "" || this.#busy !== "") return;
    const callback = control.value;
    control.value = "";
    this.#busy = "login-callback";
    this.requestUpdate();
    try {
      const status = await services.protocol.completeProviderLogin(providerId, callback);
      this.#applyLoginStatus(status);
    } catch {
      this.#setNotice("The authorization response could not be submitted. Try again.", true);
    } finally {
      control.value = "";
      this.#busy = "";
      this.requestUpdate();
    }
  }

  #applyLoginStatus(status: ProtocolLoginStatus): void {
    if (this.#login === undefined) return;
    if (status.status === "pending") {
      this.requestUpdate();
      return;
    }
    const success = status.status === "success";
    this.#login = { ...this.#login, phase: success ? "success" : "failed" };
    this.#setNotice(
      success ? `${this.#login.displayName} is connected.` : "Sign-in failed. Start the authorization flow again.",
      !success,
    );
    if (success) void this.#load();
    this.requestUpdate();
  }

  #cancelLogin(): void {
    this.#loginPoller?.stop();
    this.#loginPoller = undefined;
    this.#login = undefined;
    this.#setNotice("Sign-in was dismissed.", false);
    this.requestUpdate();
  }

  #setNotice(message: string, error: boolean): void {
    this.#notice = message;
    this.#noticeIsError = error;
    this.requestUpdate();
  }
}

if ("customElements" in globalThis && !customElements.get("trouve-provider-settings")) {
  customElements.define("trouve-provider-settings", TrouveProviderSettings);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-provider-settings": TrouveProviderSettings;
  }
}
