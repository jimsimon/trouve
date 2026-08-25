import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { appServicesContext } from "../contexts/app-contexts.js";
import type {
  ProtocolCodeReviewDashboard,
  ProtocolCodeReviewJob,
  ProtocolCodeReviewSettings,
} from "../services/protocol-client.js";
import {
  canCancelCodeReviewJob,
  canRetryCodeReviewJob,
  CODE_REVIEW_STATUS_FILTERS,
  codeReviewSettingsDraft,
  codeReviewSettingsRequest,
  codeReviewNeedsAttention,
  codeReviewStatusClass,
  codeReviewStatusLabel,
  groupCodeReviewJobs,
  MAX_PARALLEL_REVIEWS,
  moveReviewGroup,
  orderReviewJobGroups,
  reconcileReviewGroupOrder,
  reorderReviewGroup,
  reviewGroupRepositoryKeys,
  safeCodeReviewHref,
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  type CodeReviewJobAction,
  type CodeReviewSettingsDraft,
  type CodeReviewStatusFilter,
} from "./code-review-dashboard-model.js";
import {
  createBrowserCodeReviewGroupOrderStorage,
  type CodeReviewGroupOrderStorage,
} from "./code-review-group-order-preferences.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import "./code-review-configuration.js";

const CODE_REVIEW_REFRESH_MS = 30_000;
const CODE_REVIEW_RETRY_MS = 5_000;
const RETRY_WHOLE_REVIEW_TITLE = "Starts a replacement review with current repository settings";

interface PendingJobAction {
  readonly action: CodeReviewJobAction;
  readonly jobId: string;
}

type GroupOrderControl = "grip" | "up" | "down";
const REVIEW_GROUP_DRAG_TYPE = "application/x-trouve-review-group";

const emptySettingsDraft = (): CodeReviewSettingsDraft => ({
  maxParallel: "",
  totalMinutes: "",
  reviewerMinutes: "",
  coordinatorMinutes: "",
});

const errorMessage = (cause: unknown, fallback: string): string =>
  cause instanceof Error && cause.message.trim() !== "" ? cause.message : fallback;

const formatDate = (value: string | null | undefined): string => {
  if (value === undefined || value === null || value === "") return "Not yet";
  const date = new Date(value);
  return Number.isFinite(date.getTime())
    ? new Intl.DateTimeFormat(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      }).format(date)
    : "Unknown time";
};

