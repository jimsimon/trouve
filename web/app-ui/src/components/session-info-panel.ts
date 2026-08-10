import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  appStoreContext,
  sessionContext,
  threadContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import {
  type ProtocolAgentMode,
  type ProtocolMcpServerInfo,
  type ProtocolPrInfo,
  type ProtocolThread,
  type ProtocolThreadStatus,
  type ProtocolTodoItem,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  sessionIndicatorPresentation,
  type SessionIndicatorPresentation,
} from "../state/session-indicator-model.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import { MCP_CONFIG_CHANGED_EVENT } from "./session-mcp-model.js";
import "./turn-metadata.js";
import {
  checkSummary,
  mergeabilitySummary,
  reviewSummary,
  safeSessionPrHref,
  type PrSummary,
} from "./session-pr-panel-model.js";
import { buildTodoPlanModel } from "./todo-plan-model.js";
import { subagentThreadIsReadOnly } from "./subagent-access.js";
import { threadNavigationTitle } from "./thread-title.js";

const DIFF_REFRESH_MS = 2_000;
const RESOURCE_REFRESH_MS = 30_000;

export interface SessionDiffOverview {
  readonly additions: number;
  readonly deletions: number;
  readonly files: number;
}

export interface McpBadgePresentation {
  readonly label: string;
  readonly tone: "active" | "muted" | "warning" | "failed";
}

export interface McpAvailability {
  readonly enablement: McpBadgePresentation;
  readonly health: McpBadgePresentation;
  readonly tone: McpBadgePresentation["tone"];
  readonly active: boolean;
}

interface ThreadSubagentOverview {
  readonly id: string;
  readonly sessionId: string;
  readonly title: string;
  readonly readOnly: boolean;
  readonly model: string;
  readonly indicator: SessionIndicatorPresentation;
  readonly active: boolean;
  readonly startedAt: string;
  readonly durationMs: number | undefined;
}

export const completedThreadDurationMs = (
  status: Pick<ProtocolThreadStatus, "started_at" | "completed_at"> | undefined,
): number | undefined => {
  const started = Date.parse(status?.started_at ?? "");
  const completed = Date.parse(status?.completed_at ?? "");
  if (!Number.isFinite(started) || !Number.isFinite(completed)) return undefined;
  return Math.max(0, completed - started);
};

export const sessionMcpAvailability = (
  server: Pick<ProtocolMcpServerInfo, "enabled" | "health" | "scope">,
): McpAvailability => {
  const enabled = server.enabled !== false && server.health !== "disabled";
  const enablement: McpBadgePresentation = enabled
    ? { label: "Enabled", tone: "active" }
    : { label: "Disabled", tone: "muted" };
  const health: McpBadgePresentation = server.health === "ok"
    ? { label: "Ready", tone: "active" }
    : server.health === "error"
        || server.health === "untrusted"
        || server.scope !== "app-wide"
      ? { label: "Error", tone: "failed" }
      : { label: "Unknown", tone: "muted" };
  return {
    enablement,
    health,
    tone: enabled ? health.tone : "muted",
    active: enabled && health.label === "Ready",
  };
};

