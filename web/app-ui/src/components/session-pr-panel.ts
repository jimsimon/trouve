import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  appStoreContext,
  sessionContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import {
  ProtocolClientError,
  type ProtocolCreatePrRequest,
  type ProtocolPrInfo,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  canMergePr,
  checkSummary,
  createPrRequest,
  githubIntegrationConfigured,
  mergeabilitySummary,
  mergeMethod,
  reviewSummary,
  safeSessionPrHref,
  type PrSummary,
} from "./session-pr-panel-model.js";

const humanize = (value: string): string => {
  const text = value.trim().replaceAll("_", " ");
  return text === "" ? "Unknown" : `${text[0]?.toUpperCase() ?? ""}${text.slice(1)}`;
};

const formInput = (form: HTMLFormElement, name: string): string => {
  const control = form.elements.namedItem(name);
  return control instanceof HTMLInputElement || control instanceof HTMLTextAreaElement
    ? control.value
    : "";
};

export class TrouveSessionPrPanel extends withSignalTracking(LitElement) {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
    sessionTitle: { type: String, attribute: "session-title" },
  };

  static override styles = css`
    :host { display: block; height: 100%; min-height: 0; color: var(--trouve-text); }
    * { box-sizing: border-box; }
    h2, h3, h4, p { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 15px; }
    h3 { color: var(--trouve-text-hi); font-size: 13px; }
    h4 { color: var(--trouve-text-hi); font-size: 12px; }
    p, small { color: var(--trouve-text-dim); }
    .panel { display: flex; min-height: 100%; height: 100%; flex-direction: column; gap: 6px; padding: 6px; overflow: auto; }
    .panel-header { display: flex; align-items: start; gap: 10px; }
    .panel-header > div { flex: 1; min-width: 0; }
    .panel-header p { margin-top: 3px; }
    .notice { display: flex; align-items: center; gap: 8px; min-height: 18px; color: var(--trouve-text-dim); font-size: 11px; }
    .notice > span { flex: 1; min-width: 0; }
    .notice button { min-height: 28px; }
    .notice.error { color: var(--trouve-err); }
    .settings-card, .pr-card {
      padding: 11px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .pr-list { display: grid; gap: 8px; }
    .pr-card { display: grid; gap: 9px; }
    .pr-heading { display: flex; align-items: start; gap: 9px; }
    .pr-heading > div { flex: 1; min-width: 0; }
    .pr-heading h3, .pr-meta { overflow-wrap: anywhere; }
    .pr-meta { display: flex; flex-wrap: wrap; gap: 3px 10px; margin-top: 3px; }
    .summary-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1px; background: var(--trouve-rule); }
    .summary-item { min-width: 0; padding: 7px 8px; background: var(--trouve-inset-bg); }
    .summary-item > span { display: block; margin-bottom: 2px; color: var(--trouve-text-dim); font-size: 10px; text-transform: uppercase; letter-spacing: .04em; }
    .summary-item strong { overflow-wrap: anywhere; color: var(--trouve-text); font-size: 11px; }
    .summary-item strong.ready { color: var(--trouve-ok); }
    .summary-item strong.pending { color: var(--trouve-warn); }
    .summary-item strong.warning { color: var(--trouve-warn); }
    .summary-item strong.failed { color: var(--trouve-err); }
    .summary-item strong.muted { color: var(--trouve-text-dim); }
    .status-pill {
      display: inline-flex;
      align-items: center;
      min-height: 22px;
      padding: 2px 7px;
      border-radius: 999px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 10px;
      text-transform: capitalize;
    }
    .status-pill.open { color: var(--trouve-ok); }
    .status-pill.merged { color: var(--trouve-merged); }
    .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
    button, input, textarea, select {
      min-height: 34px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
    }
    button { padding: 4px 9px; cursor: pointer; }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.danger { border-color: var(--trouve-err); color: var(--trouve-err-soft); }
    button:disabled, input:disabled, textarea:disabled, select:disabled { cursor: not-allowed; opacity: .56; }
    button:focus-visible, input:focus-visible, textarea:focus-visible, select:focus-visible, summary:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    form { display: grid; gap: 9px; margin-top: 9px; }
    .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 9px; }
    label { display: grid; gap: 4px; min-width: 0; color: var(--trouve-text-hi); font-weight: 600; }
    input, textarea, select { width: 100%; padding: 5px 7px; font-weight: 400; }
    textarea { min-height: 82px; resize: vertical; }
    .checkbox { display: flex; align-items: center; gap: 7px; }
    .checkbox input { width: 17px; min-height: 17px; accent-color: var(--trouve-accent); }
    details { border-top: 1px solid var(--trouve-rule); }
    summary { min-height: 32px; padding: 7px 0 4px; color: var(--trouve-text-hi); cursor: pointer; font-weight: 600; }
    .detail-list { display: grid; gap: 4px; margin: 4px 0 0; padding: 0; list-style: none; }
    .detail-list li { display: flex; align-items: start; gap: 8px; padding: 5px 7px; border-radius: var(--trouve-radius-sm); background: var(--trouve-inset-bg); }
    .detail-list li > span:first-child { flex: 1; min-width: 0; overflow-wrap: anywhere; }
    .review-copy { padding: 7px 8px; border-left: 2px solid var(--trouve-accent); background: var(--trouve-accent-veil); }
    .review-copy p { margin-top: 3px; white-space: pre-wrap; overflow-wrap: anywhere; }
    .merge-box { display: grid; gap: 8px; padding: 9px; border: 1px solid var(--trouve-warn-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-warn-bg); }
    .merge-controls { display: grid; grid-template-columns: minmax(130px, 1fr) auto; align-items: end; gap: 8px; }
    .empty { display: grid; justify-items: center; gap: 6px; padding: 18px 10px; color: var(--trouve-text-dim); text-align: center; }
    .pr-empty { flex: 1; min-height: 120px; align-content: center; }
    .pr-setup { min-height: calc(100vh - 96px); align-content: center; gap: 12px; padding: 24px; }
    .pr-setup strong { color: var(--trouve-text-hi); font-size: 14px; }
    .pr-setup-actions { display: flex; flex-wrap: wrap; justify-content: center; gap: 6px; }
    .pr-toolbar { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 6px; }
    .pr-toolbar > select { min-width: 0; }
    .pr-refresh-additive { position: absolute; width: 1px; height: 1px; min-height: 0; overflow: hidden; padding: 0; clip: rect(0, 0, 0, 0); }
    .pr-refresh-additive:focus-visible { position: static; width: auto; height: auto; min-height: 34px; overflow: visible; padding: 4px 9px; clip: auto; }
    .slint-pr-card { gap: 10px; padding: 14px; border: 0; }
    .slint-pr-meta { display: flex; align-items: center; gap: 8px; color: var(--trouve-text-dim); font-size: 12px; }
    .slint-pr-meta > span:last-child { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .slint-pr-actions { display: flex; }
    .slint-pr-detail { display: grid; gap: 4px; }
    .slint-pr-detail p { color: var(--trouve-text); font-size: 12px; white-space: pre-wrap; }
    .pr-management { margin-top: 2px; }
    .pr-management > summary { color: var(--trouve-text-dim); font-size: 11px; }
    .pr-management[open] { display: grid; gap: 9px; }
    .management-note { font-size: 11px; }
    @media (max-width: 640px) {
      .panel { padding: 10px 8px; }
      .summary-grid, .form-grid, .merge-controls { grid-template-columns: 1fr; }
      .pr-heading { display: grid; grid-template-columns: minmax(0, 1fr) auto; }
      .pr-heading .actions { grid-column: 1 / -1; justify-content: stretch; }
      .actions button, button, input, textarea, select, summary { min-height: 44px; }
      .actions button { flex: 1 1 auto; }
    }
  `;

  sessionId = "";
  sessionTitle = "";

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });
  #loadedServices: AppServices | undefined;
  #loadedSessionId = "";
  #prs: readonly ProtocolPrInfo[] = [];
  #loading = false;
  #loadGeneration = 0;
  #busy = "";
  #notice = "";
  #noticeIsError = false;
  #loadError = false;
  #githubConfigured: boolean | undefined;
  #repositorySetupRequired = false;
  #mergeMethod = "squash";
  #confirmMergeNumber: number | undefined;
  #selectedPrNumber: number | undefined;
  #createOpen = false;

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#loadedServices = undefined;
    this.#loadedSessionId = "";
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (sessionId !== this.#loadedSessionId) {
      this.#prs = [];
      this.#confirmMergeNumber = undefined;
      this.#selectedPrNumber = undefined;
      this.#createOpen = false;
      this.#notice = "";
      this.#noticeIsError = false;
      this.#loadError = false;
      this.#githubConfigured = undefined;
      this.#repositorySetupRequired = false;
    }
    if (
      services !== undefined &&
      sessionId !== "" &&
      (services !== this.#loadedServices || sessionId !== this.#loadedSessionId)
    ) {
      this.#loadedServices = services;
      this.#loadedSessionId = sessionId;
      void this.#load();
    }
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override render() {
    if (this.#effectiveSessionId === "") {
      return html`<div class="empty" role="status">Select a session to view pull requests.</div>`;
    }
    const prs = this.#currentPullRequests();
    const openPr = prs.find((pr) => pr.state === "open");
    const selectedPr = prs.find((pr) => pr.number === this.#selectedPrNumber)
      ?? prs[0];
    return html`
      <section class="panel" aria-label="Pull requests">
        ${this.#githubConfigured === false
          ? html`
              <section class="pr-setup empty" aria-labelledby="session-pr-setup-title">
                <strong id="session-pr-setup-title">Connect GitHub to see this session's pull requests</strong>
                <span>trouve looks up the pull requests opened from each session's branch and shows their checks and reviews here. Sign in with GitHub OAuth under Settings → Integrations; each GitHub Enterprise host uses its own OAuth app.</span>
                <button class="primary" type="button" @click=${this.#openIntegrationsSettings}>Set up GitHub integration</button>
              </section>
            `
          : this.#repositorySetupRequired
            ? html`
                <section class="pr-setup empty" aria-labelledby="session-pr-repository-setup-title">
                  <strong id="session-pr-repository-setup-title">Connect this workspace to a GitHub repository</strong>
                  <span>A GitHub account is connected, but this workspace's <code>origin</code> remote or its GitHub host is not ready for pull requests. Add a GitHub-style <code>origin</code> remote in the terminal. If it uses GitHub Enterprise, also add and sign in to that host under Settings → Integrations.</span>
                  <div class="pr-setup-actions">
                    <button class="primary" type="button" @click=${this.#openTerminal}>Open Terminal</button>
                    <button type="button" @click=${this.#openIntegrationsSettings}>GitHub settings</button>
                  </div>
                </section>
              `
          : html`
              <header class="pr-toolbar">
                <h2 id="session-pr-title" class="visually-hidden">Pull requests</h2>
                ${prs.length > 1
                  ? html`<label class="visually-hidden" for="session-pr-picker">Pull request</label>
                      <select
                        id="session-pr-picker"
                        .value=${String(selectedPr?.number ?? "")}
                        @change=${(event: Event) => {
                          this.#selectedPrNumber = Number((event.currentTarget as HTMLSelectElement).value);
                          this.#confirmMergeNumber = undefined;
                          this.requestUpdate();
                        }}
                      >${prs.map((pr) => html`<option value=${pr.number}>${pr.state}${pr.draft ? " · draft" : ""} · #${pr.number} · ${pr.title}</option>`)}</select>`
                  : html`<span></span>`}
                <button type="button" @click=${() => { this.#createOpen = !this.#createOpen; this.requestUpdate(); }}>Create PR</button>
                <button class="pr-refresh-additive" type="button" ?disabled=${this.#loading || this.#busy !== ""} @click=${() => void this.#load()}>
                  ${this.#loading ? "Refreshing…" : "Refresh"}
                </button>
              </header>
              ${this.#notice === "" && !this.#loadError
                ? nothing
                : html`<div class=${`notice${this.#noticeIsError ? " error" : ""}`} role=${this.#noticeIsError ? "alert" : "status"} aria-live="polite">
                    <span>${this.#notice}</span>
                    ${this.#loadError ? html`<button type="button" ?disabled=${this.#loading} @click=${() => void this.#load()}>Retry</button>` : nothing}
                  </div>`}

              ${this.#loading && prs.length === 0
                ? html`<div class="empty" role="status">Looking for pull requests…</div>`
                : this.#loadError && prs.length === 0
                  ? html`<div class="empty"><strong>Pull requests unavailable</strong><span>Retry when the server connection or GitHub configuration is ready.</span></div>`
                  : prs.length === 0
                    ? html`<div class="empty pr-empty"><span>No pull requests for this session's branch yet.</span></div>`
                    : selectedPr === undefined
                      ? nothing
                      : this.#renderPr(selectedPr, selectedPr === openPr)}

              ${this.#createOpen ? this.#renderCreate(openPr !== undefined) : nothing}
            `}
      </section>
    `;
  }

  #currentPullRequests(): readonly ProtocolPrInfo[] {
    const projected = this.#store.value?.sessionPullRequests(this.#effectiveSessionId);
    if (projected === undefined) return this.#prs;
    // App metadata normally exists before this pane mounts. Keep the local
    // response as a narrow fallback during bootstrap or isolated embedding.
    return projected.length > 0 || this.#prs.length === 0 ? projected : this.#prs;
  }

  #renderPr(pr: ProtocolPrInfo, primaryOpen: boolean) {
    const checks = checkSummary(pr);
    const reviews = reviewSummary(pr);
    const mergeability = mergeabilitySummary(pr);
    const url = safeSessionPrHref(pr.url);
    const reviewUrl = safeSessionPrHref(pr.trouve_review?.review_url);
    return html`
      <article class="pr-card slint-pr-card" aria-labelledby=${`session-pr-${pr.number}`}>
        <h3 id=${`session-pr-${pr.number}`}>${pr.title}</h3>
        <div class="slint-pr-meta">
          <span class=${`status-pill ${pr.state === "open" || pr.state === "merged" ? pr.state : ""}`}>${pr.state}${pr.draft ? " · draft" : ""}</span>
          <span>#${pr.number} · ${pr.head} → ${pr.base}</span>
        </div>
        <div class="slint-pr-actions">
          ${url === undefined ? nothing : html`<button type="button" @click=${() => this.#openExternal(url)}>Open on GitHub ↗</button>`}
        </div>
        ${pr.checks.length === 0
          ? nothing
          : html`<section class="slint-pr-detail">
              <h4>Checks</h4>
              <p>${pr.checks.map((check) => `${check.name}: ${humanize(check.conclusion ?? check.status)}`).join("\n")}</p>
            </section>`}
        ${pr.reviews.length === 0 && (pr.requested_reviewers?.length ?? 0) === 0
          ? nothing
          : html`<section class="slint-pr-detail">
              <h4>Reviews</h4>
              <p>${[
                ...pr.reviews.map((review) => `${review.reviewer}: ${humanize(review.state)}`),
                ...(pr.requested_reviewers ?? []).map((reviewer) => `${reviewer}: Review requested`),
              ].join("\n")}</p>
            </section>`}
        <details class="pr-management">
          <summary>Manage pull request</summary>
          <div class="summary-grid">
            ${this.#renderSummary("Checks", checks)}
            ${this.#renderSummary("Reviews", reviews)}
            ${this.#renderSummary("Merge", mergeability)}
          </div>
          ${primaryOpen ? html`<p class="management-note">This is the session's current merge target.</p>` : nothing}
          ${pr.trouve_review === null || pr.trouve_review === undefined ? nothing : html`
            <section class="review-copy" aria-label="Latest trouve review">
              <h4>trouve review · ${humanize(pr.trouve_review.status)}</h4>
              ${pr.trouve_review.summary ? html`<p>${pr.trouve_review.summary}</p>` : nothing}
              ${reviewUrl === undefined ? nothing : html`<button type="button" @click=${() => this.#openExternal(reviewUrl)}>Open review</button>`}
            </section>
          `}
          ${pr.state === "open" ? this.#renderMerge(pr) : nothing}
        </details>
      </article>
    `;
  }

  #renderSummary(label: string, summary: PrSummary) {
    return html`<div class="summary-item"><span>${label}</span><strong class=${summary.tone}>${summary.label}</strong></div>`;
  }

  #renderMerge(pr: ProtocolPrInfo) {
    const confirming = this.#confirmMergeNumber === pr.number;
    return html`
      <section class="settings-card" aria-labelledby="merge-pr-title">
        <h3 id="merge-pr-title">Merge pull request #${pr.number}</h3>
        <p>${canMergePr(pr)
          ? "Choose how GitHub should combine this pull request."
          : `${mergeabilitySummary(pr).label}. Resolve the current state before merging.`}</p>
        <div class="merge-controls">
          <label>
            Merge method
            <select
              .value=${this.#mergeMethod}
              ?disabled=${this.#busy !== "" || !canMergePr(pr)}
              @change=${(event: Event) => {
                this.#mergeMethod = mergeMethod((event.currentTarget as HTMLSelectElement).value);
                this.#confirmMergeNumber = undefined;
                this.requestUpdate();
              }}
            >
              <option value="merge">Merge commit</option>
              <option value="squash">Squash and merge</option>
              <option value="rebase">Rebase and merge</option>
            </select>
          </label>
          <button type="button" ?disabled=${this.#busy !== "" || !canMergePr(pr)} @click=${() => { this.#confirmMergeNumber = pr.number; this.requestUpdate(); }}>
            Review merge
          </button>
        </div>
        ${confirming ? html`
          <div class="merge-box" role="group" aria-labelledby="confirm-merge-title" aria-describedby="confirm-merge-copy">
            <h4 id="confirm-merge-title">Confirm ${humanize(this.#mergeMethod)} merge</h4>
            <p id="confirm-merge-copy">Merge pull request #${pr.number} on GitHub? This changes the remote repository and cannot be undone from this panel.</p>
            <div class="actions">
              <button type="button" @click=${() => { this.#confirmMergeNumber = undefined; this.requestUpdate(); }}>Cancel</button>
              <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#merge(pr.number)}>Confirm merge</button>
            </div>
          </div>
        ` : nothing}
      </section>
    `;
  }

  #renderCreate(hasOpenPr: boolean) {
    return html`
      <section class="settings-card" aria-labelledby="create-pr-title">
        <h3 id="create-pr-title">Create pull request</h3>
        <p>${hasOpenPr
          ? "This session already has an open pull request. Close or merge it before opening another for the same branch."
          : "This pushes the session branch and opens a pull request on its configured GitHub remote."}</p>
        <form @submit=${(event: SubmitEvent) => void this.#create(event)}>
          <label>
            Title
            <input name="title" required maxlength="200" autocomplete="off" value=${this.sessionTitle} ?disabled=${hasOpenPr || this.#busy !== ""} />
          </label>
          <label>
            Description
            <textarea name="body" maxlength="65535" ?disabled=${hasOpenPr || this.#busy !== ""}></textarea>
          </label>
          <div class="form-grid">
            <label>
              Base branch (optional)
              <input name="base" autocomplete="off" spellcheck="false" placeholder="Repository default" ?disabled=${hasOpenPr || this.#busy !== ""} />
            </label>
            <label class="checkbox">
              <input name="draft" type="checkbox" ?disabled=${hasOpenPr || this.#busy !== ""} />
              Open as draft
            </label>
          </div>
          <div class="actions">
            <button type="button" @click=${() => { this.#createOpen = false; this.requestUpdate(); }}>Cancel</button>
            <button class="primary" type="submit" ?disabled=${hasOpenPr || this.#busy !== ""}>
              ${this.#busy === "create" ? "Creating…" : "Push branch and create"}
            </button>
          </div>
        </form>
      </section>
    `;
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "") return;
    const generation = ++this.#loadGeneration;
    this.#loading = true;
    this.requestUpdate();
    try {
      // Check authentication before asking for session PRs. The PR endpoint
      // correctly rejects unauthenticated requests, but treating that response
      // as a generic load failure hides the setup state and its call to action.
      const integration = await services.protocol.githubIntegration();
      if (generation !== this.#loadGeneration || !this.isConnected || sessionId !== this.#effectiveSessionId) return;
      this.#githubConfigured = githubIntegrationConfigured(integration);
      if (!this.#githubConfigured) {
        this.#clearPrStateForSetup();
        return;
      }

      this.#repositorySetupRequired = false;
      const prs = await services.protocol.sessionPrs(sessionId);
      if (generation !== this.#loadGeneration || !this.isConnected || sessionId !== this.#effectiveSessionId) return;
      this.#prs = prs;
      this.#store.value?.replaceSessionPullRequests(sessionId, prs);
      if (!prs.some((pr) => pr.number === this.#selectedPrNumber)) {
        this.#selectedPrNumber = prs[0]?.number;
      }
      this.#loadError = false;
      if (!prs.some((pr) => pr.number === this.#confirmMergeNumber && pr.state === "open")) {
        this.#confirmMergeNumber = undefined;
      }
      if (this.#noticeIsError) {
        this.#notice = "";
        this.#noticeIsError = false;
      }
    } catch (cause) {
      if (generation !== this.#loadGeneration || !this.isConnected || sessionId !== this.#effectiveSessionId) return;
      if (
        this.#githubConfigured === true
        && cause instanceof ProtocolClientError
        && cause.status === 400
      ) {
        // The session PR endpoint uses Bad Request for repository integration
        // prerequisites: no origin, a non-GitHub origin, an unknown Enterprise
        // host, or no OAuth session for that host. These are setup states, not
        // transient server failures.
        this.#repositorySetupRequired = true;
        this.#clearPrStateForSetup();
      } else {
        this.#loadError = true;
        this.#setNotice("Pull requests could not be loaded. Check the server connection and retry.", true);
      }
    } finally {
      if (generation === this.#loadGeneration && sessionId === this.#effectiveSessionId) {
        this.#loading = false;
        this.requestUpdate();
      }
    }
  }

  async #create(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const form = event.currentTarget as HTMLFormElement;
    const draftControl = form.elements.namedItem("draft");
    if (services === undefined || sessionId === "" || this.#busy !== "" || this.#currentPullRequests().some((pr) => pr.state === "open")) return;
    let request: ProtocolCreatePrRequest;
    try {
      request = createPrRequest({
        title: formInput(form, "title"),
        body: formInput(form, "body"),
        base: formInput(form, "base"),
        draft: draftControl instanceof HTMLInputElement && draftControl.checked,
      });
    } catch {
      this.#setNotice("A pull-request title is required.", true);
      return;
    }
    this.#busy = "create";
    this.#setNotice("Pushing the branch and creating the pull request…", false);
    this.requestUpdate();
    try {
      await services.protocol.createSessionPr(sessionId, request);
      if (sessionId !== this.#effectiveSessionId) return;
      form.reset();
      this.#createOpen = false;
      this.#setNotice("Pull request created.", false);
      await this.#load();
    } catch {
      this.#setNotice("The pull request could not be created. Check GitHub access and the branch state, then retry.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #merge(prNumber: number): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const pr = this.#currentPullRequests().find((candidate) => candidate.number === prNumber);
    if (
      services === undefined ||
      sessionId === "" ||
      this.#busy !== "" ||
      this.#confirmMergeNumber !== prNumber ||
      pr === undefined ||
      !canMergePr(pr)
    ) return;
    this.#busy = "merge";
    this.#setNotice(`Merging pull request #${prNumber}…`, false);
    this.requestUpdate();
    try {
      await services.protocol.mergeSessionPr(sessionId, mergeMethod(this.#mergeMethod));
      if (sessionId !== this.#effectiveSessionId) return;
      this.#confirmMergeNumber = undefined;
      this.#setNotice(`Pull request #${prNumber} was merged.`, false);
      await this.#load();
    } catch {
      this.#setNotice("The pull request could not be merged. Recheck its reviews, checks, and mergeability, then retry.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  #openExternal(value: string): void {
    const href = safeSessionPrHref(value);
    if (href === undefined) {
      this.#setNotice("The pull-request link was rejected because it is not a safe HTTPS URL.", true);
      return;
    }
    this.dispatchEvent(new CustomEvent<{ readonly href: string }>("trouve-open-external", {
      detail: { href },
      bubbles: true,
      composed: true,
    }));
  }

  readonly #openIntegrationsSettings = (): void => {
    this.#services.value?.router.navigate({ kind: "settings", section: "integrations" });
  };

  readonly #openTerminal = (): void => {
    const services = this.#services.value;
    if (services === undefined) return;
    const route = readSignal(services.router.route);
    if (route.kind !== "session" || route.sessionId !== this.#effectiveSessionId) return;
    services.router.navigate({ ...route, inspection: "terminal" });
  };

  #clearPrStateForSetup(): void {
    this.#prs = [];
    const sessionId = this.#effectiveSessionId;
    if (sessionId !== "") {
      this.#store.value?.clearSessionPullRequests(sessionId);
    }
    this.#selectedPrNumber = undefined;
    this.#confirmMergeNumber = undefined;
    this.#createOpen = false;
    this.#loadError = false;
    this.#notice = "";
    this.#noticeIsError = false;
  }

  #setNotice(message: string, error: boolean): void {
    this.#notice = message;
    this.#noticeIsError = error;
    this.requestUpdate();
  }
}

if ("customElements" in globalThis && !customElements.get("trouve-session-pr-panel")) {
  customElements.define("trouve-session-pr-panel", TrouveSessionPrPanel);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-session-pr-panel": TrouveSessionPrPanel;
  }
}