const formatDuration = (milliseconds: number | undefined): string => {
  if (milliseconds === undefined || !Number.isFinite(milliseconds) || milliseconds < 0) {
    return "—";
  }
  const seconds = Math.floor(milliseconds / 1_000);
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  const remainder = seconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${remainder}s`;
  return `${remainder}s`;
};

export class TrouveCodeReviewDashboard extends LitElement {
  static override styles = css`
    :host {
      display: block;
      min-width: 0;
      min-height: 0;
      height: 100%;
      overflow: auto;
      color: var(--trouve-text);
      background: var(--trouve-panel-bg);
      font: var(--trouve-font-size) / 1.45 var(--trouve-font-sans);
    }

    * { box-sizing: border-box; }
    button, input, select { font: inherit; }
    button { color: inherit; }
    button:focus-visible, input:focus-visible, select:focus-visible, a:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    button:disabled, input:disabled, select:disabled { cursor: not-allowed; opacity: 0.56; }

    .dashboard {
      width: min(1180px, 100%);
      min-height: 100%;
      margin-inline: auto;
      padding: clamp(18px, 4vw, 42px);
    }
    trouve-code-review-configuration { display: block; margin-top: 14px; }

    .page-header, .panel-header, .group-header, .job-heading, .actions, .health-heading {
      display: flex;
      align-items: center;
      gap: 9px;
    }

    .page-header { align-items: flex-start; margin-bottom: 18px; }
    .page-header > div:first-child, .panel-header > div:first-child, .job-heading > div {
      min-width: 0;
      flex: 1;
    }
    .eyebrow {
      margin: 0 0 3px;
      color: var(--trouve-text-dim);
      font-size: 10px;
      font-weight: 650;
      letter-spacing: 0.06em;
      text-transform: uppercase;
    }
    h1, h2, h3, h4, p { margin-block-start: 0; }
    h1 { margin-block-end: 4px; color: var(--trouve-text-hi); font-size: 22px; }
    h2 { margin-block-end: 3px; color: var(--trouve-text-hi); font-size: 14px; }
    h3 { margin: 0; color: var(--trouve-text-hi); font-size: 12px; }
    h4 {
      min-width: 0;
      margin: 0;
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 12px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .page-header p, .panel-header p, .muted { margin-block-end: 0; color: var(--trouve-text-dim); }

    button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 10px;
      background: var(--trouve-control-bg);
      cursor: pointer;
    }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary { border-color: var(--trouve-accent); color: var(--trouve-accent-fg, white); background: var(--trouve-accent); }
    button.danger { color: var(--trouve-err); }
    button.compact { min-height: 26px; padding: 2px 7px; font-size: 10px; }

    .panel {
      margin-top: 16px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
      overflow: hidden;
    }
    .panel-header { align-items: flex-start; padding: 12px 14px; border-bottom: 1px solid var(--trouve-rule); }
    .panel-header p { font-size: 11px; }

    .overview-grid {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 10px;
      margin-top: 16px;
    }
    .overview-card {
      min-width: 0;
      padding: 12px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .health-heading { align-items: flex-start; }
    .health-dot { flex: none; width: 8px; height: 8px; margin-top: 5px; border-radius: 50%; background: var(--trouve-text-faint); }
    .health-dot.ok { background: var(--trouve-ok); }
    .health-dot.warn { background: var(--trouve-warn); }
    .health-dot.bad { background: var(--trouve-err); }
    .overview-card > p { margin: 5px 0 0; color: var(--trouve-text-dim); font-size: 10px; }
    .overview-card dl {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 5px 8px;
      margin: 10px 0 0;
    }
    .overview-card dt { color: var(--trouve-text-dim); font-size: 10px; }
    .overview-card dd { margin: 0; color: var(--trouve-text-hi); font-size: 10px; text-align: end; }
    .health-error { color: var(--trouve-err) !important; overflow-wrap: anywhere; }
    .roster {
      display: flex;
      flex-wrap: wrap;
      gap: 4px;
      max-height: 82px;
      margin: 9px 0 0;
      padding: 0;
      overflow: auto;
      list-style: none;
    }
    .roster li {
      max-width: 100%;
      padding: 2px 6px;
      overflow: hidden;
      border: 1px solid var(--trouve-border);
      border-radius: 999px;
      color: var(--trouve-text-mid);
      background: var(--trouve-control-bg);
      font-size: 10px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .filter { display: flex; align-items: center; gap: 6px; color: var(--trouve-text-dim); font-size: 10px; }
    select, input {
      min-width: 0;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 7px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
    }
    .job-groups { display: grid; gap: 14px; padding: 14px; }
    .review-job-group { position: relative; min-width: 0; border-radius: var(--trouve-radius-sm); }
    .review-group-drop-placeholder { min-height: 42px; border: 1px dashed var(--trouve-accent); border-radius: var(--trouve-radius-sm); background: var(--trouve-accent-veil); }
    .group-header { margin-bottom: 6px; }
    .group-header h3 { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .group-count { color: var(--trouve-text-dim); font-size: 10px; white-space: nowrap; }
    .group-order-controls { display: flex; flex: none; align-items: center; gap: 3px; }
    .group-order-controls button { width: 28px; min-height: 28px; padding: 0; color: var(--trouve-text-dim); font-family: var(--trouve-font-mono); }
    .group-order-controls button:hover:not(:disabled), .group-order-controls button:focus-visible { color: var(--trouve-text-hi); }
    .group-grip { cursor: grab; }
    .group-grip:active { cursor: grabbing; }
    .job-list { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 7px; }
    .job-card {
      min-width: 0;
      padding: 9px 10px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-panel-bg);
    }
    .status {
      flex: none;
      border: 1px solid var(--trouve-border);
      border-radius: 999px;
      padding: 1px 6px;
      color: var(--trouve-text-dim);
      background: var(--trouve-control-bg);
      font-size: 9px;
      line-height: 1.6;
    }
    .status.running { border-color: var(--trouve-accent); color: var(--trouve-accent); background: var(--trouve-accent-bg); }
    .status.queued { border-color: var(--trouve-warn); color: var(--trouve-warn); }
    .status.succeeded { border-color: var(--trouve-ok); color: var(--trouve-ok); }
    .status.failed { border-color: var(--trouve-err); color: var(--trouve-err); }
    .status.cancelled, .status.stale { color: var(--trouve-text-mid); }
    .job-subtitle { margin: 2px 0 0; overflow: hidden; color: var(--trouve-text-dim); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
    .job-meta {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 6px;
      margin: 8px 0 0;
    }
    .job-meta div { min-width: 0; }
    .job-meta dt { overflow: hidden; color: var(--trouve-text-dim); font-size: 8px; letter-spacing: 0.04em; text-overflow: ellipsis; text-transform: uppercase; white-space: nowrap; }
    .job-meta dd { margin: 2px 0 0; overflow: hidden; color: var(--trouve-text-hi); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
    .progress-track { height: 4px; margin-top: 8px; overflow: hidden; border-radius: 999px; background: var(--trouve-rule); }
    .progress-track > span { display: block; height: 100%; background: var(--trouve-accent); }
    .job-error { margin: 7px 0 0; color: var(--trouve-err); font-size: 10px; overflow-wrap: anywhere; }
    .job-footer { display: flex; align-items: center; gap: 7px; margin-top: 8px; padding-top: 7px; border-top: 1px solid var(--trouve-rule); }
    .job-links { min-width: 0; flex: 1; display: flex; flex-wrap: wrap; gap: 4px 9px; }
    a { color: var(--trouve-accent); font-size: 10px; text-decoration: none; }
    a:hover { text-decoration: underline; }
    .actions { flex: none; gap: 4px; }

    .confirmation {
      margin-top: 8px;
      padding: 8px;
      border: 1px solid var(--trouve-warn);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text-mid);
      background: var(--trouve-warn-bg);
      font-size: 10px;
    }
    .confirmation p { margin: 0 0 7px; }
    .confirmation .actions { justify-content: flex-end; }

    .settings-body { padding: 14px; }
    .settings-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 11px; }
    .settings-grid label { min-width: 0; color: var(--trouve-text-hi); font-size: 11px; font-weight: 600; }
    .settings-grid input { display: block; width: 100%; margin-top: 5px; }
    .settings-grid small { display: block; margin-top: 3px; color: var(--trouve-text-dim); font-size: 9px; font-weight: 400; }
    .settings-actions { display: flex; justify-content: flex-end; align-items: center; gap: 8px; margin-top: 12px; }
    .settings-error { min-width: 0; flex: 1; margin: 0; color: var(--trouve-err); font-size: 10px; }

    .banner, .empty-state {
      display: grid;
      justify-items: center;
      align-content: center;
      gap: 7px;
      min-height: 190px;
      padding: 24px;
      color: var(--trouve-text-dim);
      text-align: center;
    }
    .banner {
      min-height: 0;
      grid-template-columns: minmax(0, 1fr) auto;
      justify-items: start;
      margin: 12px 0;
      padding: 9px 11px;
      border: 1px solid var(--trouve-err);
      border-radius: var(--trouve-radius);
      color: var(--trouve-err);
      text-align: start;
    }
    .empty-state strong { color: var(--trouve-text-hi); font-size: 13px; }
    .empty-state p { max-width: 38rem; margin: 0; }
    .panel .empty-state { min-height: 150px; }

    .visually-hidden {
      position: absolute;
      width: 1px;
      height: 1px;
      margin: -1px;
      padding: 0;
      overflow: hidden;
      clip: rect(0 0 0 0);
      clip-path: inset(50%);
      border: 0;
      white-space: nowrap;
    }

    @media (max-width: 840px) {
      .overview-grid { grid-template-columns: 1fr; }
      .job-list { grid-template-columns: 1fr; }
    }

    @media (max-width: 600px) {
      .dashboard { padding: 14px; }
      .page-header, .panel-header { flex-wrap: wrap; }
      .page-header .actions, .panel-header .filter { width: 100%; }
      .panel-header .filter select { flex: 1; }
      .settings-grid { grid-template-columns: 1fr; }
      .job-footer { align-items: flex-start; flex-direction: column; }
      .job-footer .actions { align-self: stretch; justify-content: flex-end; }
      .group-header { flex-wrap: wrap; }
      .group-header h3 { flex-basis: calc(100% - 90px); }
      .group-order-controls { margin-inline-start: auto; }
      .group-order-controls button { width: 36px; min-height: 36px; }
    }

    @media (any-pointer: coarse) {
      .group-order-controls button { width: 42px; min-height: 42px; }
    }

    @media (prefers-reduced-motion: reduce) {
      *, *::before, *::after { scroll-behavior: auto !important; transition: none !important; }
    }
  `;

  #dashboard: ProtocolCodeReviewDashboard | undefined;
  #settings: ProtocolCodeReviewSettings | undefined;
  #settingsDraft = emptySettingsDraft();
  #loading = true;
  #refreshing = false;
  #savingSettings = false;
  #filter: CodeReviewStatusFilter = "all";
  #error = "";
  #settingsError = "";
  #liveStatus = "";
  #busyJobId = "";
  #pendingAction: PendingJobAction | undefined;
  readonly #groupOrderStorage: CodeReviewGroupOrderStorage | undefined;
  #groupOrder: readonly string[] = [];
  #customGroupOrder = false;
  #draggedGroup = "";
  #dropTarget = "";
  #dropAfter = false;
  #loadedServices: object | undefined;
  #autoRefreshTimer: ReturnType<typeof setTimeout> | undefined;

  constructor() {
    super();
    this.#groupOrderStorage = createBrowserCodeReviewGroupOrderStorage();
    const savedOrder = this.#groupOrderStorage?.load();
    if (savedOrder !== undefined) {
      this.#groupOrder = savedOrder;
      this.#customGroupOrder = true;
    }
  }

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });

  protected override firstUpdated(): void {
    this.#loadForCurrentServices();
  }

  protected override updated(): void {
    this.#loadForCurrentServices();
  }

  override disconnectedCallback(): void {
    this.#clearAutoRefresh();
    super.disconnectedCallback();
  }

  #loadForCurrentServices(): void {
    const services = this.#services.value;
    if (services === undefined) {
      this.#loadedServices = undefined;
      return;
    }
    if (services === this.#loadedServices) return;
    this.#loadedServices = services;
    void this.#loadInitial();
  }

  override render() {
    const dashboard = this.#dashboard;
    return html`
      <main
        class="dashboard"
        aria-labelledby="code-review-title"
        aria-busy=${this.#loading || this.#refreshing}
      >
        <header class="page-header">
          <div>
            <p class="eyebrow">Pull requests</p>
            <h1 id="code-review-title">Code reviews</h1>
            <p>Monitor automated reviews without leaving the main trouve workspace.</p>
          </div>
        </header>

        <p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
          ${this.#liveStatus}
        </p>

        ${this.#error !== "" && dashboard !== undefined
          ? html`<div class="banner" role="alert"><span>${this.#error}</span><span>Retrying automatically.</span></div>`
          : nothing}

        ${dashboard === undefined
          ? this.#renderUnavailable()
          : html`
              ${this.#renderOverview(dashboard)}
              ${this.#renderJobs(dashboard)}
              ${this.#renderSettings()}
              <trouve-code-review-configuration
                @trouve-code-review-configuration-changed=${() => void this.#loadInitial()}
              ></trouve-code-review-configuration>
            `}
      </main>
    `;
  }

  #renderUnavailable() {
    if (this.#loading) {
      return html`<div class="empty-state" role="status"><strong>Loading review operations…</strong><p>Reading GitHub App health, repositories, and recent jobs.</p></div>`;
    }
    return html`
      <div class="empty-state" role="alert">
        <strong>Unable to load code reviews</strong>
        <p>${this.#error || "The review dashboard request failed."}</p>
        <p>Retrying automatically.</p>
      </div>
    `;
  }

  #renderOverview(dashboard: ProtocolCodeReviewDashboard) {
    const app = dashboard.app;
    const activeJobs = dashboard.jobs.filter((job) => canCancelCodeReviewJob(job.status)).length;
    const configuredRepositories = dashboard.repositories.filter(
      (repository) => (repository.mode ?? "off") !== "off",
    ).length;
    const appHealthClass = !app.configured ? "warn" : app.last_error ? "bad" : "ok";
    const appHealthLabel = !app.configured
      ? "GitHub App not configured"
      : app.last_error
        ? "GitHub App needs attention"
        : "GitHub App configured";

    return html`
      <section class="overview-grid" aria-label="Review operations overview">
        <article class="overview-card">
          <div class="health-heading">
            <span class="health-dot ${appHealthClass}" aria-hidden="true"></span>
            <h2>${appHealthLabel}</h2>
          </div>
          <p>Read-only application and polling health.</p>
          <dl>
            <dt>Installations</dt><dd>${app.installation_count ?? 0}</dd>
            <dt>Last poll</dt><dd>${formatDate(app.last_poll_at)}</dd>
            <dt>Rate limit</dt><dd>${app.rate_limit_remaining ?? "Unknown"}</dd>
            <dt>Checks write</dt><dd>${app.checks_write_configured === true ? "Ready" : "Unavailable"}</dd>
          </dl>
          ${app.last_error ? html`<p class="health-error">${app.last_error}</p>` : nothing}
        </article>

        <article class="overview-card">
          <h2>Repositories</h2>
          <p>${configuredRepositories} of ${dashboard.repositories.length} enabled for reviews.</p>
          <ul class="roster" aria-label="Configured review repositories">
            ${dashboard.repositories.length === 0
              ? html`<li>None installed</li>`
              : dashboard.repositories.slice(0, 12).map(
                  (repository) => html`<li title=${repository.repository}>${repository.repository} · ${repository.mode ?? "off"}</li>`,
                )}
            ${dashboard.repositories.length > 12
              ? html`<li>+${dashboard.repositories.length - 12} more</li>`
              : nothing}
          </ul>
        </article>

        <article class="overview-card">
          <h2>Reviewer personas</h2>
          <p>${dashboard.reviewers.length} available · ${activeJobs} active job${activeJobs === 1 ? "" : "s"}.</p>
          <ul class="roster" aria-label="Available reviewer personas">
            ${dashboard.reviewers.length === 0
              ? html`<li>None configured</li>`
              : dashboard.reviewers.slice(0, 12).map(
                  (reviewer) => html`<li title=${reviewer.name}>${reviewer.name}${reviewer.built_in === true ? " · built in" : ""}</li>`,
                )}
            ${dashboard.reviewers.length > 12
              ? html`<li>+${dashboard.reviewers.length - 12} more</li>`
              : nothing}
          </ul>
        </article>
      </section>
    `;
  }

  #renderJobs(dashboard: ProtocolCodeReviewDashboard) {
    const groups = orderReviewJobGroups(
      groupCodeReviewJobs(dashboard.jobs, this.#filter),
      this.#groupOrder,
    );
    const visibleRepositories = groups.map((group) => group.repository);
    const finalEditorRetryableJobIds = new Set(
      dashboard.final_editor_retryable_job_ids ?? [],
    );
    const statusCounts = new Map<string, number>();
    for (const job of dashboard.jobs) {
      statusCounts.set(job.status, (statusCounts.get(job.status) ?? 0) + 1);
    }

    return html`
      <section class="panel" aria-labelledby="review-jobs-title">
        <header class="panel-header">
          <div>
            <h2 id="review-jobs-title">Review jobs</h2>
            <p>Active work stays first within each repository. Reorder repository groups to match your workflow.</p>
          </div>
          <label class="filter">
            Status
            <select
              aria-label="Filter code review jobs by status"
              .value=${this.#filter}
              @change=${this.#changeFilter}
            >
              ${CODE_REVIEW_STATUS_FILTERS.map(
                (status) => html`<option value=${status}>${status === "all" ? "All" : codeReviewStatusLabel(status)} (${status === "all" ? dashboard.jobs.length : (statusCounts.get(status) ?? 0)})</option>`,
              )}
            </select>
          </label>
        </header>

        ${dashboard.jobs.length === 0
          ? html`<div class="empty-state"><strong>No review jobs yet</strong><p>Jobs will appear after an installed repository receives or requests a review.</p></div>`
          : groups.length === 0
            ? html`<div class="empty-state"><strong>No matching review jobs</strong><p>Choose another status to see recent review activity.</p><button type="button" @click=${() => { this.#filter = "all"; this.requestUpdate(); }}>Show all jobs</button></div>`
            : html`<div class="job-groups">${groups.map((group, index) => {
                const dropTarget = this.#dropTarget === group.repository;
                const placeholder = html`<div
                  class="review-group-drop-placeholder"
                  data-drop-placeholder="code-review-group"
                  aria-hidden="true"
                  @dragover=${this.#keepGroupDropActive}
                  @drop=${(event: DragEvent) => this.#dropGroup(event, group.repository, visibleRepositories)}
                ></div>`;
                return html`
                  ${dropTarget && !this.#dropAfter ? placeholder : nothing}
                  <section
                    class="review-job-group"
                    data-review-group=${group.repository}
                    aria-label=${`${group.repository} review jobs`}
                    @dragover=${(event: DragEvent) => this.#dragOverGroup(event, group.repository)}
                    @drop=${(event: DragEvent) => this.#dropGroup(event, group.repository, visibleRepositories)}
                  >
                    <header class="group-header">
                      <h3 title=${group.repository}>${group.repository}</h3>
                      <span class="group-count">${group.jobs.length} job${group.jobs.length === 1 ? "" : "s"}${group.activeCount > 0 ? ` · ${group.activeCount} active` : ""}</span>
                      <span class="group-order-controls" role="group" aria-label=${`Position of ${group.repository}, ${index + 1} of ${groups.length}`}>
                        <button
                          class="group-grip"
                          type="button"
                          data-group-order-control="grip"
                          .draggable=${groups.length > 1}
                          aria-label=${`Reorder ${group.repository}. Position ${index + 1} of ${groups.length}. Use Up and Down arrow keys or drag.`}
                          title="Drag to reorder, or use Up and Down arrow keys"
                          @keydown=${(event: KeyboardEvent) => this.#groupOrderKeyDown(event, group.repository, visibleRepositories)}
                          @dragstart=${(event: DragEvent) => this.#startGroupDrag(event, group.repository)}
                          @dragend=${this.#endGroupDrag}
                        >${fontAwesomeIcon("grip-vertical")}</button>
                        <button
                          type="button"
                          data-group-order-control="up"
                          aria-label=${`Move ${group.repository} up`}
                          title="Move repository group up"
                          ?disabled=${index === 0}
                          @click=${() => this.#moveGroup(group.repository, -1, visibleRepositories, "up")}
                        >${fontAwesomeIcon("arrow-up")}</button>
                        <button
                          type="button"
                          data-group-order-control="down"
                          aria-label=${`Move ${group.repository} down`}
                          title="Move repository group down"
                          ?disabled=${index + 1 === groups.length}
                          @click=${() => this.#moveGroup(group.repository, 1, visibleRepositories, "down")}
                        >${fontAwesomeIcon("arrow-down")}</button>
                      </span>
                    </header>
                    <div class="job-list">${group.jobs.map((job) =>
                      this.#renderJob(job, finalEditorRetryableJobIds))}</div>
                  </section>
                  ${dropTarget && this.#dropAfter ? placeholder : nothing}
                `;
              })}</div>`}
      </section>
    `;
  }

  #renderJob(job: ProtocolCodeReviewJob, finalEditorRetryableJobIds: ReadonlySet<string>) {
    const active = canCancelCodeReviewJob(job.status);
    const finalEditorRetryable = finalEditorRetryableJobIds.has(job.id);
    const progress = job.progress;
    const percent = Math.max(0, Math.min(100, progress?.percent ?? (active ? 0 : 100)));
    const pending = this.#pendingAction?.jobId === job.id ? this.#pendingAction : undefined;
    const busy = this.#busyJobId === job.id;
    const needsAttention = codeReviewNeedsAttention(job);
    const outcomeLabel = needsAttention ? "Needs attention" : codeReviewStatusLabel(job.status);
    const outcomeClass = needsAttention ? "failed" : codeReviewStatusClass(job.status);

    return html`
      <article class="job-card" aria-label=${`${job.repository} pull request ${job.pull_number}, ${outcomeLabel}`}>
        <header class="job-heading">
          <div>
            <h4 title=${job.pull_title}>#${job.pull_number} · ${job.pull_title}</h4>
            <p class="job-subtitle" title=${`${job.base_ref} ← ${job.head_ref}`}>${job.base_ref} ← ${job.head_ref}</p>
          </div>
          <span class="status ${outcomeClass}">${outcomeLabel}</span>
        </header>

        <dl class="job-meta">
          <div><dt>Findings</dt><dd>${job.issue_count ?? 0} new${job.open_issue_count == null ? " · open status unknown" : ` · ${job.open_issue_count} open`}</dd></div>
          ${job.churn
            ? html`<div><dt>Fix churn</dt><dd>${job.churn.finding_round_streak} round${job.churn.finding_round_streak === 1 ? "" : "s"} with new issues · ${job.churn.clean_rounds}/${job.churn.required_clean_rounds} full-branch clean since</dd></div>`
            : nothing}
          <div><dt>${job.status === "queued" ? "Waiting" : "Elapsed"}</dt><dd>${formatDuration(job.status === "queued" ? job.pending_elapsed_ms : job.running_elapsed_ms)}</dd></div>
          <div><dt>Started</dt><dd title=${formatDate(job.started_at ?? job.created_at)}>${formatDate(job.started_at ?? job.created_at)}</dd></div>
        </dl>

        ${active
          ? html`<div
              class="progress-track"
              role="progressbar"
              aria-label=${`Review progress for pull request ${job.pull_number}`}
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow=${percent}
            ><span style=${`width: ${percent}%`}></span></div>`
          : nothing}
        ${job.error ? html`<p class="job-error">${job.error}</p>` : nothing}

        <footer class="job-footer">
          <div class="job-links">
            ${this.#externalLink(job.pull_url, "Open PR", `Open pull request ${job.pull_number} on GitHub`)}
            ${this.#externalLink(job.review_url, "Open review", `Open published review for pull request ${job.pull_number}`)}
          </div>
          <div class="actions">
            ${active
              ? html`
                  <button class="compact danger" type="button" ?disabled=${this.#busyJobId !== ""} @click=${() => this.#confirmJobAction(job, "cancel")}>${busy ? "Working…" : "Cancel"}</button>
                  <button class="compact" type="button" title=${RETRY_WHOLE_REVIEW_TITLE} ?disabled=${this.#busyJobId !== ""} @click=${() => this.#confirmJobAction(job, "retry")}>Cancel & retry</button>
                `
              : canRetryCodeReviewJob(job.status)
                ? html`
                    ${finalEditorRetryable
                      ? html`<button class="compact" type="button" ?disabled=${this.#busyJobId !== ""} @click=${() => this.#confirmJobAction(job, "final-editor")}>${busy ? "Retrying…" : "Retry final editor"}</button>`
                      : nothing}
                    <button class="compact" type="button" title=${RETRY_WHOLE_REVIEW_TITLE} ?disabled=${this.#busyJobId !== ""} @click=${() => this.#confirmJobAction(job, "retry")}>${busy ? "Retrying…" : "Retry whole review"}</button>
                  `
                : nothing}
          </div>
        </footer>

        ${pending === undefined
          ? nothing
          : html`
              <div class="confirmation" role="alertdialog" aria-label=${`${pending.action === "cancel" ? "Cancel" : pending.action === "final-editor" ? "Retry final editor" : "Retry"} code review confirmation`}>
                <p>${pending.action === "cancel"
                  ? `Cancel the review for PR #${job.pull_number}? Completed output remains in review history.`
                  : pending.action === "final-editor"
                    ? `Retry only the final review editor for PR #${job.pull_number}? Successful reviewer output will be retained.`
                  : active
                    ? `Cancel current work and queue a replacement for PR #${job.pull_number} using current repository settings? Every currently selected reviewer persona will run again.`
                    : `Queue a replacement for PR #${job.pull_number} using current repository settings? Every currently selected reviewer persona will run again.`}</p>
                <div class="actions">
                  <button class="compact" type="button" @click=${this.#dismissConfirmation}>Keep current job</button>
                  <button class="compact ${pending.action === "cancel" ? "danger" : "primary"}" type="button" @click=${() => this.#runJobAction(job, pending.action)}>Confirm ${pending.action === "cancel" ? "cancel" : pending.action === "final-editor" ? "final-editor retry" : "whole-review retry"}</button>
                </div>
              </div>
            `}
      </article>
    `;
  }

  #groupOrderKeyDown(
    event: KeyboardEvent,
    repository: string,
    visibleRepositories: readonly string[],
  ): void {
    if (event.altKey || event.ctrlKey || event.metaKey || event.isComposing) return;
    const offset = event.key === "ArrowUp"
      ? -1
      : event.key === "ArrowDown"
        ? 1
        : event.key === "Home"
          ? -visibleRepositories.length
          : event.key === "End"
            ? visibleRepositories.length
            : 0;
    if (offset === 0) return;
    event.preventDefault();
    event.stopPropagation();
    this.#moveGroup(repository, offset, visibleRepositories, "grip");
  }

  #moveGroup(
    repository: string,
    offset: number,
    visibleRepositories: readonly string[],
    focusControl: GroupOrderControl,
  ): void {
    this.#commitGroupOrder(
      moveReviewGroup(
        this.#groupOrder,
        visibleRepositories,
        repository,
        offset,
      ),
      repository,
      visibleRepositories,
      focusControl,
    );
  }

  #commitGroupOrder(
    nextOrder: readonly string[],
    repository: string,
    visibleRepositories: readonly string[],
    focusControl: GroupOrderControl,
  ): void {
    if (nextOrder === this.#groupOrder) {
      this.#resetGroupDrag();
      return;
    }
    this.#groupOrder = nextOrder;
    this.#customGroupOrder = true;
    const persisted = this.#groupOrderStorage?.save(nextOrder) ?? false;
    const visible = nextOrder.filter((key) => visibleRepositories.includes(key));
    const position = visible.indexOf(repository) + 1;
    this.#liveStatus = `${repository} moved to position ${position} of ${visible.length}.${persisted
      ? ""
      : " The order is active for this view but could not be saved."}`;
    this.#resetGroupDrag();
    this.requestUpdate();
    void this.updateComplete.then(() => {
      if (!this.isConnected) return;
      const group = [...this.renderRoot.querySelectorAll<HTMLElement>("[data-review-group]")]
        .find((candidate) => candidate.dataset["reviewGroup"] === repository);
      const requested = group?.querySelector<HTMLButtonElement>(
        `[data-group-order-control="${focusControl}"]`,
      );
      const fallback = group?.querySelector<HTMLButtonElement>(
        '[data-group-order-control="grip"]',
      );
      (requested?.disabled === false ? requested : fallback)?.focus();
    });
  }

  #startGroupDrag(event: DragEvent, repository: string): void {
    const transfer = event.dataTransfer;
    if (transfer === null) return;
    this.#draggedGroup = repository;
    transfer.effectAllowed = "move";
    try {
      transfer.setData("text/plain", "trouve-review-group");
      try {
        transfer.setData(REVIEW_GROUP_DRAG_TYPE, "move");
      } catch {
        // Some embedded engines only accept text/plain drag payloads.
      }
    } catch {
      this.#draggedGroup = "";
      event.preventDefault();
    }
  }

  #dragOverGroup(event: DragEvent, targetRepository: string): void {
    if (this.#draggedGroup === "" || this.#draggedGroup === targetRepository) return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
    const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const after = event.clientY >= bounds.top + bounds.height / 2;
    if (this.#dropTarget === targetRepository && this.#dropAfter === after) return;
    this.#dropTarget = targetRepository;
    this.#dropAfter = after;
    this.requestUpdate();
  }

  readonly #keepGroupDropActive = (event: DragEvent): void => {
    if (this.#draggedGroup === "") return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
  };

  #dropGroup(
    event: DragEvent,
    targetRepository: string,
    visibleRepositories: readonly string[],
  ): void {
    const repository = this.#draggedGroup;
    if (repository === "" || repository === targetRepository) {
      this.#resetGroupDrag();
      return;
    }
    event.preventDefault();
    this.#commitGroupOrder(
      reorderReviewGroup(
        this.#groupOrder,
        repository,
        targetRepository,
        this.#dropAfter,
      ),
      repository,
      visibleRepositories,
      "grip",
    );
  }

  readonly #endGroupDrag = (): void => {
    this.#resetGroupDrag();
  };

  #resetGroupDrag(): void {
    if (
      this.#draggedGroup === "" &&
      this.#dropTarget === "" &&
      !this.#dropAfter
    ) return;
    this.#draggedGroup = "";
    this.#dropTarget = "";
    this.#dropAfter = false;
    this.requestUpdate();
  }

  #renderSettings() {
    const settings = this.#settings;
    return html`
      <section class="panel" aria-labelledby="review-settings-title">
        <header class="panel-header">
          <div>
            <h2 id="review-settings-title">Review execution</h2>
            <p>Concurrency and deadlines for unattended review jobs. Repository and reviewer configuration remains read-only here.</p>
          </div>
        </header>
        ${settings === undefined
          ? html`<div class="empty-state" role=${this.#settingsError ? "alert" : "status"}>
              <strong>${this.#settingsError ? "Unable to load review settings" : "Loading review settings…"}</strong>
              ${this.#settingsError ? html`<p>${this.#settingsError} Retrying automatically.</p>` : nothing}
            </div>`
          : html`
              <form class="settings-body" @submit=${this.#saveSettings}>
                <div class="settings-grid">
                  <label>
                    Max parallel reviews
                    <input type="number" min="1" max=${MAX_PARALLEL_REVIEWS} step="1" required .value=${this.#settingsDraft.maxParallel} @input=${(event: Event) => this.#updateSettingsDraft("maxParallel", event)} />
                    <small>Concurrent pull-request review jobs; changes apply immediately.</small>
                  </label>
                  <label>
                    Total review timeout (minutes)
                    <input type="number" min=${TIMEOUT_MINUTES_INPUT_MIN} step=${TIMEOUT_MINUTES_INPUT_STEP} required .value=${this.#settingsDraft.totalMinutes} @input=${(event: Event) => this.#updateSettingsDraft("totalMinutes", event)} />
                    <small>Outer deadline from preparation through publication.</small>
                  </label>
                  <label>
                    Reviewer timeout (minutes)
                    <input type="number" min=${TIMEOUT_MINUTES_INPUT_MIN} step=${TIMEOUT_MINUTES_INPUT_STEP} required .value=${this.#settingsDraft.reviewerMinutes} @input=${(event: Event) => this.#updateSettingsDraft("reviewerMinutes", event)} />
                    <small>Deadline for one reviewer persona batch.</small>
                  </label>
                  <label>
                    Final editor timeout (minutes)
                    <input type="number" min=${TIMEOUT_MINUTES_INPUT_MIN} step=${TIMEOUT_MINUTES_INPUT_STEP} required .value=${this.#settingsDraft.coordinatorMinutes} @input=${(event: Event) => this.#updateSettingsDraft("coordinatorMinutes", event)} />
                    <small>Deadline for coordinating and publishing the final review.</small>
                  </label>
                </div>
                <div class="settings-actions">
                  ${this.#settingsError ? html`<p class="settings-error" role="alert">${this.#settingsError}</p>` : nothing}
                  <button class="primary" type="submit" ?disabled=${this.#savingSettings}>${this.#savingSettings ? "Saving…" : "Save execution settings"}</button>
                </div>
              </form>
            `}
      </section>
    `;
  }

  #externalLink(href: string | null | undefined, label: string, accessibleLabel: string) {
    const safe = safeCodeReviewHref(href);
    return safe === undefined
      ? nothing
      : html`<a href=${safe} aria-label=${accessibleLabel} @click=${(event: MouseEvent) => this.#openExternal(event, safe)}>${label}</a>`;
  }

  #openExternal(event: MouseEvent, href: string): void {
    event.preventDefault();
    this.dispatchEvent(
      new CustomEvent<{ readonly href: string }>("trouve-open-external", {
        detail: { href },
        bubbles: true,
        composed: true,
      }),
    );
  }

  readonly #changeFilter = (event: Event): void => {
    const value = (event.currentTarget as HTMLSelectElement).value;
    if (CODE_REVIEW_STATUS_FILTERS.includes(value as CodeReviewStatusFilter)) {
      this.#filter = value as CodeReviewStatusFilter;
      this.#pendingAction = undefined;
      this.requestUpdate();
    }
  };

  #confirmJobAction(job: ProtocolCodeReviewJob, action: CodeReviewJobAction): void {
    this.#pendingAction = { action, jobId: job.id };
    this.requestUpdate();
  }

  readonly #dismissConfirmation = (): void => {
    this.#pendingAction = undefined;
    this.requestUpdate();
  };

  async #runJobAction(job: ProtocolCodeReviewJob, action: CodeReviewJobAction): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busyJobId !== "") return;
    this.#pendingAction = undefined;
    this.#busyJobId = job.id;
    this.#error = "";
    this.#liveStatus = action === "cancel"
      ? "Cancelling review…"
      : action === "final-editor"
        ? "Retrying final review editor…"
        : "Retrying whole review…";
    this.requestUpdate();

    let updated: ProtocolCodeReviewJob;
    try {
      updated = action === "cancel"
        ? await services.protocol.cancelCodeReviewJob(job.id)
        : action === "final-editor"
          ? await services.protocol.retryCodeReviewFinalEditor(job.id)
          : await services.protocol.retryCodeReviewJob(job.id);
    } catch (cause) {
      if (!this.isConnected) return;
      this.#error = errorMessage(
        cause,
        action === "cancel"
          ? "The review could not be cancelled."
          : action === "final-editor"
            ? "The final review editor could not be retried."
            : "The whole review could not be retried.",
      );
      this.#liveStatus = this.#error;
      this.#busyJobId = "";
      this.requestUpdate();
      return;
    }

    if (!this.isConnected) return;
    this.#upsertJob(updated);
    this.#liveStatus = action === "cancel"
      ? "Review cancelled."
      : action === "final-editor"
        ? "Final review editor retry queued."
        : updated.id === job.id
          ? "Review publication had already started; the existing review was reconciled instead of retried."
          : `Replacement review ${updated.id} queued; all currently selected reviewer personas will run again.`;
    try {
      this.#replaceDashboard(await services.protocol.codeReviewDashboard());
    } catch {
      if (this.isConnected) {
        this.#error = "The action completed, but the review dashboard could not be reloaded.";
      }
    } finally {
      if (this.isConnected) {
        this.#busyJobId = "";
        this.requestUpdate();
      }
    }
  }

  #upsertJob(updated: ProtocolCodeReviewJob): void {
    const dashboard = this.#dashboard;
    if (dashboard === undefined) return;
    const exists = dashboard.jobs.some((job) => job.id === updated.id);
    this.#replaceDashboard({
      ...dashboard,
      jobs: exists
        ? dashboard.jobs.map((job) => (job.id === updated.id ? updated : job))
        : [updated, ...dashboard.jobs],
    });
  }

  #replaceDashboard(dashboard: ProtocolCodeReviewDashboard): void {
    this.#dashboard = dashboard;
    const defaultGroups = groupCodeReviewJobs(dashboard.jobs, "all");
    const currentRepositories = reviewGroupRepositoryKeys(
      defaultGroups,
      dashboard.repositories.map((repository) => repository.repository),
    );
    if (!this.#customGroupOrder) {
      this.#groupOrder = currentRepositories;
      return;
    }
    const reconciled = reconcileReviewGroupOrder(
      this.#groupOrder,
      currentRepositories,
    );
    this.#groupOrder = reconciled.order;
    if (reconciled.changed) this.#groupOrderStorage?.save(reconciled.order);
  }

  readonly #refresh = async (): Promise<void> => {
    const services = this.#services.value;
    if (services === undefined || this.#refreshing) return;
    this.#clearAutoRefresh();
    this.#refreshing = true;
    this.#error = "";
    this.#liveStatus = "Refreshing code reviews from GitHub…";
    this.requestUpdate();
    try {
      await services.protocol.refreshCodeReviews();
      const dashboard = await services.protocol.codeReviewDashboard();
      if (!this.isConnected) return;
      this.#replaceDashboard(dashboard);
      this.#liveStatus = "Code reviews refreshed.";
    } catch (cause) {
      if (!this.isConnected) return;
      this.#error = errorMessage(cause, "The code review refresh failed.");
      this.#liveStatus = this.#error;
    } finally {
      if (this.isConnected) {
        this.#refreshing = false;
        this.requestUpdate();
        this.#scheduleAutoRefresh(this.#error === "" ? CODE_REVIEW_REFRESH_MS : CODE_REVIEW_RETRY_MS);
      }
    }
  };

  readonly #loadInitial = async (): Promise<void> => {
    this.#clearAutoRefresh();
    const services = this.#services.value;
    if (services === undefined) {
      this.#loading = false;
      this.#error = "Application services are unavailable.";
      this.#scheduleAutoRefresh(CODE_REVIEW_RETRY_MS);
      this.requestUpdate();
      return;
    }
    this.#loading = true;
    this.#error = "";
    this.#settingsError = "";
    this.#liveStatus = "Loading code reviews…";
    this.requestUpdate();
    const [dashboardResult, settingsResult] = await Promise.allSettled([
      services.protocol.codeReviewDashboard(),
      services.protocol.codeReviewSettings(),
    ]);
    if (!this.isConnected) return;

    if (dashboardResult.status === "fulfilled") {
      this.#replaceDashboard(dashboardResult.value);
    } else {
      this.#error = errorMessage(dashboardResult.reason, "The review dashboard request failed.");
    }
    if (settingsResult.status === "fulfilled") {
      this.#applySettings(settingsResult.value);
    } else {
      this.#settingsError = errorMessage(settingsResult.reason, "The review settings request failed.");
    }
    this.#loading = false;
    this.#liveStatus = this.#error || "Code reviews loaded.";
    this.requestUpdate();
    this.#scheduleAutoRefresh(
      this.#error === "" && this.#settingsError === ""
        ? CODE_REVIEW_REFRESH_MS
        : CODE_REVIEW_RETRY_MS,
    );
  };

  #applySettings(settings: ProtocolCodeReviewSettings): void {
    this.#settings = settings;
    this.#settingsDraft = codeReviewSettingsDraft(settings);
    this.#settingsError = "";
  }

  #scheduleAutoRefresh(delayMs: number): void {
    this.#clearAutoRefresh();
    if (!this.isConnected) return;
    this.#autoRefreshTimer = globalThis.setTimeout(() => {
      this.#autoRefreshTimer = undefined;
      if (
        this.#busyJobId !== ""
        || this.#savingSettings
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) {
        this.#scheduleAutoRefresh(CODE_REVIEW_RETRY_MS);
        return;
      }
      if (this.#dashboard === undefined || this.#settings === undefined) {
        void this.#loadInitial();
      } else {
        void this.#refresh();
      }
    }, delayMs);
  }

  #clearAutoRefresh(): void {
    if (this.#autoRefreshTimer === undefined) return;
    globalThis.clearTimeout(this.#autoRefreshTimer);
    this.#autoRefreshTimer = undefined;
  }

  #updateSettingsDraft(field: keyof CodeReviewSettingsDraft, event: Event): void {
    const value = (event.currentTarget as HTMLInputElement).value;
    this.#settingsDraft = { ...this.#settingsDraft, [field]: value };
    this.#settingsError = "";
    this.requestUpdate();
  }

  readonly #saveSettings = async (event: SubmitEvent): Promise<void> => {
    event.preventDefault();
    const services = this.#services.value;
    if (services === undefined || this.#savingSettings) return;

    let request;
    try {
      request = codeReviewSettingsRequest(this.#settingsDraft);
    } catch (cause) {
      this.#settingsError = errorMessage(cause, "Review settings are invalid.");
      this.#liveStatus = this.#settingsError;
      this.requestUpdate();
      return;
    }

    this.#savingSettings = true;
    this.#settingsError = "";
    this.#liveStatus = "Saving review execution settings…";
    this.requestUpdate();
    try {
      const settings = await services.protocol.setCodeReviewSettings(request);
      if (!this.isConnected) return;
      this.#applySettings(settings);
      this.#liveStatus = "Review execution settings saved.";
    } catch (cause) {
      if (!this.isConnected) return;
      this.#settingsError = errorMessage(cause, "Review settings could not be saved.");
      this.#liveStatus = this.#settingsError;
    } finally {
      if (this.isConnected) {
        this.#savingSettings = false;
        this.requestUpdate();
      }
    }
  };
}

customElements.define("trouve-code-review-dashboard", TrouveCodeReviewDashboard);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-dashboard": TrouveCodeReviewDashboard;
  }
}
