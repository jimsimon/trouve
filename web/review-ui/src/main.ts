import "./styles.css";
import {
  type CliInfo,
  type CliInstallStatus,
  cliIsInstalled,
  cliProgressLabel,
  cliVersionLabel,
  idleCliInstallStatus,
} from "./cli";
import {
  defaultThinkingSelection,
  thinkingLevelLabel,
  thinkingOptions,
} from "./model-settings";
import { jobStatusClass, normalizedReviewMode, safeExternalUrl } from "./security";

type ReviewMode = "off" | "manual" | "automatic";
type ReviewerPromptMode = "inherit" | "append" | "replace";
type RepositoryModeFilter = "all" | "enabled" | ReviewMode;

interface GithubAppStatus {
  configured: boolean;
  app_id?: number;
  slug: string;
  bot_login: string;
  webhook_configured: boolean;
  installation_count: number;
  last_poll_at?: string;
  last_error: string;
  rate_limit_remaining?: number;
  rate_limit_reset_at?: string;
}

interface Repository {
  installation_id: number;
  repository: string;
  private: boolean;
  mode: ReviewMode;
  model?: string;
  prompt: string;
  reviewer_ids: string[];
  reviewer_overrides?: ReviewerOverride[];
}

interface ReviewerProfile {
  id: string;
  name: string;
  prompt: string;
  model?: string;
  default_thinking_level?: string;
  built_in: boolean;
}

interface ReviewerOverride {
  reviewer_id: string;
  model?: string;
  prompt_mode: ReviewerPromptMode;
  prompt: string;
}

interface ReviewJob {
  id: string;
  repository: string;
  pull_number: number;
  pull_title: string;
  pull_url: string;
  head_sha: string;
  trigger: string;
  status: string;
  model?: string;
  review_url: string;
  error: string;
  created_at: string;
}

interface Dashboard {
  app: GithubAppStatus;
  reviewers: ReviewerProfile[];
  repositories: Repository[];
  jobs: ReviewJob[];
}

interface Provider {
  id: string;
  kind: string;
  base_url?: string;
  has_credentials: boolean;
  auth: string;
  category: string;
  experimental: boolean;
}

interface ProvidersResponse {
  providers: Provider[];
  default_model: string;
  default_thinking_level?: string;
}

interface KnownProvider {
  id: string;
  display_name: string;
  kind: string;
  base_url?: string;
  api_key_env?: string;
  auth: string;
  category: string;
  experimental: boolean;
}

interface LoginStarted {
  verification_url: string;
  user_code?: string;
}

interface LoginStatus {
  status: "none" | "pending" | "success" | "failed";
  error?: string;
}

interface ProviderLogin {
  attempt: number;
  provider_id: string;
  display_name: string;
  state: "starting" | "pending" | "success" | "failed";
  verification_url: string;
  user_code?: string;
  callback_required: boolean;
  callback_submitted?: boolean;
  callback_error?: string;
  error: string;
}

interface Model {
  id: string;
  display_name: string;
  options_schema?: unknown;
}

interface CliNotice {
  message: string;
  error: boolean;
}

const root = document.querySelector<HTMLElement>("#app")!;
let dashboard: Dashboard | null = null;
let providers: Provider[] = [];
let knownProviders: KnownProvider[] = [];
let models: Model[] = [];
let defaultModel = "";
let defaultThinkingLevel: string | undefined;
let clis: CliInfo[] = [];
let cliInstallStatuses: Record<string, CliInstallStatus> = {};
let clisLoaded = false;
let clisLoading = false;
let cliLoadError = "";
let cliActionId = "";
let cliNotice: CliNotice | null = null;
let selectedCliProviderId = "";
let timer: number | undefined;
let providerLogin: ProviderLogin | null = null;
let providerLoginAttempt = 0;
let repositoryQuery = "";
let repositoryModeFilter: RepositoryModeFilter = "all";
let repositoryPage = 0;
let repositoryPageSize = 10;
let repositorySearchTimer: number | undefined;
const expandedRepositories = new Set<string>();
const cliInstallPolls = new Set<string>();