export const mcpServersNeedHealthReconciliation = (
  servers: readonly Pick<ProtocolMcpServerInfo, "enabled" | "health" | "scope">[],
): boolean => servers.some((server) =>
  server.enabled !== false
  && server.health === "unknown"
  && server.scope === "app-wide"
);

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
    .visually-hidden { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); clip-path: inset(50%); white-space: nowrap; }
    h2, h3, p { margin: 0; }
    button { color: var(--trouve-text-mid); background: var(--trouve-control-bg); font: inherit; }
    button:not(:disabled) { cursor: pointer; }
    .session-info-surface { height: 100%; display: flex; flex-direction: column; gap: 8px; overflow: auto; padding: 10px; }
    .session-info-section-header { min-width: 0; display: flex; align-items: flex-start; gap: 10px; }
    .session-info-section-header > div { min-width: 0; flex: 1; }
    .session-info-card h3 { color: var(--trouve-text-hi); font-size: 13px; }
    .session-info-section-header p { margin-top: 3px; color: var(--trouve-text-dim); font-size: 11px; line-height: 1.35; }
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
    .session-info-session-groups,
    .session-info-thread-groups { display: grid; gap: 8px; }
    .session-info-session-group,
    .session-info-thread-group { min-width: 0; display: grid; gap: 6px; }
    .session-info-session-group + .session-info-session-group,
    .session-info-thread-group + .session-info-thread-group { border-top: 1px solid var(--trouve-rule); padding-top: 8px; }
    .session-info-thread-group-header { min-width: 0; display: flex; align-items: center; gap: 7px; }
    .session-info-thread-group-header > div { min-width: 0; flex: 1; display: flex; align-items: baseline; gap: 6px; }
    .session-info-thread-group-header strong { color: var(--trouve-text-hi); font-size: 11px; }
    .session-info-thread-group-header small { color: var(--trouve-text-dim); font-size: 9px; }
    .session-info-thread-group-header button { min-height: 26px; border: 0; border-radius: var(--trouve-radius-sm); padding: 3px 6px; background: transparent; font-size: 10px; }
    .session-info-thread-group-header button:hover, .session-info-thread-group-header button:focus-visible { color: var(--trouve-text-hi); background: var(--trouve-hover-bg); }
    .session-info-todo-list, .session-info-subagent-list { display: grid; gap: 1px; margin: 0; padding: 0; background: var(--trouve-rule); list-style: none; }
    .session-info-todo-row { min-width: 0; display: grid; grid-template-columns: 14px minmax(0, 1fr); align-items: center; gap: 7px; padding: 7px 8px; background: var(--trouve-inset-bg); }
    .session-info-todo-row span { overflow: hidden; color: var(--trouve-text); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-todo-row.completed .trouve-icon { color: var(--trouve-ok); }
    .session-info-todo-row.in_progress .trouve-icon { color: var(--trouve-accent); }
    .session-info-todo-row.cancelled .trouve-icon { color: var(--trouve-err); }
    .session-info-subagent-row { min-width: 0; }
    .session-info-subagent-row button { width: 100%; min-width: 0; display: grid; grid-template-columns: 14px minmax(0, 1fr) 14px; align-items: center; gap: 8px; border: 0; padding: 8px; background: var(--trouve-inset-bg); text-align: start; }
    .session-info-subagent-row button:hover, .session-info-subagent-row button:focus-visible { color: var(--trouve-text-hi); background: var(--trouve-hover-bg); }
    .session-info-subagent-indicator { width: 14px; height: 18px; display: grid; place-items: center; border-radius: 3px; color: transparent; line-height: 1; }
    .session-info-subagent-indicator.approval,
    .session-info-subagent-indicator.question,
    .session-info-subagent-indicator.both { color: var(--trouve-warn); font-size: 10px; }
    .session-info-subagent-indicator.error { color: var(--trouve-err); font-size: 12px; }
    .session-info-subagent-indicator.unread { color: var(--trouve-accent); font-size: 7px; }
    .session-info-subagent-indicator.busy::before { width: 8px; height: 8px; border-radius: 50%; background: var(--trouve-accent); opacity: .55; animation: trouve-subagent-busy-pulse 1.6s linear infinite; content: ""; }
    .session-info-subagent-copy { min-width: 0; display: grid; gap: 2px; }
    .session-info-subagent-copy strong, .session-info-subagent-copy small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-subagent-copy strong { color: var(--trouve-text-hi); font-size: 11px; }
    .session-info-subagent-copy small { display: flex; min-width: 0; gap: 4px; color: var(--trouve-text-dim); font-size: 9px; }
    .session-info-subagent-copy small > span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-subagent-duration { flex: none; font-variant-numeric: tabular-nums; }
    .session-info-pr-list, .session-info-mcp-list { display: grid; gap: 1px; margin: 0; padding: 0; background: var(--trouve-rule); list-style: none; }
    .session-info-pr-row { min-width: 0; display: flex; align-items: flex-start; gap: 8px; padding: 9px; background: var(--trouve-inset-bg); }
    .session-info-pr-copy { min-width: 0; flex: 1; display: grid; gap: 4px; }
    .session-info-pr-copy > strong { color: var(--trouve-text-hi); font-size: 12px; line-height: 1.35; overflow-wrap: anywhere; }
    .session-info-pr-copy > small { overflow: hidden; color: var(--trouve-text-dim); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-pr-statuses { display: flex; flex-wrap: wrap; gap: 4px; }
    .session-info-status { display: inline-flex; width: fit-content; border-radius: 999px; padding: 2px 6px; color: var(--trouve-text-dim); background: var(--trouve-pill-bg); font-size: 9px; white-space: nowrap; }
    .session-info-status.ready, .session-info-status.active { color: var(--trouve-ok); }
    .session-info-status.pending, .session-info-status.warning { color: var(--trouve-warn); }
    .session-info-status.failed { color: var(--trouve-err); }
    .session-info-mcp-row { min-width: 0; display: grid; grid-template-columns: 18px minmax(0, 1fr) auto; align-items: center; gap: 7px; padding: 8px 9px; background: var(--trouve-inset-bg); }
    .session-info-mcp-row > span:nth-child(2) { min-width: 0; display: grid; gap: 2px; }
    .session-info-mcp-row strong, .session-info-mcp-row small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .session-info-mcp-row strong { color: var(--trouve-text-hi); font-size: 12px; }
    .session-info-mcp-row small { color: var(--trouve-text-dim); font-size: 10px; }
    .session-info-mcp-statuses { display: flex; align-items: center; justify-content: flex-end; gap: 4px; }
    .session-info-mcp-icon { color: var(--trouve-text-dim); }
    .session-info-mcp-row.active .session-info-mcp-icon { color: var(--trouve-ok); }
    .session-info-mcp-row.warning .session-info-mcp-icon { color: var(--trouve-warn); }
    .session-info-mcp-row.failed .session-info-mcp-icon { color: var(--trouve-err); }
    .session-info-mcp-row.muted { opacity: .62; }
    .trouve-icon-spin { animation: trouve-session-info-spin 900ms linear infinite; }
    :host-context([data-reduce-motion]) .trouve-icon-spin { animation: none; }
    @keyframes trouve-session-info-spin { to { transform: rotate(360deg); } }
    @keyframes trouve-subagent-busy-pulse {
      0%, 50%, 100% { opacity: .55; }
      25% { opacity: 1; }
      75% { opacity: .1; }
    }
    @media (prefers-reduced-motion: reduce) { .trouve-icon-spin { animation: none; } }
    @media (max-width: 760px) {
      .session-info-surface { padding: 8px; }
      .session-info-card { padding: 10px; }
      .session-info-section-header button, .session-info-icon-button { min-height: 44px; }
      .session-info-icon-button { width: 44px; }
      .session-info-mcp-statuses { grid-column: 2; justify-content: flex-start; }
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
  readonly #threadScope = new ContextConsumer(this, {
    context: threadContext,
    subscribe: true,
  });

  #observedServices: AppServices | undefined;
  #observedSessionId = "";
  #observedThreadId = "";
  #generation = 0;
  #diffRequestActive = false;
  #resourcesRequestActive = false;
  #resourceRefreshQueued = false;
  #mcpHealthReconcileActive = false;
  #mcpHealthReconcileQueued = false;
  #diffOverview: SessionDiffOverview | undefined;
  #diffManifest = "";
  #mcpServers: readonly ProtocolMcpServerInfo[] = [];
  #modes: readonly ProtocolAgentMode[] = [];
  #subagents: readonly ProtocolThread[] = [];
  #diffError = "";
  #mcpError = "";
  #subagentError = "";
  #loadScheduled = false;
  #diffRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #resourceRefreshTimer: ReturnType<typeof setInterval> | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    globalThis.addEventListener(
      "trouve-checkpoint-restored",
      this.#checkpointRestored,
    );
    globalThis.addEventListener(MCP_CONFIG_CHANGED_EVENT, this.#mcpConfigChanged);
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
    this.#resourceRefreshQueued = false;
    this.#mcpHealthReconcileQueued = false;
    globalThis.removeEventListener(
      "trouve-checkpoint-restored",
      this.#checkpointRestored,
    );
    globalThis.removeEventListener(MCP_CONFIG_CHANGED_EVENT, this.#mcpConfigChanged);
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
    const threadId = this.#effectiveThreadId;
    const servicesChanged = services !== this.#observedServices;
    const sessionChanged = sessionId !== this.#observedSessionId;
    const threadChanged = threadId !== this.#observedThreadId;
    if (
      !changed.has("sessionId")
      && !servicesChanged
      && !sessionChanged
      && !threadChanged
    ) return;
    this.#observedServices = services;
    if (servicesChanged || sessionChanged || threadChanged) {
      this.#generation += 1;
      this.#observedSessionId = sessionId;
      this.#observedThreadId = threadId;
      this.#diffRequestActive = false;
      this.#resourcesRequestActive = false;
      this.#resourceRefreshQueued = false;
      this.#mcpHealthReconcileQueued = false;
      this.#diffOverview = undefined;
      this.#diffManifest = "";
      this.#mcpServers = [];
      this.#modes = [];
      this.#subagents = [];
      this.#diffError = "";
      this.#mcpError = "";
      this.#subagentError = "";
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

  get #effectiveThreadId(): string {
    return this.#threadScope.value?.threadId || "";
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
    const threadId = this.#effectiveThreadId;
    const sessionThreads = store?.threadsForSession(sessionId) ?? [];
    const thread = threadId === "" ? undefined : store?.thread(threadId);
    const threadTitle = thread === undefined
      ? "Current thread"
      : threadNavigationTitle({
          thread,
          sessionTitle: title,
          initialThreadId: sessionThreads[0]?.id,
        });
    const todos = threadId === "" || store === undefined
      ? []
      : store.threadView(threadId).todos;
    const subagents = new Map<string, ThreadSubagentOverview>();
    const subagentOverview = (
      child: {
        readonly id: ProtocolThread["id"];
        readonly session_id: ProtocolThread["session_id"];
        readonly title?: ProtocolThread["title"] | undefined;
        readonly mode: ProtocolThread["mode"];
        readonly model: ProtocolThread["model"];
      },
      fallbackTitle: string,
    ): ThreadSubagentOverview => {
      const status = store?.threadStatus(child.id);
      return {
        id: child.id,
        sessionId: child.session_id,
        title: child.title?.trim() || fallbackTitle,
        readOnly: subagentThreadIsReadOnly(
          { spawned: true, mode: child.mode },
          this.#modes,
        ),
        model: child.model,
        indicator: sessionIndicatorPresentation(
          store?.threadIndicatorState(child.id)
            ?? { active: false, attention: "none", outcome: "idle", unread: false },
        ),
        active: status?.active ?? false,
        startedAt: status?.started_at ?? "",
        durationMs: completedThreadDurationMs(status),
      };
    };
    for (const child of this.#subagents) {
      subagents.set(child.id, subagentOverview(child, `Subagent: ${child.model}`));
    }
    if (threadId !== "" && store !== undefined) {
      for (const item of store.threadView(threadId).items) {
        if (item.kind !== "subagent" || subagents.has(item.threadId)) continue;
        const child = store.thread(item.threadId);
        const prompt = item.prompt.trim().replaceAll(/\s+/gu, " ");
        subagents.set(item.threadId, subagentOverview({
          id: item.threadId,
          session_id: item.sessionId,
          title: child?.title,
          mode: child?.mode ?? "",
          model: child?.model ?? item.model,
        }, `Subagent: ${prompt || item.model}`));
      }
    }
    const refreshing = this.#diffRequestActive || this.#resourcesRequestActive;
    return html`
      <section
        class="session-info-surface"
        aria-labelledby="session-info-title"
        aria-busy=${refreshing ? "true" : "false"}
      >
        ${this.#renderSessionOverview(title, branch, pullRequests)}
        ${this.#renderThreadOverview(
          threadTitle,
          todos,
          [...subagents.values()],
        )}
        ${this.#renderMcpServers()}
      </section>
    `;
  }

  #renderSessionOverview(
    title: string,
    branch: string,
    pullRequests: readonly ProtocolPrInfo[],
  ) {
    return html`
      <section class="session-info-card" aria-labelledby="session-info-title">
        <header class="session-info-section-header">
          <div>
            <h3 id="session-info-title">Session overview</h3>
            <p>Branch activity, pull requests, and tools available to this session.</p>
          </div>
        </header>
        <div class="session-info-session-groups">
          <section class="session-info-session-group session-info-identity" aria-label="Session details">
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
        </div>
      </section>
    `;
  }

  #renderThreadOverview(
    threadTitle: string,
    todos: readonly ProtocolTodoItem[],
    subagents: readonly ThreadSubagentOverview[],
  ) {
    const plan = buildTodoPlanModel(todos);
    return html`
      <section class="session-info-card" aria-labelledby="session-info-thread-title">
        <header class="session-info-section-header">
          <div>
            <h3 id="session-info-thread-title">Thread overview</h3>
            <p title=${threadTitle}>TODOs and subagents for ${threadTitle}.</p>
          </div>
        </header>
        <div class="session-info-thread-groups">
          <section class="session-info-thread-group" aria-labelledby="session-info-todos-title">
            <header class="session-info-thread-group-header">
              <div>
                <strong id="session-info-todos-title">TODOs</strong>
                ${plan.total === 0
                  ? nothing
                  : html`<small>${plan.progressLabel}</small>`}
              </div>
            </header>
            ${plan.rows.length === 0
              ? html`<p class="session-info-empty">No TODOs are defined for this thread.</p>`
              : html`<ol class="session-info-todo-list">
                  ${plan.rows.map((todo) => html`
                    <li
                      class=${`session-info-todo-row ${todo.status}`}
                      title=${todo.content}
                      data-todo-id=${todo.id}
                    >
                      ${fontAwesomeIcon(todo.icon)}
                      <span>${todo.content}</span>
                      <span class="visually-hidden">Status: ${todo.statusLabel}</span>
                    </li>
                  `)}
                </ol>`}
          </section>
          <section class="session-info-thread-group" aria-labelledby="session-info-subagents-title">
            <header class="session-info-thread-group-header">
              <div>
                <strong id="session-info-subagents-title">Subagents</strong>
                ${subagents.length === 0
                  ? nothing
                  : html`<small>${plural(subagents.length, "subagent")}</small>`}
              </div>
            </header>
            ${this.#subagentError === ""
              ? nothing
              : html`<p class="session-info-notice error" role="alert">${this.#subagentError}</p>`}
            ${subagents.length === 0
              ? html`<p class="session-info-empty">No subagents have been spawned from this thread.</p>`
              : html`<ul class="session-info-subagent-list">
                  ${subagents.map((subagent) => html`
                    <li class="session-info-subagent-row">
                      <button
                        type="button"
                        title=${subagent.title}
                        aria-label=${`Open ${subagent.title}`}
                        @click=${() => this.#openSubagent(subagent)}
                      >
                        <span
                          class=${`session-info-subagent-indicator ${subagent.indicator.kind}`}
                          title=${subagent.indicator.tooltip || (subagent.active ? "Processing" : "")}
                          aria-hidden="true"
                        >${subagent.indicator.icon === undefined
                          ? nothing
                          : fontAwesomeIcon(subagent.indicator.icon)}</span>
                        <span class="session-info-subagent-copy">
                          <strong>${subagent.title}</strong>
                          <small>
                            <span>${subagent.readOnly ? "Read-only" : "Interactive"} · ${subagent.model}</span>
                            ${subagent.startedAt === "" || (!subagent.active && subagent.durationMs === undefined)
                              ? nothing
                              : html`<span class="session-info-subagent-duration">
                                  · <trouve-turn-metadata
                                    .running=${subagent.active}
                                    .startedAt=${subagent.startedAt}
                                    .durationMs=${subagent.durationMs}
                                  ></trouve-turn-metadata>
                                </span>`}
                          </small>
                          <span class="visually-hidden">${subagent.indicator.tooltip
                            || (subagent.active ? "Processing" : "Idle")}</span>
                        </span>
                        ${fontAwesomeIcon("arrow-right")}
                      </button>
                    </li>
                  `)}
                </ul>`}
          </section>
        </div>
      </section>
    `;
  }

  #renderChanges() {
    const summary = this.#diffOverview;
    return html`
      <section class="session-info-session-group" aria-labelledby="session-info-changes-title">
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
      <section class="session-info-session-group" aria-labelledby="session-info-pr-title">
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
    const description = detail === ""
      ? `${server.scope} configuration`
      : `${server.scope} · ${detail}`;
    return html`
      <li class="session-info-mcp-row ${availability.tone}">
        ${fontAwesomeIcon("plug", { className: "session-info-mcp-icon" })}
        <span>
          <strong>${server.name}</strong>
          <small>${description}</small>
        </span>
        <span class="session-info-mcp-statuses" aria-label="MCP server status">
          <span
            class="session-info-status ${availability.enablement.tone}"
            aria-label=${`Configuration: ${availability.enablement.label}`}
          >${availability.enablement.label}</span>
          <span
            class="session-info-status ${availability.health.tone}"
            aria-label=${`Health: ${availability.health.label}`}
          >${availability.health.label}</span>
        </span>
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
    const threadId = this.#effectiveThreadId;
    if (services === undefined || sessionId === "" || this.#resourcesRequestActive) return;
    const generation = this.#generation;
    this.#resourcesRequestActive = true;
    this.#mcpError = "";
    this.#subagentError = "";
    this.requestUpdate();
    const store = this.#store.value;
    const workspaceId = store?.session(sessionId)?.workspaceId
      ?? store?.sessionMetadata(sessionId)?.workspace_id;
    const [mcpResult, subagentResult, modeResult] = await Promise.all([
      Promise.resolve(services.protocol.sessionMcpServers(sessionId)).then(
        (value) => ({ status: "fulfilled" as const, value }),
        (reason: unknown) => ({ status: "rejected" as const, reason }),
      ),
      threadId === ""
        ? Promise.resolve({ status: "fulfilled" as const, value: [] })
        : Promise.resolve(services.protocol.threadSubagents(threadId)).then(
            (value) => ({ status: "fulfilled" as const, value }),
            (reason: unknown) => ({ status: "rejected" as const, reason }),
          ),
      this.#modes.length > 0
        ? Promise.resolve({ status: "fulfilled" as const, value: this.#modes })
        : Promise.resolve(services.protocol.modes(workspaceId)).then(
            (value) => ({ status: "fulfilled" as const, value }),
            (reason: unknown) => ({ status: "rejected" as const, reason }),
          ),
    ]);
    if (
      generation !== this.#generation
      || sessionId !== this.#effectiveSessionId
      || threadId !== this.#effectiveThreadId
    ) return;
    if (mcpResult.status === "fulfilled") {
      this.#mcpServers = mcpResult.value;
    } else {
      this.#mcpError = "The effective MCP configuration could not be loaded.";
    }
    if (subagentResult.status === "fulfilled") {
      this.#subagents = subagentResult.value;
    } else {
      this.#subagentError = "The complete subagent list could not be loaded.";
    }
    if (modeResult.status === "fulfilled") this.#modes = modeResult.value;
    this.#resourcesRequestActive = false;
    this.requestUpdate();
    if (mcpResult.status === "fulfilled") {
      void this.#reconcileUnknownMcpHealth();
    }
    if (this.#resourceRefreshQueued) {
      this.#resourceRefreshQueued = false;
      globalThis.queueMicrotask(() => void this.#refreshResources());
    }
  }

  async #reconcileUnknownMcpHealth(): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (
      services === undefined
      || sessionId === ""
      || !mcpServersNeedHealthReconciliation(this.#mcpServers)
    ) return;
    if (this.#mcpHealthReconcileActive) {
      this.#mcpHealthReconcileQueued = true;
      return;
    }
    const generation = this.#generation;
    const workspaceId = this.#store.value?.session(sessionId)?.workspaceId
      ?? this.#store.value?.sessionMetadata(sessionId)?.workspace_id;
    this.#mcpHealthReconcileActive = true;
    try {
      await services.protocol.mcpServers(workspaceId, true);
      const reconciled = await services.protocol.sessionMcpServers(sessionId);
      if (
        generation !== this.#generation
        || sessionId !== this.#effectiveSessionId
      ) return;
      this.#mcpServers = reconciled;
      this.#mcpError = "";
      this.requestUpdate();
    } catch {
      // Keep the state unknown. The bounded resource refresh will retry;
      // failed MCP handshakes themselves are returned as a conclusive error.
    } finally {
      this.#mcpHealthReconcileActive = false;
      if (this.#mcpHealthReconcileQueued) {
        this.#mcpHealthReconcileQueued = false;
        globalThis.queueMicrotask(() => void this.#reconcileUnknownMcpHealth());
      }
    }
  }

  readonly #checkpointRestored = (): void => {
    void this.#refreshDiff(false);
  };

  readonly #mcpConfigChanged = (): void => {
    if (this.#resourcesRequestActive) {
      this.#resourceRefreshQueued = true;
      return;
    }
    void this.#refreshResources();
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

  #openSubagent(subagent: ThreadSubagentOverview): void {
    const store = this.#store.value;
    const services = this.#services.value;
    if (store === undefined || services === undefined) return;
    const route = readSignal(services.router.route);
    if (route.kind !== "session") return;
    const childSession = store.session(subagent.sessionId);
    services.router.navigate({
      ...route,
      workspaceId: childSession?.workspaceId ?? route.workspaceId,
      sessionId: subagent.sessionId,
      threadId: subagent.id,
    });
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
