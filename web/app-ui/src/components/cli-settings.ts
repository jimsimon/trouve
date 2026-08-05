import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { appServicesContext } from "../contexts/app-contexts.js";
import { DownloadRateTracker } from "../services/download-rate.js";
import type {
  ProtocolCliInfo,
  ProtocolCliInstallStatus,
} from "../services/protocol-client.js";
import {
  CLI_POLL_INTERVAL_MS,
  cliIsInstalled,
  cliPrimaryActionLabel,
  cliProgressLabel,
  cliProgressPercent,
  cliSourceLabel,
  cliVersionLabel,
  idleCliInstallStatus,
  MAX_CLI_POLL_ATTEMPTS,
  pendingCliIds,
  shouldPollCliInstalls,
} from "./cli-settings-model.js";

const genericFailure = (action: string): string =>
  `${action} failed. Check the server connection, then retry.`;

export class TrouveCliSettings extends LitElement {
  static override styles = css`
    :host {
      display: block;
      width: 100%;
      color: var(--trouve-text);
      font: var(--trouve-font-size, 14px) / 1.45 var(--trouve-font-sans, system-ui);
    }
    * { box-sizing: border-box; }
    h2, h3, p, dl { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 16px; }
    h3 { color: var(--trouve-text-hi); font-size: 13px; }
    p, small { color: var(--trouve-text-dim); }
    button { font: inherit; }
    button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 10px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      cursor: pointer;
    }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button:disabled { cursor: not-allowed; opacity: 0.56; }
    button:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    button.primary {
      border-color: var(--trouve-primary-border, var(--trouve-accent));
      color: var(--trouve-on-accent, white);
      background: var(--trouve-primary-bg, var(--trouve-accent));
    }
    button.danger { border-color: var(--trouve-err); color: var(--trouve-err-soft, var(--trouve-err)); }

    .settings-stack { display: grid; gap: 8px; }
    .section-heading, .cli-heading, .actions, .status-row {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    .section-heading { align-items: flex-start; gap: 12px; }
    .section-heading > div, .cli-heading > div { min-width: 0; flex: 1; }
    .section-heading p { margin-top: 4px; }
    .cli-refresh-action { position: absolute; width: 1px; height: 1px; min-height: 0; overflow: hidden; padding: 0; clip: rect(0, 0, 0, 0); }
    .cli-refresh-action:focus-visible { position: static; width: auto; height: 30px; min-height: 30px; overflow: visible; padding: 4px 8px; clip: auto; }
    .settings-card {
      height: 130px;
      overflow: auto;
      padding: 0;
      border: 0;
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .notice { color: var(--trouve-text-dim); }
    .notice.error { color: var(--trouve-err); }

    .cli-list { display: grid; gap: 0; margin: 0; }
    .cli-row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 6px 8px;
      align-items: center;
      min-height: 42px;
      padding: 4px 6px 4px 10px;
      border: 0;
      border-radius: 0;
      background: transparent;
    }
    .cli-copy { min-width: 0; }
    .cli-heading strong {
      min-width: 0;
      overflow: hidden;
      color: var(--trouve-text-hi);
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .badges { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 5px; }
    .badge {
      max-width: 100%;
      min-height: 21px;
      padding: 2px 7px;
      overflow: hidden;
      border-radius: 999px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 10px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .badge.managed { color: var(--trouve-ok); }
    .badge.path { color: var(--trouve-text-accent-soft, var(--trouve-accent)); }
    .badge.update { color: var(--trouve-warn); }

    .metadata {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 7px;
      margin-top: 9px;
    }
    .metadata > div { min-width: 0; }
    .metadata dt {
      color: var(--trouve-text-dim);
      font-size: 9px;
      letter-spacing: 0.04em;
      text-transform: uppercase;
    }
    .metadata dd {
      margin: 2px 0 0;
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 11px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .path {
      grid-column: 1 / -1;
      margin-top: 7px;
      overflow-wrap: anywhere;
      color: var(--trouve-text-dim);
      font: 10px / 1.4 var(--trouve-font-mono, monospace);
    }
    .badges, .metadata, .path { display: none; }

    .actions { flex-wrap: wrap; justify-content: flex-end; }
    .progress-row {
      grid-column: 1 / -1;
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 5px 8px;
      padding-top: 8px;
      border-top: 1px solid var(--trouve-rule);
    }
    .progress-row small { overflow-wrap: anywhere; }
    progress {
      grid-column: 1 / -1;
      width: 100%;
      height: 8px;
      accent-color: var(--trouve-accent);
    }
    .install-error { grid-column: 1 / -1; color: var(--trouve-err); }

    .confirmation {
      grid-column: 1 / -1;
      padding: 9px;
      border: 1px solid var(--trouve-warn);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text-mid);
      background: var(--trouve-accent-veil);
    }
    .confirmation p { color: inherit; }
    .confirmation .actions { margin-top: 8px; }

    .empty {
      display: grid;
      justify-items: start;
      gap: 7px;
      min-height: 90px;
      align-content: center;
      color: var(--trouve-text-dim);
    }
    .empty strong { color: var(--trouve-text-hi); }

    @media (max-width: 680px) {
      .section-heading, .cli-row { grid-template-columns: 1fr; }
      .section-heading { flex-wrap: wrap; }
      .section-heading > button { width: 100%; }
      .actions { justify-content: stretch; }
      .actions button { flex: 1 1 auto; min-height: 44px; }
      .metadata { grid-template-columns: 1fr 1fr; }
      .path { grid-column: 1 / -1; }
      button { min-height: 44px; }
    }

    @media (max-width: 420px) {
      .metadata { grid-template-columns: 1fr; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });

  #clis: readonly ProtocolCliInfo[] | undefined;
  #statuses = new Map<string, ProtocolCliInstallStatus>();
  #loading = false;
  #busyId = "";
  #confirmUninstallId = "";
  #message = "";
  #error = false;
  #lifecycleGeneration = 0;
  #loadRequest = 0;
  #pollGeneration = 0;
  #pollAttempts = 0;
  #pollFailures = 0;
  #pollTimer: ReturnType<typeof setTimeout> | undefined;
  readonly #rateTracker = new DownloadRateTracker();
  readonly #downloadRates = new Map<string, number>();

  override connectedCallback(): void {
    super.connectedCallback();
    const lifecycle = ++this.#lifecycleGeneration;
    queueMicrotask(() => {
      if (this.isConnected && lifecycle === this.#lifecycleGeneration) void this.#load();
    });
  }

  override disconnectedCallback(): void {
    this.#lifecycleGeneration += 1;
    this.#loadRequest += 1;
    this.#stopPolling();
    this.#rateTracker.clear();
    this.#downloadRates.clear();
    this.#busyId = "";
    this.#confirmUninstallId = "";
    super.disconnectedCallback();
  }

  override render() {
    const clis = this.#clis;
    return html`
      <section class="settings-stack" aria-labelledby="cli-settings-heading" aria-busy=${this.#loading}>
        <header class="section-heading">
          <div>
            <h2 id="cli-settings-heading">Vendor CLIs</h2>
            <p>Agent backends (Cursor, Claude Code, Codex) run through the vendor's CLI. trouve can download and update these directly — managed installs live in trouve's data directory and take precedence over system packages.</p>
          </div>
          <button
            class="cli-refresh-action"
            type="button"
            ?disabled=${this.#loading || this.#busyId !== ""}
            aria-label="Refresh vendor CLI status"
            @click=${() => void this.#load()}
          >${this.#loading ? "Refreshing…" : "Refresh"}</button>
        </header>

        ${this.#message === ""
          ? nothing
          : html`<p class="notice ${this.#error ? "error" : ""}" role="status" aria-live="polite" aria-atomic="true">${this.#message}</p>`}

        <section class="settings-card" aria-label="Vendor CLI installations">
          ${clis === undefined
            ? this.#loading
              ? html`<div class="empty" role="status"><strong>Loading vendor CLIs…</strong><span>Checking installed sources and available versions.</span></div>`
              : html`<div class="empty" role="alert"><strong>Vendor CLIs could not be loaded.</strong><span>Check the server connection and try again.</span><button type="button" @click=${() => void this.#load()}>Retry</button></div>`
            : clis.length === 0
              ? html`<div class="empty"><strong>No vendor CLIs reported.</strong><span>This server did not return any supported vendor binaries.</span></div>`
              : html`<div class="cli-list">${clis.map((cli, index) => this.#renderCli(cli, index))}</div>`}
        </section>
      </section>
    `;
  }

  #renderCli(cli: ProtocolCliInfo, index: number) {
    const status = this.#statuses.get(cli.id) ?? idleCliInstallStatus();
    const pending = status.status === "pending";
    const percent = cliProgressPercent(status);
    const confirming = this.#confirmUninstallId === cli.id;
    const titleId = `cli-title-${index}`;
    const confirmTitleId = `cli-remove-title-${index}`;
    const confirmDescriptionId = `cli-remove-description-${index}`;

    return html`
      <article class="cli-row" aria-labelledby=${titleId}>
        <div class="cli-copy">
          <header class="cli-heading">
            <div>
              <strong id=${titleId}>${cli.display_name}</strong>
              <small>${cliVersionLabel(cli)}</small>
            </div>
          </header>
          <div class="badges" aria-label=${`${cli.display_name} capabilities and source`}>
            <span class="badge ${cli.source === "managed" ? "managed" : cli.source === "path" ? "path" : ""}">${cliSourceLabel(cli)}</span>
            ${cli.update_available ? html`<span class="badge update">Update available</span>` : nothing}
            ${cli.kinds.map((kind) => html`<span class="badge" title=${kind}>${kind}</span>`)}
          </div>
          <dl class="metadata">
            <div><dt>Installed</dt><dd title=${cli.installed_version ?? "Not installed"}>${cli.installed_version ?? "Not installed"}</dd></div>
            <div><dt>Latest</dt><dd title=${cli.latest_version ?? "Unknown"}>${cli.latest_version ?? "Unknown"}</dd></div>
            <div><dt>Source</dt><dd>${cliSourceLabel(cli)}</dd></div>
          </dl>
          ${cli.path ? html`<p class="path" title=${cli.path}>${cli.path}</p>` : nothing}
        </div>

        <div class="actions">
          ${pending
            ? html`<button type="button" ?disabled=${this.#busyId !== ""} @click=${() => void this.#cancel(cli)}>${this.#busyId === cli.id ? "Cancelling…" : "Cancel install"}</button>`
            : html`<button class="primary" type="button" ?disabled=${this.#busyId !== ""} @click=${() => void this.#install(cli)}>${this.#busyId === cli.id ? "Starting…" : cliPrimaryActionLabel(cli)}</button>`}
          ${cli.source === "managed" && !pending
            ? html`<button class="danger" type="button" ?disabled=${this.#busyId !== ""} @click=${() => this.#requestUninstall(cli.id)}>Uninstall</button>`
            : nothing}
        </div>

        ${pending
          ? html`
              <div class="progress-row" aria-label=${`${cli.display_name} install progress`}>
                <small>${cliProgressLabel(status, this.#downloadRates.get(cli.id))}</small>
                <small>${percent === undefined ? "Size unknown" : `${percent}%`}</small>
                ${percent === undefined
                  ? html`<progress max="100" aria-label=${`${cli.display_name} download in progress`}></progress>`
                  : html`<progress max="100" value=${percent} aria-label=${`${cli.display_name} download ${percent}% complete`}></progress>`}
              </div>
            `
          : status.status === "failed"
            ? html`<small class="install-error" role="alert">Installation failed. Retry the install or check the server logs.</small>`
            : nothing}

        ${confirming
          ? html`
              <div
                class="confirmation"
                role="alertdialog"
                aria-labelledby=${confirmTitleId}
                aria-describedby=${confirmDescriptionId}
              >
                <h3 id=${confirmTitleId}>Uninstall trouve's managed ${cli.display_name}?</h3>
                <p id=${confirmDescriptionId}>Only the managed copy is removed. A system PATH installation, if present, is never deleted and may become active again.</p>
                <div class="actions">
                  <button type="button" @click=${this.#dismissUninstall}>Back</button>
                  <button class="danger" type="button" @click=${() => void this.#uninstall(cli)}>Confirm uninstall</button>
                </div>
              </div>
            `
          : nothing}
      </article>
    `;
  }

  async #load(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) {
      this.#clis = undefined;
      this.#loading = false;
      this.#message = genericFailure("Loading vendor CLIs");
      this.#error = true;
      this.requestUpdate();
      return;
    }

    const lifecycle = this.#lifecycleGeneration;
    const request = ++this.#loadRequest;
    this.#stopPolling();
    this.#loading = true;
    this.#message = "Loading vendor CLI status…";
    this.#error = false;
    this.requestUpdate();
    try {
      const response = await protocol.clis();
      if (!this.#loadIsCurrent(lifecycle, request)) return;
      const statuses = new Map<string, ProtocolCliInstallStatus>();
      let partialFailure = false;
      for (const cli of response.clis) {
        try {
          const status = await protocol.cliInstallStatus(cli.id);
          if (!this.#loadIsCurrent(lifecycle, request)) return;
          statuses.set(cli.id, status);
        } catch {
          if (!this.#loadIsCurrent(lifecycle, request)) return;
          statuses.set(cli.id, idleCliInstallStatus());
          partialFailure = true;
        }
      }
      this.#clis = response.clis;
      this.#replaceStatuses(statuses);
      this.#confirmUninstallId = "";
      this.#message = partialFailure
        ? "Some install progress could not be loaded. Refresh to retry."
        : "";
      this.#error = partialFailure;
    } catch {
      if (!this.#loadIsCurrent(lifecycle, request)) return;
      this.#message = genericFailure("Loading vendor CLIs");
      this.#error = true;
    } finally {
      if (this.#loadIsCurrent(lifecycle, request)) {
        this.#loading = false;
        this.requestUpdate();
        if (pendingCliIds(this.#statuses).length > 0) this.#startPolling();
      }
    }
  }

  #loadIsCurrent(lifecycle: number, request: number): boolean {
    return this.isConnected &&
      lifecycle === this.#lifecycleGeneration &&
      request === this.#loadRequest;
  }

  async #install(cli: ProtocolCliInfo): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busyId !== "") return;
    const lifecycle = this.#lifecycleGeneration;
    this.#stopPolling();
    this.#busyId = cli.id;
    this.#confirmUninstallId = "";
    this.#message = `Starting ${cli.display_name} installation…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.startCliInstall(cli.id);
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      this.#setStatus(cli.id, {
        status: "pending",
        received_bytes: 0,
        total_bytes: 0,
      });
      this.#message = `${cli.display_name} installation started.`;
    } catch {
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      this.#message = genericFailure(`Starting ${cli.display_name} installation`);
      this.#error = true;
    } finally {
      if (this.#actionIsCurrent(lifecycle, cli.id)) {
        this.#busyId = "";
        this.requestUpdate();
        this.#resumePollingIfNeeded();
      }
    }
  }