function escape(value: unknown): string {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`/v1${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init.headers ?? {}),
    },
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({ message: response.statusText }));
    throw new Error(body.message ?? `Request failed (${response.status})`);
  }
  const body = await response.text();
  if (!body) return undefined as T;
  return JSON.parse(body) as T;
}

function time(value?: string): string {
  return value ? new Date(value).toLocaleString() : "Never";
}

function modelOptions(selected?: string, inheritedLabel = "Use review/system default"): string {
  const choices = selected && !models.some((model) => model.id === selected)
    ? [{ id: selected, display_name: selected }, ...models]
    : models;
  return [
    `<option value="" ${selected ? "" : "selected"}>${escape(inheritedLabel)}</option>`,
    ...choices.map(
      (model) => `<option value="${escape(model.id)}" ${model.id === selected ? "selected" : ""}>${escape(model.display_name)} · ${escape(model.id)}</option>`,
    ),
  ].join("");
}

function explicitModelOptions(selected: string): string {
  const choices = selected && !models.some((model) => model.id === selected)
    ? [{ id: selected, display_name: selected, options_schema: undefined }, ...models]
    : models;
  return choices.map(
    (model) => `<option value="${escape(model.id)}" ${model.id === selected ? "selected" : ""}>${escape(model.display_name)} · ${escape(model.id)}</option>`,
  ).join("");
}

function modelById(id: string): Model | undefined {
  return models.find((model) => model.id === id);
}

function thinkingOptionsMarkup(
  modelId: string,
  selected: string | undefined,
  inheritedLabel?: string,
): { markup: string; disabled: boolean } {
  const { values } = thinkingOptions(modelById(modelId));
  const choices = inheritedLabel && selected && !values.includes(selected)
    ? [selected, ...values]
    : values;
  if (inheritedLabel) {
    return {
      markup: [
        `<option value="" ${selected ? "" : "selected"}>${escape(inheritedLabel)}</option>`,
        ...choices.map((value) => `<option value="${escape(value)}" ${value === selected ? "selected" : ""}>${escape(thinkingLevelLabel(value))}</option>`),
      ].join(""),
      disabled: choices.length === 0,
    };
  }
  const resolved = defaultThinkingSelection(modelById(modelId), selected);
  if (choices.length === 0) {
    return {
      markup: `<option value="">Not available for this model</option>`,
      disabled: true,
    };
  }
  return {
    markup: choices.map((value) => `<option value="${escape(value)}" ${value === resolved ? "selected" : ""}>${escape(thinkingLevelLabel(value))}</option>`).join(""),
    disabled: false,
  };
}

function reviewerThinkingOptions(reviewer: ReviewerProfile): { markup: string; disabled: boolean } {
  return thinkingOptionsMarkup(
    reviewer.model ?? defaultModel,
    reviewer.default_thinking_level,
    "Use review/global thinking default",
  );
}

function knownProvider(id: string): KnownProvider | undefined {
  return knownProviders.find((provider) => provider.id === id);
}

function providerDisplayName(id: string): string {
  return knownProvider(id)?.display_name ?? id;
}

function cliForKind(kind: string): CliInfo | undefined {
  return clis.find((cli) => cli.kinds.includes(kind));
}

function cliForProviderId(providerId: string): CliInfo | undefined {
  const provider = providers.find((candidate) => candidate.id === providerId)
    ?? knownProvider(providerId);
  return provider ? cliForKind(provider.kind) : undefined;
}

function cliStatus(cliId: string): CliInstallStatus {
  return cliInstallStatuses[cliId] ?? idleCliInstallStatus();
}

function providerCredentialLabel(provider: Provider): string {
  if (provider.auth === "cli") {
    const requiredCli = cliForKind(provider.kind);
    if (requiredCli && !cliIsInstalled(requiredCli)) return "CLI missing";
    if (!clisLoaded && clisLoading) return "checking CLI";
    return provider.has_credentials ? "CLI ready" : "sign-in required";
  }
  if (provider.auth === "oauth") return provider.has_credentials ? "signed in" : "sign-in required";
  if (provider.auth === "none") return "ready";
  return provider.has_credentials ? "API key ready" : "API key required";
}

function providerIsReady(provider: Provider): boolean {
  const requiredCli = provider.auth === "cli" ? cliForKind(provider.kind) : undefined;
  return provider.has_credentials && (!requiredCli || cliIsInstalled(requiredCli));
}

function renderProviderLogin(): string {
  if (!providerLogin) return "";
  const login = providerLogin;
  const stateLabel = {
    starting: "Starting sign-in",
    pending: "Waiting for authorization",
    success: "Signed in",
    failed: "Sign-in failed",
  }[login.state];
  const callbackFallback = login.state === "pending" && login.callback_required
    ? `<p>If the browser ends on a localhost connection error, copy the full URL from its address bar and paste it here.</p>
       <form class="login-callback" data-provider-login-callback>
         <input name="callback_url" type="text" autocomplete="off" spellcheck="false" placeholder="http://localhost:…?code=…&state=…" aria-label="Claude browser callback URL" required>
         <button type="submit">Complete sign-in</button>
       </form>
       ${login.callback_error ? `<p class="error">${escape(login.callback_error)}</p>` : ""}`
    : login.state === "pending" && login.callback_submitted
      ? `<p>Browser callback sent to Claude Code. Waiting for sign-in to finish…</p>`
      : "";
  const instructions = login.state === "pending"
    ? login.verification_url
      ? `<p>Finish authorization in the newly opened tab. If it did not open, use the button below.</p>
         <div class="login-actions"><a class="button-link" href="${escape(login.verification_url)}" target="_blank" rel="noopener noreferrer">Open sign-in page ↗</a></div>
         ${callbackFallback}`
      : `<p>${escape(login.error || "The CLI started but did not expose a browser URL. Check the server terminal for its login instructions.")}</p>
         ${callbackFallback}`
    : login.state === "success"
      ? "<p>Credentials are ready. The provider's models are now available to reviewers.</p>"
      : login.state === "failed"
        ? `<p class="error">${escape(login.error || "The vendor CLI did not complete sign-in.")}</p>`
        : "<p>Starting the vendor CLI and waiting for its authorization URL…</p>";
  const requiredCli = cliForProviderId(login.provider_id);
  const install = requiredCli ? cliStatus(requiredCli.id) : idleCliInstallStatus();
  const recoveryAction = login.state === "failed" && requiredCli && !cliIsInstalled(requiredCli)
    ? install.status === "pending"
      ? `<div class="login-actions"><button class="ghost" type="button" data-cli-cancel="${escape(requiredCli.id)}">Cancel CLI install</button></div>`
      : `<div class="login-actions"><button type="button" data-cli-install="${escape(requiredCli.id)}" ${cliActionId === requiredCli.id ? "disabled" : ""}>Install ${escape(requiredCli.display_name)}</button></div>`
    : "";
  return `<aside class="provider-login ${login.state}" role="status" aria-live="polite">
    <div><span class="provider-state ${login.state === "success" ? "ready" : ""}">${stateLabel}</span><strong>${escape(login.display_name)}</strong></div>
    ${login.user_code ? `<p>Enter this code when prompted: <code>${escape(login.user_code)}</code></p>` : ""}
    ${instructions}
    ${recoveryAction}
  </aside>`;
}

function renderProviderAction(provider: Provider, loginStarting: boolean): string {
  const canLogin = provider.auth === "cli" || provider.auth === "oauth";
  if (!canLogin) return "";
  if (provider.auth === "cli") {
    const requiredCli = cliForKind(provider.kind);
    if (requiredCli) {
      const install = cliStatus(requiredCli.id);
      if (install.status === "pending") {
        return `<button class="ghost provider-login-button" type="button" data-cli-cancel="${escape(requiredCli.id)}">Cancel install</button>`;
      }
      if (!cliIsInstalled(requiredCli)) {
        return `<button class="ghost provider-login-button" type="button" data-cli-install="${escape(requiredCli.id)}" ${cliActionId === requiredCli.id ? "disabled" : ""}>Install CLI</button>`;
      }
    } else if (!clisLoaded && clisLoading) {
      return `<button class="ghost provider-login-button" type="button" disabled>Checking CLI…</button>`;
    }
  }
  return `<button class="ghost provider-login-button" type="button" data-provider-login="${escape(provider.id)}" ${loginStarting ? "disabled" : ""}>${provider.has_credentials ? "Sign in again" : "Sign in"}</button>`;
}

function renderCliManager(): string {
  const body = !clisLoaded
    ? `<p class="${cliLoadError ? "error" : "muted"}">${escape(cliLoadError || (clisLoading ? "Checking the server's CLI binaries…" : "CLI status has not loaded yet."))}</p>`
    : `<div class="cli-list">${clis.map((cli) => {
      const install = cliStatus(cli.id);
      const pending = install.status === "pending";
      const installed = cliIsInstalled(cli);
      const canInstall = !installed || cli.update_available || install.status === "failed";
      const providerNames = knownProviders
        .filter((provider) => cli.kinds.includes(provider.kind))
        .map((provider) => provider.display_name)
        .join(", ");
      const received = Math.max(0, Number(install.received_bytes) || 0);
      const total = Math.max(0, Number(install.total_bytes) || 0);
      const progress = pending
        ? `<div class="cli-progress"><progress ${total > 0 ? `max="${total}" value="${Math.min(received, total)}"` : ""}></progress><small>${escape(cliProgressLabel(install))}</small></div>`
        : install.status === "failed"
          ? `<p class="cli-error">${escape(install.error || "Install failed.")}</p>`
          : "";
      const actions = pending
        ? `<button class="ghost cli-button" type="button" data-cli-cancel="${escape(cli.id)}">Cancel</button>`
        : `${canInstall ? `<button class="ghost cli-button" type="button" data-cli-install="${escape(cli.id)}" ${cliActionId === cli.id ? "disabled" : ""}>${install.status === "failed" ? "Retry" : installed ? "Update" : "Install"}</button>` : ""}
           ${cli.source === "managed" ? `<button class="ghost cli-button danger" type="button" data-cli-uninstall="${escape(cli.id)}" ${cliActionId === cli.id ? "disabled" : ""}>Remove</button>` : ""}`;
      return `<article class="cli-item">
        <div class="cli-copy">
          <div><strong>${escape(cli.display_name)}</strong><span class="cli-source ${installed ? "ready" : ""}">${installed ? (cli.source === "managed" ? "managed" : "system") : "missing"}</span></div>
          <small>${escape(cliVersionLabel(cli))}</small>
          ${providerNames ? `<small>Used by ${escape(providerNames)}</small>` : ""}
          ${progress}
        </div>
        <div class="cli-actions">${actions}</div>
      </article>`;
    }).join("") || `<p class="muted">This server did not report any manageable CLIs.</p>`}</div>`;
  return `<section class="cli-manager" aria-labelledby="cli-manager-title">
    <div class="cli-manager-title">
      <div><h3 id="cli-manager-title">Subscription CLI binaries</h3><p class="muted form-help">Install vendor CLIs into trouve's data directory. Managed versions take precedence over system copies on PATH.</p></div>
      <button class="ghost cli-refresh" type="button" data-cli-refresh ${clisLoading ? "disabled" : ""}>Refresh</button>
    </div>
    ${cliNotice ? `<p class="cli-notice ${cliNotice.error ? "error" : ""}" role="status">${escape(cliNotice.message)}</p>` : ""}
    ${cliLoadError && clisLoaded ? `<p class="cli-notice error" role="status">${escape(cliLoadError)}</p>` : ""}
    ${body}
  </section>`;
}

function renderProviderSettings(): string {
  const cliProviders = knownProviders.filter((provider) => provider.auth === "cli");
  const apiProviders = knownProviders.filter((provider) => provider.auth !== "cli");
  const loginProviderId = providerLogin?.provider_id ?? "";
  const rememberedCli = selectedCliProviderId
    || (cliProviders.some((provider) => provider.id === loginProviderId)
      ? loginProviderId
      : "");
  const selectedCli = cliProviders.some((provider) => provider.id === rememberedCli)
    ? rememberedCli
    : "";
  const loginStarting = providerLogin?.state === "starting";

  return `<section class="card provider-settings" id="provider-settings">
    <p class="eyebrow">Models</p><h2>Providers</h2>
    <p class="muted">Connect a subscription through its vendor CLI, or add a usage-billed API provider.</p>
    <div class="provider-list">
      ${providers.map((provider) => {
        const name = providerDisplayName(provider.id);
        return `<article class="provider-item">
          <div class="provider-copy"><strong>${escape(name)}</strong><small>${escape(provider.id)} · ${escape(provider.kind)}</small>${provider.experimental ? "<em>experimental</em>" : ""}</div>
          <div class="provider-actions">
            <span class="provider-state ${providerIsReady(provider) ? "ready" : "needs"}">${providerCredentialLabel(provider)}</span>
            ${renderProviderAction(provider, loginStarting)}
          </div>
        </article>`;
      }).join("") || "<p class=\"muted\">No providers configured yet.</p>"}
    </div>
    ${renderProviderLogin()}
    ${renderCliManager()}
    <div class="provider-setup">
      <form id="cli-provider-form" class="stack compact">
        <div><h3>Subscription provider</h3><p class="muted form-help">Choose a provider. If its CLI is missing, trouve installs it first; click again after installation to configure the provider and open the vendor's authorization page.</p></div>
        <label>CLI provider<select name="provider" required>
          <option value="">Choose a CLI provider…</option>
          ${cliProviders.map((provider) => `<option value="${escape(provider.id)}" ${provider.id === selectedCli ? "selected" : ""}>${escape(provider.display_name)}${provider.experimental ? " · Experimental" : ""}</option>`).join("")}
        </select></label>
        <button id="cli-provider-submit" ${loginStarting ? "disabled" : ""}>Configure and sign in</button>
      </form>
      <form id="provider-form" class="stack compact">
        <div><h3>API or custom provider</h3><p class="muted form-help">Pick a preset to fill in its endpoint, or choose Custom for another OpenAI-compatible or Anthropic API.</p></div>
        <label>Preset<select name="preset" id="provider-preset">
          <option value="">Custom provider</option>
          ${apiProviders.map((provider) => `<option value="${escape(provider.id)}">${escape(provider.display_name)}</option>`).join("")}
        </select></label>
        <div class="split"><label>Provider ID<input name="id" placeholder="openrouter" required /></label><label>Protocol<select name="kind"><option value="openai-compat">OpenAI compatible</option><option value="anthropic">Anthropic</option></select></label></div>
        <label>Base URL <small>(optional)</small><input name="base_url" placeholder="https://openrouter.ai/api/v1" /></label>
        <label>API key <small id="api-key-hint">stored in trouve's secret store</small><input name="api_key" type="password" autocomplete="new-password" /></label>
        <button>Save API provider</button>
      </form>
    </div>
  </section>`;
}

function renderGlobalDefaults(): string {
  const thinking = thinkingOptionsMarkup(defaultModel, defaultThinkingLevel);
  return `<section class="card wide model-defaults" id="model-defaults">
    <div class="section-title"><div><p class="eyebrow">Defaults</p><h2>System model defaults</h2></div><span class="muted">Base fallback for reviews that do not select more specific defaults.</span></div>
    ${models.length === 0 ? `<p class="defaults-warning">No provider models are available yet. Configure and sign in to a provider before changing the system default.</p>` : ""}
    <form id="global-defaults-form" class="defaults-form">
      <label>Global default model<select name="model" required ${models.length === 0 ? "disabled" : ""}>${explicitModelOptions(defaultModel)}</select></label>
      <label>Global thinking level<select name="default_thinking_level" ${thinking.disabled ? "disabled" : ""}>${thinking.markup}</select></label>
      <button ${models.length === 0 ? "disabled" : ""}>Save system defaults</button>
    </form>
    <p class="muted defaults-help">Thinking choices come from the selected model. Models without a thinking control use their own behavior.</p>
  </section>`;
}

function renderReviewerDefaultControls(reviewer: ReviewerProfile): string {
  const thinking = reviewerThinkingOptions(reviewer);
  return `<div class="reviewer-default-controls">
    <label>Default model<select name="model" data-reviewer-model-default>${modelOptions(reviewer.model, "Use repository/review default")}</select></label>
    <label>Thinking level<select name="default_thinking_level" data-reviewer-thinking-default ${thinking.disabled ? "disabled" : ""}>${thinking.markup}</select></label>
  </div>`;
}

function repositoryKey(repo: Repository): string {
  return `${repo.installation_id}:${repo.repository}`;
}

function filteredRepositories(): Repository[] {
  if (!dashboard) return [];
  const query = repositoryQuery.trim().toLowerCase();
  return dashboard.repositories.filter((repo) => {
    const mode = normalizedReviewMode(repo.mode);
    const matchesQuery = !query || repo.repository.toLowerCase().includes(query);
    const matchesMode = repositoryModeFilter === "all"
      || (repositoryModeFilter === "enabled" ? mode !== "off" : mode === repositoryModeFilter);
    return matchesQuery && matchesMode;
  });
}

function renderRepositorySection(reviewers: ReviewerProfile[]): string {
  const allRepositories = dashboard?.repositories ?? [];
  const matches = filteredRepositories();
  const pageCount = Math.max(1, Math.ceil(matches.length / repositoryPageSize));
  repositoryPage = Math.min(Math.max(repositoryPage, 0), pageCount - 1);
  const pageStart = repositoryPage * repositoryPageSize;
  const pageRepositories = matches.slice(pageStart, pageStart + repositoryPageSize);
  const pageEnd = pageStart + pageRepositories.length;
  const filtersActive = repositoryQuery.trim() !== "" || repositoryModeFilter !== "all";
  const resultSummary = matches.length === 0
    ? filtersActive
      ? `No matches among ${allRepositories.length} repositories`
      : "No repositories discovered yet"
    : `Showing ${pageStart + 1}–${pageEnd} of ${matches.length}${matches.length === allRepositories.length ? "" : ` (${allRepositories.length} total)`}`;

  return `<section class="card wide" id="repositories-section">
    <div class="section-title"><div><p class="eyebrow">Policy</p><h2>Repositories</h2></div><span class="muted">Comment <code>@trouve-ai review</code> on a PR to request a manual review.</span></div>
    <div class="repository-toolbar">
      <label class="repository-search">Find a repository<input id="repository-search" type="search" value="${escape(repositoryQuery)}" placeholder="Search owner or repository…" autocomplete="off" spellcheck="false" /></label>
      <label>Review mode<select id="repository-mode-filter"><option value="all" ${repositoryModeFilter === "all" ? "selected" : ""}>All modes</option><option value="enabled" ${repositoryModeFilter === "enabled" ? "selected" : ""}>Enabled only</option><option value="automatic" ${repositoryModeFilter === "automatic" ? "selected" : ""}>Automatic</option><option value="manual" ${repositoryModeFilter === "manual" ? "selected" : ""}>Manual</option><option value="off" ${repositoryModeFilter === "off" ? "selected" : ""}>Off</option></select></label>
      <label>Per page<select id="repository-page-size"><option value="10" ${repositoryPageSize === 10 ? "selected" : ""}>10</option><option value="25" ${repositoryPageSize === 25 ? "selected" : ""}>25</option><option value="50" ${repositoryPageSize === 50 ? "selected" : ""}>50</option></select></label>
      ${filtersActive ? "<button class=\"ghost repository-clear\" id=\"repository-filter-clear\" type=\"button\">Clear filters</button>" : ""}
    </div>
    <div class="repository-results"><span>${resultSummary}</span>${pageCount > 1 ? `<nav class="repository-pagination" aria-label="Repository pages"><button class="ghost" id="repository-page-previous" type="button" ${repositoryPage === 0 ? "disabled" : ""}>Previous</button><span>Page ${repositoryPage + 1} of ${pageCount}</span><button class="ghost" id="repository-page-next" type="button" ${repositoryPage + 1 >= pageCount ? "disabled" : ""}>Next</button></nav>` : ""}</div>
    <div class="repo-list">
      ${pageRepositories.map((repo) => {
        const key = repositoryKey(repo);
        const mode = normalizedReviewMode(repo.mode);
        const modeLabel = mode === "automatic" ? "Automatic" : mode === "manual" ? "Manual" : "Off";
        return `<details class="repo-shell" data-repository-key="${escape(key)}" ${expandedRepositories.has(key) ? "open" : ""}>
          <summary><div class="repo-summary-name"><strong>${escape(repo.repository)}</strong><small>Installation ${repo.installation_id}</small></div><div class="repo-summary-status">${repo.private ? "<span class=\"repo-visibility\">private</span>" : ""}<span class="repo-mode ${mode}">${modeLabel}</span><span class="repo-expand-label" aria-hidden="true"></span></div></summary>
          <form class="repo" data-installation-id="${repo.installation_id}" data-repository="${escape(repo.repository)}">
            <div class="repo-controls">
              <label>Review mode<select name="mode"><option value="off" ${mode === "off" ? "selected" : ""}>Off</option><option value="manual" ${mode === "manual" ? "selected" : ""}>Manual</option><option value="automatic" ${mode === "automatic" ? "selected" : ""}>Automatic</option></select></label>
              <label>Default review model<select name="model">${modelOptions(repo.model)}</select></label>
              <label>Repository instructions<input name="prompt" value="${escape(repo.prompt)}" placeholder="Extra repository instructions" /></label>
              <button>Save repository</button>
            </div>
            <fieldset><legend>Reviewers</legend><div class="reviewer-policies">${reviewers.map((reviewer) => {
              const reviewerOverride = repo.reviewer_overrides?.find((item) => item.reviewer_id === reviewer.id);
              const promptMode = reviewerOverride?.prompt_mode ?? "inherit";
              return `<article class="reviewer-policy" data-reviewer-id="${escape(reviewer.id)}">
                <label class="reviewer-toggle" title="${escape(reviewer.prompt)}"><input type="checkbox" name="reviewer_id" value="${escape(reviewer.id)}" ${repo.reviewer_ids.includes(reviewer.id) ? "checked" : ""} /><span><strong>${escape(reviewer.name)}</strong><small>${reviewer.model ? escape(reviewer.model) : "repository/default model"}</small></span></label>
                <div class="reviewer-override-controls">
                  <select data-reviewer-model aria-label="${escape(reviewer.name)} model override">${modelOptions(reviewerOverride?.model, "Inherit reviewer model")}</select>
                  <select data-prompt-mode aria-label="${escape(reviewer.name)} prompt behavior"><option value="inherit" ${promptMode === "inherit" ? "selected" : ""}>Use profile prompt</option><option value="append" ${promptMode === "append" ? "selected" : ""}>Add to profile prompt</option><option value="replace" ${promptMode === "replace" ? "selected" : ""}>Replace profile prompt</option></select>
                  <textarea data-reviewer-prompt rows="2" aria-label="${escape(reviewer.name)} repository prompt" placeholder="Repository-specific instructions for this reviewer">${escape(reviewerOverride?.prompt ?? "")}</textarea>
                </div>
              </article>`;
            }).join("")}</div></fieldset>
          </form>
        </details>`;
      }).join("") || `<p class="empty">${filtersActive ? "No repositories match these filters." : "No repositories discovered yet. Install the App, then poll GitHub."}</p>`}
    </div>
  </section>`;
}

function renderConnectionError(message: string): void {
  root.innerHTML = `
    <section class="connection-error card">
      <p class="eyebrow">trouve</p>
      <h1>Dashboard unavailable</h1>
      <p class="lede">The browser could not connect to this trouve server.</p>
      <p class="error">${escape(message)}</p>
      <button id="retry-load">Retry</button>
    </section>`;
  document.querySelector<HTMLButtonElement>("#retry-load")!.onclick = () => void load();
}

function render(): void {
  if (!dashboard) return;
  const app = dashboard.app;
  const reviewers = dashboard.reviewers;
  const builtInReviewers = reviewers.filter((reviewer) => reviewer.built_in);
  const customReviewers = reviewers.filter((reviewer) => !reviewer.built_in);
  root.innerHTML = `
    <header>
      <div><p class="eyebrow">trouve</p><h1>Review control room</h1></div>
      <div class="header-actions"><span class="status ${app.last_error ? "bad" : "good"}">${app.last_error ? "Needs attention" : "Online"}</span></div>
    </header>
    ${app.last_error ? `<div class="banner error">${escape(app.last_error)}</div>` : ""}
    <section class="metrics">
      <article><span>Bot</span><strong>${escape(app.bot_login || "Not configured")}</strong></article>
      <article><span>Installations</span><strong>${app.installation_count}</strong></article>
      <article><span>GitHub requests left</span><strong>${app.rate_limit_remaining ?? "—"}</strong><small>${app.rate_limit_reset_at ? `resets ${time(app.rate_limit_reset_at)}` : "installation quota"}</small></article>
      <article><span>Last reconciliation</span><strong>${time(app.last_poll_at)}</strong></article>
    </section>
    <div class="grid">
      <section class="card">
        <div class="section-title"><div><p class="eyebrow">Connection</p><h2>GitHub App</h2></div><button class="ghost" id="refresh-github">Poll now</button></div>
        <p class="muted">Credentials are validated against GitHub and stored in trouve's secret store.</p>
        <form id="app-form" class="stack">
          <label>App ID<input name="app_id" inputmode="numeric" value="${app.app_id ?? ""}" required /></label>
          <label>Private key (.pem)<textarea name="private_key_pem" rows="5" placeholder="-----BEGIN RSA PRIVATE KEY-----" required></textarea></label>
          <label>Webhook secret <small>(recommended for immediate comment triggers; polling is the fallback; grant Issues: read and subscribe to issue comments)</small><input name="webhook_secret" type="password" /></label>
          <button>${app.configured ? "Replace credentials" : "Connect GitHub App"}</button>
        </form>
      </section>
      ${renderProviderSettings()}
    </div>
    ${renderGlobalDefaults()}
    <section class="card wide">
      <div class="section-title"><div><p class="eyebrow">Review passes</p><h2>Reviewers</h2></div><span class="muted">Each selected reviewer examines every changed file batch; a final editor validates and deduplicates their findings.</span></div>
      <div class="reviewer-grid">
        ${builtInReviewers.map((reviewer) => `<form class="reviewer-card built-in-reviewer" data-id="${escape(reviewer.id)}">
          <div><strong>${escape(reviewer.name)}</strong><span>built-in</span></div>
          <p>${escape(reviewer.prompt)}</p>
          ${renderReviewerDefaultControls(reviewer)}
          <button>Save persona defaults</button>
        </form>`).join("")}
      </div>
      <h3>Custom reviewers</h3>
      <div class="custom-reviewers">
        ${customReviewers.map((reviewer) => `<form class="custom-reviewer" data-id="${escape(reviewer.id)}">
          <input name="name" value="${escape(reviewer.name)}" aria-label="Reviewer name" required />
          ${renderReviewerDefaultControls(reviewer)}
          <textarea name="prompt" rows="3" aria-label="Reviewer prompt" required>${escape(reviewer.prompt)}</textarea>
          <div class="reviewer-actions"><button>Save</button><button class="ghost delete-reviewer" type="button">Delete</button></div>
        </form>`).join("") || `<p class="empty">No custom reviewers yet.</p>`}
      </div>
      <form id="reviewer-form" class="stack reviewer-create">
        <div class="reviewer-create-fields">
          <label>Name<input name="name" placeholder="Domain invariants" required /></label>
          ${renderReviewerDefaultControls({ id: "", name: "", prompt: "", built_in: false })}
        </div>
        <label>Prompt<textarea name="prompt" rows="3" placeholder="Describe this reviewer's focused mandate." required></textarea></label>
        <button>Add custom reviewer</button>
      </form>
    </section>
    ${renderRepositorySection(reviewers)}
    <section class="card wide">
      <p class="eyebrow">History</p><h2>Review jobs</h2>
      <div class="jobs">
        ${dashboard.jobs.map((job) => {
          const pullUrl = safeExternalUrl(job.pull_url);
          const reviewUrl = safeExternalUrl(job.review_url);
          const pullLabel = `${escape(job.repository)} #${escape(job.pull_number)}`;
          const pullReference = pullUrl
            ? `<a href="${escape(pullUrl)}" target="_blank" rel="noopener noreferrer">${pullLabel}</a>`
            : `<span>${pullLabel}</span>`;
          return `<article>
            <span class="job-status ${jobStatusClass(job.status)}">${escape(job.status)}</span>
            <div>${pullReference}<strong>${escape(job.pull_title)}</strong><small>${escape(job.trigger)} · ${escape(job.model ?? "default model")} · ${time(job.created_at)} · ${escape(job.head_sha.slice(0, 8))}</small>${job.error ? `<p class="error">${escape(job.error)}</p>` : ""}</div>
            ${reviewUrl ? `<a class="review-link" href="${escape(reviewUrl)}" target="_blank" rel="noopener noreferrer">Open review ↗</a>` : ""}
          </article>`;
        }).join("") || `<p class="empty">No reviews have run yet.</p>`}
      </div>
    </section>`;
  bind();
}

function openLoginPlaceholder(): Window | null {
  const popup = window.open("about:blank", "_blank");
  if (!popup) return null;
  popup.opener = null;
  popup.document.title = "Preparing provider sign-in";
  const message = popup.document.createElement("p");
  message.textContent = "Preparing provider sign-in…";
  message.style.font = "16px system-ui, sans-serif";
  message.style.margin = "2rem";
  popup.document.body.append(message);
  return popup;
}

async function startProviderLogin(providerId: string, preset?: KnownProvider): Promise<void> {
  const attempt = ++providerLoginAttempt;
  const displayName = preset?.display_name ?? providerDisplayName(providerId);
  const callbackRequired = (preset?.kind
    ?? providers.find((provider) => provider.id === providerId)?.kind) === "claude-cli";
  const popup = openLoginPlaceholder();
  providerLogin = {
    attempt,
    provider_id: providerId,
    display_name: displayName,
    state: "starting",
    verification_url: "",
    callback_required: callbackRequired,
    error: "",
  };
  render();

  try {
    if (preset) {
      await api(`/providers/${encodeURIComponent(providerId)}`, {
        method: "PUT",
        body: JSON.stringify({
          kind: preset.kind,
          base_url: preset.base_url ?? null,
          api_key: null,
        }),
      });
    }
    const started = await api<LoginStarted>(`/providers/${encodeURIComponent(providerId)}/login`, {
      method: "POST",
      body: "{}",
    });
    if (attempt !== providerLoginAttempt) {
      popup?.close();
      return;
    }
    const verificationUrl = safeExternalUrl(started.verification_url);
    if (started.verification_url && !verificationUrl) {
      throw new Error("The vendor CLI returned an unsupported authorization URL.");
    }
    providerLogin = {
      attempt,
      provider_id: providerId,
      display_name: displayName,
      state: "pending",
      verification_url: verificationUrl,
      user_code: started.user_code,
      callback_required: callbackRequired,
      error: verificationUrl ? "" : "Login is running, but the vendor CLI did not provide a browser URL. Check the server terminal for any remaining instructions.",
    };
    if (verificationUrl && popup && !popup.closed) {
      popup.location.replace(verificationUrl);
    } else {
      popup?.close();
    }
    void pollProviderLogin(providerId, displayName, attempt);
    try {
      await loadData();
    } catch {
      render();
    }
  } catch (error) {
    popup?.close();
    if (attempt !== providerLoginAttempt) return;
    providerLogin = {
      attempt,
      provider_id: providerId,
      display_name: displayName,
      state: "failed",
      verification_url: "",
      callback_required: false,
      error: error instanceof Error ? error.message : String(error),
    };
    try {
      await loadData();
    } catch {
      render();
    }
  }
}

async function pollProviderLogin(providerId: string, displayName: string, attempt: number): Promise<void> {
  let consecutiveErrors = 0;
  let lastError = "";
  for (let poll = 0; poll < 300; poll += 1) {
    await new Promise((resolve) => window.setTimeout(resolve, 2_000));
    if (attempt !== providerLoginAttempt) return;
    let status: LoginStatus;
    try {
      status = await api<LoginStatus>(`/providers/${encodeURIComponent(providerId)}/login`);
      consecutiveErrors = 0;
    } catch (error) {
      consecutiveErrors += 1;
      lastError = error instanceof Error ? error.message : String(error);
      if (consecutiveErrors < 3) continue;
      status = { status: "failed", error: `Could not check sign-in status: ${lastError}` };
    }
    if (status.status === "pending") continue;
    if (status.status === "success") {
      providerLogin = {
        attempt,
        provider_id: providerId,
        display_name: displayName,
        state: "success",
        verification_url: "",
        callback_required: false,
        error: "",
      };
    } else {
      providerLogin = {
        attempt,
        provider_id: providerId,
        display_name: displayName,
        state: "failed",
        verification_url: "",
        callback_required: false,
        error: status.error || (status.status === "none" ? "The server no longer has a sign-in attempt in progress." : "The vendor CLI did not complete sign-in."),
      };
    }
    try {
      await loadData();
    } catch {
      render();
    }
    return;
  }
  if (attempt !== providerLoginAttempt) return;
  providerLogin = {
    attempt,
    provider_id: providerId,
    display_name: displayName,
    state: "failed",
    verification_url: "",
    callback_required: false,
    error: "Sign-in timed out after 10 minutes. Start it again to retry.",
  };
  render();
}

function refreshProviderSettings(): void {
  if (!dashboard) return;
  const section = document.querySelector<HTMLElement>("#provider-settings");
  if (!section) return;
  section.outerHTML = renderProviderSettings();
  bindProviderSettings();
}

function ensureCliInstallPoll(cliId: string): void {
  if (cliInstallPolls.has(cliId)) return;
  cliInstallPolls.add(cliId);
  void pollCliInstall(cliId);
}

async function refreshCliData(updateUi = true): Promise<void> {
  clisLoading = true;
  cliLoadError = "";
  if (updateUi) refreshProviderSettings();
  try {
    const list = await api<{ clis: CliInfo[] }>("/clis");
    const statuses = await Promise.all(list.clis.map(async (cli): Promise<[string, CliInstallStatus]> => {
      try {
        return [cli.id, await api<CliInstallStatus>(`/clis/${encodeURIComponent(cli.id)}/install`)];
      } catch {
        return [cli.id, cliInstallStatuses[cli.id] ?? idleCliInstallStatus()];
      }
    }));
    clis = list.clis;
    cliInstallStatuses = Object.fromEntries(statuses);
    clisLoaded = true;
    for (const [cliId, status] of statuses) {
      if (status.status === "pending") ensureCliInstallPoll(cliId);
    }
  } catch (error) {
    cliLoadError = error instanceof Error ? error.message : String(error);
  } finally {
    clisLoading = false;
    if (updateUi) refreshProviderSettings();
  }
}

async function startCliInstall(cliId: string): Promise<void> {
  if (cliActionId) return;
  cliActionId = cliId;
  cliNotice = { message: `Starting ${cliId} install…`, error: false };
  refreshProviderSettings();
  try {
    await api(`/clis/${encodeURIComponent(cliId)}/install`, { method: "POST" });
    cliInstallStatuses = {
      ...cliInstallStatuses,
      [cliId]: {
        status: "pending",
        received_bytes: 0,
        total_bytes: 0,
      },
    };
    cliNotice = { message: `Installing ${cliId}. You can leave this page open while it downloads.`, error: false };
    ensureCliInstallPoll(cliId);
  } catch (error) {
    cliNotice = {
      message: error instanceof Error ? error.message : String(error),
      error: true,
    };
  } finally {
    cliActionId = "";
    refreshProviderSettings();
  }
}

async function cancelCliInstall(cliId: string): Promise<void> {
  if (cliActionId) return;
  cliActionId = cliId;
  cliNotice = { message: `Cancelling ${cliId} install…`, error: false };
  refreshProviderSettings();
  try {
    await api(`/clis/${encodeURIComponent(cliId)}/install`, { method: "DELETE" });
    ensureCliInstallPoll(cliId);
  } catch (error) {
    cliNotice = {
      message: error instanceof Error ? error.message : String(error),
      error: true,
    };
  } finally {
    cliActionId = "";
    refreshProviderSettings();
  }
}

async function uninstallCli(cliId: string): Promise<void> {
  if (cliActionId) return;
  const cli = clis.find((candidate) => candidate.id === cliId);
  if (!window.confirm(`Remove trouve's managed ${cli?.display_name ?? cliId}? A system copy on PATH will still be used if one exists.`)) return;
  cliActionId = cliId;
  cliNotice = { message: `Removing ${cliId}…`, error: false };
  refreshProviderSettings();
  try {
    await api(`/clis/${encodeURIComponent(cliId)}`, { method: "DELETE" });
    cliInstallStatuses = { ...cliInstallStatuses, [cliId]: idleCliInstallStatus() };
    if (providerLogin && cliForProviderId(providerLogin.provider_id)?.id === cliId) {
      providerLoginAttempt += 1;
      providerLogin = null;
    }
    cliNotice = { message: `Removed the managed ${cli?.display_name ?? cliId}.`, error: false };
    await refreshCliData(false);
    await loadData();
  } catch (error) {
    cliNotice = {
      message: error instanceof Error ? error.message : String(error),
      error: true,
    };
  } finally {
    cliActionId = "";
    refreshProviderSettings();
  }
}

