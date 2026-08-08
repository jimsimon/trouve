import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { appServicesContext, appStoreContext } from "../contexts/app-contexts.js";
import type { ProtocolGithubIntegration } from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  buildPullRequestGroups,
  movePullRequestGroup,
  PULL_REQUEST_GROUP_KEYS,
  pullRequestRepositories,
  reconcilePullRequestGroupOrder,
  reorderPullRequestGroup,
  type PullRequestGroup,
  type PullRequestGroupKey,
  type PullRequestPill,
  type PullRequestRow,
} from "./pull-requests-dashboard-model.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import { safeSessionPrHref } from "./session-pr-panel-model.js";
import "./code-review-dashboard.js";

export const PULL_REQUEST_CHAT_EVENT = "trouve-pull-request-chat";
export const PULL_REQUEST_FIX_EVENT = "trouve-pull-request-fix";

export interface PullRequestChatDetail {
  readonly workspaceId: string;
  readonly branch: string;
}

export interface PullRequestFixDetail extends PullRequestChatDetail {
  readonly prompt: string;
}

type ReviewsView = "pull-requests" | "operations";
const GROUP_DRAG_TYPE = "application/x-trouve-pull-request-group";
const REFRESH_INTERVAL_MS = 30_000;

const errorMessage = (cause: unknown, fallback: string): string =>
  cause instanceof Error && cause.message.trim() !== "" ? cause.message : fallback;

const isGroupKey = (value: string): value is PullRequestGroupKey =>
  PULL_REQUEST_GROUP_KEYS.includes(value as PullRequestGroupKey);

const integrationConfigured = (
  integration: ProtocolGithubIntegration | undefined,
): boolean => integration?.hosts?.some(({ configured }) => configured) ??
  integration?.configured ?? false;

