import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  appStoreContext,
} from "../contexts/app-contexts.js";
import type {
  ProtocolLocalStatus,
  ProtocolSubscriptionHealth,
  ProtocolUsageSummary,
} from "../services/protocol-client.js";
import { withSignalTracking } from "../state/reactivity.js";
import {
  boundedSubscriptionUsage,
  subscriptionUsageTone,
} from "./model-health.js";
import {
  latestCompletedTurnDuration,
  localMemoryUtilization,
  sessionUsagePanelKind,
  type SessionUsagePanelKind,
  type UsageBreakdownRow,
  usageBreakdownRows,
  usageThroughput,
} from "./session-usage-model.js";
import { nextHorizontalTabIndex, rovingTabIndex } from "./tab-navigation.js";

type UsagePanelTab = "usage" | "thread" | "session";

const USAGE_PANEL_TABS = [
  ["usage", "Usage"],
  ["thread", "Thread"],
  ["session", "Session"],
] as const satisfies readonly (readonly [UsagePanelTab, string])[];

let nextSessionUsagePanelId = 0;

const formatCount = (value: number): string =>
  new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);

const formatBytes = (bytes: number): string => {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1,
  );
  return `${(bytes / 1024 ** exponent).toFixed(exponent < 3 ? 0 : 1)} ${units[exponent]}`;
};

