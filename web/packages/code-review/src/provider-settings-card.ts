import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";
import { createRef, ref } from "lit/directives/ref.js";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import {
  cliIsInstalled,
  cliProgressLabel,
  cliVersionLabel,
  idleCliInstallStatus,
  type CliInfo,
  type CliInstallStatus,
} from "./cli";
import { errorMessage, Flash, ReviewElement, targetValue } from "./element";
import { defaultThinkingSelection, thinkingOptions } from "./model-settings";
import { AUTOMATIC_RETRY_MS, CLI_IDLE_REFRESH_MS } from "./presentation";
import {
  consumeCursorMigrationFocusRequest,
  cursorSdkPreset,
  providerNeedsCursorSdkMigration,
  providerSetupGroups,
  savedProviderMessage,
} from "./provider-settings";
import { selectValue } from "./select-value";
import { externalLink, panelTitle, statusPill, thinkingSetting } from "./shared-views";
import { html } from "./template";
import type {
  KnownProvider,
  LoginStarted,
  Model,
  Provider,
  ProvidersResponse,
} from "./types";

interface LoginView {
  provider: Provider;
  started?: LoginStarted;
  status: "starting" | "pending" | "success" | "failed";
  error: string;
  codeSubmitted: boolean;
}

/** Models, providers, sign-in flows, and managed CLI runtimes. */
export class ProviderSettings extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    providers: { attribute: false },
    models: { attribute: false },
    onChanged: { attribute: false },
    login: { state: true },
    defaultModel: { state: true },
    defaultThinking: { state: true },
    knownProviders: { state: true },
    clis: { state: true },
    cliStatuses: { state: true },
    cliBusy: { state: true },
    subscriptionId: { state: true },
    subscriptionApiKey: { state: true },
    cursorMigrationFocusRequest: { state: true },
    apiPresetId: { state: true },
    providerId: { state: true },
    providerKind: { state: true },
    providerBaseUrl: { state: true },
    providerApiKey: { state: true },
  };

  api!: ReviewApi;
  providers: ProvidersResponse | null = null;
  models: Model[] = [];
  onChanged: () => void = () => {};

  private login: LoginView | null = null;
  private defaultModel = "";
  private defaultThinking = "";
  private knownProviders: KnownProvider[] = [];
  private clis: CliInfo[] = [];
  private cliStatuses: Record<string, CliInstallStatus> = {};
  private cliBusy = "";
  private subscriptionId = "";
  private subscriptionApiKey = "";
  private readonly subscriptionApiKeyInput = createRef<HTMLInputElement>();
  private cursorMigrationFocusRequest = 0;
  private apiPresetId = "";
  private providerId = "";
  private providerKind = "openai-compat";
  private providerBaseUrl = "";
  private providerApiKey = "";
  private readonly message = new Flash(this);

  private readonly defaultModelEffect = this.effect();
  private readonly defaultThinkingEffect = this.effect();
  private readonly cliRefreshEffect = this.effect();
  private readonly cliPollEffect = this.effect();
  private readonly loginPollEffect = this.effect();
  private readonly focusEffect = this.effect();

  protected override willUpdate(): void {
    if (this.hasUpdated) return;
    this.defaultModel = this.providers?.default_model ?? "";
    this.defaultThinking = this.providers?.default_thinking_level ?? "";
  }

  protected override updated(): void {
    const providers = this.providers;
    this.defaultModelEffect.run([providers?.default_model], () => {
      this.defaultModel = providers?.default_model ?? "";
    });
    this.defaultThinkingEffect.run([providers?.default_thinking_level], () => {
      this.defaultThinking = providers?.default_thinking_level ?? "";
    });
    this.cliRefreshEffect.run([], () => {
      let disposed = false;
      let timer: number | undefined;
      const refresh = async (): Promise<void> => {
        if (document.visibilityState !== "visible") {
          timer = window.setTimeout(() => void refresh(), AUTOMATIC_RETRY_MS);
          return;
        }
        const loaded = await this.loadCliData();
        if (!disposed) {
          timer = window.setTimeout(
            () => void refresh(),
            loaded ? CLI_IDLE_REFRESH_MS : AUTOMATIC_RETRY_MS,
          );
        }
      };
      void refresh();
      return () => {
        disposed = true;
        if (timer !== undefined) window.clearTimeout(timer);
      };
    });
    const cliInstalling = Object.values(this.cliStatuses).some(
      (status) => status.status === "pending",
    );
    this.cliPollEffect.run([cliInstalling], (isCurrent) => {
      if (!cliInstalling) return;
      const timer = window.setInterval(async () => {
        const pendingIds = Object.entries(this.cliStatuses)
          .filter(([, status]) => status.status === "pending")
          .map(([id]) => id);
        const fetched: Record<string, CliInstallStatus> = {};
        for (const id of pendingIds) {
          try {
            fetched[id] = await this.api.getCliInstallStatus(id);
          } catch {
            // A transient status error should not stop the next polling tick.
          }
        }
        if (!isCurrent()) return;
        this.cliStatuses = { ...this.cliStatuses, ...fetched };
        if (
          pendingIds.length > 0 &&
          pendingIds.every((id) => fetched[id] && fetched[id].status !== "pending")
        ) {
          const clis = await this.api.getClis();
          if (!isCurrent()) return;
          this.clis = clis;
          this.onChanged();
        }
      }, 1_000);
      return () => window.clearInterval(timer);
    });
    const login = this.login;
    this.loginPollEffect.run([login?.provider.id, login?.status], (isCurrent) => {
      if (!login || login.status !== "pending") return;
      // A poll result may arrive after the user started another provider's
      // sign-in, submitted an authentication code, or the effect was disposed.
      // Merge only into the login object that is current at that moment (so a
      // submitted code is never forgotten) and only while it is still this
      // provider's pending sign-in.
      const currentPendingLogin = (): LoginView | undefined => {
        const current = this.login;
        return isCurrent()
          && current !== null
          && current.provider.id === login.provider.id
          && current.status === "pending"
          ? current
          : undefined;
      };
      const timer = window.setInterval(async () => {
        try {
          const state = await this.api.loginStatus(login.provider.id);
          const current = currentPendingLogin();
          if (!current) return;
          if (state.status === "success") {
            this.login = { ...current, status: "success", error: "" };
            this.onChanged();
          } else if (state.status === "failed") {
            this.login = { ...current, status: "failed", error: state.error || "Sign-in failed" };
          }
        } catch (cause) {
          const current = currentPendingLogin();
          if (!current) return;
          this.login = { ...current, error: errorMessage(cause) };
        }
      }, 1_000);
      return () => window.clearInterval(timer);
    });
    const { subscriptionProviders } = providerSetupGroups(this.knownProviders);
    const selectedSubscription = subscriptionProviders.find(
      (provider) => provider.id === this.subscriptionId,
    );
    const focusRequest = this.cursorMigrationFocusRequest;
    this.focusEffect.run(
      [focusRequest, selectedSubscription?.auth, selectedSubscription?.kind],
      () => {
        const remainingRequest = consumeCursorMigrationFocusRequest(
          focusRequest,
          selectedSubscription,
          this.subscriptionApiKeyInput.value ?? null,
        );
        if (remainingRequest !== focusRequest) {
          this.cursorMigrationFocusRequest = remainingRequest;
        }
      },
    );
  }

  private async loadCliData(): Promise<boolean> {
    try {
      const [nextKnown, nextClis] = await Promise.all([
        this.api.getKnownProviders(),
        this.api.getClis(),
      ]);
      this.knownProviders = nextKnown;
      this.clis = nextClis;
      const statusEntries = await Promise.all(
        nextClis.map(async (cli): Promise<[string, CliInstallStatus]> => {
          try {
            return [cli.id, await this.api.getCliInstallStatus(cli.id)];
          } catch {
            return [cli.id, idleCliInstallStatus()];
          }
        }),
      );
      this.cliStatuses = Object.fromEntries(statusEntries);
      return true;
    } catch (cause) {
      this.message.flash(`${errorMessage(cause)} Retrying automatically.`);
      return false;
    }
  }

  private async begin(provider: Provider): Promise<void> {
    this.login = { provider, status: "starting", error: "", codeSubmitted: false };
    try {
      const started = await this.api.startLogin(provider);
      this.login = { provider, started, status: "pending", error: "", codeSubmitted: false };
      if (started.verification_url) {
        window.open(started.verification_url, "_blank", "noopener,noreferrer");
      }
    } catch (cause) {
      this.login = {
        provider,
        status: "failed",
        error: errorMessage(cause),
        codeSubmitted: false,
      };
    }
  }

  private async runCliAction(
    id: string,
    action: "install" | "cancel" | "uninstall",
  ): Promise<void> {
    const label = this.clis.find((runtime) => runtime.id === id)?.display_name ?? id;
    this.cliBusy = id;
    try {
      if (action === "install") {
        await this.api.installCli(id);
        this.cliStatuses = {
          ...this.cliStatuses,
          [id]: { status: "pending", received_bytes: 0, total_bytes: 0 },
        };
        this.message.flash(`Installing ${label}…`);
      } else if (action === "cancel") {
        await this.api.cancelCliInstall(id);
        this.message.flash(`Cancelling ${label} install…`);
      } else {
        if (!window.confirm(`Remove trouve's managed ${label}?`)) return;
        await this.api.uninstallCli(id);
        this.message.flash(`Removed managed ${label}`);
        await this.loadCliData();
      }
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    } finally {
      this.cliBusy = "";
    }
  }

  private async saveDefaults(event: Event): Promise<void> {
    event.preventDefault();
    const selectedModel = this.models.find((model) => model.id === this.defaultModel);
    const defaultThinkingOptions = thinkingOptions(selectedModel);
    try {
      await this.api.saveDefaultModel(
        this.defaultModel,
        defaultThinkingOptions.values.length || defaultThinkingOptions.budget
          ? this.defaultThinking
          : undefined,
      );
      this.message.flash("System model defaults saved");
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    }
  }

  private async submitLoginCode(event: Event, login: LoginView): Promise<void> {
    event.preventDefault();
    const data = new FormData(event.currentTarget as HTMLFormElement);
    const code = String(data.get("authentication_code") ?? "").trim();
    if (!code) return;
    try {
      await this.api.submitLoginCode(login.provider.id, code);
      this.login = { ...login, codeSubmitted: true, error: "" };
    } catch (cause) {
      this.login = { ...login, error: errorMessage(cause) };
    }
  }

  private async submitSubscription(
    event: Event,
    selectedSubscription: KnownProvider | undefined,
    requiredRuntime: CliInfo | undefined,
  ): Promise<void> {
    event.preventDefault();
    if (!selectedSubscription) return;
    if (requiredRuntime && !cliIsInstalled(requiredRuntime)) {
      await this.runCliAction(requiredRuntime.id, "install");
      return;
    }
    try {
      const configured = await this.api.saveProvider(
        selectedSubscription.id,
        selectedSubscription.kind,
        selectedSubscription.base_url,
        selectedSubscription.auth === "api-key"
          ? this.subscriptionApiKey || undefined
          : undefined,
      );
      this.onChanged();
      if (selectedSubscription.auth === "api-key") {
        this.subscriptionApiKey = "";
        this.message.flash(savedProviderMessage(selectedSubscription.display_name, configured));
      } else {
        await this.begin(configured);
      }
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    }
  }

  private async submitApiProvider(event: Event): Promise<void> {
    event.preventDefault();
    try {
      await this.api.saveProvider(
        this.providerId,
        this.providerKind,
        this.providerBaseUrl || undefined,
        this.providerApiKey || undefined,
      );
      this.message.flash(`Saved ${this.providerId}`);
      this.providerApiKey = "";
      this.onChanged();
    } catch (cause) {
      this.message.flash(errorMessage(cause));
    }
  }

  protected override render(): TemplateResult {
    const providers = this.providers;
    const models = this.models;
    const login = this.login;
    const clis = this.clis;
    const cliStatuses = this.cliStatuses;
    const cliBusy = this.cliBusy;
    const message = this.message.message;
    const { subscriptionProviders, apiProviders } = providerSetupGroups(this.knownProviders);
    const selectedSubscription = subscriptionProviders.find(
      (provider) => provider.id === this.subscriptionId,
    );
    const cursorMigration = cursorSdkPreset(subscriptionProviders);
    const requiredRuntime = selectedSubscription
      ? clis.find((cli) => cli.kinds.includes(selectedSubscription.kind))
      : undefined;
    const selectedModel = models.find((model) => model.id === this.defaultModel);
    const defaultThinkingOptions = thinkingOptions(selectedModel);
    const apiPreset = apiProviders.find((provider) => provider.id === this.apiPresetId);
    return html`<section class="panel settings-card">
      ${panelTitle(
        "Models and providers",
        "Reviewer timings are recorded against the actual provider-qualified model.",
      )}
      <form @submit=${(event: Event) => void this.saveDefaults(event)}>
        <label>
          Global default model
          <select
            @change=${(event: Event) => {
              const next = targetValue(event);
              this.defaultModel = next;
              this.defaultThinking = defaultThinkingSelection(
                models.find((model) => model.id === next),
                this.defaultThinking,
              );
            }}
            required
          >
            ${models.map(
              (model) => html`<option value=${model.id}>${model.display_name} · ${model.id}</option>`,
            )}
            ${selectValue(this.defaultModel)}
          </select>
          <small>Base model for interactive threads and settings that inherit the global default. Enabled repositories still require an explicit coordinator model.</small>
        </label>
        <label>
          ${defaultThinkingOptions.budget
            ? "Global thinking budget (tokens)"
            : "Global thinking level"}
          ${thinkingSetting({
            options: defaultThinkingOptions,
            value: this.defaultThinking,
            onChange: (value) => {
              this.defaultThinking = value;
            },
            inheritLabel: "Use the model's default",
          })}
          <small>Base reasoning setting used when a persona or repository does not specify its own thinking level.</small>
        </label>
        <button type="submit" ?disabled=${!this.defaultModel}>Save system defaults</button>
      </form>
      <div class="provider-list">
        ${repeat(
          providers?.providers ?? [],
          (provider) => provider.id,
          (provider) => html`<article>
            <span>
              <strong>${provider.id}</strong>
              <small>${provider.kind} · ${provider.category}</small>
            </span>
            ${statusPill(provider.has_credentials ? "ready" : "credentials required")}
            ${providerNeedsCursorSdkMigration(provider)
              ? cursorMigration
                ? html`<button
                    class="ghost compact"
                    type="button"
                    @click=${() => {
                      this.cursorMigrationFocusRequest += 1;
                      this.subscriptionId = cursorMigration.id;
                      this.subscriptionApiKey = "";
                      this.login = null;
                      this.message.flash(
                        "Cursor Agent SDK selected; save an API key below to finish migration",
                      );
                    }}
                  >
                    Migrate to Agent SDK
                  </button>`
                : html`<small>Cursor Agent SDK setup is unavailable</small>`
              : provider.auth === "oauth" || provider.auth === "cli"
                ? html`<button class="ghost compact" type="button" @click=${() => void this.begin(provider)}>
                    ${provider.has_credentials ? "Sign in again" : "Sign in"}
                  </button>`
                : nothing}
          </article>`,
        )}
      </div>
      ${login
        ? html`<aside class="login-card ${login.status}" aria-live="polite">
            <header><strong>${login.provider.id}</strong>${statusPill(login.status)}</header>
            ${login.started?.verification_url
              ? externalLink(login.started.verification_url, "Open authorization page")
              : nothing}
            ${login.started?.user_code
              ? html`<p>Enter code <code>${login.started.user_code}</code> in the authorization page.</p>`
              : nothing}
            ${login.provider.kind === "claude-cli" && login.status === "pending" && !login.codeSubmitted
              ? html`<form @submit=${(event: Event) => void this.submitLoginCode(event, login)}>
                  <p>After authorizing, copy the authentication code shown by Claude and paste the code itself here.</p>
                  <label>
                    Claude authentication code
                    <input
                      name="authentication_code"
                      autocomplete="off"
                      spellcheck="false"
                      placeholder="Authentication code (not a URL)"
                      required
                    />
                  </label>
                  <button type="submit">Submit code</button>
                </form>`
              : nothing}
            ${login.codeSubmitted && login.status === "pending"
              ? html`<p>Authentication code sent. Waiting for Claude Code…</p>`
              : nothing}
            ${login.status === "success" ? html`<p>Provider credentials are ready.</p>` : nothing}
            ${login.error ? html`<p class="error-text">${login.error}</p>` : nothing}
          </aside>`
        : nothing}
      <section class="provider-setup">
        <form
          @submit=${(event: Event) =>
            void this.submitSubscription(event, selectedSubscription, requiredRuntime)}
        >
          <h3>Subscription provider</h3>
          <p class="muted">Configure a membership-backed provider with its supported sign-in or API-key flow.</p>
          <label>
            Provider
            <select
              @change=${(event: Event) => {
                this.subscriptionId = targetValue(event);
                this.subscriptionApiKey = "";
              }}
              required
            >
              <option value="">Choose a provider…</option>
              ${subscriptionProviders.map(
                (provider) => html`<option value=${provider.id}>${provider.display_name}${provider.experimental ? " · Experimental" : ""}</option>`,
              )}
              ${selectValue(this.subscriptionId)}
            </select>
          </label>
          ${selectedSubscription?.auth === "api-key"
            ? html`<label>
                API key
                <input
                  ${ref(this.subscriptionApiKeyInput)}
                  type="password"
                  autocomplete="new-password"
                  aria-describedby="subscription-api-key-guidance"
                  .value=${live(this.subscriptionApiKey)}
                  @input=${(event: Event) => {
                    this.subscriptionApiKey = targetValue(event);
                  }}
                  placeholder="Stored in trouve's secret store; leave empty to keep"
                />
                <small id="subscription-api-key-guidance">${selectedSubscription.api_key_env
                    ? `Or set ${selectedSubscription.api_key_env} on the server.`
                    : "A supported vendor API key is required for this subscription."}</small>
              </label>`
            : nothing}
          <button type="submit" ?disabled=${!selectedSubscription || cliBusy !== ""}>
            ${requiredRuntime && !cliIsInstalled(requiredRuntime)
              ? `Install ${requiredRuntime.display_name}`
              : selectedSubscription?.auth === "api-key"
                ? "Save provider"
                : "Configure and sign in"}
          </button>
        </form>
        <form @submit=${(event: Event) => void this.submitApiProvider(event)}>
          <h3>API or custom provider</h3>
          <p class="muted">Use a preset or configure another compatible API endpoint.</p>
          <label>
            Preset
            <select
              @change=${(event: Event) => {
                const id = targetValue(event);
                const preset = apiProviders.find((provider) => provider.id === id);
                this.apiPresetId = id;
                this.providerId = preset?.id ?? "";
                this.providerKind = preset?.kind ?? "openai-compat";
                this.providerBaseUrl = preset?.base_url ?? "";
              }}
            >
              <option value="">Custom provider</option>
              ${apiProviders.map(
                (provider) => html`<option value=${provider.id}>${provider.display_name}</option>`,
              )}
              ${selectValue(this.apiPresetId)}
            </select>
          </label>
          <div class="split-fields">
            <label>
              Provider ID
              <input
                .value=${live(this.providerId)}
                @input=${(event: Event) => {
                  this.providerId = targetValue(event);
                }}
                required
              />
            </label>
            <label>
              Protocol
              <select
                @change=${(event: Event) => {
                  this.providerKind = targetValue(event);
                }}
              >
                <option value="openai-compat">OpenAI compatible</option>
                <option value="anthropic">Anthropic</option>
                ${selectValue(this.providerKind)}
              </select>
            </label>
          </div>
          <label>
            Base URL
            <input
              .value=${live(this.providerBaseUrl)}
              @input=${(event: Event) => {
                this.providerBaseUrl = targetValue(event);
              }}
              placeholder="https://api.example.com/v1"
            />
          </label>
          <label>
            API key
            <input
              type="password"
              autocomplete="new-password"
              aria-describedby="provider-api-key-guidance"
              .value=${live(this.providerApiKey)}
              @input=${(event: Event) => {
                this.providerApiKey = targetValue(event);
              }}
              ?disabled=${apiPreset?.auth === "none"}
            />
            <small id="provider-api-key-guidance">${apiPreset?.api_key_env
                ? `Or set ${apiPreset.api_key_env} on the server.`
                : "Stored in trouve's secret store."}</small>
          </label>
          <button type="submit">Save API provider</button>
        </form>
      </section>
      <section class="cli-manager">
        <header>
          <div>
            <h3>Subscription agent runtimes</h3>
            <p class="muted">Cursor's Agent SDK Bridge and managed vendor CLIs take precedence over system copies on PATH. Status updates automatically.</p>
          </div>
        </header>
        <div class="cli-list">
          ${repeat(
            clis,
            (cli) => cli.id,
            (cli) => {
              const status = cliStatuses[cli.id] ?? idleCliInstallStatus();
              return html`<article>
                <span>
                  <strong>${cli.display_name}</strong>
                  <small>${cliVersionLabel(cli)}</small>
                  ${status.status === "pending" ? html`<small>${cliProgressLabel(status)}</small>` : nothing}
                  ${status.warning ? html`<small role="status">${status.warning}</small>` : nothing}
                  ${status.status === "failed" ? html`<small class="error-text">${status.error}</small>` : nothing}
                </span>
                <div class="action-row">
                  ${status.status === "pending"
                    ? html`<button
                        class="ghost compact"
                        type="button"
                        @click=${() => void this.runCliAction(cli.id, "cancel")}
                      >
                        Cancel
                      </button>`
                    : html`<button
                        class="ghost compact"
                        type="button"
                        ?disabled=${cliBusy === cli.id}
                        @click=${() => void this.runCliAction(cli.id, "install")}
                      >
                        ${cli.update_available ? "Update" : cliIsInstalled(cli) ? "Reinstall" : "Install"}
                      </button>`}
                  ${cli.source === "managed" && status.status !== "pending"
                    ? html`<button
                        class="danger ghost compact"
                        type="button"
                        @click=${() => void this.runCliAction(cli.id, "uninstall")}
                      >
                        Remove
                      </button>`
                    : nothing}
                </div>
              </article>`;
            },
          )}
        </div>
      </section>
      ${message ? html`<p role="status">${message}</p>` : nothing}
    </section>`;
  }
}

customElements.define("trouve-code-review-provider-settings", ProviderSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-provider-settings": ProviderSettings;
  }
}