async function pollCliInstall(cliId: string): Promise<void> {
  let consecutiveErrors = 0;
  const refreshProviderSettingsWhenIdle = (): void => {
    if (!hasEditableFocus(document.querySelector("#provider-settings"))) {
      refreshProviderSettings();
    }
  };
  try {
    for (let poll = 0; poll < 1200; poll += 1) {
      await new Promise((resolve) => window.setTimeout(resolve, 1_000));
      let status: CliInstallStatus;
      try {
        status = await api<CliInstallStatus>(`/clis/${encodeURIComponent(cliId)}/install`);
        consecutiveErrors = 0;
      } catch (error) {
        consecutiveErrors += 1;
        if (consecutiveErrors < 3) continue;
        cliNotice = {
          message: `Could not check ${cliId} install status: ${error instanceof Error ? error.message : String(error)}`,
          error: true,
        };
        refreshProviderSettingsWhenIdle();
        return;
      }
      cliInstallStatuses = { ...cliInstallStatuses, [cliId]: status };
      if (status.status === "pending") {
        refreshProviderSettingsWhenIdle();
        continue;
      }

      if (status.status === "success") {
        const version = status.version ? ` ${status.version}` : "";
        clis = clis.map((cli) => cli.id === cliId
          ? {
              ...cli,
              installed_version: status.version ?? cli.latest_version,
              source: "managed",
              update_available: false,
            }
          : cli);
        cliNotice = { message: `Installed ${cliId}${version}. You can sign in now.`, error: false };
        if (providerLogin?.state === "failed"
          && cliForProviderId(providerLogin.provider_id)?.id === cliId
          && /not installed|not on path/i.test(providerLogin.error)) {
          providerLoginAttempt += 1;
          providerLogin = null;
        }
      } else if (status.status === "failed") {
        cliNotice = { message: `Install of ${cliId} failed: ${status.error || "unknown error"}`, error: true };
      } else {
        cliNotice = { message: `Cancelled the ${cliId} install.`, error: false };
      }
      refreshProviderSettingsWhenIdle();
      await refreshCliData(false);
      try {
        await loadData();
      } catch {
        refreshProviderSettingsWhenIdle();
      }
      return;
    }
    cliNotice = { message: `The ${cliId} install is still running. Refresh CLI status to check it again.`, error: true };
    refreshProviderSettingsWhenIdle();
  } finally {
    cliInstallPolls.delete(cliId);
  }
}

