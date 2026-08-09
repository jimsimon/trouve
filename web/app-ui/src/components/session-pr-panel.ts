import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing, type TemplateResult } from "lit";

import {
  appServicesContext,
  appStoreContext,
  sessionContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import {
  ProtocolClientError,
  type ProtocolCreatePrRequest,
  type ProtocolPrActionRequest,
  type ProtocolPrDetail,
  type ProtocolPrDetailSection,
  type ProtocolPrFileDiff,
  type ProtocolPrInfo,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import type { DiffMode } from "./diff-view.js";
import { languageForPath } from "./file-language.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import "./diff-view.js";
import "./markdown-view.js";
import {
  canMergePr,
  checkSummary,
  createPrRequest,
  githubIntegrationConfigured,
  mergeabilitySummary,
  safeSessionPrHref,
  sessionPullRequestsListHref,
} from "./session-pr-panel-model.js";

type PrTab = "conversation" | "checks" | "commits" | "files";
type PrComment = NonNullable<ProtocolPrDetail["comments"]>[number];
type PrReviewThread = NonNullable<ProtocolPrDetail["review_threads"]>[number];
type PrReaction = NonNullable<PrComment["reactions"]>[number];
type PrActor = NonNullable<ProtocolPrDetail["assignees"]>[number];
type PrReview = NonNullable<ProtocolPrDetail["reviews"]>[number];

const AUTOMATIC_RETRY_MS = 5_000;

const detailSectionForTab = (tab: PrTab): ProtocolPrDetailSection =>
  tab === "checks" ? "overview" : tab;

const prProjectionKey = (pr: ProtocolPrInfo | undefined): string => pr === undefined
  ? ""
  : JSON.stringify([
      pr.host,
      pr.repository,
      pr.number,
      pr.head_sha ?? "",
      pr.state,
      pr.draft,
      pr.title,
      pr.merge_state_status ?? "",
      pr.comments ?? 0,
      pr.checks,
      pr.reviews,
      pr.requested_reviewers ?? [],
    ]);

const mergePrDetailSection = (
  current: ProtocolPrDetail | undefined,
  incoming: ProtocolPrDetail,
  section: ProtocolPrDetailSection,
): ProtocolPrDetail => {
  if (current === undefined || current.info.number !== incoming.info.number) return incoming;
  const stack = incoming.stack ?? current.stack;
  return {
      ...incoming,
      comments: (section === "conversation" ? incoming.comments : current.comments) ?? [],
      review_threads: section === "conversation"
        ? incoming.review_threads ?? []
        : current.review_threads ?? [],
      reviews: (section === "conversation" ? incoming.reviews : current.reviews) ?? [],
      commits: (section === "commits" ? incoming.commits : current.commits) ?? [],
      commit_count: section === "commits" ? incoming.commit_count : current.commit_count,
      files: (section === "files" ? incoming.files : current.files) ?? [],
      ...(stack === undefined ? {} : { stack }),
    };
};

const REACTIONS = Object.freeze([
  ["thumbs_up", "👍", "Thumbs up"],
  ["thumbs_down", "👎", "Thumbs down"],
  ["laugh", "😄", "Laugh"],
  ["hooray", "🎉", "Hooray"],
  ["confused", "😕", "Confused"],
  ["heart", "❤️", "Heart"],
  ["rocket", "🚀", "Rocket"],
  ["eyes", "👀", "Eyes"],
] as const);

const humanize = (value: string): string => {
  const text = value.trim().replaceAll("_", " ");
  return text === "" ? "Unknown" : `${text[0]?.toUpperCase() ?? ""}${text.slice(1)}`;
};

const formInput = (form: HTMLFormElement, name: string): string => {
  const control = form.elements.namedItem(name);
  return control instanceof HTMLInputElement
      || control instanceof HTMLTextAreaElement
      || control instanceof HTMLSelectElement
    ? control.value
    : "";
};

const formChecked = (form: HTMLFormElement, name: string): boolean => {
  const control = form.elements.namedItem(name);
  return control instanceof HTMLInputElement && control.checked;
};

const checkedValues = (form: HTMLFormElement, name: string): string[] =>
  [...form.querySelectorAll<HTMLInputElement>(`input[name="${name}"]:checked`)]
    .map(({ value }) => value);

const splitLogins = (value: string): string[] => [...new Set(
  value.split(/[\s,]+/u).map((part) => part.trim().replace(/^@/u, "")).filter(Boolean),
)];

const actorName = (actor: PrActor | undefined): string =>
  actor === undefined ? "ghost" : actor.login || actor.name || "unknown";

const dateLabel = (value: string | null | undefined): string => {
  if (value == null || value === "") return "";
  const date = new Date(value);
  return Number.isNaN(date.valueOf())
    ? value
    : new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(date);
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
    button, input, textarea, select {
      min-height: 32px;
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
    button:disabled, input:disabled, textarea:disabled, select:disabled {
      cursor: not-allowed;
      opacity: .56;
    }
    button:focus-visible, input:focus-visible, textarea:focus-visible, select:focus-visible,
    summary:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    input, textarea, select { width: 100%; padding: 5px 7px; font-weight: 400; }
    textarea { min-height: 82px; resize: vertical; }
    label { display: grid; min-width: 0; gap: 4px; color: var(--trouve-text-hi); font-weight: 600; }
    form { display: grid; gap: 8px; }
    code { color: var(--trouve-text-hi); overflow-wrap: anywhere; }
    .panel {
      display: flex;
      height: 100%;
      min-height: 100%;
      flex-direction: column;
      gap: 7px;
      padding: 7px;
      overflow: auto;
    }
    .empty { display: grid; place-content: center; justify-items: center; gap: 8px; min-height: 130px; padding: 20px; text-align: center; }
    .pr-empty { flex: 1; }
    .pr-setup { flex: 1; min-height: calc(100vh - 96px); }
    .pr-setup strong { color: var(--trouve-text-hi); font-size: 14px; }
    .pr-setup-actions, .actions, .inline-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
    .actions { justify-content: flex-end; }
    .notice { display: flex; align-items: center; gap: 8px; min-height: 20px; color: var(--trouve-text-dim); font-size: 11px; }
    .notice > span { flex: 1; min-width: 0; }
    .notice.error { color: var(--trouve-err); }
    .pr-toolbar { display: grid; gap: 6px; }
    .pr-toolbar-heading, .pr-title-row { display: flex; min-width: 0; align-items: center; gap: 7px; }
    .pr-toolbar-heading h2, .pr-title-copy { flex: 1; min-width: 0; }
    .pr-toolbar-actions, .pr-title-actions { display: flex; align-items: center; gap: 2px; }
    button.icon-button {
      display: inline-grid;
      width: 30px;
      min-width: 30px;
      min-height: 30px;
      place-items: center;
      padding: 0;
      border-color: transparent;
      color: var(--trouve-text-dim);
      background: transparent;
    }
    button.icon-button:hover:not(:disabled), button.icon-button[aria-expanded="true"] {
      color: var(--trouve-text-hi);
      background: var(--trouve-hover-bg);
    }
    .pr-shell { display: grid; min-height: 0; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius); background: var(--trouve-surface); }
    .pr-title { display: grid; gap: 5px; padding: 10px 11px 8px; }
    .pr-title-copy h3 { overflow-wrap: anywhere; font-size: 14px; }
    .pr-meta { display: flex; flex-wrap: wrap; gap: 3px 10px; margin-top: 3px; color: var(--trouve-text-dim); font-size: 11px; }
    .status-pill { display: inline-flex; align-items: center; min-height: 21px; padding: 2px 7px; border-radius: 999px; color: var(--trouve-text-dim); background: var(--trouve-pill-bg); font-size: 10px; text-transform: capitalize; }
    .status-pill.open { color: var(--trouve-ok); }
    .status-pill.merged { color: var(--trouve-merged); }
    .pr-tabs { display: flex; gap: 2px; padding: 0 8px; border-top: 1px solid var(--trouve-rule); border-bottom: 1px solid var(--trouve-rule); overflow-x: auto; }
    .pr-tabs button { min-width: max-content; min-height: 34px; padding: 5px 9px; border: 0; border-bottom: 2px solid transparent; border-radius: 0; color: var(--trouve-text-dim); background: transparent; }
    .pr-tabs button[aria-selected="true"] { border-bottom-color: var(--trouve-accent); color: var(--trouve-text-hi); }
    .pr-layout { display: grid; grid-template-columns: minmax(0, 1fr) minmax(225px, 280px); min-height: 0; }
    .pr-main { display: grid; align-content: start; min-width: 0; gap: 10px; padding: 11px; border-right: 1px solid var(--trouve-rule); }
    .pr-sidebar { display: grid; min-width: 0; align-content: start; gap: 0; padding: 3px 10px 10px; }
    .pr-loading { padding: 22px; color: var(--trouve-text-dim); text-align: center; }
    .timeline-section, .description, .review-thread, .review-card, .settings-card { display: grid; gap: 8px; }
    .timeline-section + .timeline-section { padding-top: 10px; border-top: 1px solid var(--trouve-rule); }
    .section-heading { display: flex; align-items: center; gap: 8px; }
    .section-heading h3 { flex: 1; }
    .description, .review-thread, .review-card, .comment-card {
      min-width: 0;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-inset-bg);
    }
    .description, .review-card { padding: 10px; }
    .comment-card { display: grid; }
    .comment-header, .thread-header { display: flex; align-items: center; gap: 7px; padding: 7px 9px; border-bottom: 1px solid var(--trouve-rule); color: var(--trouve-text-dim); font-size: 11px; }
    .comment-header strong, .thread-header strong { color: var(--trouve-text-hi); }
    .comment-header > span:nth-child(2), .thread-header > span:nth-child(2) { flex: 1; min-width: 0; }
    .comment-body { min-width: 0; padding: 9px; }
    trouve-markdown-view { display: block; min-width: 0; overflow-wrap: anywhere; }
    .muted-copy { color: var(--trouve-text-dim); font-style: italic; }
    .reaction-row { display: flex; flex-wrap: wrap; gap: 4px; padding-top: 5px; }
    .reaction-row button { min-height: 26px; padding: 2px 7px; border-radius: 999px; color: var(--trouve-text-dim); background: transparent; font-size: 11px; }
    .reaction-row button.active { border-color: var(--trouve-accent); color: var(--trouve-accent); background: var(--trouve-accent-veil); }
    .thread-copy { padding: 8px 9px; color: var(--trouve-text-dim); font-size: 11px; }
    .thread-comments { display: grid; gap: 7px; padding: 0 8px 8px; }
    .resolved { opacity: .72; }
    .side-section { border-bottom: 1px solid var(--trouve-rule); }
    .side-section > summary { min-height: 36px; padding: 9px 1px; color: var(--trouve-text-hi); cursor: pointer; font-weight: 600; list-style-position: inside; }
    .side-section[open] { padding-bottom: 9px; }
    .side-content { display: grid; gap: 7px; }
    .side-copy { color: var(--trouve-text-dim); font-size: 11px; overflow-wrap: anywhere; }
    .actor-list, .label-list, .check-list, .commit-list, .file-list, .stack-list { display: grid; gap: 5px; margin: 0; padding: 0; list-style: none; }
    .actor, .label-row, .check-row, .commit-row, .file-row, .stack-row { display: flex; min-width: 0; align-items: center; gap: 7px; padding: 6px 7px; border-radius: var(--trouve-radius-sm); background: var(--trouve-inset-bg); }
    .actor > span, .label-row > span, .check-row > span, .commit-row > span, .file-row > span, .stack-row > span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
    .actor small, .check-row small, .commit-row small, .file-row small, .stack-row small { display: block; margin-top: 1px; }
    .pr-file-workspace {
      display: grid;
      grid-template-columns: minmax(190px, 260px) minmax(0, 1fr);
      min-width: 0;
      min-height: 480px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius-sm);
      overflow: hidden;
      background: var(--trouve-inset-bg);
    }
    .pr-file-tree {
      display: grid;
      min-width: 0;
      align-content: start;
      border-right: 1px solid var(--trouve-rule);
      overflow: auto;
      background: var(--trouve-sidebar-bg);
    }
    .pr-file-tree-row { display: flex; min-width: 0; align-items: center; border-bottom: 1px solid var(--trouve-rule); }
    .pr-file-select {
      display: flex;
      min-width: 0;
      min-height: 42px;
      flex: 1;
      align-items: center;
      gap: 7px;
      padding: 6px 7px;
      border: 0;
      border-radius: 0;
      text-align: left;
      background: transparent;
    }
    .pr-file-select > span { min-width: 0; flex: 1; }
    .pr-file-select strong { display: block; overflow: hidden; color: var(--trouve-text-hi); text-overflow: ellipsis; white-space: nowrap; }
    .pr-file-select small { display: block; margin-top: 1px; }
    .pr-file-tree-row.selected { background: var(--trouve-accent-dim); }
    .pr-file-tree-row > .icon-button { flex: none; margin-right: 3px; }
    .pr-file-preview { display: grid; min-width: 0; min-height: 0; align-content: start; }
    .pr-file-preview-state { display: grid; min-height: 430px; place-content: center; justify-items: center; gap: 8px; padding: 20px; text-align: center; }
    .pr-file-preview-state p { max-width: 54ch; }
    .pr-file-notice { padding: 7px 9px; border-bottom: 1px solid var(--trouve-rule); color: var(--trouve-warn); font-size: 11px; }
    .pr-file-diff { min-height: 430px; height: min(70vh, 720px); border: 0; }
    .pr-file-review-form { padding: 10px; border-top: 1px solid var(--trouve-rule); background: var(--trouve-surface); }
    .avatar { width: 22px; height: 22px; border-radius: 50%; object-fit: cover; background: var(--trouve-pill-bg); }
    .tone-ok { color: var(--trouve-ok); }
    .tone-warn { color: var(--trouve-warn); }
    .tone-error { color: var(--trouve-err); }
    .label-dot { width: 10px; height: 10px; flex: none; border-radius: 50%; background: var(--label-color, var(--trouve-text-dim)); }
    .stats { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1px; background: var(--trouve-rule); }
    .stat { padding: 7px; background: var(--trouve-inset-bg); }
    .stat span { display: block; color: var(--trouve-text-dim); font-size: 9px; text-transform: uppercase; letter-spacing: .04em; }
    .stat strong { color: var(--trouve-text-hi); font-size: 11px; }
    .additions { color: var(--trouve-ok) !important; }
    .deletions { color: var(--trouve-err) !important; }
    .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; }
    .checkbox { display: flex; align-items: center; gap: 7px; font-weight: 400; }
    .checkbox input { width: 16px; min-height: 16px; accent-color: var(--trouve-accent); }
    .choice-list { display: grid; max-height: 190px; gap: 4px; overflow: auto; padding: 5px; border: 1px solid var(--trouve-rule); border-radius: var(--trouve-radius-sm); }
    .choice-list label { display: flex; align-items: center; gap: 6px; font-weight: 400; }
    .choice-list input { width: 16px; min-height: 16px; }
    .merge-box { display: grid; gap: 7px; padding: 8px; border: 1px solid var(--trouve-warn-border); border-radius: var(--trouve-radius-sm); background: var(--trouve-warn-bg); }
    .truncated { color: var(--trouve-warn); font-size: 11px; }
    @media (max-width: 760px) {
      .panel { padding: 6px; }
      .pr-layout { grid-template-columns: 1fr; }
      .pr-main { border-right: 0; border-bottom: 1px solid var(--trouve-rule); }
      .form-grid, .stats { grid-template-columns: 1fr; }
      .pr-file-workspace { grid-template-columns: 1fr; }
      .pr-file-tree { max-height: 220px; border-right: 0; border-bottom: 1px solid var(--trouve-rule); }
      button, input, textarea, select { min-height: 42px; }
      button.icon-button { width: 42px; min-width: 42px; }
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
  #detail: ProtocolPrDetail | undefined;
  #loading = false;
  #detailLoading = false;
  #loadGeneration = 0;
  #detailGeneration = 0;
  #fileDiffGeneration = 0;
  #busy = "";
  #notice = "";
  #noticeIsError = false;
  #loadError = false;
  #githubConfigured: boolean | undefined;
  #repositorySetupRequired = false;
  #selectedPrNumber: number | undefined;
  #loadedPrProjectionKey = "";
  #loadedDetailSections = new Set<ProtocolPrDetailSection>();
  #createOpen = false;
  #activeTab: PrTab = "conversation";
  #mergeMethod = "squash";
  #mergeCommitTitle = "";
  #mergeCommitMessage = "";
  #confirmAction = "";
  #editingComment: { readonly id: string; readonly kind: "issue" | "review" } | undefined;
  #editingReviewId = "";
  #dismissingReviewId = "";
  #replyingThreadId = "";
  #selectedFilePath = "";
  #fileDiff: ProtocolPrFileDiff | undefined;
  #fileDiffLoading = false;
  #fileDiffError = "";
  #diffMode: DiffMode = "unified";
  #loadRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #detailRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #fileDiffRetryTimer: ReturnType<typeof setTimeout> | undefined;

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#detailGeneration += 1;
    this.#fileDiffGeneration += 1;
    this.#clearRetryTimers();
    this.#loadedServices = undefined;
    this.#loadedSessionId = "";
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (sessionId !== this.#loadedSessionId) this.#resetSessionState();
    if (
      services !== undefined
      && sessionId !== ""
      && (services !== this.#loadedServices || sessionId !== this.#loadedSessionId)
    ) {
      this.#loadedServices = services;
      this.#loadedSessionId = sessionId;
      void this.#load();
      return;
    }
    if (this.#githubConfigured === true && sessionId !== "") {
      this.#syncProjectedSelection();
    }
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override render() {
    const sessionId = this.#effectiveSessionId;
    if (sessionId === "") {
      return html`<div class="empty" role="status">Select a session to view pull requests.</div>`;
    }
    const prs = this.#currentPullRequests();
    const selectedPr = prs.find((pr) => pr.number === this.#selectedPrNumber) ?? prs[0];
    const store = this.#store.value;
    const session = store?.session(sessionId);
    const accountLists = store === undefined
      ? []
      : readSignal(store.githubPullRequests).map(({ pullRequests }) => pullRequests);
    const associated = selectedPr === undefined
      ? prs
      : [selectedPr, ...prs.filter((pr) => pr !== selectedPr)];
    const pullRequestsHref = sessionPullRequestsListHref(
      associated,
      session?.workspaceId ?? "",
      accountLists,
    );

    return html`
      <section class="panel" aria-label="Pull requests">
        ${this.#githubConfigured === false
          ? this.#renderGithubSetup()
          : this.#repositorySetupRequired
            ? this.#renderRepositorySetup()
            : html`
                ${this.#renderToolbar(prs, selectedPr, pullRequestsHref)}
                ${this.#renderNotice()}
                ${this.#loading && prs.length === 0
                  ? html`<div class="empty pr-empty" role="status">Looking for pull requests…</div>`
                  : this.#loadError && prs.length === 0
                    ? html`<div class="empty pr-empty"><strong>Pull requests unavailable</strong><span>trouve will retry automatically when the server connection and GitHub configuration are ready.</span></div>`
                    : prs.length === 0
                      ? html`<div class="empty pr-empty"><span>No pull requests are associated with this session yet.</span></div>`
                      : selectedPr === undefined
                        ? nothing
                        : this.#renderPr(selectedPr)}
                ${this.#createOpen
                  ? this.#renderCreate(prs.some((pr) => pr.state === "open"))
                  : nothing}
              `}
      </section>
    `;
  }

  #renderGithubSetup(): TemplateResult {
    return html`
      <section class="pr-setup empty" aria-labelledby="session-pr-setup-title">
        <strong id="session-pr-setup-title">Connect GitHub to manage this session's pull requests</strong>
        <span>Sign in with GitHub OAuth under Settings → Integrations. Each GitHub Enterprise host uses its own OAuth app.</span>
        <button class="primary" type="button" @click=${this.#openIntegrationsSettings}>Set up GitHub integration</button>
      </section>
    `;
  }

  #renderRepositorySetup(): TemplateResult {
    return html`
      <section class="pr-setup empty" aria-labelledby="session-pr-repository-setup-title">
        <strong id="session-pr-repository-setup-title">Connect this workspace to a GitHub repository</strong>
        <span>A GitHub account is connected, but this workspace's <code>origin</code> remote or GitHub host is not ready. Add a GitHub-style <code>origin</code> remote, and configure its Enterprise host when applicable.</span>
        <div class="pr-setup-actions">
          <button class="primary" type="button" @click=${this.#openTerminal}>Open Terminal</button>
          <button type="button" @click=${this.#openIntegrationsSettings}>GitHub settings</button>
        </div>
      </section>
    `;
  }

  #renderToolbar(
    prs: readonly ProtocolPrInfo[],
    selectedPr: ProtocolPrInfo | undefined,
    pullRequestsHref: string | undefined,
  ): TemplateResult {
    return html`
      <header class="pr-toolbar">
        <div class="pr-toolbar-heading">
          <h2 id="session-pr-title">Pull requests</h2>
          <div class="pr-toolbar-actions" aria-label="Pull request actions">
            <button
              class="icon-button"
              type="button"
              title="Create pull request"
              aria-label="Create pull request"
              aria-controls="create-pr-panel"
              aria-expanded=${this.#createOpen ? "true" : "false"}
              @click=${() => {
                this.#createOpen = !this.#createOpen;
                this.requestUpdate();
              }}
            >${fontAwesomeIcon("code-pull-request")}</button>
            <button
              class="icon-button"
              type="button"
              title="Open repository pull requests on GitHub"
              aria-label="Open repository pull requests on GitHub"
              ?disabled=${pullRequestsHref === undefined}
              @click=${() => {
                if (pullRequestsHref !== undefined) this.#openExternal(pullRequestsHref);
              }}
            >${fontAwesomeIcon("arrow-up-right-from-square")}</button>
          </div>
        </div>
        ${prs.length > 1
          ? html`
              <select
                aria-label="Pull request"
                .value=${String(selectedPr?.number ?? "")}
                @change=${(event: Event) => void this.#selectPr(
                  Number((event.currentTarget as HTMLSelectElement).value),
                )}
              >
                ${prs.map((pr) => html`
                  <option value=${pr.number}>${pr.state}${pr.draft ? " · draft" : ""} · #${pr.number} · ${pr.title}</option>
                `)}
              </select>
            `
          : nothing}
      </header>
    `;
  }

  #renderNotice(): TemplateResult | typeof nothing {
    if (this.#notice === "" && !this.#loadError) return nothing;
    return html`
      <div
        class=${`notice${this.#noticeIsError ? " error" : ""}`}
        role=${this.#noticeIsError ? "alert" : "status"}
        aria-live="polite"
      >
        <span>${this.#notice}</span>
      </div>
    `;
  }

  #renderPr(summary: ProtocolPrInfo): TemplateResult {
    const detail = this.#detail?.info.number === summary.number ? this.#detail : undefined;
    const url = safeSessionPrHref(summary.url);
    return html`
      <article class="pr-shell" aria-labelledby=${`session-pr-${summary.number}`} aria-busy=${this.#detailLoading ? "true" : "false"}>
        <header class="pr-title">
          <div class="pr-title-row">
            <div class="pr-title-copy">
              <h3 id=${`session-pr-${summary.number}`}>${summary.title}</h3>
              <div class="pr-meta">
                <span class=${`status-pill ${summary.state}`}>${summary.state}${summary.draft ? " · draft" : ""}</span>
                <span>#${summary.number}</span>
                <span>${summary.head} → ${summary.base}</span>
                <span>by ${summary.author || "unknown"}</span>
              </div>
            </div>
            <div class="pr-title-actions" aria-label="Selected pull request links">
              <button
                class="icon-button"
                type="button"
                title="Copy pull request URL"
                aria-label="Copy pull request URL"
                ?disabled=${url === undefined}
                @click=${() => {
                  if (url !== undefined) void this.#copyUrl(url);
                }}
              >${fontAwesomeIcon("copy")}</button>
              <button
                class="icon-button"
                type="button"
                title="Open on GitHub"
                aria-label="Open on GitHub"
                ?disabled=${url === undefined}
                @click=${() => {
                  if (url !== undefined) this.#openExternal(url);
                }}
              >${fontAwesomeIcon("arrow-up-right-from-square")}</button>
            </div>
          </div>
          ${detail === undefined
            ? nothing
            : html`
                <div class="stats" aria-label="Pull request change summary">
                  <div class="stat"><span>Files</span><strong>${detail.changed_files}</strong></div>
                  <div class="stat"><span>Additions</span><strong class="additions">+${detail.additions}</strong></div>
                  <div class="stat"><span>Deletions</span><strong class="deletions">−${detail.deletions}</strong></div>
                </div>
              `}
        </header>
        ${this.#renderTabs(summary)}
        ${detail === undefined
          ? html`<div class="pr-loading" role="status">${this.#detailLoading ? "Loading pull request…" : "Pull request detail is unavailable."}</div>`
          : html`
              <div class="pr-layout">
                <main class="pr-main" id=${`pr-tab-${this.#activeTab}`} role="tabpanel">
                  ${this.#renderActiveTab(detail)}
                </main>
                <aside class="pr-sidebar" aria-label="Pull request settings">
                  ${this.#renderSidebar(detail)}
                </aside>
              </div>
            `}
      </article>
    `;
  }

  #renderTabs(detail: ProtocolPrInfo): TemplateResult {
    const tabs = [
      ["conversation", "Conversation", detail.comments ?? 0],
      ["checks", "Checks", detail.checks.length],
      ["commits", "Commits", this.#detail?.commit_count ?? 0],
      ["files", "Files", this.#detail?.changed_files ?? 0],
    ] as const;
    return html`
      <nav class="pr-tabs" role="tablist" aria-label="Pull request sections">
        ${tabs.map(([tab, label, count]) => html`
          <button
            id=${`pr-tab-button-${tab}`}
            type="button"
            role="tab"
            aria-selected=${this.#activeTab === tab ? "true" : "false"}
            aria-controls=${`pr-tab-${tab}`}
            @click=${() => this.#selectTab(tab)}
          >${label} · ${count}</button>
        `)}
      </nav>
    `;
  }

  #selectTab(tab: PrTab): void {
    this.#activeTab = tab;
    this.requestUpdate();
    const section = detailSectionForTab(tab);
    if (!this.#loadedDetailSections.has(section) && this.#selectedPrNumber !== undefined) {
      void this.#loadDetail(this.#selectedPrNumber, section);
      return;
    }
    if (tab !== "files") return;
    const files = this.#detail?.files ?? [];
    const selected = files.find(({ path }) => path === this.#selectedFilePath) ?? files[0];
    if (selected !== undefined) void this.#selectFile(selected.path);
  }

  async #selectFile(path: string): Promise<void> {
    if (path === "") return;
    this.#selectedFilePath = path;
    if (this.#fileDiff?.path === path && this.#fileDiffError === "") {
      this.requestUpdate();
      return;
    }
    this.#fileDiff = undefined;
    this.#fileDiffError = "";
    this.requestUpdate();
    await this.#loadFileDiff(path);
  }

  async #loadFileDiff(path: string): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const number = this.#selectedPrNumber;
    if (services === undefined || sessionId === "" || number === undefined || path === "") return;
    const generation = ++this.#fileDiffGeneration;
    if (this.#fileDiffRetryTimer !== undefined) {
      globalThis.clearTimeout(this.#fileDiffRetryTimer);
      this.#fileDiffRetryTimer = undefined;
    }
    this.#fileDiffLoading = true;
    this.#fileDiffError = "";
    this.requestUpdate();
    try {
      const diff = await services.protocol.sessionPrFileDiff(sessionId, number, path);
      if (
        generation !== this.#fileDiffGeneration
        || sessionId !== this.#effectiveSessionId
        || number !== this.#selectedPrNumber
        || path !== this.#selectedFilePath
      ) return;
      this.#fileDiff = diff;
    } catch (cause) {
      if (generation !== this.#fileDiffGeneration || path !== this.#selectedFilePath) return;
      this.#fileDiff = undefined;
      this.#fileDiffError = cause instanceof Error
        ? cause.message
        : "The selected file could not be loaded.";
      this.#scheduleFileDiffRetry(path);
    } finally {
      if (generation === this.#fileDiffGeneration) {
        this.#fileDiffLoading = false;
        this.requestUpdate();
      }
    }
  }

  #renderActiveTab(detail: ProtocolPrDetail): TemplateResult {
    const section = detailSectionForTab(this.#activeTab);
    if (!this.#loadedDetailSections.has(section)) {
      return html`<div class="pr-loading" role="status">Loading ${this.#activeTab}…</div>`;
    }
    switch (this.#activeTab) {
      case "checks": return this.#renderChecks(detail);
      case "commits": return this.#renderCommits(detail);
      case "files": return this.#renderFiles(detail);
      default: return this.#renderConversation(detail);
    }
  }

  #renderConversation(detail: ProtocolPrDetail): TemplateResult {
    const comments = detail.comments ?? [];
    const threads = detail.review_threads ?? [];
    const reviews = detail.reviews ?? [];
    return html`
      ${detail.truncated === true
        ? html`<p class="truncated" role="status">This pull request exceeded the safety cap for one or more very large conversations. Open GitHub for the remaining history.</p>`
        : nothing}
      <section class="timeline-section" aria-labelledby="pr-description-heading">
        <div class="section-heading">
          <h3 id="pr-description-heading">Description</h3>
          <small>${dateLabel(detail.created_at)}</small>
        </div>
        <article class="description">
          ${(detail.body ?? "").trim() === ""
            ? html`<p class="muted-copy">No description provided.</p>`
            : html`<trouve-markdown-view .content=${detail.body ?? ""}></trouve-markdown-view>`}
          ${this.#renderReactions(detail.id, detail.reactions ?? [])}
        </article>
        ${detail.capabilities.can_update === true ? this.#renderEditPullRequest(detail) : nothing}
      </section>
      ${comments.length === 0
        ? nothing
        : html`
            <section class="timeline-section" aria-labelledby="pr-comments-heading">
              <div class="section-heading"><h3 id="pr-comments-heading">Comments</h3><small>${comments.length}</small></div>
              ${comments.map((comment) => this.#renderComment(comment, "issue"))}
            </section>
          `}
      ${reviews.length === 0
        ? nothing
        : html`
            <section class="timeline-section" aria-labelledby="pr-reviews-heading">
              <div class="section-heading"><h3 id="pr-reviews-heading">Reviews</h3><small>${reviews.length}</small></div>
              ${reviews.map((review) => this.#renderReview(review, detail))}
            </section>
          `}
      ${threads.length === 0
        ? nothing
        : html`
            <section class="timeline-section" aria-labelledby="pr-threads-heading">
              <div class="section-heading"><h3 id="pr-threads-heading">Review threads</h3><small>${threads.length}</small></div>
              ${threads.map((thread) => this.#renderReviewThread(thread))}
            </section>
          `}
      <section class="timeline-section" aria-labelledby="pr-add-comment-heading">
        <h3 id="pr-add-comment-heading">Add a comment</h3>
        <form @submit=${(event: SubmitEvent) => void this.#addComment(event)}>
          <label>Comment<textarea name="body" required maxlength="65535"></textarea></label>
          <div class="actions"><button class="primary" type="submit" ?disabled=${this.#busy !== ""}>Comment</button></div>
        </form>
      </section>
      <section class="timeline-section" aria-labelledby="pr-submit-review-heading">
        <h3 id="pr-submit-review-heading">Submit a review</h3>
        <form @submit=${(event: SubmitEvent) => void this.#submitReview(event)}>
          <label>Review<textarea name="body" maxlength="65535"></textarea></label>
          <label>Decision<select name="event"><option value="comment">Comment</option><option value="approve">Approve</option><option value="request_changes">Request changes</option></select></label>
          <div class="actions"><button class="primary" type="submit" ?disabled=${this.#busy !== ""}>Submit review</button></div>
        </form>
      </section>
    `;
  }

  #renderEditPullRequest(detail: ProtocolPrDetail): TemplateResult {
    return html`
      <details>
        <summary>Edit title and description</summary>
        <form @submit=${(event: SubmitEvent) => void this.#updatePullRequest(event)}>
          <label>Title<input name="title" required maxlength="256" .value=${detail.info.title} /></label>
          <label>Description<textarea name="body" maxlength="65535" .value=${detail.body ?? ""}></textarea></label>
          <label>Base branch<input name="base" .value=${detail.info.base} /></label>
          <label class="checkbox"><input name="maintainer" type="checkbox" .checked=${detail.maintainer_can_modify === true} />Allow maintainer edits</label>
          <div class="actions"><button class="primary" type="submit" ?disabled=${this.#busy !== ""}>Save</button></div>
        </form>
      </details>
    `;
  }

  #renderComment(comment: PrComment, kind: "issue" | "review"): TemplateResult {
    const editing = this.#editingComment?.id === comment.id;
    const confirmDelete = this.#confirmAction === `delete-comment:${comment.id}`;
    return html`
      <article class="comment-card">
        <header class="comment-header">
          ${comment.author?.avatar_url
            ? html`<img class="avatar" src=${comment.author.avatar_url} alt="" />`
            : fontAwesomeIcon("user")}
          <span><strong>${actorName(comment.author ?? undefined)}</strong> commented ${dateLabel(comment.created_at)}</span>
          <div class="inline-actions">
            ${comment.viewer_can_update === true
              ? html`<button class="icon-button" type="button" title="Edit comment" aria-label="Edit comment" @click=${() => {
                this.#editingComment = { id: comment.id, kind };
                this.requestUpdate();
              }}>${fontAwesomeIcon("pen")}</button>`
              : nothing}
            ${comment.viewer_can_delete === true
              ? html`<button class=${confirmDelete ? "danger" : "icon-button"} type="button" title=${confirmDelete ? "Confirm delete comment" : "Delete comment"} aria-label=${confirmDelete ? "Confirm delete comment" : "Delete comment"} @click=${() => {
                if (confirmDelete) void this.#deleteComment(comment.id, kind);
                else {
                  this.#confirmAction = `delete-comment:${comment.id}`;
                  this.requestUpdate();
                }
              }}>${confirmDelete ? "Delete" : fontAwesomeIcon("trash-can")}</button>`
              : nothing}
          </div>
        </header>
        <div class="comment-body">
          ${editing
            ? html`
                <form @submit=${(event: SubmitEvent) => void this.#updateComment(event, comment.id, kind)}>
                  <textarea name="body" required maxlength="65535" .value=${comment.body}></textarea>
                  <div class="actions"><button type="button" @click=${() => {
                    this.#editingComment = undefined;
                    this.requestUpdate();
                  }}>Cancel</button><button class="primary" type="submit">Save</button></div>
                </form>
              `
            : html`<trouve-markdown-view .content=${comment.body}></trouve-markdown-view>`}
          ${this.#renderReactions(comment.id, comment.reactions ?? [])}
        </div>
      </article>
    `;
  }

  #renderReviewThread(thread: PrReviewThread): TemplateResult {
    const replying = this.#replyingThreadId === thread.id;
    const location = `${thread.path}${thread.line == null ? "" : `:${thread.line}`}`;
    return html`
      <article class=${`review-thread${thread.is_resolved === true ? " resolved" : ""}`}>
        <header class="thread-header">
          ${fontAwesomeIcon(thread.is_resolved === true ? "check" : "comments")}
          <span><strong>${location}</strong>${thread.is_outdated === true ? " · outdated" : ""}</span>
          ${thread.viewer_can_resolve === true && thread.is_resolved !== true
            ? html`<button type="button" @click=${() => void this.#act({ action: "resolve_review_thread", thread_id: thread.id, resolved: true }, "resolve-thread", "Review thread resolved.")}>Resolve</button>`
            : nothing}
          ${thread.viewer_can_unresolve === true && thread.is_resolved === true
            ? html`<button type="button" @click=${() => void this.#act({ action: "resolve_review_thread", thread_id: thread.id, resolved: false }, "reopen-thread", "Review thread reopened.")}>Reopen</button>`
            : nothing}
        </header>
        ${thread.comments?.[0]?.diff_hunk
          ? html`<pre class="thread-copy">${thread.comments[0].diff_hunk}</pre>`
          : nothing}
        <div class="thread-comments">
          ${(thread.comments ?? []).map((comment) => this.#renderComment(comment, "review"))}
          ${thread.viewer_can_reply === true
            ? replying
              ? html`
                  <form @submit=${(event: SubmitEvent) => void this.#replyThread(event, thread.id)}>
                    <textarea name="body" required maxlength="65535"></textarea>
                    <div class="actions"><button type="button" @click=${() => {
                      this.#replyingThreadId = "";
                      this.requestUpdate();
                    }}>Cancel</button><button class="primary" type="submit">Reply</button></div>
                  </form>
                `
              : html`<button type="button" @click=${() => {
                this.#replyingThreadId = thread.id;
                this.requestUpdate();
              }}>Reply</button>`
            : nothing}
        </div>
      </article>
    `;
  }

  #renderReview(review: PrReview, detail: ProtocolPrDetail): TemplateResult {
    const editing = this.#editingReviewId === review.id;
    const dismissing = this.#dismissingReviewId === review.id;
    const confirmDelete = this.#confirmAction === `delete-review:${review.id}`;
    const canDismiss = detail.capabilities.can_update === true
      && review.state !== "pending"
      && review.state !== "dismissed";
    return html`
      <article class="review-card">
        <div class="section-heading">
          <strong>${actorName(review.author ?? undefined)}</strong>
          <span class=${review.state === "approved" ? "tone-ok" : review.state === "changes_requested" ? "tone-error" : ""}>${humanize(review.state)}</span>
          <small>${dateLabel(review.submitted_at)}</small>
          <div class="inline-actions">
            ${review.viewer_can_update === true
              ? html`<button class="icon-button" type="button" title="Edit review" aria-label="Edit review" @click=${() => {
                this.#editingReviewId = review.id;
                this.#dismissingReviewId = "";
                this.requestUpdate();
              }}>${fontAwesomeIcon("pen")}</button>`
              : nothing}
            ${canDismiss
              ? html`<button class="icon-button" type="button" title="Dismiss review" aria-label="Dismiss review" @click=${() => {
                this.#dismissingReviewId = review.id;
                this.#editingReviewId = "";
                this.requestUpdate();
              }}>${fontAwesomeIcon("ban")}</button>`
              : nothing}
            ${review.viewer_can_delete === true
              ? html`<button class=${confirmDelete ? "danger" : "icon-button"} type="button" title=${confirmDelete ? "Confirm delete review" : "Delete review"} aria-label=${confirmDelete ? "Confirm delete review" : "Delete review"} @click=${() => {
                if (confirmDelete) void this.#act({ action: "delete_review", id: review.id }, "delete-review", "Review deleted.");
                else {
                  this.#confirmAction = `delete-review:${review.id}`;
                  this.requestUpdate();
                }
              }}>${confirmDelete ? "Delete" : fontAwesomeIcon("trash-can")}</button>`
              : nothing}
          </div>
        </div>
        ${editing
          ? html`
              <form @submit=${(event: SubmitEvent) => void this.#updateReview(event, review.id)}>
                <textarea name="body" maxlength="65535" .value=${review.body ?? ""}></textarea>
                <div class="actions"><button type="button" @click=${() => {
                  this.#editingReviewId = "";
                  this.requestUpdate();
                }}>Cancel</button><button class="primary" type="submit">Save review</button></div>
              </form>
            `
          : (review.body ?? "").trim() === ""
            ? nothing
            : html`<trouve-markdown-view .content=${review.body ?? ""}></trouve-markdown-view>`}
        ${dismissing
          ? html`
              <form @submit=${(event: SubmitEvent) => void this.#dismissReview(event, review.id)}>
                <label>Dismissal message<textarea name="message" required maxlength="65535"></textarea></label>
                <div class="actions"><button type="button" @click=${() => {
                  this.#dismissingReviewId = "";
                  this.requestUpdate();
                }}>Cancel</button><button class="danger" type="submit">Dismiss review</button></div>
              </form>
            `
          : nothing}
      </article>
    `;
  }

  #renderReactions(subjectId: string, reactions: readonly PrReaction[]): TemplateResult {
    const byContent = new Map(reactions.map((reaction) => [reaction.content, reaction]));
    return html`
      <div class="reaction-row" aria-label="Reactions">
        ${REACTIONS.map(([content, glyph, label]) => {
          const reaction = byContent.get(content);
          return html`
            <button
              class=${reaction?.viewer_has_reacted === true ? "active" : ""}
              type="button"
              title=${label}
              aria-label=${`${label}${reaction?.count === undefined ? "" : `, ${reaction.count}`}`}
              aria-pressed=${reaction?.viewer_has_reacted === true ? "true" : "false"}
              ?disabled=${this.#busy !== ""}
              @click=${() => void this.#act({
                action: reaction?.viewer_has_reacted === true ? "remove_reaction" : "add_reaction",
                subject_id: subjectId,
                content,
              }, `reaction:${subjectId}:${content}`, "Reaction updated.")}
            >${glyph}${reaction?.count === undefined || reaction.count === 0 ? "" : ` ${reaction.count}`}</button>
          `;
        })}
      </div>
    `;
  }

  #renderChecks(detail: ProtocolPrDetail): TemplateResult {
    const summary = checkSummary(detail.info);
    return html`
      <section class="timeline-section">
        <div class="section-heading"><h3>Checks</h3><span class=${summary.tone === "failed" ? "tone-error" : summary.tone === "ready" ? "tone-ok" : "tone-warn"}>${summary.label}</span></div>
        ${detail.info.checks.length === 0
          ? html`<p>No checks have been reported for the current head commit.</p>`
          : html`<ul class="check-list">${detail.info.checks.map((check) => {
              const state = check.conclusion ?? check.status;
              const href = safeSessionPrHref(check.details_url);
              return html`<li class="check-row">
                <span><strong>${check.name}</strong><small>${humanize(state)}${check.started_at == null ? "" : ` · ${dateLabel(check.started_at)}`}</small></span>
                ${href === undefined ? nothing : html`<button class="icon-button" type="button" title="Open check on GitHub" aria-label=${`Open ${check.name} on GitHub`} @click=${() => this.#openExternal(href)}>${fontAwesomeIcon("arrow-up-right-from-square")}</button>`}
              </li>`;
            })}</ul>`}
      </section>
    `;
  }

  #renderCommits(detail: ProtocolPrDetail): TemplateResult {
    const commits = detail.commits ?? [];
    return html`
      <section class="timeline-section">
        <div class="section-heading"><h3>Commits</h3><small>${detail.commit_count}</small></div>
        ${commits.length === 0
          ? html`<p>No commits were returned.</p>`
          : html`<ul class="commit-list">${commits.map((commit) => html`
              <li class="commit-row">
                <span><strong>${commit.message_headline}</strong><small><code>${commit.abbreviated_oid}</code> · ${actorName(commit.author ?? undefined)} · ${dateLabel(commit.committed_at)}</small></span>
                <button class="icon-button" type="button" title="Open commit on GitHub" aria-label=${`Open commit ${commit.abbreviated_oid} on GitHub`} @click=${() => this.#openExternal(commit.url)}>${fontAwesomeIcon("arrow-up-right-from-square")}</button>
              </li>
            `)}</ul>`}
      </section>
    `;
  }

  #renderFiles(detail: ProtocolPrDetail): TemplateResult {
    const files = detail.files ?? [];
    const selected = files.find(({ path }) => path === this.#selectedFilePath) ?? files[0];
    const diff = this.#fileDiff?.path === selected?.path ? this.#fileDiff : undefined;
    const filesHref = safeSessionPrHref(`${detail.info.url.replace(/\/+$/u, "")}/files`);
    return html`
      <section class="timeline-section">
        <div class="section-heading"><h3>Changed files</h3><small>${detail.changed_files}</small></div>
        ${files.length === 0
          ? html`<p>No changed files were returned.</p>`
          : html`
              <div class="pr-file-workspace">
                <div class="pr-file-tree" role="tree" aria-label="Changed files">
                  ${files.map((file) => {
                    const viewed = file.viewer_viewed_state?.toLowerCase() === "viewed";
                    const isSelected = file.path === selected?.path;
                    return html`
                      <div class=${`pr-file-tree-row${isSelected ? " selected" : ""}`}>
                        <button
                          class="pr-file-select"
                          type="button"
                          role="treeitem"
                          aria-selected=${isSelected ? "true" : "false"}
                          title=${file.path}
                          @click=${() => void this.#selectFile(file.path)}
                        >
                          ${fontAwesomeIcon("file-lines")}
                          <span>
                            <strong>${file.path}</strong>
                            <small>${humanize(file.change_type)} · <span class="additions">+${file.additions}</span> <span class="deletions">−${file.deletions}</span></small>
                          </span>
                        </button>
                        <button
                          class=${viewed ? "icon-button tone-ok" : "icon-button"}
                          type="button"
                          title=${viewed ? "Mark file unviewed" : "Mark file viewed"}
                          aria-label=${`${viewed ? "Mark unviewed" : "Mark viewed"}: ${file.path}`}
                          @click=${() => void this.#act({ action: "set_file_viewed", path: file.path, viewed: !viewed }, `viewed:${file.path}`, "File viewed state updated.")}
                        >${fontAwesomeIcon("eye")}</button>
                      </div>
                    `;
                  })}
                </div>
                <div class="pr-file-preview">
                  ${selected === undefined
                    ? html`<div class="pr-file-preview-state" role="status">Select a changed file to view its diff.</div>`
                    : this.#fileDiffLoading
                      ? html`<div class="pr-file-preview-state" role="status">Loading ${selected.path}…</div>`
                      : this.#fileDiffError !== ""
                        ? html`<div class="pr-file-preview-state" role="alert">
                            <strong>File diff unavailable</strong>
                            <p>${this.#fileDiffError}</p>
                            <div class="actions">
                              <span>Retrying automatically.</span>
                              ${filesHref === undefined ? nothing : html`<button class="icon-button" type="button" title="Open files on GitHub" aria-label="Open files on GitHub" @click=${() => this.#openExternal(filesHref)}>${fontAwesomeIcon("arrow-up-right-from-square")}</button>`}
                            </div>
                          </div>`
                        : diff === undefined
                          ? html`<div class="pr-file-preview-state" role="status">Select a changed file to view its diff.</div>`
                          : html`
                              ${diff.notice === undefined || diff.notice === ""
                                ? nothing
                                : html`<p class="pr-file-notice" role="status">${diff.notice}</p>`}
                              ${diff.binary === true || diff.truncated === true
                                ? html`<div class="pr-file-preview-state" role="status">
                                    <strong>${diff.binary === true ? "Binary file" : "Preview unavailable"}</strong>
                                    <p>${diff.notice || "GitHub could not provide a bounded text preview for this file."}</p>
                                    ${filesHref === undefined ? nothing : html`<button class="icon-button" type="button" title="Open files on GitHub" aria-label="Open files on GitHub" @click=${() => this.#openExternal(filesHref)}>${fontAwesomeIcon("arrow-up-right-from-square")}</button>`}
                                  </div>`
                                : html`
                                    <trouve-diff-view
                                      class="pr-file-diff"
                                      .original=${diff.original ?? ""}
                                      .modified=${diff.modified ?? ""}
                                      .mode=${this.#diffMode}
                                      language=${languageForPath(diff.path)}
                                      label=${diff.path}
                                      @trouve-diff-mode-change=${(event: CustomEvent<{ readonly mode: DiffMode }>) => {
                                        this.#diffMode = event.detail.mode;
                                        this.requestUpdate();
                                      }}
                                    ></trouve-diff-view>
                                    <form class="pr-file-review-form" aria-labelledby="pr-new-thread-heading" @submit=${(event: SubmitEvent) => void this.#addReviewThread(event)}>
                                      <h3 id="pr-new-thread-heading">Start a review thread</h3>
                                      <input name="path" type="hidden" .value=${selected.path} />
                                      <div class="form-grid">
                                        <label>Line<input name="line" type="number" min="1" required /></label>
                                        <label>Side<select name="side"><option value="right">Changed file</option><option value="left">Base file</option></select></label>
                                      </div>
                                      <label>Comment<textarea name="body" required maxlength="65535"></textarea></label>
                                      <div class="actions"><button class="primary" type="submit" ?disabled=${this.#busy !== ""}>Start thread</button></div>
                                    </form>
                                  `}
                            `}
                </div>
              </div>
            `}
      </section>
    `;
  }

  #renderSidebar(detail: ProtocolPrDetail): TemplateResult {
    return html`
      ${this.#renderStateControls(detail)}
      ${this.#renderReviewerControls(detail)}
      ${this.#renderMetadataControls(detail)}
      ${this.#renderMergeControls(detail)}
      ${this.#renderStack(detail)}
    `;
  }

  #renderStateControls(detail: ProtocolPrDetail): TemplateResult {
    const state = detail.info.state;
    const subscription = detail.viewer_subscription ?? "unsubscribed";
    return html`
      <details class="side-section" open>
        <summary>State and notifications</summary>
        <div class="side-content">
          <div class="inline-actions">
            ${state === "open" && detail.info.draft && detail.capabilities.can_update === true
              ? html`<button type="button" @click=${() => void this.#act({ action: "set_state", state: "ready" }, "ready", "Pull request marked ready for review.")}>Mark ready</button>`
              : nothing}
            ${state === "open" && !detail.info.draft && detail.capabilities.can_update === true
              ? html`<button type="button" @click=${() => void this.#act({ action: "set_state", state: "draft" }, "draft", "Pull request converted to draft.")}>Convert to draft</button>`
              : nothing}
            ${state === "open" && detail.capabilities.can_close === true
              ? html`<button type="button" @click=${() => void this.#act({ action: "set_state", state: "close" }, "close", "Pull request closed.")}>Close</button>`
              : nothing}
            ${state === "closed" && detail.capabilities.can_reopen === true
              ? html`<button type="button" @click=${() => void this.#act({ action: "set_state", state: "reopen" }, "reopen", "Pull request reopened.")}>Reopen</button>`
              : nothing}
          </div>
          <button type="button" @click=${() => void this.#act({
            action: "set_subscription",
            state: subscription === "subscribed" ? "unsubscribed" : "subscribed",
          }, "subscription", subscription === "subscribed" ? "Notifications disabled." : "Notifications enabled.")}>${subscription === "subscribed" ? "Unsubscribe" : "Subscribe"}</button>
          <form @submit=${(event: SubmitEvent) => void this.#setLock(event, detail)}>
            <label>Conversation<select name="lock"><option value="unlocked" ?selected=${detail.locked !== true}>Unlocked</option><option value="locked" ?selected=${detail.locked === true}>Locked</option></select></label>
            <label>Lock reason<select name="reason"><option value="resolved">Resolved</option><option value="off_topic">Off topic</option><option value="too_heated">Too heated</option><option value="spam">Spam</option></select></label>
            <button type="submit">Apply</button>
          </form>
        </div>
      </details>
    `;
  }

  #renderReviewerControls(detail: ProtocolPrDetail): TemplateResult {
    const requested = detail.review_requests ?? [];
    const reviews = detail.reviews ?? [];
    return html`
      <details class="side-section" open>
        <summary>Reviewers</summary>
        <div class="side-content">
          ${requested.length === 0 ? html`<p class="side-copy">No reviews requested.</p>` : html`
            <ul class="actor-list">${requested.map((actor) => html`
              <li class="actor">
                ${actor.avatar_url ? html`<img class="avatar" src=${actor.avatar_url} alt="" />` : fontAwesomeIcon("user")}
                <span>${actor.login}<small>Review requested</small></span>
                <button class="icon-button" type="button" title=${`Remove ${actor.login}`} aria-label=${`Remove review request for ${actor.login}`} @click=${() => void this.#removeReviewRequest(detail, actor.id)}>${fontAwesomeIcon("xmark")}</button>
              </li>
            `)}</ul>`}
          ${reviews.length === 0 ? nothing : html`
            <ul class="actor-list">${reviews.map((review) => {
              const author = review.author ?? undefined;
              return html`
                <li class="actor">
                  ${author?.avatar_url ? html`<img class="avatar" src=${author.avatar_url} alt="" />` : fontAwesomeIcon("user")}
                  <span>${actorName(author)}<small>${humanize(review.state)}</small></span>
                  ${author?.login
                    ? html`<button class="icon-button" type="button" title=${`Re-request review from ${author.login}`} aria-label=${`Re-request review from ${author.login}`} @click=${() => void this.#act({ action: "request_reviewers", users: [author.login], teams: [], replace: false }, "rerequest-review", `Review requested from ${author.login}.`)}>${fontAwesomeIcon("arrows-rotate")}</button>`
                    : nothing}
                </li>
              `;
            })}</ul>`}
          <form @submit=${(event: SubmitEvent) => void this.#requestReviewers(event)}>
            <label>Users<input name="users" placeholder="octocat, monalisa" autocomplete="off" /></label>
            <label>Bots<input name="bots" placeholder="copilot-pull-request-reviewer" autocomplete="off" /></label>
            <label>Team slugs<input name="teams" placeholder="platform, maintainers" autocomplete="off" /></label>
            <button type="submit">Request review</button>
          </form>
        </div>
      </details>
    `;
  }

  #renderMetadataControls(detail: ProtocolPrDetail): TemplateResult {
    const labels = new Set((detail.labels ?? []).map(({ id }) => id));
    const assignees = new Set((detail.assignees ?? []).map(({ id }) => id));
    return html`
      <details class="side-section">
        <summary>Labels</summary>
        <form class="side-content" @submit=${(event: SubmitEvent) => void this.#setLabels(event)}>
          <div class="choice-list">${(detail.available_labels ?? []).map((label) => html`
            <label><input type="checkbox" name="label" value=${label.id} .checked=${labels.has(label.id)} /><span class="label-dot" style=${`--label-color:#${label.color || "777777"}`}></span>${label.name}</label>
          `)}</div>
          <button type="submit">Apply labels</button>
        </form>
      </details>
      <details class="side-section">
        <summary>Assignees</summary>
        <form class="side-content" @submit=${(event: SubmitEvent) => void this.#setAssignees(event)}>
          <div class="choice-list">${(detail.assignable_users ?? []).map((actor) => html`
            <label><input type="checkbox" name="assignee" value=${actor.id} .checked=${assignees.has(actor.id)} />${actor.login}</label>
          `)}</div>
          <button type="submit">Apply assignees</button>
        </form>
      </details>
      <details class="side-section">
        <summary>Milestone</summary>
        <form class="side-content" @submit=${(event: SubmitEvent) => void this.#setMilestone(event)}>
          <select name="milestone" aria-label="Milestone">
            <option value="">No milestone</option>
            ${(detail.available_milestones ?? []).map((milestone) => html`<option value=${milestone.id} ?selected=${detail.milestone?.id === milestone.id}>${milestone.title}</option>`)}
          </select>
          <button type="submit">Apply milestone</button>
        </form>
      </details>
    `;
  }

  #renderMergeControls(detail: ProtocolPrDetail): TemplateResult {
    const methods = detail.merge_methods ?? [];
    const expectedHead = detail.info.head_sha ?? null;
    const selectedMethod = methods.includes(this.#mergeMethod)
      ? this.#mergeMethod
      : detail.default_merge_method || methods[0] || "merge";
    const confirming = this.#confirmAction === `merge:${detail.info.number}`;
    const queueEntry = detail.merge_queue.entry;
    return html`
      <details class="side-section" open>
        <summary>Merge</summary>
        <div class="side-content">
          <p class="side-copy">${mergeabilitySummary(detail.info).label}</p>
          ${detail.capabilities.can_update_branch === true
            ? html`<button type="button" @click=${() => void this.#act({ action: "update_branch", expected_head_sha: expectedHead }, "update-branch", "Pull request branch updated.")}>Update branch</button>`
            : nothing}
          ${detail.merge_queue.enabled === true
            ? html`<button type="button" @click=${() => void this.#act({ action: "set_merge_queue", enabled: queueEntry == null, expected_head_sha: expectedHead }, "merge-queue", queueEntry == null ? "Pull request added to the merge queue." : "Pull request removed from the merge queue.")}>${queueEntry == null ? "Add to merge queue" : `Leave merge queue · #${queueEntry.position}`}</button>`
            : nothing}
          <label>Merge method<select .value=${selectedMethod} @change=${(event: Event) => {
            this.#mergeMethod = (event.currentTarget as HTMLSelectElement).value;
            this.#confirmAction = "";
            this.requestUpdate();
          }}>${methods.map((method) => html`<option value=${method}>${method === "merge" ? "Merge commit" : method === "squash" ? "Squash and merge" : "Rebase and merge"}</option>`)}</select></label>
          <label>Commit title (optional)<input maxlength="256" .value=${this.#mergeCommitTitle} @input=${(event: Event) => {
            this.#mergeCommitTitle = (event.currentTarget as HTMLInputElement).value;
          }} /></label>
          <label>Commit message (optional)<textarea maxlength="65535" .value=${this.#mergeCommitMessage} @input=${(event: Event) => {
            this.#mergeCommitMessage = (event.currentTarget as HTMLTextAreaElement).value;
          }}></textarea></label>
          ${detail.auto_merge_allowed === true
            ? html`<button type="button" @click=${() => void this.#act({ action: "set_auto_merge", enabled: detail.auto_merge == null, method: selectedMethod, commit_title: this.#mergeCommitTitle, commit_message: this.#mergeCommitMessage }, "auto-merge", detail.auto_merge == null ? "Auto-merge enabled." : "Auto-merge disabled.")}>${detail.auto_merge == null ? "Enable auto-merge" : `Disable auto-merge · ${humanize(detail.auto_merge.method)}`}</button>`
            : nothing}
          <button class="primary" type="button" ?disabled=${methods.length === 0 || !canMergePr(detail.info) || this.#busy !== ""} @click=${() => {
            this.#confirmAction = `merge:${detail.info.number}`;
            this.requestUpdate();
          }}>Merge pull request</button>
          ${confirming ? html`
            <div class="merge-box">
              <strong>Confirm ${humanize(selectedMethod)} merge</strong>
              <span>This updates the remote repository.</span>
              <div class="actions"><button type="button" @click=${() => {
                this.#confirmAction = "";
                this.requestUpdate();
              }}>Cancel</button><button class="danger" type="button" @click=${() => void this.#act({ action: "merge", method: selectedMethod, commit_title: this.#mergeCommitTitle, commit_message: this.#mergeCommitMessage, expected_head_sha: expectedHead }, "merge", `Pull request #${detail.info.number} merged.`)}>Confirm merge</button></div>
            </div>
          ` : nothing}
        </div>
      </details>
    `;
  }

  #renderStack(detail: ProtocolPrDetail): TemplateResult | typeof nothing {
    const stack = detail.stack;
    if (stack == null) return nothing;
    return html`
      <details class="side-section" open>
        <summary>Stack · ${stack.size}</summary>
        <ul class="stack-list">${(stack.entries ?? []).map((entry) => html`
          <li class="stack-row">
            <span><strong>${entry.position}. #${entry.number} ${entry.title}</strong><small>${entry.state}${entry.draft ? " · draft" : ""} · ${entry.head} → ${entry.base}</small></span>
            <button class="icon-button" type="button" title="Open stacked pull request" aria-label=${`Open stacked pull request #${entry.number}`} @click=${() => this.#openExternal(entry.url)}>${fontAwesomeIcon("arrow-up-right-from-square")}</button>
          </li>
        `)}</ul>
      </details>
    `;
  }

  #renderCreate(hasOpenPr: boolean): TemplateResult {
    return html`
      <section id="create-pr-panel" class="settings-card" aria-labelledby="create-pr-title">
        <h3 id="create-pr-title">Create pull request</h3>
        <p>${hasOpenPr
          ? "This session already has an open pull request."
          : "This pushes the session branch and opens a pull request on its configured GitHub remote."}</p>
        <form @submit=${(event: SubmitEvent) => void this.#create(event)}>
          <label>Title<input name="title" required maxlength="200" autocomplete="off" .value=${this.sessionTitle} ?disabled=${hasOpenPr || this.#busy !== ""} /></label>
          <label>Description<textarea name="body" maxlength="65535" ?disabled=${hasOpenPr || this.#busy !== ""}></textarea></label>
          <div class="form-grid">
            <label>Base branch (optional)<input name="base" autocomplete="off" spellcheck="false" placeholder="Repository default" ?disabled=${hasOpenPr || this.#busy !== ""} /></label>
            <label class="checkbox"><input name="draft" type="checkbox" ?disabled=${hasOpenPr || this.#busy !== ""} />Open as draft</label>
          </div>
          <div class="actions"><button type="button" @click=${() => {
            this.#createOpen = false;
            this.requestUpdate();
          }}>Cancel</button><button class="primary" type="submit" ?disabled=${hasOpenPr || this.#busy !== ""}>${this.#busy === "create" ? "Creating…" : "Push branch and create"}</button></div>
        </form>
      </section>
    `;
  }

  #currentPullRequests(): readonly ProtocolPrInfo[] {
    const projected = this.#store.value?.sessionPullRequests(this.#effectiveSessionId);
    if (projected === undefined) return this.#prs;
    return projected.length > 0 || this.#prs.length === 0 ? projected : this.#prs;
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "") return;
    const generation = ++this.#loadGeneration;
    if (this.#loadRetryTimer !== undefined) {
      globalThis.clearTimeout(this.#loadRetryTimer);
      this.#loadRetryTimer = undefined;
    }
    this.#loading = true;
    this.requestUpdate();
    try {
      const integration = await services.protocol.githubIntegration();
      if (!this.#loadIsCurrent(generation, sessionId)) return;
      this.#githubConfigured = githubIntegrationConfigured(integration);
      if (!this.#githubConfigured) {
        this.#clearPrStateForSetup();
        return;
      }
      this.#repositorySetupRequired = false;
      if (!this.#loadIsCurrent(generation, sessionId)) return;
      this.#loadError = false;
      if (this.#noticeIsError) this.#setNotice("", false);
      this.#syncProjectedSelection();
    } catch (cause) {
      if (!this.#loadIsCurrent(generation, sessionId)) return;
      if (this.#githubConfigured === true && cause instanceof ProtocolClientError && cause.status === 400) {
        this.#repositorySetupRequired = true;
        this.#clearPrStateForSetup();
      } else {
        this.#loadError = true;
        this.#setNotice("Pull requests could not be loaded. trouve will retry automatically.", true);
        this.#scheduleLoadRetry();
      }
    } finally {
      if (this.#loadIsCurrent(generation, sessionId)) {
        this.#loading = false;
        this.requestUpdate();
      }
    }
  }

  #syncProjectedSelection(): void {
    const prs = this.#currentPullRequests();
    const selected = prs.find((pr) => pr.number === this.#selectedPrNumber) ?? prs[0];
    const key = prProjectionKey(selected);
    if (key === this.#loadedPrProjectionKey) return;
    this.#selectedPrNumber = selected?.number;
    this.#loadedPrProjectionKey = key;
    this.#detailGeneration += 1;
    this.#detail = undefined;
    this.#loadedDetailSections.clear();
    this.#selectedFilePath = "";
    this.#fileDiff = undefined;
    this.#fileDiffError = "";
    this.requestUpdate();
    if (selected !== undefined) {
      void this.#loadDetail(selected.number, detailSectionForTab(this.#activeTab));
    }
  }

  async #loadDetail(
    number: number,
    section: ProtocolPrDetailSection,
  ): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "") return;
    const generation = ++this.#detailGeneration;
    if (this.#detailRetryTimer !== undefined) {
      globalThis.clearTimeout(this.#detailRetryTimer);
      this.#detailRetryTimer = undefined;
    }
    this.#detailLoading = true;
    this.requestUpdate();
    try {
      const detail = await services.protocol.sessionPrDetail(sessionId, number, section);
      if (generation !== this.#detailGeneration || sessionId !== this.#effectiveSessionId || number !== this.#selectedPrNumber) return;
      this.#detail = mergePrDetailSection(this.#detail, detail, section);
      this.#loadedDetailSections.add("overview");
      this.#loadedDetailSections.add(section);
      const files = detail.files ?? [];
      const selectedFile = files.find(({ path }) => path === this.#selectedFilePath) ?? files[0];
      this.#selectedFilePath = selectedFile?.path ?? "";
      if (section === "files" && this.#activeTab === "files" && selectedFile !== undefined) {
        void this.#selectFile(selectedFile.path);
      }
      const methods = detail.merge_methods ?? [];
      if (!methods.includes(this.#mergeMethod)) {
        this.#mergeMethod = detail.default_merge_method || methods[0] || "merge";
      }
    } catch (cause) {
      if (generation !== this.#detailGeneration || sessionId !== this.#effectiveSessionId) return;
      this.#detail = undefined;
      this.#setNotice(cause instanceof Error ? cause.message : "Pull request detail could not be loaded.", true);
      this.#scheduleDetailRetry(number, section);
    } finally {
      if (generation === this.#detailGeneration && sessionId === this.#effectiveSessionId) {
        this.#detailLoading = false;
        this.requestUpdate();
      }
    }
  }

  async #selectPr(number: number): Promise<void> {
    if (!Number.isSafeInteger(number) || number <= 0) return;
    this.#selectedPrNumber = number;
    this.#loadedPrProjectionKey = prProjectionKey(
      this.#currentPullRequests().find((pr) => pr.number === number),
    );
    this.#detail = undefined;
    this.#loadedDetailSections.clear();
    this.#confirmAction = "";
    this.#mergeCommitTitle = "";
    this.#mergeCommitMessage = "";
    this.#editingComment = undefined;
    this.#editingReviewId = "";
    this.#dismissingReviewId = "";
    this.#replyingThreadId = "";
    this.#fileDiffGeneration += 1;
    this.#selectedFilePath = "";
    this.#fileDiff = undefined;
    this.#fileDiffLoading = false;
    this.#fileDiffError = "";
    this.requestUpdate();
    await this.#loadDetail(number, detailSectionForTab(this.#activeTab));
  }

  async #act(
    action: ProtocolPrActionRequest,
    busy: string,
    success: string,
  ): Promise<boolean> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const number = this.#selectedPrNumber;
    if (services === undefined || sessionId === "" || number === undefined || this.#busy !== "") {
      return false;
    }
    this.#busy = busy;
    this.#setNotice("Updating pull request…", false);
    try {
      const detail = await services.protocol.actOnSessionPr(sessionId, number, action);
      if (sessionId !== this.#effectiveSessionId || number !== this.#selectedPrNumber) {
        return false;
      }
      this.#detail = detail;
      const updatedPrs = this.#currentPullRequests().map((pr) =>
        pr.number === detail.info.number ? detail.info : pr
      );
      this.#prs = updatedPrs;
      this.#store.value?.replaceSessionPullRequests(sessionId, updatedPrs);
      this.#loadedPrProjectionKey = prProjectionKey(detail.info);
      if (action.action === "update_branch" && this.#selectedFilePath !== "") {
        this.#fileDiff = undefined;
        void this.#loadFileDiff(this.#selectedFilePath);
      }
      this.#confirmAction = "";
      this.#editingComment = undefined;
      this.#editingReviewId = "";
      this.#dismissingReviewId = "";
      this.#replyingThreadId = "";
      this.#setNotice(success, false);
      return true;
    } catch (cause) {
      this.#setNotice(cause instanceof Error ? cause.message : "GitHub rejected the pull request action.", true);
      return false;
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #create(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const form = event.currentTarget as HTMLFormElement;
    if (services === undefined || sessionId === "" || this.#busy !== "" || this.#currentPullRequests().some((pr) => pr.state === "open")) return;
    let request: ProtocolCreatePrRequest;
    try {
      request = createPrRequest({
        title: formInput(form, "title"),
        body: formInput(form, "body"),
        base: formInput(form, "base"),
        draft: formChecked(form, "draft"),
      });
    } catch {
      this.#setNotice("A pull-request title is required.", true);
      return;
    }
    this.#busy = "create";
    this.#setNotice("Pushing the branch and creating the pull request…", false);
    try {
      const created = await services.protocol.createSessionPr(sessionId, request);
      if (sessionId !== this.#effectiveSessionId) return;
      const prs = [created, ...this.#currentPullRequests().filter((pr) =>
        pr.number !== created.number
      )];
      this.#prs = prs;
      this.#store.value?.replaceSessionPullRequests(sessionId, prs);
      form.reset();
      this.#createOpen = false;
      this.#setNotice("Pull request created.", false);
      await this.#selectPr(created.number);
    } catch (cause) {
      this.#setNotice(cause instanceof Error ? cause.message : "The pull request could not be created.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #updatePullRequest(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    await this.#act({
      action: "update",
      title: formInput(form, "title").trim(),
      body: formInput(form, "body"),
      base: formInput(form, "base").trim(),
      maintainer_can_modify: formChecked(form, "maintainer"),
    }, "update", "Pull request updated.");
  }

  async #addComment(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const body = formInput(form, "body").trim();
    if (body === "") return;
    if (await this.#act({ action: "add_comment", body }, "add-comment", "Comment added.")) {
      form.reset();
    }
  }

  async #updateComment(event: SubmitEvent, id: string, kind: "issue" | "review"): Promise<void> {
    event.preventDefault();
    const body = formInput(event.currentTarget as HTMLFormElement, "body").trim();
    if (body === "") return;
    await this.#act({ action: "update_comment", id, kind, body }, "update-comment", "Comment updated.");
  }

  async #deleteComment(id: string, kind: "issue" | "review"): Promise<void> {
    await this.#act({ action: "delete_comment", id, kind }, "delete-comment", "Comment deleted.");
  }

  async #updateReview(event: SubmitEvent, id: string): Promise<void> {
    event.preventDefault();
    const body = formInput(event.currentTarget as HTMLFormElement, "body");
    await this.#act({ action: "update_review", id, body }, "update-review", "Review updated.");
  }

  async #dismissReview(event: SubmitEvent, id: string): Promise<void> {
    event.preventDefault();
    const message = formInput(event.currentTarget as HTMLFormElement, "message").trim();
    if (message === "") return;
    await this.#act({ action: "dismiss_review", id, message }, "dismiss-review", "Review dismissed.");
  }

  async #replyThread(event: SubmitEvent, threadId: string): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const body = formInput(form, "body").trim();
    if (body === "") return;
    if (await this.#act({ action: "reply_review_thread", thread_id: threadId, body }, "reply-thread", "Reply added.")) {
      form.reset();
    }
  }

  async #submitReview(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    if (await this.#act({
      action: "submit_review",
      event: formInput(form, "event"),
      body: formInput(form, "body").trim(),
    }, "submit-review", "Review submitted.")) {
      form.reset();
    }
  }

  async #addReviewThread(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const line = Number(formInput(form, "line"));
    if (!Number.isSafeInteger(line) || line < 1) return;
    if (await this.#act({
      action: "add_review_thread",
      body: formInput(form, "body").trim(),
      path: formInput(form, "path"),
      line,
      side: formInput(form, "side"),
    }, "add-review-thread", "Review thread added.")) {
      form.reset();
    }
  }

  async #requestReviewers(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const users = splitLogins(formInput(form, "users"));
    const bots = splitLogins(formInput(form, "bots"));
    const teams = splitLogins(formInput(form, "teams"));
    if (users.length === 0 && bots.length === 0 && teams.length === 0) {
      this.#setNotice("Enter at least one user or team.", true);
      return;
    }
    if (await this.#act({ action: "request_reviewers", users, bots, teams, replace: false }, "request-reviewers", "Reviewers requested.")) {
      form.reset();
    }
  }

  async #removeReviewRequest(detail: ProtocolPrDetail, actorId: string): Promise<void> {
    const remaining = (detail.review_requests ?? []).filter(({ id }) => id !== actorId);
    await this.#act({
      action: "request_reviewers",
      users: remaining.filter(({ kind }) => kind !== "team" && kind !== "bot").map(({ login }) => login),
      bots: remaining.filter(({ kind }) => kind === "bot").map(({ login }) => login),
      teams: remaining.filter(({ kind }) => kind === "team").map(({ login }) => login),
      replace: true,
    }, "remove-reviewer", "Review request removed.");
  }

  async #setLabels(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    await this.#act({ action: "set_labels", label_ids: checkedValues(event.currentTarget as HTMLFormElement, "label") }, "labels", "Labels updated.");
  }

  async #setAssignees(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    await this.#act({ action: "set_assignees", assignee_ids: checkedValues(event.currentTarget as HTMLFormElement, "assignee") }, "assignees", "Assignees updated.");
  }

  async #setMilestone(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const value = formInput(event.currentTarget as HTMLFormElement, "milestone");
    await this.#act({ action: "set_milestone", milestone_id: value === "" ? null : value }, "milestone", "Milestone updated.");
  }

  async #setLock(event: SubmitEvent, detail: ProtocolPrDetail): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const locked = formInput(form, "lock") === "locked";
    if (locked === detail.locked) return;
    await this.#act({ action: "set_lock", locked, reason: locked ? formInput(form, "reason") : null }, "lock", locked ? "Conversation locked." : "Conversation unlocked.");
  }

  async #copyUrl(url: string): Promise<void> {
    try {
      await globalThis.navigator.clipboard.writeText(url);
      this.#setNotice("Pull request URL copied.", false);
    } catch {
      this.#setNotice("The pull request URL could not be copied.", true);
    }
  }

  #openExternal(value: string): void {
    const href = safeSessionPrHref(value);
    if (href === undefined) {
      this.#setNotice("The GitHub link was rejected because it is not a safe HTTPS URL.", true);
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

  #scheduleLoadRetry(): void {
    if (this.#loadRetryTimer !== undefined) return;
    this.#loadRetryTimer = globalThis.setTimeout(() => {
      this.#loadRetryTimer = undefined;
      if (!this.isConnected) return;
      if (globalThis.document?.visibilityState === "hidden") {
        this.#scheduleLoadRetry();
        return;
      }
      void this.#load();
    }, AUTOMATIC_RETRY_MS);
  }

  #scheduleDetailRetry(number: number, section: ProtocolPrDetailSection): void {
    if (this.#detailRetryTimer !== undefined) return;
    this.#detailRetryTimer = globalThis.setTimeout(() => {
      this.#detailRetryTimer = undefined;
      if (!this.isConnected || number !== this.#selectedPrNumber) return;
      if (globalThis.document?.visibilityState === "hidden") {
        this.#scheduleDetailRetry(number, section);
        return;
      }
      void this.#loadDetail(number, section);
    }, AUTOMATIC_RETRY_MS);
  }

  #scheduleFileDiffRetry(path: string): void {
    if (this.#fileDiffRetryTimer !== undefined) return;
    this.#fileDiffRetryTimer = globalThis.setTimeout(() => {
      this.#fileDiffRetryTimer = undefined;
      if (!this.isConnected || path !== this.#selectedFilePath) return;
      if (globalThis.document?.visibilityState === "hidden") {
        this.#scheduleFileDiffRetry(path);
        return;
      }
      void this.#loadFileDiff(path);
    }, AUTOMATIC_RETRY_MS);
  }

  #clearRetryTimers(): void {
    for (const timer of [
      this.#loadRetryTimer,
      this.#detailRetryTimer,
      this.#fileDiffRetryTimer,
    ]) {
      if (timer !== undefined) globalThis.clearTimeout(timer);
    }
    this.#loadRetryTimer = undefined;
    this.#detailRetryTimer = undefined;
    this.#fileDiffRetryTimer = undefined;
  }

  #loadIsCurrent(generation: number, sessionId: string): boolean {
    return generation === this.#loadGeneration
      && this.isConnected
      && sessionId === this.#effectiveSessionId;
  }

  #resetSessionState(): void {
    this.#clearRetryTimers();
    this.#fileDiffGeneration += 1;
    this.#prs = [];
    this.#detail = undefined;
    this.#selectedPrNumber = undefined;
    this.#loadedPrProjectionKey = "";
    this.#loadedDetailSections.clear();
    this.#createOpen = false;
    this.#activeTab = "conversation";
    this.#confirmAction = "";
    this.#mergeCommitTitle = "";
    this.#mergeCommitMessage = "";
    this.#editingComment = undefined;
    this.#editingReviewId = "";
    this.#dismissingReviewId = "";
    this.#replyingThreadId = "";
    this.#selectedFilePath = "";
    this.#fileDiff = undefined;
    this.#fileDiffLoading = false;
    this.#fileDiffError = "";
    this.#notice = "";
    this.#noticeIsError = false;
    this.#loadError = false;
    this.#githubConfigured = undefined;
    this.#repositorySetupRequired = false;
  }

  #clearPrStateForSetup(): void {
    this.#fileDiffGeneration += 1;
    this.#prs = [];
    this.#detail = undefined;
    this.#loadedPrProjectionKey = "";
    this.#loadedDetailSections.clear();
    this.#selectedFilePath = "";
    this.#fileDiff = undefined;
    this.#fileDiffLoading = false;
    this.#fileDiffError = "";
    const sessionId = this.#effectiveSessionId;
    if (sessionId !== "") this.#store.value?.clearSessionPullRequests(sessionId);
    this.#selectedPrNumber = undefined;
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
