import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  appStoreContext,
  sessionContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import {
  type ProtocolMcpServerInfo,
  type ProtocolPrInfo,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import {
  checkSummary,
  mergeabilitySummary,
  reviewSummary,
  safeSessionPrHref,
  type PrSummary,
} from "./session-pr-panel-model.js";

const DIFF_REFRESH_MS = 2_000;
const RESOURCE_REFRESH_MS = 30_000;

export interface SessionDiffOverview {
  readonly additions: number;
  readonly deletions: number;
  readonly files: number;
}

export interface McpAvailability {
  readonly label: string;
  readonly tone: "active" | "muted" | "warning" | "failed";
  readonly active: boolean;
}

export const sessionMcpAvailability = (
  server: Pick<ProtocolMcpServerInfo, "health" | "scope">,
): McpAvailability => {
  if (server.health === "disabled") {
    return { label: "Disabled", tone: "muted", active: false };
  }
  if (server.health === "untrusted" || server.scope !== "app-wide") {
    return { label: "Not trusted", tone: "warning", active: false };
  }
  if (server.health === "error") {
    return { label: "Unavailable", tone: "failed", active: false };
  }
  return { label: "Active", tone: "active", active: true };
};

const sessionMcpToolCount = (server: ProtocolMcpServerInfo): number | undefined => {
  const match = /^(\d+) tools?$/u.exec(server.detail.trim());
  return match === null ? undefined : Number(match[1]);
};

const plural = (value: number, singular: string): string =>
  `${value} ${singular}${value === 1 ? "" : "s"}`;

export class TrouveSessionInfoPanel extends withSignalTracking(LitElement) {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
  };

  static override styles = css`
    :host { display: block; height: 100%; min-height: 0; color: var(--trouve-text); }
    * { box-sizing: border-box; }
    h2, h3, p { margin: 0; }
    button { color: var(--trouve-text-mid); background: var(--trouve-control-bg); font: inherit; }
    button:not(:disabled) { cursor: pointer; }
    .session-info-surface { height: 100%; display: flex; flex-direction: column; gap: 8px; overflow: auto; padding: 10px; }
    .session-info-header, .session-info-section-header { min-width: 0; display: flex; align-items: flex-start; gap: 10px; }
    .session-info-header > div, .session-info-section-header > div { min-width: 0; flex: 1; }
    .session-info-header h2, .session-info-card h3 { color: var(--trouve-text-hi); }
    .session-info-header h2 { font-size: 15px; }
    .session-info-card h3 { font-size: 13px; }
    .session-info-header p, .session-info-section-header p { margin-top: 3px; color: var(--trouve-text-dim); font-size: 11px; line-height: 1.35; }
    .session-info-section-header button, .session-info-icon-button { min-height: 28px; border: 1px solid var(--trouve-border-strong); border-radius: var(--trouve-radius-sm); }
    .session-info-section-header button { flex: none; padding: 3px 7px; }
    .session-info-icon-button { width: 28px; flex: none; padding: 0; }
    .session-info-section-header button:hover:not(:disabled), .session-info-icon-button:hover:not(:disabled) { color: var(--trouve-text-hi); background: var(--trouve-hover-bg); }
    .session-info-section-header button:focus-visible, .session-info-icon-button:focus-visible { outline: 2px solid var(--trouve-accent); outline-offset: 1px; }
    .session-info-card { display: grid; gap: 9px; padding: 11px; border: 1px solid var(--trouve-card-border); border-radius: var(--trouve-radius); background: var(--trouve-surface); }
    .session-info-identity dl { display: grid; gap: 1px; margin: 0; background: var(--trouve-rule); }
    .session-info-identity dl > div { min-width: 0; display: grid; grid-template-columns: 64px minmax(0, 1fr); align-items: center; gap: 10px; padding: 7px 8px; background: var(--trouve-inset-bg); }
    .session-info-identity dt, .session-info-metrics dt { color: var(--trouve-text-dim); font-size: 10px; text-transform: uppercase; letter-spacing: .04em; }
    .session-info-identity dd, .session-info-metrics dd { min-width: 0; margin: 0; color: var(--trouve-text); }
    .session-info-identity dd { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-identity code { color: var(--trouve-text-hi); font: 11px/1.35 var(--trouve-font-mono); }
    .session-info-metrics { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 1px; margin: 0; background: var(--trouve-rule); }
    .session-info-metrics > div { display: grid; gap: 2px; padding: 8px; background: var(--trouve-inset-bg); }
    .session-info-metrics dd { font-size: 14px; font-weight: 650; }
    .session-info-metrics .additions dd { color: var(--trouve-ok); }
    .session-info-metrics .deletions dd { color: var(--trouve-err); }
    .session-info-notice, .session-info-empty { color: var(--trouve-text-dim); font-size: 11px; line-height: 1.4; }
    .session-info-notice.error { color: var(--trouve-err); }
    .session-info-empty { padding: 10px 8px; border-radius: var(--trouve-radius-sm); background: var(--trouve-inset-bg); text-align: center; }
    .session-info-unscoped-empty { height: 100%; display: grid; place-items: center; }
    .session-info-pr-list, .session-info-mcp-list { display: grid; gap: 1px; margin: 0; padding: 0; background: var(--trouve-rule); list-style: none; }
    .session-info-pr-row { min-width: 0; display: flex; align-items: flex-start; gap: 8px; padding: 9px; background: var(--trouve-inset-bg); }
    .session-info-pr-copy { min-width: 0; flex: 1; display: grid; gap: 4px; }
    .session-info-pr-copy > strong { color: var(--trouve-text-hi); font-size: 12px; line-height: 1.35; overflow-wrap: anywhere; }
    .session-info-pr-copy > small { overflow: hidden; color: var(--trouve-text-dim); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-pr-statuses { display: flex; flex-wrap: wrap; gap: 4px; }
    .session-info-status, .session-info-scope { display: inline-flex; width: fit-content; border-radius: 999px; padding: 2px 6px; color: var(--trouve-text-dim); background: var(--trouve-pill-bg); font-size: 9px; white-space: nowrap; }
    .session-info-status.ready, .session-info-status.active { color: var(--trouve-ok); }
    .session-info-status.pending, .session-info-status.warning { color: var(--trouve-warn); }
    .session-info-status.failed { color: var(--trouve-err); }
    .session-info-mcp-row { min-width: 0; display: grid; grid-template-columns: 18px minmax(0, 1fr) auto auto; align-items: center; gap: 7px; padding: 8px 9px; background: var(--trouve-inset-bg); }
    .session-info-mcp-row > span:nth-child(2) { min-width: 0; display: grid; gap: 2px; }
    .session-info-mcp-row strong, .session-info-mcp-row small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-mcp-row strong { color: var(--trouve-text-hi); font-size: 12px; }
    .session-info-mcp-row small { color: var(--trouve-text-dim); font-size: 10px; }
    .session-info-mcp-icon { color: var(--trouve-text-dim); }
    .session-info-mcp-row.active .session-info-mcp-icon { color: var(--trouve-ok); }
    .session-info-mcp-row.warning .session-info-mcp-icon { color: var(--trouve-warn); }
    .session-info-mcp-row.failed .session-info-mcp-icon { color: var(--trouve-err); }
    .session-info-mcp-row.muted { opacity: .62; }
    .trouve-icon-spin { animation: trouve-session-info-spin 900ms linear infinite; }
    :host-context([data-reduce-motion]) .trouve-icon-spin { animation: none; }
    @keyframes trouve-session-info-spin { to { transform: rotate(360deg); } }
    @media (prefers-reduced-motion: reduce) { .trouve-icon-spin { animation: none; } }
    @media (max-width: 760px) {
      .session-info-surface { padding: 8px; }
      .session-info-card { padding: 10px; }
      .session-info-section-header button, .session-info-icon-button { min-height: 44px; }
      .session-info-icon-button { width: 44px; }
      .session-info-mcp-row { grid-template-columns: 18px minmax(0, 1fr) auto; }
      .session-info-mcp-row .session-info-scope { grid-column: 2; }
    }
  `;

  sessionId = "";

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

  #observedServices: AppServices | undefined;
  #observedSessionId = "";
  #generation = 0;
  #diffRequestActive = false;
  #resourcesRequestActive = false;
  #diffOverview: SessionDiffOverview | undefined;
  #diffManifest = "";
  #mcpServers: readonly ProtocolMcpServerInfo[] = [];
  #diffError = "";
  #mcpError = "";
  #loadScheduled = false;
  #diffRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #resourceRefreshTimer: ReturnType<typeof setInterval> | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    globalThis.addEventListener(
      "trouve-checkpoint-restored",
      this.#checkpointRestored,
    );
    this.#diffRefreshTimer ??= globalThis.setInterval(() => {
      if (globalThis.document?.visibilityState === "hidden") return;
      void this.#refreshDiff(true);
    }, DIFF_REFRESH_MS);
    this.#resourceRefreshTimer ??= globalThis.setInterval(() => {
      if (globalThis.document?.visibilityState === "hidden") return;
      void this.#refreshResources();
    }, RESOURCE_REFRESH_MS);
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    this.#diffRequestActive = false;
    this.#resourcesRequestActive = false;
    globalThis.removeEventListener(
      "trouve-checkpoint-restored",
      this.#checkpointRestored,
    );
    if (this.#diffRefreshTimer !== undefined) {
      globalThis.clearInterval(this.#diffRefreshTimer);
      this.#diffRefreshTimer = undefined;
    }
    if (this.#resourceRefreshTimer !== undefined) {
      globalThis.clearInterval(this.#resourceRefreshTimer);
      this.#resourceRefreshTimer = undefined;
    }
    super.disconnectedCallback();
  }

  protected override updated(changed: PropertyValues<this>): void {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    const servicesChanged = services !== this.#observedServices;
    const sessionChanged = sessionId !== this.#observedSessionId;
    if (
      !changed.has("sessionId")
      && !servicesChanged
      && !sessionChanged
    ) return;
    this.#observedServices = services;
    if (servicesChanged || sessionChanged) {
      this.#generation += 1;
      this.#observedSessionId = sessionId;
      this.#diffRequestActive = false;
      this.#resourcesRequestActive = false;
      this.#diffOverview = undefined;
      this.#diffManifest = "";
      this.#mcpServers = [];
      this.#diffError = "";
      this.#mcpError = "";
    }
    if (services !== undefined && sessionId !== "" && !this.#loadScheduled) {
      this.#loadScheduled = true;
      globalThis.queueMicrotask(() => {
        this.#loadScheduled = false;
        if (
          this.isConnected
          && services === this.#services.value
          && sessionId === this.#effectiveSessionId
        ) void this.#refreshAll();
      });
    }
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override render() {
    const sessionId = this.#effectiveSessionId;
    if (sessionId === "") {
      return html`<div class="session-info-empty session-info-unscoped-empty" role="status">Select a session to view its overview.</div>`;
    }
    const store = this.#store.value;
    const metadata = store?.sessionMetadata(sessionId);
    const session = store?.session(sessionId);
    const pullRequests = store?.sessionPullRequests(sessionId) ?? [];
    const title = metadata?.title ?? session?.title ?? "Untitled session";
    const branch = metadata?.branch ?? session?.branch ?? sessionId;
    const refreshing = this.#diffRequestActive || this.#resourcesRequestActive;
    return html`
      <section
        class="session-info-surface"
        aria-labelledby="session-info-title"
        aria-busy=${refreshing ? "true" : "false"}
      >
        <header class="session-info-header">
          <div>
            <h2 id="session-info-title">Session overview</h2>
            <p>Branch activity, pull requests, and tools available to this session.</p>
          </div>
        </header>

        <section class="session-info-card session-info-identity" aria-labelledby="session-info-identity-title">
          <h3 id="session-info-identity-title">Session</h3>
          <dl>
            <div>
              <dt>Name</dt>
              <dd title=${title}>${title}</dd>
            </div>
            <div>
              <dt>Branch</dt>
              <dd title=${branch}><code>${branch}</code></dd>
            </div>
          </dl>
        </section>

        ${this.#renderChanges()}
        ${this.#renderPullRequests(pullRequests)}
        ${this.#renderMcpServers()}
      </section>
    `;
  }

  #renderChanges() {
    const summary = this.#diffOverview;
    return html`
      <section class="session-info-card" aria-labelledby="session-info-changes-title">
        <header class="session-info-section-header">
          <div>
            <h3 id="session-info-changes-title">Changes</h3>
            <p>Diff against the session's base branch.</p>
          </div>
          <button type="button" @click=${this.#openDiff}>
            View diff ${fontAwesomeIcon("arrow-right")}
          </button>
        </header>
        ${this.#diffError === ""
          ? nothing
          : html`<p class="session-info-notice error" role="alert">${this.#diffError}</p>`}
        <dl class="session-info-metrics" aria-label="Diff summary">
          <div>
            <dt>Files</dt>
            <dd>${summary?.files ?? "—"}</dd>
          </div>
          <div class="additions">
            <dt>Added</dt>
            <dd>${summary === undefined ? "—" : `+${summary.additions}`}</dd>
          </div>
          <div class="deletions">
            <dt>Deleted</dt>
            <dd>${summary === undefined ? "—" : `−${summary.deletions}`}</dd>
          </div>
        </dl>
      </section>
    `;
  }

  #renderPullRequests(pullRequests: readonly ProtocolPrInfo[]) {
    return html`
      <section class="session-info-card" aria-labelledby="session-info-pr-title">
        <header class="session-info-section-header">
          <div>
            <h3 id="session-info-pr-title">Pull requests</h3>
            <p>${pullRequests.length === 0
              ? "Associated with this session's branch."
              : plural(pullRequests.length, "pull request")}</p>
          </div>
          <button type="button" @click=${this.#openPullRequests}>
            Manage ${fontAwesomeIcon("arrow-right")}
          </button>
        </header>
        ${pullRequests.length === 0
          ? html`<p class="session-info-empty">No pull requests are associated with this branch.</p>`
          : html`<ul class="session-info-pr-list">
              ${pullRequests.map((pullRequest) => this.#renderPullRequest(pullRequest))}
            </ul>`}
      </section>
    `;
  }

  #renderPullRequest(pullRequest: ProtocolPrInfo) {
    const url = safeSessionPrHref(pullRequest.url);
    const summaries = [
      mergeabilitySummary(pullRequest),
      checkSummary(pullRequest),
      reviewSummary(pullRequest),
    ];
    return html`
      <li class="session-info-pr-row">
        <div class="session-info-pr-copy">
          <strong>#${pullRequest.number} ${pullRequest.title}</strong>
          <small>${[pullRequest.repository, pullRequest.host].filter(Boolean).join(" · ")}</small>
          <span class="session-info-pr-statuses">
            ${summaries.map((summary) => this.#renderPrSummary(summary))}
          </span>
        </div>
        <button
          class="session-info-icon-button"
          type="button"
          title="Open pull request"
          aria-label=${`Open pull request #${pullRequest.number}`}
          ?disabled=${url === undefined}
          @click=${() => {
            if (url !== undefined) this.#openExternal(url);
          }}
        >${fontAwesomeIcon("arrow-up-right-from-square")}</button>
      </li>
    `;
  }

  #renderPrSummary(summary: PrSummary) {
    return html`<span class="session-info-status ${summary.tone}">${summary.label}</span>`;
  }

  #renderMcpServers() {
    const active = this.#mcpServers.filter(
      (server) => sessionMcpAvailability(server).active,
    );
    const toolCounts = active
      .map(sessionMcpToolCount)
      .filter((count): count is number => count !== undefined);
    const knownToolCount = toolCounts.reduce((total, count) => total + count, 0);
    const summary = active.length > 0 && toolCounts.length === active.length
      ? `${plural(knownToolCount, "tool")} across ${plural(active.length, "active server")}`
      : plural(active.length, "active server");
    return html`
      <section class="session-info-card" aria-labelledby="session-info-mcp-title">
        <header class="session-info-section-header">
          <div>
            <h3 id="session-info-mcp-title">MCP tools</h3>
            <p>${summary} for this branch.</p>
          </div>
        </header>
        ${this.#mcpError === ""
          ? nothing
          : html`<p class="session-info-notice error" role="alert">${this.#mcpError}</p>`}
        ${this.#mcpServers.length === 0
          ? html`<p class="session-info-empty">No MCP tools are configured for this session.</p>`
          : html`<ul class="session-info-mcp-list">
              ${this.#mcpServers.map((server) => this.#renderMcpServer(server))}
            </ul>`}
      </section>
    `;
  }

  #renderMcpServer(server: ProtocolMcpServerInfo) {
    const availability = sessionMcpAvailability(server);
    const detail = server.detail.trim();
    return html`
      <li class="session-info-mcp-row ${availability.tone}">
        ${fontAwesomeIcon("plug", { className: "session-info-mcp-icon" })}
        <span>
          <strong>${server.name}</strong>
          <small>${detail === "" ? `${server.scope} configuration` : detail}</small>
        </span>
        <span class="session-info-status ${availability.tone}">${availability.label}</span>
        <span class="session-info-scope">${server.scope}</span>
      </li>
    `;
  }

  async #refreshAll(): Promise<void> {
    await Promise.all([
      this.#refreshDiff(false),
      this.#refreshResources(),
    ]);
  }

  async #refreshDiff(silent: boolean): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "" || this.#diffRequestActive) return;
    const generation = this.#generation;
    let shouldRender = !silent;
    this.#diffRequestActive = true;
    if (!silent) {
      this.#diffError = "";
      this.requestUpdate();
    }
    try {
      const response = await services.protocol.sessionDiffSummary(sessionId);
      const manifest = JSON.stringify(response.files);
      if (this.#diffOverview !== undefined && manifest === this.#diffManifest) return;
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return;
      this.#diffOverview = {
        additions: response.additions,
        deletions: response.deletions,
        files: response.files.length,
      };
      this.#diffManifest = manifest;
      this.#diffError = "";
      shouldRender = true;
    } catch {
      if (generation === this.#generation && !silent) {
        this.#diffError = "The diff summary could not be refreshed.";
      }
    } finally {
      if (generation === this.#generation) {
        this.#diffRequestActive = false;
        if (shouldRender) this.requestUpdate();
      }
    }
  }

  async #refreshResources(): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "" || this.#resourcesRequestActive) return;
    const generation = this.#generation;
    this.#resourcesRequestActive = true;
    this.#mcpError = "";
    this.requestUpdate();
    const mcpResult = await Promise.resolve(
      services.protocol.sessionMcpServers(sessionId),
    ).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason: unknown) => ({ status: "rejected" as const, reason }),
    );
    if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return;
    if (mcpResult.status === "fulfilled") {
      this.#mcpServers = mcpResult.value;
    } else {
      this.#mcpError = "The effective MCP configuration could not be loaded.";
    }
    this.#resourcesRequestActive = false;
    this.requestUpdate();
  }

  readonly #checkpointRestored = (): void => {
    void this.#refreshDiff(false);
  };

  readonly #openDiff = (): void => {
    this.#openInspection("diff");
  };

  readonly #openPullRequests = (): void => {
    this.#openInspection("pr");
  };

  #openInspection(inspection: "diff" | "pr"): void {
    const services = this.#services.value;
    if (services === undefined) return;
    const route = readSignal(services.router.route);
    if (route.kind !== "session" || route.sessionId !== this.#effectiveSessionId) return;
    services.router.navigate({ ...route, inspection });
  }

  #openExternal(href: string): void {
    this.dispatchEvent(new CustomEvent<{ readonly href: string }>(
      "trouve-open-external",
      {
        detail: { href },
        bubbles: true,
        composed: true,
      },
    ));
  }
}

if ("customElements" in globalThis && !customElements.get("trouve-session-info-panel")) {
  customElements.define("trouve-session-info-panel", TrouveSessionInfoPanel);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-session-info-panel": TrouveSessionInfoPanel;
  }
}
