import "./styles.css";
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
  error: string;
}

interface Model {
  id: string;
  display_name: string;
}

const root = document.querySelector<HTMLElement>("#app")!;
let dashboard: Dashboard | null = null;
let providers: Provider[] = [];
let knownProviders: KnownProvider[] = [];
let models: Model[] = [];
let timer: number | undefined;
let providerLogin: ProviderLogin | null = null;
let providerLoginAttempt = 0;
let repositoryQuery = "";
let repositoryModeFilter: RepositoryModeFilter = "all";
let repositoryPage = 0;
let repositoryPageSize = 10;
let repositorySearchTimer: number | undefined;
const expandedRepositories = new Set<string>();

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
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

function time(value?: string): string {
  return value ? new Date(value).toLocaleString() : "Never";
}

function modelOptions(selected?: string, inheritedLabel = "Use review/default model"): string {
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

function knownProvider(id: string): KnownProvider | undefined {
  return knownProviders.find((provider) => provider.id === id);
}

function providerDisplayName(id: string): string {
  return knownProvider(id)?.display_name ?? id;
}

function providerCredentialLabel(provider: Provider): string {
  if (provider.auth === "cli") return provider.has_credentials ? "CLI ready" : "sign-in required";
  if (provider.auth === "oauth") return provider.has_credentials ? "signed in" : "sign-in required";
  if (provider.auth === "none") return "ready";
  return provider.has_credentials ? "API key ready" : "API key required";
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
  const instructions = login.state === "pending"
    ? login.verification_url
      ? `<p>Finish authorization in the newly opened tab. If it did not open, use the button below.</p>
         <div class="login-actions"><a class="button-link" href="${escape(login.verification_url)}" target="_blank" rel="noopener noreferrer">Open sign-in page ↗</a></div>`
      : `<p>${escape(login.error || "The CLI started but did not expose a browser URL. Check the server terminal for its login instructions.")}</p>`
    : login.state === "success"
      ? "<p>Credentials are ready. The provider's models are now available to reviewers.</p>"
      : login.state === "failed"
        ? `<p class="error">${escape(login.error || "The vendor CLI did not complete sign-in.")}</p>`
        : "<p>Starting the vendor CLI and waiting for its authorization URL…</p>";
  return `<aside class="provider-login ${login.state}" role="status" aria-live="polite">
    <div><span class="provider-state ${login.state === "success" ? "ready" : ""}">${stateLabel}</span><strong>${escape(login.display_name)}</strong></div>
    ${login.user_code ? `<p>Enter this code when prompted: <code>${escape(login.user_code)}</code></p>` : ""}
    ${instructions}
  </aside>`;
}

function renderProviderSettings(): string {
  const cliProviders = knownProviders.filter((provider) => provider.auth === "cli");
  const apiProviders = knownProviders.filter((provider) => provider.auth !== "cli");
  const selectedCli = providerLogin && cliProviders.some((provider) => provider.id === providerLogin?.provider_id)
    ? providerLogin.provider_id
    : "";
  const loginStarting = providerLogin?.state === "starting" ? "disabled" : "";

  return `<section class="card provider-settings">
    <p class="eyebrow">Models</p><h2>Providers</h2>
    <p class="muted">Connect a subscription through its vendor CLI, or add a usage-billed API provider.</p>
    <div class="provider-list">
      ${providers.map((provider) => {
        const name = providerDisplayName(provider.id);
        const canLogin = provider.auth === "cli" || provider.auth === "oauth";
        return `<article class="provider-item">
          <div class="provider-copy"><strong>${escape(name)}</strong><small>${escape(provider.id)} · ${escape(provider.kind)}</small>${provider.experimental ? "<em>experimental</em>" : ""}</div>
          <div class="provider-actions">
            <span class="provider-state ${provider.has_credentials ? "ready" : "needs"}">${providerCredentialLabel(provider)}</span>
            ${canLogin ? `<button class="ghost provider-login-button" type="button" data-provider-login="${escape(provider.id)}" ${loginStarting}>${provider.has_credentials ? "Sign in again" : "Sign in"}</button>` : ""}
          </div>
        </article>`;
      }).join("") || "<p class=\"muted\">No providers configured yet.</p>"}
    </div>
    ${renderProviderLogin()}
    <div class="provider-setup">
      <form id="cli-provider-form" class="stack compact">
        <div><h3>Subscription CLI</h3><p class="muted form-help">Choose a vendor CLI installed on the server. trouve configures the provider, starts its login flow, and opens the vendor's authorization page here.</p></div>
        <label>CLI provider<select name="provider" required>
          <option value="">Choose a CLI provider…</option>
          ${cliProviders.map((provider) => `<option value="${escape(provider.id)}" ${provider.id === selectedCli ? "selected" : ""}>${escape(provider.display_name)}${provider.experimental ? " · Experimental" : ""}</option>`).join("")}
        </select></label>
        <button ${loginStarting}>Configure and sign in</button>
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
    <div class="section-title"><div><p class="eyebrow">Policy</p><h2>Repositories</h2></div><span class="muted">Manual means GitHub reviewer requests only.</span></div>
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
          <label>Webhook secret <small>(optional; leave empty for polling only)</small><input name="webhook_secret" type="password" /></label>
          <button>${app.configured ? "Replace credentials" : "Connect GitHub App"}</button>
        </form>
      </section>
      ${renderProviderSettings()}
    </div>
    <section class="card wide">
      <div class="section-title"><div><p class="eyebrow">Review passes</p><h2>Reviewers</h2></div><span class="muted">Each selected reviewer examines every changed file batch; a final editor validates and deduplicates their findings.</span></div>
      <div class="reviewer-grid">
        ${builtInReviewers.map((reviewer) => `<article class="reviewer-card"><div><strong>${escape(reviewer.name)}</strong><span>built-in</span></div><p>${escape(reviewer.prompt)}</p><small>Uses the repository/default model</small></article>`).join("")}
      </div>
      <h3>Custom reviewers</h3>
      <div class="custom-reviewers">
        ${customReviewers.map((reviewer) => `<form class="custom-reviewer" data-id="${escape(reviewer.id)}">
          <input name="name" value="${escape(reviewer.name)}" aria-label="Reviewer name" required />
          <select name="model" aria-label="Reviewer model">${modelOptions(reviewer.model)}</select>
          <textarea name="prompt" rows="3" aria-label="Reviewer prompt" required>${escape(reviewer.prompt)}</textarea>
          <div class="reviewer-actions"><button>Save</button><button class="ghost delete-reviewer" type="button">Delete</button></div>
        </form>`).join("") || `<p class="empty">No custom reviewers yet.</p>`}
      </div>
      <form id="reviewer-form" class="stack reviewer-create">
        <div class="split"><label>Name<input name="name" placeholder="Domain invariants" required /></label><label>Model<select name="model">${modelOptions()}</select></label></div>
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
  const popup = openLoginPlaceholder();
  providerLogin = {
    attempt,
    provider_id: providerId,
    display_name: displayName,
    state: "starting",
    verification_url: "",
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
        error: "",
      };
    } else {
      providerLogin = {
        attempt,
        provider_id: providerId,
        display_name: displayName,
        state: "failed",
        verification_url: "",
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
    error: "Sign-in timed out after 10 minutes. Start it again to retry.",
  };
  render();
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
  document.querySelectorAll<HTMLButtonElement>("[data-provider-login]").forEach((button) => {
    button.onclick = () => {
      const providerId = button.dataset.providerLogin;
      if (providerId) void startProviderLogin(providerId);
    };
  });
  document.querySelector<HTMLFormElement>("#cli-provider-form")!.onsubmit = (event) => {
    event.preventDefault();
    const providerId = String(new FormData(event.currentTarget as HTMLFormElement).get("provider") ?? "");
    const preset = knownProvider(providerId);
    if (preset) void startProviderLogin(providerId, preset);
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
  document.querySelector<HTMLFormElement>("#provider-form")!.onsubmit = async (event) => {
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
  document.querySelector<HTMLFormElement>("#reviewer-form")!.onsubmit = async (event) => {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    try {
      await api("/code-review/reviewer", {
        method: "PUT",
        body: JSON.stringify({
          name: data.get("name"),
          model: String(data.get("model") || "") || null,
          prompt: data.get("prompt"),
        }),
      });
      form.reset();
      await loadData();
    } catch (error) {
      alert(String(error));
    }
  };
  document.querySelectorAll<HTMLFormElement>("form.custom-reviewer").forEach((form) => {
    form.onsubmit = async (event) => {
      event.preventDefault();
      const data = new FormData(form);
      try {
        await api("/code-review/reviewer", {
          method: "PUT",
          body: JSON.stringify({
            id: form.dataset.id,
            name: data.get("name"),
            model: String(data.get("model") || "") || null,
            prompt: data.get("prompt"),
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

function hasEditableFocus(): boolean {
  const active = document.activeElement;
  return active instanceof HTMLElement
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
  [dashboard, providers, models, knownProviders] = await Promise.all([
    api<Dashboard>("/code-review"),
    api<{ providers: Provider[] }>("/providers").then((value) => value.providers),
    api<Model[]>("/models"),
    api<KnownProvider[]>("/providers/known"),
  ]);
  if (renderDashboard) render();
}

async function load(): Promise<void> {
  try {
    await loadData();
    if (timer) window.clearInterval(timer);
    timer = window.setInterval(() => {
      void loadData(!hasEditableFocus()).catch(handleLoadError);
    }, 15_000);
  } catch (error) {
    handleLoadError(error);
  }
}

void load();
