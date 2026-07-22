import "./styles.css";

type ReviewMode = "off" | "manual" | "automatic";
type ReviewerPromptMode = "inherit" | "append" | "replace";

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
  has_credentials: boolean;
}

interface Model {
  id: string;
  display_name: string;
}

const root = document.querySelector<HTMLElement>("#app")!;
let token = sessionStorage.getItem("trouve-token") ?? "";
let dashboard: Dashboard | null = null;
let providers: Provider[] = [];
let models: Model[] = [];
let timer: number | undefined;

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
      Authorization: `Bearer ${token}`,
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

function renderLogin(message = ""): void {
  root.innerHTML = `
    <section class="login card">
      <p class="eyebrow">trouve</p>
      <h1>Code review, on your server.</h1>
      <p class="lede">Enter the API token configured for this deployment. It stays in this browser tab.</p>
      <form id="login-form">
        <label>Server API token<input name="token" type="password" autocomplete="current-password" required /></label>
        ${message ? `<p class="error">${escape(message)}</p>` : ""}
        <button>Connect</button>
      </form>
    </section>`;
  document.querySelector<HTMLFormElement>("#login-form")!.onsubmit = (event) => {
    event.preventDefault();
    token = String(new FormData(event.currentTarget as HTMLFormElement).get("token") ?? "");
    sessionStorage.setItem("trouve-token", token);
    void load();
  };
}

function render(): void {
  if (!dashboard) return renderLogin();
  const app = dashboard.app;
  const reviewers = dashboard.reviewers;
  const builtInReviewers = reviewers.filter((reviewer) => reviewer.built_in);
  const customReviewers = reviewers.filter((reviewer) => !reviewer.built_in);
  root.innerHTML = `
    <header>
      <div><p class="eyebrow">trouve</p><h1>Review control room</h1></div>
      <div class="header-actions"><span class="status ${app.last_error ? "bad" : "good"}">${app.last_error ? "Needs attention" : "Online"}</span><button class="ghost" id="disconnect">Disconnect</button></div>
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
      <section class="card">
        <p class="eyebrow">Models</p><h2>Providers</h2>
        <div class="provider-list">${providers.map((provider) => `<span>${escape(provider.id)} <i class="${provider.has_credentials ? "ready" : ""}">${provider.has_credentials ? "ready" : "needs credentials"}</i></span>`).join("") || "<p class=\"muted\">No providers configured.</p>"}</div>
        <form id="provider-form" class="stack compact">
          <div class="split"><label>Provider ID<input name="id" placeholder="openrouter" required /></label><label>Kind<select name="kind"><option value="openai-compat">OpenAI compatible</option><option value="anthropic">Anthropic</option><option value="codex-app-server">Codex CLI</option><option value="cursor-cli">Cursor CLI</option><option value="claude-cli">Claude CLI</option></select></label></div>
          <label>Base URL <small>(optional)</small><input name="base_url" placeholder="https://openrouter.ai/api/v1" /></label>
          <label>API key <small>(optional for CLI providers)</small><input name="api_key" type="password" /></label>
          <button>Add or update provider</button>
        </form>
      </section>
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
    <section class="card wide">
      <div class="section-title"><div><p class="eyebrow">Policy</p><h2>Repositories</h2></div><span class="muted">Manual means GitHub reviewer requests only.</span></div>
      <div class="repo-list">
        ${dashboard.repositories.map((repo) => `
          <form class="repo" data-installation-id="${repo.installation_id}" data-repository="${escape(repo.repository)}">
            <div class="repo-controls">
              <div class="repo-name"><strong>${escape(repo.repository)}</strong>${repo.private ? "<span>private</span>" : ""}</div>
              <select name="mode" aria-label="Review mode"><option value="off" ${repo.mode === "off" ? "selected" : ""}>Off</option><option value="manual" ${repo.mode === "manual" ? "selected" : ""}>Manual</option><option value="automatic" ${repo.mode === "automatic" ? "selected" : ""}>Automatic</option></select>
              <select name="model" aria-label="Default review model">${modelOptions(repo.model)}</select>
              <input name="prompt" value="${escape(repo.prompt)}" placeholder="Extra repository instructions" aria-label="Repository instructions" />
              <button>Save</button>
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
          </form>`).join("") || `<p class="empty">No repositories discovered yet. Install the App, then poll GitHub.</p>`}
      </div>
    </section>
    <section class="card wide">
      <p class="eyebrow">History</p><h2>Review jobs</h2>
      <div class="jobs">
        ${dashboard.jobs.map((job) => `<article>
          <span class="job-status ${escape(job.status)}">${escape(job.status)}</span>
          <div><a href="${escape(job.pull_url)}" target="_blank" rel="noreferrer">${escape(job.repository)} #${job.pull_number}</a><strong>${escape(job.pull_title)}</strong><small>${escape(job.trigger)} · ${escape(job.model ?? "default model")} · ${time(job.created_at)} · ${escape(job.head_sha.slice(0, 8))}</small>${job.error ? `<p class="error">${escape(job.error)}</p>` : ""}</div>
          ${job.review_url ? `<a class="review-link" href="${escape(job.review_url)}" target="_blank" rel="noreferrer">Open review ↗</a>` : ""}
        </article>`).join("") || `<p class="empty">No reviews have run yet.</p>`}
      </div>
    </section>`;
  bind();
}

function bind(): void {
  document.querySelector<HTMLButtonElement>("#disconnect")!.onclick = () => {
    token = "";
    dashboard = null;
    sessionStorage.removeItem("trouve-token");
    if (timer) window.clearInterval(timer);
    renderLogin();
  };
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
  renderLogin(error instanceof Error ? error.message : String(error));
}

async function loadData(renderDashboard = true): Promise<void> {
  [dashboard, providers, models] = await Promise.all([
    api<Dashboard>("/code-review"),
    api<{ providers: Provider[] }>("/providers").then((value) => value.providers),
    api<Model[]>("/models"),
  ]);
  if (renderDashboard) render();
}

async function load(): Promise<void> {
  if (!token) return renderLogin();
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