function bindProviderSettings(): void {
  document.querySelectorAll<HTMLButtonElement>("[data-provider-login]").forEach((button) => {
    button.onclick = () => {
      const providerId = button.dataset.providerLogin;
      if (providerId) {
        selectedCliProviderId = providerId;
        void startProviderLogin(providerId);
      }
    };
  });
  const callbackForm = document.querySelector<HTMLFormElement>("[data-provider-login-callback]");
  if (callbackForm) {
    callbackForm.onsubmit = async (event) => {
      event.preventDefault();
      const login = providerLogin;
      if (!login || login.state !== "pending") return;
      const input = callbackForm.elements.namedItem("callback_url");
      const callbackUrl = input instanceof HTMLInputElement ? input.value.trim() : "";
      if (!callbackUrl) return;
      const submit = callbackForm.querySelector<HTMLButtonElement>("button[type=submit]");
      if (submit) submit.disabled = true;
      try {
        await api<LoginStatus>(`/providers/${encodeURIComponent(login.provider_id)}/login/callback`, {
          method: "POST",
          body: JSON.stringify({ callback_url: callbackUrl }),
        });
        if (providerLogin?.attempt !== login.attempt) return;
        providerLogin = {
          ...login,
          callback_required: false,
          callback_submitted: true,
          callback_error: "",
        };
      } catch (error) {
        if (providerLogin?.attempt !== login.attempt) return;
        providerLogin = {
          ...login,
          callback_error: error instanceof Error ? error.message : String(error),
        };
      }
      refreshProviderSettings();
    };
  }
  document.querySelectorAll<HTMLButtonElement>("[data-cli-install]").forEach((button) => {
    button.onclick = () => {
      const cliId = button.dataset.cliInstall;
      if (cliId) void startCliInstall(cliId);
    };
  });
  document.querySelectorAll<HTMLButtonElement>("[data-cli-cancel]").forEach((button) => {
    button.onclick = () => {
      const cliId = button.dataset.cliCancel;
      if (cliId) void cancelCliInstall(cliId);
    };
  });
  document.querySelectorAll<HTMLButtonElement>("[data-cli-uninstall]").forEach((button) => {
    button.onclick = () => {
      const cliId = button.dataset.cliUninstall;
      if (cliId) void uninstallCli(cliId);
    };
  });
  const cliRefresh = document.querySelector<HTMLButtonElement>("[data-cli-refresh]");
  if (cliRefresh) {
    cliRefresh.onclick = () => {
      cliNotice = null;
      void refreshCliData();
    };
  }

  const cliProviderForm = document.querySelector<HTMLFormElement>("#cli-provider-form")!;
  const cliProviderSelect = cliProviderForm.elements.namedItem("provider") as HTMLSelectElement;
  const cliProviderSubmit = cliProviderForm.querySelector<HTMLButtonElement>("#cli-provider-submit")!;
  const syncCliProvider = () => {
    const preset = knownProvider(cliProviderSelect.value);
    const requiredCli = preset ? cliForKind(preset.kind) : undefined;
    const install = requiredCli ? cliStatus(requiredCli.id) : idleCliInstallStatus();
    const checking = Boolean(preset && !clisLoaded && clisLoading);
    cliProviderSubmit.disabled = providerLogin?.state === "starting"
      || checking
      || install.status === "pending";
    cliProviderSubmit.textContent = install.status === "pending"
      ? `Installing ${requiredCli?.display_name ?? "CLI"}…`
      : requiredCli && !cliIsInstalled(requiredCli)
        ? `Install ${requiredCli.display_name}`
        : checking
          ? "Checking CLI…"
          : "Configure and sign in";
  };
  cliProviderSelect.onchange = () => {
    selectedCliProviderId = cliProviderSelect.value;
    syncCliProvider();
  };
  syncCliProvider();
  cliProviderForm.onsubmit = (event) => {
    event.preventDefault();
    const providerId = cliProviderSelect.value;
    const preset = knownProvider(providerId);
    if (!preset) return;
    selectedCliProviderId = providerId;
    const requiredCli = cliForKind(preset.kind);
    if (requiredCli && !cliIsInstalled(requiredCli)) {
      void startCliInstall(requiredCli.id);
      return;
    }
    void startProviderLogin(providerId, preset);
  };

  const providerForm = document.querySelector<HTMLFormElement>("#provider-form")!;
  const providerPreset = providerForm.querySelector<HTMLSelectElement>("#provider-preset")!;
  const syncProviderPreset = () => {
    const preset = knownProvider(providerPreset.value);
    const id = providerForm.elements.namedItem("id") as HTMLInputElement;
    const kind = providerForm.elements.namedItem("kind") as HTMLSelectElement;
    const baseUrl = providerForm.elements.namedItem("base_url") as HTMLInputElement;
    const apiKey = providerForm.elements.namedItem("api_key") as HTMLInputElement;
    const apiKeyHint = providerForm.querySelector<HTMLElement>("#api-key-hint")!;
    if (!preset) {
      id.value = "";
      kind.value = "openai-compat";
      baseUrl.value = "";
      apiKey.disabled = false;
      apiKeyHint.textContent = "stored in trouve's secret store";
      return;
    }
    id.value = preset.id;
    kind.value = preset.kind;
    baseUrl.value = preset.base_url ?? "";
    apiKey.disabled = preset.auth === "none";
    apiKeyHint.textContent = preset.auth === "none"
      ? "not required for this provider"
      : preset.api_key_env
        ? `or set ${preset.api_key_env} on the server`
        : "stored in trouve's secret store";
  };
  providerPreset.onchange = syncProviderPreset;
  providerForm.onsubmit = async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const id = encodeURIComponent(String(data.get("id")));
    try {
      await api(`/providers/${id}`, {
        method: "PUT",
        body: JSON.stringify({
          kind: data.get("kind"),
          base_url: String(data.get("base_url") || "") || null,
          api_key: String(data.get("api_key") || "") || null,
        }),
      });
      form.reset();
      await loadData();
    } catch (error) {
      alert(String(error));
    }
  };
}