export class TrouvePullRequestsDashboard extends withSignalTracking(LitElement) {
  static override styles = css`
    :host {
      display: block;
      min-width: 0;
      min-height: 0;
      height: 100%;
      color: var(--trouve-text);
      background: var(--trouve-win-bg);
      font: var(--trouve-font-size) / var(--trouve-line-height) var(--trouve-font-sans);
    }

    * { box-sizing: border-box; }
    button, select { color: inherit; font: inherit; }
    button:focus-visible, select:focus-visible, a:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    button:disabled { cursor: not-allowed; opacity: 0.52; }

    .screen {
      display: grid;
      grid-template-rows: 52px minmax(0, 1fr);
      min-width: 0;
      min-height: 0;
      height: 100%;
    }
    .page-header {
      display: flex;
      align-items: center;
      gap: 12px;
      min-width: 0;
      padding: 0 16px;
      background: var(--trouve-sidebar-bg);
    }
    .page-header h1 {
      min-width: 0;
      flex: 1;
      margin: 0;
      color: var(--trouve-text-hi);
      font-size: 18px;
      line-height: 1.15;
    }
    .page-actions { display: flex; align-items: center; gap: 6px; }
    button.control {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 10px;
      background: var(--trouve-control-bg);
      cursor: pointer;
    }
    button.control:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.control.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }

    .account-scroll {
      min-width: 0;
      min-height: 0;
      overflow: auto;
      scrollbar-color: var(--trouve-scroll-thumb) transparent;
    }
    .account-body {
      width: min(1400px, calc(100% - 32px));
      min-height: 100%;
      margin-inline: auto;
      padding: 16px;
    }
    .operations {
      min-width: 0;
      min-height: 0;
      height: 100%;
    }
    trouve-code-review-dashboard { display: block; height: 100%; }

    .dashboard-controls {
      display: flex;
      align-items: center;
      gap: 8px;
      min-width: 0;
      margin-bottom: 12px;
    }
    .dashboard-controls label {
      display: flex;
      align-items: center;
      gap: 8px;
      color: var(--trouve-text-mid);
      font-size: 12px;
    }
    .dashboard-controls .control { flex: none; }
    .dashboard-controls select {
      width: min(300px, 44vw);
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 8px;
      background: var(--trouve-control-bg);
    }
    .refresh-status {
      min-width: 0;
      flex: 1;
      overflow: hidden;
      color: var(--trouve-text-soft);
      font-size: 11px;
      text-align: right;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .banner {
      margin-bottom: 12px;
      border: 1px solid var(--trouve-border);
      border-radius: var(--trouve-radius);
      padding: 10px 12px;
      color: var(--trouve-text-mid);
      background: var(--trouve-surface);
      font-size: 12px;
    }
    .banner.error { border-color: var(--trouve-err); color: var(--trouve-err-soft); background: var(--trouve-err-bg); }
    .banner.warning { border-color: var(--trouve-warn-border); color: var(--trouve-warn); background: var(--trouve-warn-bg); }
    .setup {
      display: grid;
      justify-items: center;
      gap: 12px;
      padding-top: 24px;
      text-align: center;
    }
    .setup strong { color: var(--trouve-text-hi); font-size: 14px; }
    .setup p { max-width: 620px; margin: 0; }

    .groups-grid {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      align-items: start;
      gap: 10px;
    }
    .group-column { display: grid; min-width: 0; gap: 10px; }
    .group-card {
      min-width: 0;
      border: 1px solid var(--trouve-border);
      border-radius: 8px;
      padding: 12px;
      background: var(--trouve-surface);
    }
    .group-drop-placeholder { min-height: 66px; border: 1px dashed var(--trouve-accent); border-radius: 8px; background: var(--trouve-accent-veil); }
    .group-header { display: flex; align-items: center; gap: 8px; min-width: 0; }
    .group-toggle {
      display: grid;
      grid-template-columns: 12px 20px minmax(0, 1fr) auto;
      align-items: center;
      gap: 8px;
      min-width: 0;
      min-height: 40px;
      flex: 1;
      border: 0;
      border-radius: 6px;
      padding: 3px 4px;
      text-align: left;
      background: transparent;
      cursor: pointer;
    }
    .group-toggle:hover { background: var(--trouve-hover-bg); }
    .chevron { color: var(--trouve-text-soft); font-size: 11px; }
    .group-icon { font-size: 14px; text-align: center; }
    .group-icon.accent { color: var(--trouve-accent); }
    .group-icon.muted { color: var(--trouve-text-soft); }
    .group-icon.warning { color: var(--trouve-warn); }
    .group-icon.ok { color: var(--trouve-ok); }
    .group-icon.danger { color: var(--trouve-err); }
    .group-icon.tint { color: var(--trouve-accent-tint); }
    .group-copy { min-width: 0; }
    .group-copy strong {
      display: block;
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 14px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .group-copy small {
      display: block;
      overflow: hidden;
      margin-top: 2px;
      color: var(--trouve-text-soft);
      font-size: 11px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .count {
      min-width: 24px;
      border-radius: 999px;
      padding: 2px 6px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 10px;
      font-weight: 700;
      text-align: center;
    }
    .group-grip {
      width: 26px;
      height: 28px;
      border: 0;
      border-radius: 5px;
      padding: 0;
      color: var(--trouve-text-soft);
      background: transparent;
      cursor: grab;
    }
    .group-grip:hover { background: var(--trouve-border); }
    .group-grip:active { cursor: grabbing; }
    .group-grip[aria-disabled="true"] { opacity: 0.45; cursor: default; }
    .touch-group-order { display: none; align-items: center; gap: 4px; }
    .touch-group-order button {
      width: 44px;
      min-height: 44px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: 5px;
      padding: 0;
      color: var(--trouve-text-mid);
      background: var(--trouve-control-bg);
    }

    .empty-group {
      margin-top: 8px;
      border-radius: 6px;
      padding: 14px 10px;
      color: var(--trouve-text-dim);
      background: var(--trouve-inset-bg);
      font-size: 12px;
      text-align: center;
    }
    .pr-list { display: grid; gap: 6px; margin-top: 8px; }
    .pr-row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 10px;
      min-width: 0;
      border-radius: 6px;
      padding: 8px;
      background: var(--trouve-inset-bg);
    }
    .pr-main { display: grid; min-width: 0; gap: 5px; }
    .pr-title-row, .pr-meta, .pr-actions, .review-heading, .finding-heading {
      display: flex;
      align-items: center;
      gap: 8px;
      min-width: 0;
    }
    .repository {
      flex: none;
      max-width: 180px;
      overflow: hidden;
      border-radius: 4px;
      padding: 3px 7px;
      color: var(--trouve-accent-tint);
      background: var(--trouve-accent-bg);
      font-size: 10px;
      font-weight: 700;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .number { flex: none; color: var(--trouve-text-dim); font-size: 12px; }
    .title {
      min-width: 80px;
      flex: 1;
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 13px;
      font-weight: 700;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .pill {
      flex: none;
      border-radius: 999px;
      padding: 3px 8px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 10px;
      font-weight: 700;
      white-space: nowrap;
    }
    .pill.ok { color: var(--trouve-ok); background: var(--trouve-diff-add-bg); }
    .pill.warning { border: 1px solid var(--trouve-warn-border); color: var(--trouve-warn); background: var(--trouve-warn-bg); }
    .pill.danger { color: var(--trouve-err-soft); background: var(--trouve-err-bg); }
    .branch {
      max-width: 220px;
      overflow: hidden;
      border-radius: 999px;
      padding: 3px 8px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 10px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .metadata { color: var(--trouve-text-soft); font-size: 11px; }
    .pr-actions { align-self: center; gap: 4px; }
    .icon-button {
      min-width: 28px;
      height: 26px;
      border: 0;
      border-radius: 4px;
      padding: 0 5px;
      color: var(--trouve-text-soft);
      background: transparent;
      cursor: pointer;
    }
    .icon-button:hover:not(:disabled) { color: var(--trouve-text-hi); background: var(--trouve-hover-strong); }
    .icon-button.active { color: var(--trouve-accent); }

    .review-card {
      display: grid;
      gap: 6px;
      border: 1px solid var(--trouve-border);
      border-radius: 6px;
      padding: 8px;
      background: var(--trouve-surface);
    }
    .review-heading strong { min-width: 0; flex: 1; color: var(--trouve-accent-tint); font-size: 11px; }
    .review-card p { margin: 0; color: var(--trouve-text-mid); font-size: 11px; }
    .review-card .control { min-height: 26px; padding: 2px 7px; font-size: 10px; }
    .finding {
      display: grid;
      gap: 4px;
      border-radius: 4px;
      padding: 6px;
      background: var(--trouve-inset-bg);
    }
    .finding-heading strong {
      min-width: 0;
      flex: 1;
      overflow: hidden;
      color: var(--trouve-warn);
      font-size: 10px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .finding-heading strong.severe { color: var(--trouve-err-soft); }

    .visually-hidden {
      position: absolute !important;
      width: 1px !important;
      height: 1px !important;
      overflow: hidden !important;
      clip: rect(0 0 0 0) !important;
      clip-path: inset(50%) !important;
      white-space: nowrap !important;
    }

    .operations-button, .manual-refresh {
      position: absolute;
      width: 1px;
      height: 1px;
      min-height: 0 !important;
      overflow: hidden;
      padding: 0 !important;
      clip: rect(0 0 0 0);
      clip-path: inset(50%);
      white-space: nowrap;
    }
    .operations-button:focus-visible, .manual-refresh:focus-visible {
      position: static;
      width: auto;
      height: auto;
      min-height: 30px !important;
      overflow: visible;
      padding: 4px 10px !important;
      clip: auto;
      clip-path: none;
      white-space: normal;
    }

    @media (max-width: 1031px) {
      .groups-grid { grid-template-columns: minmax(0, 1fr); }
    }
    @media (max-width: 720px) {
      .screen { grid-template-rows: calc(52px + env(safe-area-inset-top)) minmax(0, 1fr); }
      .page-header {
        padding-top: env(safe-area-inset-top);
        padding-inline: max(12px, env(safe-area-inset-left)) max(12px, env(safe-area-inset-right));
      }
      .account-body {
        padding: 12px max(12px, env(safe-area-inset-right)) max(12px, env(safe-area-inset-bottom)) max(12px, env(safe-area-inset-left));
      }
      .dashboard-controls { align-items: stretch; flex-direction: column; }
      .dashboard-controls label { display: grid; }
      .dashboard-controls select { width: 100%; }
      .refresh-status { text-align: left; white-space: normal; }
      .pr-row { grid-template-columns: minmax(0, 1fr); }
      .pr-actions { justify-content: flex-end; }
      .pr-title-row { flex-wrap: wrap; }
      .title { order: 3; min-width: 100%; white-space: normal; }
      .pr-meta { flex-wrap: wrap; }
    }
    @media (pointer: coarse) {
      button.control, .group-toggle { min-height: 44px; }
      .group-grip { display: none; }
      .touch-group-order { display: flex; }
      .icon-button { min-width: 44px; height: 44px; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });

  readonly #close = (): void => {
    this.dispatchEvent(new CustomEvent("trouve-close-full-screen", {
      bubbles: true,
      composed: true,
    }));
  };

  #view: ReviewsView = "pull-requests";
  #integration: ProtocolGithubIntegration | undefined;
  #integrationLoading = true;
  #refreshing = false;
  #error = "";
  #repository = "";
  #collapsed = new Set<PullRequestGroupKey>();
  #draggedGroup: PullRequestGroupKey | undefined;
  #dropTarget: PullRequestGroupKey | undefined;
  #dropAfter = false;
  #orderStatus = "";
  #copiedRow = "";
  #copyTimer: ReturnType<typeof setTimeout> | undefined;
  #clock = Date.now();
  #lastRefreshAt: number | undefined;
  #nextRefreshAt = 0;
  #tickTimer: ReturnType<typeof setInterval> | undefined;

  protected override firstUpdated(): void {
    this.#tickTimer = setInterval(() => this.#tick(), 1_000);
    void this.#initialize();
  }

  override disconnectedCallback(): void {
    if (this.#tickTimer !== undefined) clearInterval(this.#tickTimer);
    if (this.#copyTimer !== undefined) clearTimeout(this.#copyTimer);
    this.#tickTimer = undefined;
    this.#copyTimer = undefined;
    super.disconnectedCallback();
  }

  override render() {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) {
      return html`<div class="setup" role="status">Loading pull requests…</div>`;
    }
    const snapshots = readSignal(store.githubPullRequests);
    const lists = snapshots.map(({ pullRequests }) => pullRequests);
    const sessions = readSignal(store.sessions);
    const server = readSignal(store.serverInfo);
    const savedOrder = readSignal(services.pullRequestGroupOrder);
    const repositoryOptions = pullRequestRepositories(lists);
    const repository = repositoryOptions.includes(this.#repository)
      ? this.#repository
      : "";
    const order = reconcilePullRequestGroupOrder(savedOrder).order;
    const groups = buildPullRequestGroups(lists, sessions, {
      order,
      collapsed: this.#collapsed,
      ...(repository === "" ? {} : { repository }),
      now: new Date(this.#clock),
    });
    const split = Math.ceil(groups.length / 2);
    const columns = [groups.slice(0, split), groups.slice(split)].filter(
      (column) => column.length > 0,
    );
    const configured = integrationConfigured(this.#integration);
    const offline = server?.online === false;
    const refreshStatus = this.#refreshStatus(snapshots.map(({ refreshedAt }) => refreshedAt));

    return html`
      <section class="screen" aria-labelledby="pull-requests-title">
        <header class="page-header">
          <h1 id="pull-requests-title">Pull Requests</h1>
          <div class="page-actions">
            ${this.#view === "pull-requests"
              ? html`<button
                  class="control operations-button"
                  type="button"
                  @click=${() => this.#selectView("operations")}
                >Review operations</button>`
              : html`<button
                  class="control"
                  type="button"
                  @click=${() => this.#selectView("pull-requests")}
                >${fontAwesomeIcon("arrow-left")} Pull requests</button>`}
            <button class="control" type="button" @click=${this.#close}>${fontAwesomeIcon("xmark")} Close</button>
          </div>
        </header>

        ${this.#view === "operations"
          ? html`<div
              id="review-operations-panel"
              class="operations"
              aria-label="Review operations"
            ><trouve-code-review-dashboard></trouve-code-review-dashboard></div>`
          : html`<div
              id="pull-request-inbox-panel"
              class="account-scroll"
              aria-busy=${this.#refreshing || this.#integrationLoading}
            >
              <div class="account-body">
                <p class="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
                  ${this.#orderStatus}
                </p>
                ${this.#integrationLoading
                  ? html`<div class="banner" role="status">Checking GitHub integration…</div>`
                  : this.#integration === undefined && this.#error !== ""
                    ? html`<div class="setup" role="alert">
                        <strong>GitHub integration status is unavailable</strong>
                        <p>${this.#error}</p>
                        <button class="control" type="button" @click=${() => void this.#initialize()}>Retry</button>
                      </div>`
                    : !configured
                    ? html`<div class="setup">
                        <strong>Connect GitHub to see your pull requests</strong>
                        <p>trouve shows pull requests you authored, reviewed, or were asked to review across repositories available to your GitHub account, organized by what needs your attention. Sign in to each GitHub host with OAuth to load them.</p>
                        <button class="control primary" type="button" @click=${this.#openIntegrations}>Set up GitHub integration</button>
                      </div>`
                    : html`
                        ${offline
                          ? html`<div class="banner warning" role="status">The server is offline. Existing pull request data remains available; refresh resumes after connectivity returns.</div>`
                          : nothing}
                        ${this.#error === ""
                          ? nothing
                          : html`<div class="banner error" role="alert">${this.#error}</div>`}
                        <div class="dashboard-controls">
                          <label>Project
                            <select
                              aria-label="Pull request project"
                              .value=${repository}
                              @change=${this.#selectRepository}
                            >
                              <option value="">All projects</option>
                              ${repositoryOptions.map((key) => html`<option value=${key}>${key}</option>`)}
                            </select>
                          </label>
                          <button
                            class="control manual-refresh"
                            type="button"
                            ?disabled=${this.#refreshing || this.#integrationLoading || offline}
                            @click=${() => void this.#refresh()}
                          >${this.#refreshing ? "Refreshing…" : "Refresh"}</button>
                          <span class="refresh-status" role="status">${refreshStatus}</span>
                        </div>
                        <div class="groups-grid">
                          ${columns.map((column) => html`<div class="group-column">
                            ${column.map((group) => this.#renderGroup(group))}
                          </div>`)}
                        </div>
                      `}
              </div>
            </div>`}
      </section>
    `;
  }

  #renderGroup(group: PullRequestGroup) {
    const dropTarget = this.#dropTarget === group.key;
    const placeholder = html`<div
      class="group-drop-placeholder"
      data-drop-placeholder="pull-request-group"
      aria-hidden="true"
      @dragover=${this.#keepDropActive}
      @drop=${(event: DragEvent) => this.#drop(event, group.key)}
    ></div>`;
    return html`
      ${dropTarget && !this.#dropAfter ? placeholder : nothing}
      <section
        class="group-card"
        data-group-key=${group.key}
        @dragover=${(event: DragEvent) => this.#dragOver(event, group.key)}
        @drop=${(event: DragEvent) => this.#drop(event, group.key)}
      >
        <header class="group-header">
          <button
            class="group-toggle"
            type="button"
            aria-expanded=${!group.collapsed}
            @click=${() => this.#toggleGroup(group.key)}
          >
            ${fontAwesomeIcon(group.collapsed ? "caret-right" : "caret-down", {
              className: "chevron",
            })}
            ${fontAwesomeIcon(group.icon, { className: `group-icon ${group.tone}` })}
            <span class="group-copy">
              <strong>${group.title}</strong>
              <small>${group.description}</small>
            </span>
            <span class="count" aria-label=${`${group.pullRequests.length} pull requests`}>${group.pullRequests.length}</span>
          </button>
          <button
            class="group-grip"
            type="button"
            draggable=${group.groupCount > 1 ? "true" : "false"}
            aria-disabled=${group.groupCount <= 1 ? "true" : "false"}
            aria-label=${`Reorder ${group.title}, position ${group.position + 1} of ${group.groupCount}. Use Up or Down Arrow.`}
            title="Drag to reorder; use Up or Down Arrow from the keyboard"
            @dragstart=${(event: DragEvent) => this.#dragStart(event, group.key)}
            @dragend=${this.#finishDrag}
            @keydown=${(event: KeyboardEvent) => this.#groupKeyDown(event, group)}
          >${fontAwesomeIcon("grip-vertical")}</button>
          <span class="touch-group-order" role="group" aria-label=${`Reorder ${group.title}`}>
            <button
              type="button"
              aria-label=${`Move ${group.title} up`}
              ?disabled=${group.position === 0}
              @click=${() => this.#moveGroup(group, -1)}
            >${fontAwesomeIcon("arrow-up")}</button>
            <button
              type="button"
              aria-label=${`Move ${group.title} down`}
              ?disabled=${group.position === group.groupCount - 1}
              @click=${() => this.#moveGroup(group, 1)}
            >${fontAwesomeIcon("arrow-down")}</button>
          </span>
        </header>
        ${group.collapsed
          ? nothing
          : group.pullRequests.length === 0
            ? html`<div class="empty-group">${group.emptyText}</div>`
            : html`<div class="pr-list">${group.pullRequests.map((row) => this.#renderRow(row))}</div>`}
      </section>
      ${dropTarget && this.#dropAfter ? placeholder : nothing}
    `;
  }

  #renderRow(row: PullRequestRow) {
    const safeUrl = safeSessionPrHref(row.url);
    const copied = this.#copiedRow === row.key;
    return html`
      <article class="pr-row">
        <div class="pr-main">
          <div class="pr-title-row">
            <span class="repository" title=${row.repository}>${row.repository}</span>
            <span class="number">#${row.number}</span>
            <span class="title" title=${row.title}>${row.title}</span>
            ${this.#renderPill(row.check)}
            ${this.#renderPill(row.approval)}
            ${row.merge === undefined ? nothing : this.#renderPill(row.merge)}
          </div>
          <div class="pr-meta">
            <span class="branch" title=${row.branch}>${row.branch}</span>
            <span class="metadata">${fontAwesomeIcon("comments")} ${row.commentsLabel}</span>
            <span class="metadata">· ${row.lastComment}</span>
          </div>
          ${row.reviewSummary === "" && row.reviewFindings.length === 0
            ? nothing
            : html`<section class="review-card" aria-label="trouve review">
                <div class="review-heading">
                  <strong>trouve review</strong>
                  <button
                    class="control primary"
                    type="button"
                    ?disabled=${row.reviewPrompt === "" || row.workspaceId === ""}
                    @click=${() => this.#fix(row, row.reviewPrompt)}
                  >Fix all</button>
                </div>
                ${row.reviewSummary === "" ? nothing : html`<p>${row.reviewSummary}</p>`}
                ${row.reviewFindings.map((finding) => html`<article class="finding">
                  <div class="finding-heading">
                    <strong class=${finding.severity === "critical" || finding.severity === "high" ? "severe" : ""}>${finding.severity} · ${finding.location}</strong>
                    <button
                      class="control"
                      type="button"
                      ?disabled=${finding.prompt === "" || finding.status !== "open" || row.workspaceId === ""}
                      @click=${() => this.#fix(row, finding.prompt)}
                    >Fix</button>
                  </div>
                  <p>${finding.body}</p>
                </article>`)}
              </section>`}
        </div>
        <div class="pr-actions" aria-label=${`Actions for pull request ${row.number}`}>
          <button
            class="icon-button ${row.hasChat ? "active" : ""}"
            type="button"
            ?disabled=${row.workspaceId === "" || row.branch === ""}
            aria-label=${row.hasChat
              ? "Open the chat this pull request came from"
              : "Start a new chat for this pull request"}
            @click=${() => this.#chat(row)}
          >${row.hasChat
            ? fontAwesomeIcon("comments")
            : html`${fontAwesomeIcon("plus")}${fontAwesomeIcon("comments")}`}</button>
          <button
            class="icon-button"
            type="button"
            ?disabled=${safeUrl === undefined}
            aria-label=${copied ? "Pull request URL copied" : "Copy pull request URL"}
            @click=${() => void this.#copyUrl(row, safeUrl)}
          >${fontAwesomeIcon(copied ? "check" : "copy")}</button>
          <button
            class="icon-button"
            type="button"
            ?disabled=${safeUrl === undefined}
            aria-label="Open the pull request in your browser"
            @click=${() => this.#openExternal(safeUrl)}
          >${fontAwesomeIcon("arrow-up-right-from-square")}</button>
        </div>
      </article>
    `;
  }

  #renderPill(pill: PullRequestPill) {
    return html`<span class="pill ${pill.tone}">${pill.label}</span>`;
  }

  async #initialize(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#integrationLoading = true;
    this.requestUpdate();
    try {
      this.#integration = await protocol.githubIntegration();
      this.#error = "";
    } catch (cause) {
      this.#error = errorMessage(cause, "GitHub integration status could not be loaded.");
    } finally {
      this.#integrationLoading = false;
      this.requestUpdate();
    }
    if (integrationConfigured(this.#integration)) await this.#refresh();
  }

  async #refresh(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#refreshing) return;
    this.#refreshing = true;
    this.#error = "";
    this.#nextRefreshAt = Date.now() + REFRESH_INTERVAL_MS;
    this.requestUpdate();
    try {
      await protocol.refreshGithubPrs();
      this.#lastRefreshAt = Date.now();
    } catch (cause) {
      this.#error = errorMessage(cause, "Pull requests could not be refreshed.");
    } finally {
      this.#refreshing = false;
      this.requestUpdate();
    }
  }

  #tick(): void {
    this.#clock = Date.now();
    const store = this.#store.value;
    const online = store === undefined || readSignal(store.serverInfo)?.online !== false;
    if (
      this.#nextRefreshAt !== 0 &&
      this.#clock >= this.#nextRefreshAt &&
      integrationConfigured(this.#integration) &&
      online &&
      !this.#refreshing
    ) {
      void this.#refresh();
    }
    this.requestUpdate();
  }

  #refreshStatus(snapshotTimes: readonly string[]): string {
    const snapshotMilliseconds = snapshotTimes
      .map((value) => Date.parse(value))
      .filter(Number.isFinite);
    const last = Math.max(this.#lastRefreshAt ?? Number.NEGATIVE_INFINITY, ...snapshotMilliseconds);
    const nextSeconds = this.#nextRefreshAt === 0
      ? undefined
      : Math.max(0, Math.round((this.#nextRefreshAt - this.#clock) / 1_000));
    if (!Number.isFinite(last)) {
      if (this.#refreshing) {
        return nextSeconds === undefined
          ? "Refreshing for the first time"
          : `Refreshing for the first time, next refresh in ${nextSeconds} seconds`;
      }
      return "Not refreshed yet";
    }
    const age = Math.max(0, Math.floor((this.#clock - last) / 1_000));
    const ageLabel = age === 1 ? "1 second ago" : `${age} seconds ago`;
    return nextSeconds === undefined
      ? `Last refreshed ${ageLabel}`
      : `Last refreshed ${ageLabel}, next refresh in ${nextSeconds} seconds`;
  }

  #selectView(view: ReviewsView): void {
    this.#view = view;
    this.requestUpdate();
  }

  readonly #openIntegrations = (): void => {
    this.#services.value?.router.navigate({ kind: "settings", section: "integrations" });
  };

  readonly #selectRepository = (event: Event): void => {
    this.#repository = (event.currentTarget as HTMLSelectElement).value;
    this.requestUpdate();
  };

  #toggleGroup(key: PullRequestGroupKey): void {
    const collapsed = new Set(this.#collapsed);
    if (!collapsed.delete(key)) collapsed.add(key);
    this.#collapsed = collapsed;
    this.requestUpdate();
  }

  #saveOrder(order: readonly PullRequestGroupKey[], moved: PullRequestGroupKey): void {
    this.#services.value?.setPullRequestGroupOrder(order);
    const position = order.indexOf(moved);
    this.#orderStatus = `Group moved to position ${position + 1} of ${order.length}.`;
    this.requestUpdate();
    void this.updateComplete.then(() =>
      this.renderRoot.querySelector<HTMLButtonElement>(
        `.group-card[data-group-key="${CSS.escape(moved)}"] .group-grip`,
      )?.focus());
  }

  #groupKeyDown(event: KeyboardEvent, group: PullRequestGroup): void {
    const offset = event.key === "ArrowUp" ? -1 : event.key === "ArrowDown" ? 1 : 0;
    if (offset === 0 || event.altKey || event.ctrlKey || event.metaKey) return;
    event.preventDefault();
    this.#moveGroup(group, offset);
  }

  #moveGroup(group: PullRequestGroup, offset: -1 | 1): void {
    const services = this.#services.value;
    if (services === undefined || group.groupCount <= 1) return;
    const saved = readSignal(services.pullRequestGroupOrder);
    this.#saveOrder(movePullRequestGroup(saved, group.key, offset), group.key);
  }

  #dragStart(event: DragEvent, key: PullRequestGroupKey): void {
    if (event.dataTransfer === null) return;
    this.#draggedGroup = key;
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData(GROUP_DRAG_TYPE, key);
    event.dataTransfer.setData("text/plain", key);
  }

  #dragOver(event: DragEvent, target: PullRequestGroupKey): void {
    const key = this.#draggedGroup ?? event.dataTransfer?.getData(GROUP_DRAG_TYPE);
    if (key === undefined || !isGroupKey(key) || key === target) return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
    const bounds = (event.currentTarget as HTMLElement).getBoundingClientRect();
    this.#dropTarget = target;
    this.#dropAfter = event.clientY >= bounds.top + bounds.height / 2;
    this.requestUpdate();
  }

  readonly #keepDropActive = (event: DragEvent): void => {
    if (this.#draggedGroup === undefined) return;
    event.preventDefault();
    if (event.dataTransfer !== null) event.dataTransfer.dropEffect = "move";
  };

  #drop(event: DragEvent, target: PullRequestGroupKey): void {
    event.preventDefault();
    const raw = this.#draggedGroup ?? event.dataTransfer?.getData(GROUP_DRAG_TYPE);
    const after = this.#dropAfter;
    this.#finishDrag();
    if (raw === undefined || !isGroupKey(raw) || raw === target) return;
    const saved = readSignal(this.#services.value!.pullRequestGroupOrder);
    this.#saveOrder(reorderPullRequestGroup(saved, raw, target, after), raw);
  }

  readonly #finishDrag = (): void => {
    this.#draggedGroup = undefined;
    this.#dropTarget = undefined;
    this.#dropAfter = false;
    this.requestUpdate();
  };

  #chat(row: PullRequestRow): void {
    if (row.workspaceId === "" || row.branch === "") return;
    this.dispatchEvent(new CustomEvent<PullRequestChatDetail>(PULL_REQUEST_CHAT_EVENT, {
      detail: { workspaceId: row.workspaceId, branch: row.branch },
      bubbles: true,
      composed: true,
    }));
  }

  #fix(row: PullRequestRow, prompt: string): void {
    if (row.workspaceId === "" || row.branch === "" || prompt === "") return;
    this.dispatchEvent(new CustomEvent<PullRequestFixDetail>(PULL_REQUEST_FIX_EVENT, {
      detail: { workspaceId: row.workspaceId, branch: row.branch, prompt },
      bubbles: true,
      composed: true,
    }));
  }

  async #copyUrl(row: PullRequestRow, href: string | undefined): Promise<void> {
    if (href === undefined) return;
    let copied = false;
    try {
      await globalThis.navigator.clipboard.writeText(href);
      copied = true;
    } catch {
      const textarea = document.createElement("textarea");
      textarea.value = href;
      textarea.setAttribute("readonly", "");
      textarea.style.position = "fixed";
      textarea.style.opacity = "0";
      this.renderRoot.append(textarea);
      textarea.select();
      try {
        copied = document.execCommand("copy");
      } finally {
        textarea.remove();
      }
    }
    if (!copied) return;
    this.#copiedRow = row.key;
    if (this.#copyTimer !== undefined) clearTimeout(this.#copyTimer);
    this.#copyTimer = setTimeout(() => {
      this.#copiedRow = "";
      this.#copyTimer = undefined;
      this.requestUpdate();
    }, 1_200);
    this.requestUpdate();
  }

  #openExternal(href: string | undefined): void {
    if (href === undefined) return;
    this.dispatchEvent(new CustomEvent<{ readonly href: string }>("trouve-open-external", {
      detail: { href },
      bubbles: true,
      composed: true,
    }));
  }
}

if (
  "customElements" in globalThis &&
  !customElements.get("trouve-pull-requests-dashboard")
) {
  customElements.define("trouve-pull-requests-dashboard", TrouvePullRequestsDashboard);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-pull-requests-dashboard": TrouvePullRequestsDashboard;
  }

  interface HTMLElementEventMap {
    [PULL_REQUEST_CHAT_EVENT]: CustomEvent<PullRequestChatDetail>;
    [PULL_REQUEST_FIX_EVENT]: CustomEvent<PullRequestFixDetail>;
  }
}