export class TrouveSessionUsagePanel extends withSignalTracking(LitElement) {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
    threadId: { type: String, attribute: "thread-id" },
    model: { type: String },
    placeholder: { type: Boolean },
  };

  sessionId = "";
  threadId = "";
  model = "";
  placeholder = false;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });

  #generation = 0;
  #loadKey = "";
  #dataKey = "";
  #loading = false;
  #error = "";
  #health: ProtocolSubscriptionHealth | undefined;
  #sessionSummary: ProtocolUsageSummary | undefined;
  #threadSummary: ProtocolUsageSummary | undefined;
  #localStatus: ProtocolLocalStatus | undefined;
  #activeTab: UsagePanelTab = "usage";
  readonly #idPrefix = `session-usage-${nextSessionUsagePanelId += 1}`;

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  protected override updated(_changed: PropertyValues<this>): void {
    const store = this.#store.value;
    const usageCursor = this.threadId === ""
      ? 0
      : store?.threadView(this.threadId).lastUsageCursor ?? 0;
    const sessionUsageRevision = store?.sessionUsageRevision(this.sessionId) ?? 0;
    const key = [
      this.sessionId,
      this.threadId,
      this.model,
      this.placeholder ? "placeholder" : "active",
      String(usageCursor),
      String(sessionUsageRevision),
    ].join("|");
    if (key === this.#loadKey) return;
    this.#loadKey = key;
    void this.#load();
  }

  override render() {
    const kind = sessionUsagePanelKind({
      placeholder: this.placeholder,
      sessionId: this.sessionId,
      threadId: this.threadId,
      model: this.model,
      hasSubscriptionHealth: this.#health !== undefined,
    });
    const placeholder = kind === "placeholder";
    return html`
      <section class="session-usage-box" aria-label="Usage details">
        <header class="session-usage-heading">
          ${placeholder
            ? html`<strong>Usage</strong>`
            : html`${this.#renderTabs()}<small>${this.model}</small>`}
        </header>
        ${placeholder
          ? html`<p class="session-usage-placeholder">
              Subscription and model usage details will show here once a session is started.
            </p>`
          : this.#renderActive(kind)}
      </section>
    `;
  }

  #renderActive(kind: Exclude<SessionUsagePanelKind, "placeholder">) {
    if (this.#loading) {
      return html`<div
        id=${`${this.#idPrefix}-panel`}
        class="session-usage-tab-panel"
        role="tabpanel"
        aria-labelledby=${`${this.#idPrefix}-tab-${this.#activeTab}`}
      ><p class="session-usage-placeholder" role="status">Loading usage details…</p></div>`;
    }
    return html`<div
      id=${`${this.#idPrefix}-panel`}
      class="session-usage-tab-panel"
      role="tabpanel"
      aria-labelledby=${`${this.#idPrefix}-tab-${this.#activeTab}`}
    >${this.#error !== "" && this.#hasData()
        ? html`<p class="tone-warning" role="status">${this.#error}</p>`
        : nothing}${this.#renderActiveTab(kind)}</div>`;
  }

  #hasData(): boolean {
    return this.#health !== undefined
      || this.#sessionSummary !== undefined
      || this.#threadSummary !== undefined
      || this.#localStatus !== undefined;
  }

  #renderTabs() {
    const selectedIndex = USAGE_PANEL_TABS.findIndex(([tab]) => tab === this.#activeTab);
    return html`
      <nav class="session-usage-tabs" role="tablist" aria-label="Usage views">
        ${USAGE_PANEL_TABS.map(([tab, label], index) => html`
          <button
            id=${`${this.#idPrefix}-tab-${tab}`}
            type="button"
            role="tab"
            aria-selected=${this.#activeTab === tab ? "true" : "false"}
            aria-controls=${`${this.#idPrefix}-panel`}
            tabindex=${rovingTabIndex(index, selectedIndex, USAGE_PANEL_TABS.length)}
            @keydown=${(event: KeyboardEvent) => this.#selectTabWithKeyboard(event, index)}
            @click=${() => this.#selectTab(tab)}
          >${label}</button>
        `)}
      </nav>
    `;
  }

  #selectTabWithKeyboard(event: KeyboardEvent, index: number): void {
    const target = nextHorizontalTabIndex(event.key, index, USAGE_PANEL_TABS.length);
    if (target === undefined) return;
    const tab = USAGE_PANEL_TABS[target];
    if (tab === undefined) return;
    event.preventDefault();
    this.#selectTab(tab[0]);
    void this.updateComplete.then(() => {
      this.querySelectorAll<HTMLButtonElement>('.session-usage-tabs [role="tab"]')[target]?.focus();
    });
  }

  #selectTab(tab: UsagePanelTab): void {
    this.#activeTab = tab;
    this.requestUpdate();
  }

  #renderActiveTab(kind: Exclude<SessionUsagePanelKind, "placeholder">) {
    if (this.#activeTab === "thread") {
      return this.#renderUsageScope("Active thread", this.#threadSummary);
    }
    if (this.#activeTab === "session") {
      return this.#renderUsageScope("Session", this.#sessionSummary);
    }
    if (kind === "local") {
      if (
        this.#localStatus === undefined
        && this.#sessionSummary === undefined
        && this.#threadSummary === undefined
      ) {
        return html`<p class="session-usage-placeholder" role="status">
          ${this.#error || "Usage details are not available yet."}
        </p>`;
      }
      return this.#renderLocal();
    }
    if (kind === "subscription" && this.#health !== undefined) {
      return this.#renderSubscription(this.#health);
    }
    if (kind === "api" && (this.#sessionSummary !== undefined || this.#threadSummary !== undefined)) {
      return html`<p class="session-usage-placeholder">
        Provider usage is unavailable for this model. Thread and session totals are available in their tabs.
      </p>`;
    }
    return html`<p class="session-usage-placeholder" role="status">
      ${this.#error || "Usage details are not available yet."}
    </p>`;
  }

  #renderSubscription(health: ProtocolSubscriptionHealth) {
    return html`
      <article class="session-usage-content" aria-label=${`${health.provider_id} subscription usage`}>
        <div class="session-usage-meta">
          <span>${health.plan === "" ? "Subscription" : `${health.plan} plan`}</span>
          ${health.credits === "" ? nothing : html`<small>${health.credits}</small>`}
        </div>
        ${health.note === ""
          ? nothing
          : html`<p class=${health.status === "unavailable" ? "tone-warning" : ""}>${health.note}</p>`}
        <div class="session-usage-lines">
          ${health.windows.map((window) => {
            const percent = Math.round(boundedSubscriptionUsage(window.used_percent));
            const tone = subscriptionUsageTone(percent);
            return html`
              <div class="session-usage-line">
                <div class="session-usage-line-copy">
                  <span>${window.label}</span>
                  <small class=${tone === "error" ? "tone-error" : ""}>
                    ${percent}% used${window.resets === "" ? "" : ` · ${window.resets}`}
                  </small>
                </div>
                ${this.#renderMeter(window.label, percent, tone)}
              </div>
            `;
          })}
        </div>
      </article>
    `;
  }

  #renderUsageScope(label: string, summary: ProtocolUsageSummary | undefined) {
    if (summary === undefined) {
      return html`<section class="session-usage-scope">
        <div class="session-usage-scope-heading"><strong>${label}</strong></div>
        <p class="session-usage-placeholder">Usage is unavailable.</p>
      </section>`;
    }
    const rows = usageBreakdownRows(summary);
    return html`
      <section class="session-usage-scope">
        <div class="session-usage-scope-heading">
          <strong>${label}</strong>
          <small>${summary.turns} ${summary.turns === 1 ? "turn" : "turns"}</small>
        </div>
        ${rows.length === 0
          ? html`<p class="session-usage-placeholder">No completed usage yet.</p>`
          : html`<div class="session-usage-breakdown">
              ${rows.map((row) => this.#renderUsageRow(row))}
            </div>`}
      </section>
    `;
  }

  #renderUsageRow(row: UsageBreakdownRow) {
    return html`
      <div class=${`session-usage-model-row ${row.total ? "total" : ""}`}>
        <div class="session-usage-model-heading">
          <span>${row.label}</span>
          <small>${row.turns} ${row.turns === 1 ? "turn" : "turns"}</small>
        </div>
        <dl class="session-usage-model-stats">
          <div><dt>Input</dt><dd>${formatCount(row.input_tokens)}</dd></div>
          <div><dt>Cached</dt><dd>${formatCount(row.cached_input_tokens)}</dd></div>
          <div><dt>Output</dt><dd>${formatCount(row.output_tokens)}</dd></div>
          <div><dt>Cost</dt><dd>$${row.cost_usd.toFixed(4)}</dd></div>
        </dl>
      </div>
    `;
  }

  #renderLocal() {
    const status = this.#localStatus;
    const modelId = this.model.slice("local/".length);
    const model = status?.models.find((candidate) => candidate.id === modelId);
    const running = status?.running_model === modelId;
    const gpuCapacity = Math.max(0, ...(status?.gpus.map((gpu) => gpu.vram_bytes) ?? []));
    const capacity = model?.fit === "gpu" && gpuCapacity > 0
      ? gpuCapacity
      : status?.ram_bytes ?? 0;
    const memoryPercent = localMemoryUtilization(model?.size_bytes ?? 0, capacity);
    const currentMemoryPercent = running ? memoryPercent : 0;
    const view = this.#store.value?.threadView(this.threadId);
    const usage = view?.lastUsage;
    const duration = view === undefined || view.turnRunning
      ? undefined
      : latestCompletedTurnDuration(view.turnDurationMs);
    const throughput = usageThroughput(usage?.output_tokens ?? 0, duration);
    return html`
      <article class="session-usage-content" aria-label="Local model utilization and performance">
        ${status === undefined
          ? html`<p>Local resource status is unavailable.</p>`
          : html`
              <div class="session-usage-meta">
                <span>${running ? "Running locally" : "Local model"}</span>
                <small>${status.server_status || "stopped"}</small>
              </div>
              <dl class="session-usage-stats">
                <div><dt>Model footprint</dt><dd>${formatBytes(model?.size_bytes ?? 0)}</dd></div>
                <div><dt>Memory tier</dt><dd>${model?.fit === "gpu" ? "GPU / VRAM" : "CPU / RAM"}</dd></div>
              </dl>
              ${this.#renderMeter("Current model memory utilization", currentMemoryPercent, currentMemoryPercent >= 90 ? "error" : currentMemoryPercent >= 70 ? "warning" : "ok")}
            `}
        <dl class="session-usage-stats">
          <div><dt>Prompt processed</dt><dd>${formatCount((usage?.input_tokens ?? 0) + (usage?.cached_input_tokens ?? 0))} tokens</dd></div>
          <div><dt>Last output</dt><dd>${formatCount(usage?.output_tokens ?? 0)} tokens</dd></div>
          <div><dt>Overall throughput</dt><dd>${throughput === undefined ? "Not available" : `${throughput.toFixed(1)} tok/s`}</dd></div>
          <div><dt>Last turn</dt><dd>${duration === undefined ? "Not available" : `${(duration / 1_000).toFixed(1)}s`}</dd></div>
        </dl>
      </article>
    `;
  }

  #renderMeter(label: string, percent: number, tone: "ok" | "warning" | "error") {
    return html`
      <div
        class="session-usage-meter"
        role="progressbar"
        aria-label=${`${label}: ${percent}%`}
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow=${String(percent)}
      ><span class=${`tone-${tone} ${percent > 0 ? "nonzero" : ""}`} style=${`width:${percent}%`}></span></div>
    `;
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    const placeholder = this.placeholder
      || this.sessionId === ""
      || this.threadId === ""
      || this.model === "";
    const generation = ++this.#generation;
    const dataKey = placeholder
      ? ""
      : [this.sessionId, this.threadId, this.model].join("|");
    if (dataKey !== this.#dataKey) {
      this.#dataKey = dataKey;
      this.#health = undefined;
      this.#sessionSummary = undefined;
      this.#threadSummary = undefined;
      this.#localStatus = undefined;
    }
    this.#error = "";
    if (placeholder || services === undefined) {
      if (!placeholder) this.#loadKey = "";
      this.#loading = false;
      this.requestUpdate();
      return;
    }
    this.#loading = !this.#hasData();
    this.requestUpdate();
    try {
      if (this.model.startsWith("local/")) {
        const [localStatusResult, sessionResult, threadResult] = await Promise.allSettled([
          services.protocol.localStatus(),
          services.protocol.sessionUsage(this.sessionId),
          services.protocol.threadUsage(this.threadId),
        ]);
        if (generation !== this.#generation) return;
        if (localStatusResult.status === "fulfilled") {
          this.#localStatus = localStatusResult.value;
        }
        if (sessionResult.status === "fulfilled") {
          this.#sessionSummary = sessionResult.value;
        }
        if (threadResult.status === "fulfilled") {
          this.#threadSummary = threadResult.value;
        }
        if ([localStatusResult, sessionResult, threadResult].every(
          (result) => result.status === "rejected",
        )) throw new Error("local usage refresh failed");
      } else {
        const providerId = this.model.split("/", 1)[0] ?? "";
        const [healthResult, sessionResult, threadResult] = await Promise.allSettled([
          services.subscriptionHealth.refresh("if-stale"),
          services.protocol.sessionUsage(this.sessionId),
          services.protocol.threadUsage(this.threadId),
        ]);
        if (generation !== this.#generation) return;
        if (healthResult.status === "fulfilled") {
          this.#health = healthResult.value.find(
            (candidate) => candidate.provider_id === providerId,
          );
          if (this.#health === undefined || this.#health.status === "unsupported") {
            this.#health = undefined;
          }
        }
        if (sessionResult.status === "fulfilled") {
          this.#sessionSummary = sessionResult.value;
        }
        if (threadResult.status === "fulfilled") {
          this.#threadSummary = threadResult.value;
        }
        if ([healthResult, sessionResult, threadResult].every(
          (result) => result.status === "rejected",
        )) throw new Error("usage refresh failed");
      }
    } catch {
      if (generation !== this.#generation) return;
      this.#error = "Usage refresh failed.";
    } finally {
      if (generation === this.#generation) {
        this.#loading = false;
        this.requestUpdate();
      }
    }
  }
}

customElements.define("trouve-session-usage-panel", TrouveSessionUsagePanel);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-session-usage-panel": TrouveSessionUsagePanel;
  }
}