function refreshRepositorySection(refocusSearch = false): void {
  if (!dashboard) return;
  const section = document.querySelector<HTMLElement>("#repositories-section");
  if (!section) return;
  section.outerHTML = renderRepositorySection(dashboard.reviewers);
  bindRepositorySection();
  if (refocusSearch) {
    const search = document.querySelector<HTMLInputElement>("#repository-search");
    search?.focus();
    search?.setSelectionRange(search.value.length, search.value.length);
  }
}

function clearRepositorySearchTimer(): void {
  if (repositorySearchTimer !== undefined) {
    window.clearTimeout(repositorySearchTimer);
    repositorySearchTimer = undefined;
  }
}

function bindRepositorySection(): void {
  const search = document.querySelector<HTMLInputElement>("#repository-search");
  if (!search) return;
  search.oninput = () => {
    repositoryQuery = search.value;
    repositoryPage = 0;
    clearRepositorySearchTimer();
    repositorySearchTimer = window.setTimeout(() => {
      repositorySearchTimer = undefined;
      refreshRepositorySection(document.activeElement === search);
    }, 150);
  };
  document.querySelector<HTMLSelectElement>("#repository-mode-filter")!.onchange = (event) => {
    clearRepositorySearchTimer();
    repositoryModeFilter = (event.currentTarget as HTMLSelectElement).value as RepositoryModeFilter;
    repositoryPage = 0;
    refreshRepositorySection();
  };
  document.querySelector<HTMLSelectElement>("#repository-page-size")!.onchange = (event) => {
    clearRepositorySearchTimer();
    repositoryPageSize = Number((event.currentTarget as HTMLSelectElement).value);
    repositoryPage = 0;
    refreshRepositorySection();
  };
  const clearFilters = document.querySelector<HTMLButtonElement>("#repository-filter-clear");
  if (clearFilters) {
    clearFilters.onclick = () => {
      clearRepositorySearchTimer();
      repositoryQuery = "";
      repositoryModeFilter = "all";
      repositoryPage = 0;
      refreshRepositorySection(true);
    };
  }
  const previousPage = document.querySelector<HTMLButtonElement>("#repository-page-previous");
  if (previousPage) {
    previousPage.onclick = () => {
      repositoryPage -= 1;
      refreshRepositorySection();
    };
  }
  const nextPage = document.querySelector<HTMLButtonElement>("#repository-page-next");
  if (nextPage) {
    nextPage.onclick = () => {
      repositoryPage += 1;
      refreshRepositorySection();
    };
  }
  document.querySelectorAll<HTMLDetailsElement>(".repo-shell").forEach((details) => {
    details.ontoggle = () => {
      const key = details.dataset.repositoryKey;
      if (!key) return;
      if (details.open) expandedRepositories.add(key);
      else expandedRepositories.delete(key);
    };
  });
  document.querySelectorAll<HTMLFormElement>("form.repo").forEach((form) => {
    form.querySelectorAll<HTMLSelectElement>("[data-prompt-mode]").forEach((select) => {
      const textarea = select.closest<HTMLElement>(".reviewer-policy")!
        .querySelector<HTMLTextAreaElement>("[data-reviewer-prompt]")!;
      const syncPromptMode = () => {
        textarea.disabled = select.value === "inherit";
      };
      select.onchange = syncPromptMode;
      syncPromptMode();
    });
    form.onsubmit = async (event) => {
      event.preventDefault();
      const data = new FormData(form);
      const reviewerOverrides = Array.from(form.querySelectorAll<HTMLElement>(".reviewer-policy")).flatMap((row) => {
        const model = row.querySelector<HTMLSelectElement>("[data-reviewer-model]")!.value;
        const promptMode = row.querySelector<HTMLSelectElement>("[data-prompt-mode]")!.value as ReviewerPromptMode;
        const prompt = row.querySelector<HTMLTextAreaElement>("[data-reviewer-prompt]")!.value;
        if (!model && promptMode === "inherit") return [];
        return [{ reviewer_id: row.dataset.reviewerId ?? "", model: model || null, prompt_mode: promptMode, prompt }];
      });
      try {
        await api("/code-review/repository", {
          method: "PUT",
          body: JSON.stringify({
            installation_id: Number(form.dataset.installationId),
            repository: form.dataset.repository,
            mode: data.get("mode"),
            model: String(data.get("model") || "") || null,
            prompt: data.get("prompt"),
            reviewer_ids: data.getAll("reviewer_id").map(String),
            reviewer_overrides: reviewerOverrides,
          }),
        });
        await loadData();
      } catch (error) {
        alert(String(error));
      }
    };
  });
}