  async #cancel(cli: ProtocolCliInfo): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busyId !== "") return;
    const lifecycle = this.#lifecycleGeneration;
    this.#stopPolling();
    this.#busyId = cli.id;
    this.#message = `Requesting cancellation for ${cli.display_name}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.cancelCliInstall(cli.id);
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      this.#message = `${cli.display_name} cancellation requested. Waiting for the installer to stop…`;
    } catch {
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      this.#message = genericFailure(`Cancelling ${cli.display_name} installation`);
      this.#error = true;
    } finally {
      if (this.#actionIsCurrent(lifecycle, cli.id)) {
        this.#busyId = "";
        this.requestUpdate();
        this.#resumePollingIfNeeded();
      }
    }
  }

  #requestUninstall(cliId: string): void {
    if (this.#busyId !== "") return;
    this.#confirmUninstallId = cliId;
    this.requestUpdate();
  }

  readonly #dismissUninstall = (): void => {
    this.#confirmUninstallId = "";
    this.requestUpdate();
  };

  async #uninstall(cli: ProtocolCliInfo): Promise<void> {
    const protocol = this.#services.value?.protocol;
    const current = this.#clis?.find((candidate) => candidate.id === cli.id);
    if (
      protocol === undefined ||
      this.#busyId !== "" ||
      this.#confirmUninstallId !== cli.id ||
      current?.source !== "managed"
    ) return;

    const lifecycle = this.#lifecycleGeneration;
    this.#stopPolling();
    this.#busyId = cli.id;
    this.#confirmUninstallId = "";
    this.#message = `Removing trouve's managed ${cli.display_name}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.uninstallCli(cli.id);
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      await this.#load();
      if (!this.isConnected || lifecycle !== this.#lifecycleGeneration) return;
      if (!this.#error) {
        this.#message = `Removed the managed ${cli.display_name}. System PATH installations were left untouched.`;
      }
    } catch {
      if (!this.#actionIsCurrent(lifecycle, cli.id)) return;
      this.#message = genericFailure(`Removing managed ${cli.display_name}`);
      this.#error = true;
      this.#resumePollingIfNeeded();
    } finally {
      if (this.isConnected && lifecycle === this.#lifecycleGeneration) {
        this.#busyId = "";
        this.requestUpdate();
      }
    }
  }

  #actionIsCurrent(lifecycle: number, cliId: string): boolean {
    return this.isConnected &&
      lifecycle === this.#lifecycleGeneration &&
      this.#busyId === cliId;
  }

  #setStatus(cliId: string, status: ProtocolCliInstallStatus): void {
    const statuses = new Map(this.#statuses);
    statuses.set(cliId, status);
    this.#replaceStatuses(statuses);
  }

  #replaceStatuses(statuses: Map<string, ProtocolCliInstallStatus>): void {
    const active = new Set<string>();
    for (const [cliId, status] of statuses) {
      if (status.status !== "pending") continue;
      active.add(cliId);
      const rate = this.#rateTracker.update(cliId, status.received_bytes ?? 0);
      if (rate !== undefined) this.#downloadRates.set(cliId, rate);
    }
    for (const cliId of this.#downloadRates.keys()) {
      if (active.has(cliId)) continue;
      this.#downloadRates.delete(cliId);
      this.#rateTracker.delete(cliId);
    }
    this.#rateTracker.retain(active);
    this.#statuses = statuses;
  }

  #resumePollingIfNeeded(): void {
    if (pendingCliIds(this.#statuses).length > 0) this.#startPolling();
  }

  #startPolling(): void {
    this.#stopPolling();
    this.#pollAttempts = 0;
    this.#pollFailures = 0;
    this.#schedulePoll(this.#pollGeneration);
  }

  #schedulePoll(generation: number): void {
    if (
      generation !== this.#pollGeneration ||
      !this.isConnected ||
      !shouldPollCliInstalls(this.#statuses, this.#pollAttempts)
    ) return;
    this.#pollTimer = setTimeout(
      () => void this.#poll(generation),
      CLI_POLL_INTERVAL_MS,
    );
  }

  async #poll(generation: number): Promise<void> {
    this.#pollTimer = undefined;
    if (generation !== this.#pollGeneration || !this.isConnected) return;
    if (!shouldPollCliInstalls(this.#statuses, this.#pollAttempts)) {
      if (pendingCliIds(this.#statuses).length > 0) {
        this.#message = "Install progress polling reached its safety limit. Refresh to continue checking progress.";
        this.#error = true;
        this.requestUpdate();
      }
      return;
    }

    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#pollAttempts += 1;
    const pendingIds = pendingCliIds(this.#statuses);
    const statuses = new Map(this.#statuses);
    let terminalStatusObserved = false;
    let readFailure = false;

    for (const cliId of pendingIds) {
      try {
        const status = await protocol.cliInstallStatus(cliId);
        if (generation !== this.#pollGeneration || !this.isConnected) return;
        statuses.set(cliId, status);
        terminalStatusObserved ||= status.status !== "pending";
      } catch {
        if (generation !== this.#pollGeneration || !this.isConnected) return;
        readFailure = true;
      }
    }

    this.#replaceStatuses(statuses);
    this.#pollFailures = readFailure ? this.#pollFailures + 1 : 0;
    if (terminalStatusObserved) {
      try {
        const response = await protocol.clis();
        if (generation !== this.#pollGeneration || !this.isConnected) return;
        this.#clis = response.clis;
      } catch {
        if (generation !== this.#pollGeneration || !this.isConnected) return;
        this.#message = genericFailure("Refreshing installed CLI versions");
        this.#error = true;
      }
    }

    const remaining = pendingCliIds(this.#statuses);
    if (remaining.length === 0) {
      const anyFailed = [...this.#statuses.values()].some(
        (status) => status.status === "failed",
      );
      this.#message = anyFailed
        ? "A CLI installation failed. Retry the install or check the server logs."
        : "CLI install activity finished.";
      this.#error = anyFailed;
    } else if (this.#pollFailures >= 3) {
      this.#message = "Install progress is temporarily unavailable. Polling will keep retrying.";
      this.#error = true;
    } else if (
      this.#pollFailures === 0 &&
      this.#message === "Install progress is temporarily unavailable. Polling will keep retrying."
    ) {
      this.#message = "Install progress restored.";
      this.#error = false;
    }
    this.requestUpdate();

    if (
      this.#pollAttempts >= MAX_CLI_POLL_ATTEMPTS &&
      pendingCliIds(this.#statuses).length > 0
    ) {
      this.#message = "Install progress polling reached its safety limit. Refresh to continue checking progress.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    this.#schedulePoll(generation);
  }

  #stopPolling(): void {
    this.#pollGeneration += 1;
    if (this.#pollTimer !== undefined) clearTimeout(this.#pollTimer);
    this.#pollTimer = undefined;
  }
}

customElements.define("trouve-cli-settings", TrouveCliSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-cli-settings": TrouveCliSettings;
  }
}