function bindReviewerThinking(form: HTMLFormElement): void {
  const model = form.querySelector<HTMLSelectElement>("[data-reviewer-model-default]");
  const thinking = form.querySelector<HTMLSelectElement>("[data-reviewer-thinking-default]");
  if (!model || !thinking) return;
  model.onchange = () => {
    const options = thinkingOptionsMarkup(
      model.value || defaultModel,
      undefined,
      "Use review/global thinking default",
    );
    thinking.innerHTML = options.markup;
    thinking.disabled = options.disabled;
  };
}

function reviewerDefaultsPayload(form: HTMLFormElement): {
  model: string | null;
  default_thinking_level: string | null;
} {
  const model = form.querySelector<HTMLSelectElement>("[data-reviewer-model-default]")?.value ?? "";
  const thinking = form.querySelector<HTMLSelectElement>("[data-reviewer-thinking-default]")?.value ?? "";
  return {
    model: model || null,
    default_thinking_level: thinking || null,
  };
}

function bind(): void {
  document.querySelector<HTMLButtonElement>("#refresh-github")!.onclick = async (event) => {
    const button = event.currentTarget as HTMLButtonElement;
    button.disabled = true;
    try {
      await api("/code-review/refresh", { method: "POST" });
      await loadData();
    } catch (error) {
      alert(String(error));
    } finally {
      button.disabled = false;
    }
  };
  document.querySelector<HTMLFormElement>("#app-form")!.onsubmit = async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    try {
      await api("/code-review/github-app", {
        method: "PUT",
        body: JSON.stringify({
          app_id: Number(data.get("app_id")),
          private_key_pem: data.get("private_key_pem"),
          webhook_secret: data.get("webhook_secret"),
        }),
      });
      form.reset();
      await loadData();
    } catch (error) {
      alert(String(error));
    }
  };
  bindProviderSettings();
  const globalDefaultsForm = document.querySelector<HTMLFormElement>("#global-defaults-form")!;
  const globalModel = globalDefaultsForm.elements.namedItem("model") as HTMLSelectElement;
  const globalThinking = globalDefaultsForm.elements.namedItem("default_thinking_level") as HTMLSelectElement;
  globalModel.onchange = () => {
    const options = thinkingOptionsMarkup(globalModel.value, undefined);
    globalThinking.innerHTML = options.markup;
    globalThinking.disabled = options.disabled;
  };
  globalDefaultsForm.onsubmit = async (event) => {
    event.preventDefault();
    try {
      await api("/config/default-model", {
        method: "PUT",
        body: JSON.stringify({
          model: globalModel.value,
          default_thinking_level: globalThinking.disabled ? null : globalThinking.value || null,
        }),
      });
      await loadData();
    } catch (error) {
      alert(String(error));
    }
  };

  document.querySelectorAll<HTMLFormElement>("form.built-in-reviewer").forEach((form) => {
    bindReviewerThinking(form);
    form.onsubmit = async (event) => {
      event.preventDefault();
      const reviewer = dashboard?.reviewers.find((candidate) => candidate.id === form.dataset.id);
      if (!reviewer) return;
      try {
        await api("/code-review/reviewer", {
          method: "PUT",
          body: JSON.stringify({
            id: reviewer.id,
            name: reviewer.name,
            prompt: reviewer.prompt,
            ...reviewerDefaultsPayload(form),
          }),
        });
        await loadData();
      } catch (error) {
        alert(String(error));
      }
    };
  });

  const reviewerForm = document.querySelector<HTMLFormElement>("#reviewer-form")!;
  bindReviewerThinking(reviewerForm);
  reviewerForm.onsubmit = async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    try {
      await api("/code-review/reviewer", {
        method: "PUT",
        body: JSON.stringify({
          name: data.get("name"),
          prompt: data.get("prompt"),
          ...reviewerDefaultsPayload(form),
        }),
      });
      form.reset();
      await loadData();
    } catch (error) {
      alert(String(error));
    }
  };
  document.querySelectorAll<HTMLFormElement>("form.custom-reviewer").forEach((form) => {
    bindReviewerThinking(form);
    form.onsubmit = async (event) => {
      event.preventDefault();
      const data = new FormData(form);
      try {
        await api("/code-review/reviewer", {
          method: "PUT",
          body: JSON.stringify({
            id: form.dataset.id,
            name: data.get("name"),
            prompt: data.get("prompt"),
            ...reviewerDefaultsPayload(form),
          }),
        });
        await loadData();
      } catch (error) {
        alert(String(error));
      }
    };
    form.querySelector<HTMLButtonElement>(".delete-reviewer")!.onclick = async () => {
      if (!window.confirm("Delete this custom reviewer? Repositories using it will return to the core reviewer set when necessary.")) return;
      try {
        await api(`/code-review/reviewer/${encodeURIComponent(form.dataset.id ?? "")}`, { method: "DELETE" });
        await loadData();
      } catch (error) {
        alert(String(error));
      }
    };
  });
  bindRepositorySection();
}

function hasEditableFocus(scope?: Element | null): boolean {
  const active = document.activeElement;
  return active instanceof HTMLElement
    && (scope === undefined || scope?.contains(active) === true)
    && active.matches("input, textarea, select, [contenteditable='true']");
}

function handleLoadError(error: unknown): void {
  dashboard = null;
  if (timer) {
    window.clearInterval(timer);
    timer = undefined;
  }
  renderConnectionError(error instanceof Error ? error.message : String(error));
}

async function loadData(renderDashboard = true): Promise<void> {
  const [loadedDashboard, providerResponse, loadedModels, loadedKnownProviders] = await Promise.all([
    api<Dashboard>("/code-review"),
    api<ProvidersResponse>("/providers"),
    api<Model[]>("/models"),
    api<KnownProvider[]>("/providers/known"),
  ]);
  dashboard = loadedDashboard;
  providers = providerResponse.providers;
  defaultModel = providerResponse.default_model;
  defaultThinkingLevel = providerResponse.default_thinking_level;
  models = loadedModels;
  knownProviders = loadedKnownProviders;
  if (renderDashboard) render();
}

async function load(): Promise<void> {
  try {
    await loadData();
    void refreshCliData();
    if (timer) window.clearInterval(timer);
    timer = window.setInterval(() => {
      void loadData(!hasEditableFocus()).catch(handleLoadError);
    }, 15_000);
  } catch (error) {
    handleLoadError(error);
  }
}

void load();
