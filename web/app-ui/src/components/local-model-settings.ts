import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import type {
  ProtocolAddLocalModelRequest,
  ProtocolCliInstallStatus,
  ProtocolLocalModelInfo,
  ProtocolLocalSearchResult,
  ProtocolLocalStatus,
} from "../services/protocol-client.js";
import {
  DownloadRateTracker,
  formatDownloadRate,
} from "../services/download-rate.js";

const LOCAL_REFRESH_MS = 1_000;
const MAX_LOCAL_REFRESH_ATTEMPTS = 600;

export const formatBytes = (bytes: number): string => {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"] as const;
  const exponent = Math.min(
    units.length - 1,
    Math.floor(Math.log(bytes) / Math.log(1024)),
  );
  const value = bytes / 1024 ** exponent;
  return `${value >= 10 || exponent === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[exponent]}`;
};

export const localModelHostCopy = (deployment: AppServices["deployment"]): string =>
  deployment === "pwa"
    ? "Local models run on the remote server host connected to this PWA—not on this phone. Downloads, memory use, and inference happen on that server host."
    : deployment === "desktop"
      ? "Local models, downloads, and inference run on this desktop's trouve server host."
      : "Local models, downloads, and inference run on the remote trouve server host connected to this browser.";

export const localModelRequest = (
  repo: string,
  file: string,
  displayName = "",
): ProtocolAddLocalModelRequest => ({
  repo: repo.trim(),
  file: file.trim(),
  ...(displayName.trim() === "" ? {} : { display_name: displayName.trim() }),
});

export const shouldRefreshLocalStatus = (
  status: ProtocolLocalStatus,
  runtimeInstall?: ProtocolCliInstallStatus,
): boolean =>
  status.server_status === "starting" ||
  status.models.some((model) => model.download_status === "pending") ||
  runtimeInstall?.status === "pending";

export interface LocalSearchFitFilters {
  readonly gpu: boolean;
  readonly cpu: boolean;
  readonly tooLarge: boolean;
}

export interface LocalModelSections {
  readonly yours: readonly ProtocolLocalModelInfo[];
  readonly recommended: readonly ProtocolLocalModelInfo[];
}

/** Match the native catalog: active, owned, and failed rows stay together;
 * untouched curated entries remain a separate recommendations section. */
export const localModelSections = (
  models: readonly ProtocolLocalModelInfo[],
): LocalModelSections => {
  const yours: ProtocolLocalModelInfo[] = [];
  const recommended: ProtocolLocalModelInfo[] = [];
  for (const model of models) {
    const owned = model.downloaded || model.custom || model.download_status === "pending" ||
      (model.download_error ?? "") !== "";
    (owned ? yours : recommended).push(model);
  }
  return { yours, recommended };
};

/** Match the established Slint behavior: a repository remains visible when
 * any file fits an enabled tier, and its complete file picker remains intact. */
export const filterLocalSearchResults = (
  results: readonly ProtocolLocalSearchResult[],
  filters: LocalSearchFitFilters,
): readonly ProtocolLocalSearchResult[] => results.filter((result) =>
  result.files.some((file) => {
    if (file.fit === "gpu") return filters.gpu;
    if (file.fit === "cpu") return filters.cpu;
    return filters.tooLarge;
  })
);

const fitLabel = (fit: string): string => {
  switch (fit) {
    case "gpu": return "Fits GPU";
    case "cpu": return "Runs in RAM (slower)";
    case "too-large": return "Too large for this host";
    default: return fit || "Fit unknown";
  }
};

const formInput = (form: HTMLFormElement, name: string): string => {
  const control = form.elements.namedItem(name);
  return control instanceof HTMLInputElement ? control.value : "";
};

export class TrouveLocalModelSettings extends LitElement {
  static override styles = css`
    :host { display: block; color: var(--trouve-text); }
    * { box-sizing: border-box; }
    .settings-stack { display: grid; gap: 12px; }
    .section-heading { display: flex; align-items: center; gap: 12px; }
    .section-heading > div { flex: 1; min-width: 0; }
    h2, h3, p { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 18px; }
    h3 { color: var(--trouve-text-hi); font-size: 13px; }
    p, small { color: var(--trouve-text-dim); }
    .section-heading p, .settings-card > p { margin-top: 4px; }
    .settings-card {
      padding: 10px;
      border: 0;
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .host-notice {
      padding: 10px 12px;
      border: 1px solid var(--trouve-accent);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text-accent-soft);
      background: var(--trouve-accent-veil);
    }
    .notice { min-height: 20px; color: var(--trouve-text-dim); }
    .notice.error { color: var(--trouve-err); }
    .local-enabled-switch { display: inline-flex; grid-auto-flow: column; align-items: center; gap: 7px; font-weight: 400; }
    .local-enabled-switch input { width: 18px; min-height: 18px; margin: 0; accent-color: var(--trouve-accent); }
    .machine-line, .disabled-copy { color: var(--trouve-text-mid); font-size: 12px; }
    .runtime-card { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: center; gap: 8px; }
    .runtime-copy { min-width: 0; display: grid; gap: 4px; }
    .runtime-copy strong { color: var(--trouve-text); font-size: 13px; font-weight: 400; }
    .runtime-card .actions { margin: 0; }
    .server-card { display: flex; align-items: center; gap: 8px; padding: 8px; border-radius: var(--trouve-radius); color: var(--trouve-text-accent-soft); background: var(--trouve-accent-bg); }
    .server-card > span { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .summary-grid {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 1px;
      margin-top: 10px;
      padding: 1px;
      background: var(--trouve-rule);
    }
    .summary-grid > div { min-width: 0; padding: 9px 10px; background: var(--trouve-inset-bg); }
    .summary-grid dt { color: var(--trouve-text-dim); font-size: 10px; text-transform: uppercase; letter-spacing: .04em; }
    .summary-grid dd { margin: 3px 0 0; overflow-wrap: anywhere; color: var(--trouve-text-hi); }
    .card-list { display: grid; gap: 8px; margin-top: 10px; }
    .model-row, .search-row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 8px 12px;
      align-items: center;
      padding: 10px;
      border: 1px solid var(--trouve-rule);
      border-radius: var(--trouve-radius-sm);
      background: var(--trouve-inset-bg);
    }
    .model-copy { min-width: 0; }
    .model-copy strong, .model-copy small { display: block; overflow-wrap: anywhere; }
    .model-copy small { margin-top: 2px; }
    .model-copy .error-copy { color: var(--trouve-err); }
    .actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
    form { display: grid; gap: 10px; margin-top: 11px; }
    .inline-form { grid-template-columns: minmax(140px, 1fr) auto; align-items: end; }
    .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
    label { display: grid; gap: 4px; min-width: 0; color: var(--trouve-text-hi); font-weight: 600; }
    input, select, button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
    }
    input, select { width: 100%; padding: 6px 8px; font-weight: 400; }
    button { padding: 5px 10px; cursor: pointer; }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.danger { border-color: var(--trouve-err); color: var(--trouve-err-soft); }
    button:disabled, input:disabled, select:disabled { cursor: not-allowed; opacity: .56; }
    button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    .toggle-row { display: flex; align-items: center; gap: 9px; margin-top: 10px; }
    .toggle-row input { width: 18px; min-height: 18px; accent-color: var(--trouve-accent); }
    .status-pill {
      display: inline-flex;
      align-items: center;
      min-height: 22px;
      padding: 2px 7px;
      border-radius: 999px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      font-size: 11px;
    }
    .status-pill.ready { color: var(--trouve-ok); }
    .status-pill.warning { color: var(--trouve-warn); }
    .status-pill.failed { color: var(--trouve-err); }
    .progress { grid-column: 1 / -1; display: grid; grid-template-columns: 1fr auto; gap: 5px 8px; }
    progress { grid-column: 1 / -1; width: 100%; accent-color: var(--trouve-accent); }
    details { margin-top: 12px; }
    summary { min-height: 36px; padding: 7px 0; color: var(--trouve-text-hi); cursor: pointer; font-weight: 600; }
    .search-meta { display: flex; flex-wrap: wrap; gap: 5px 10px; margin-top: 3px; }
    .search-row form { min-width: min(280px, 40vw); margin: 0; }
    .empty { padding: 9px 0; color: var(--trouve-text-dim); }
    .fit-filters { min-width: 0; display: flex; flex-wrap: wrap; align-items: center; gap: 7px 14px; margin: 10px 0 2px; border: 0; padding: 0; color: var(--trouve-text-mid); }
    .fit-filters legend { float: left; margin-inline-end: 7px; padding: 0; color: var(--trouve-text-dim); font-size: 11px; }
    .fit-filters label { display: inline-flex; grid-auto-flow: column; align-items: center; gap: 5px; color: var(--trouve-text-mid); font-size: 11px; font-weight: 400; }
    .fit-filters input { width: auto; margin: 0; accent-color: var(--trouve-accent); }
    .filter-status { margin-top: 8px; color: var(--trouve-text-dim); font-size: 11px; }
    .model-catalog-card { min-height: 220px; padding: 0; overflow: auto; }
    .model-catalog-card .card-list { gap: 0; margin: 0; }
    .model-catalog-card .model-row { min-height: 58px; border: 0; border-radius: 0; background: transparent; }
    .model-section-header { padding: 8px 10px 4px; color: var(--trouve-text-dim); font-size: 10px; letter-spacing: .05em; }
    .search-models { display: grid; gap: 6px; }
    @media (max-width: 640px) {
      .summary-grid, .form-grid { grid-template-columns: 1fr; }
      .model-row, .search-row, .inline-form { grid-template-columns: 1fr; }
      .actions { justify-content: stretch; }
      .actions button, button, input, select, summary { min-height: 44px; }
      .search-row form { min-width: 0; width: 100%; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  #loadedServices: AppServices | undefined;
  #status: ProtocolLocalStatus | undefined;
  #runtimeInstallStatus: ProtocolCliInstallStatus | undefined;
  #searchResults: readonly ProtocolLocalSearchResult[] = [];
  #selectedSearchFiles = new Map<string, string>();
  #searchFitGpu = true;
  #searchFitCpu = true;
  #searchFitTooLarge = false;
  #loading = true;
  #searching = false;
  #busy = "";
  #notice = "";
  #noticeIsError = false;
  #confirmDelete = "";
  #loadGeneration = 0;
  #refreshTimer: ReturnType<typeof setTimeout> | undefined;
  #refreshAttempts = 0;
  readonly #rateTracker = new DownloadRateTracker();
  readonly #downloadRates = new Map<string, number>();

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#loadedServices = undefined;
    this.#clearRefreshTimer();
    this.#rateTracker.clear();
    this.#downloadRates.clear();
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const services = this.#services.value;
    if (services !== undefined && services !== this.#loadedServices) {
      this.#loadedServices = services;
      void this.#load();
    }
  }

  async refresh(): Promise<void> {
    await this.#load();
  }

  override render() {
    const services = this.#services.value;
    const status = this.#status;
    if (services === undefined || (this.#loading && status === undefined)) {
      return html`<div class="settings-card" role="status">Loading local models…</div>`;
    }
    if (status === undefined) {
      return html`
        <div class="settings-card" role="alert">
          Local-model status could not be loaded.
          <button type="button" @click=${() => void this.#load()}>Retry</button>
        </div>
      `;
    }
    const visibleSearchResults = filterLocalSearchResults(this.#searchResults, {
      gpu: this.#searchFitGpu,
      cpu: this.#searchFitCpu,
      tooLarge: this.#searchFitTooLarge,
    });
    const allSearchResultsHidden =
      this.#searchResults.length > 0 && visibleSearchResults.length === 0;
    const runtimeInstall = this.#runtimeInstallStatus;
    const runtimePending = runtimeInstall?.status === "pending";
    const runtimeReceived = runtimeInstall?.received_bytes ?? 0;
    const runtimeTotal = runtimeInstall?.total_bytes ?? 0;
    const runtimeProgressMax = Math.max(
      runtimeTotal,
      runtimeReceived,
      1,
    );
    const runtimeRate = formatDownloadRate(this.#downloadRates.get("runtime"));
    const modelSections = localModelSections(status.models);

    return html`
      <section class="settings-stack" aria-labelledby="local-model-settings-title">
        <header class="section-heading">
          <div>
            <h2 id="local-model-settings-title">Managed Local Models</h2>
          </div>
          <label class="local-enabled-switch">
            <input
              type="checkbox"
              .checked=${status.enabled}
              ?disabled=${this.#busy !== ""}
              @change=${(event: Event) => void this.#setEnabled((event.currentTarget as HTMLInputElement).checked)}
            />
            <span>${status.enabled ? "Enabled" : "Disabled"}</span>
          </label>
          ${status.enabled
            ? html`<button type="button" ?disabled=${this.#loading} @click=${() => void this.#load()}>${this.#loading ? "Refreshing…" : "⟳ Refresh"}</button>`
            : nothing}
        </header>

        <p>Run models fully offline. trouve manages the llama.cpp runtime and downloads models from HuggingFace; downloaded models appear in the model picker as local/&lt;model&gt;. Expect far less capability than the cloud providers — this is an offline fallback tier, not a peer.</p>
        ${!status.enabled
          ? html`<p class="disabled-copy">Local models are disabled: the llama-server sidecar is stopped and local models are hidden from the model pickers. Downloaded files stay on disk.</p>`
          : html`<p class="machine-line">Your machine: ${formatBytes(status.ram_bytes)} RAM · ${status.gpus.length === 0 ? "No dedicated GPU reported" : status.gpus.map((gpu) => `${gpu.name} · ${formatBytes(gpu.vram_bytes)}`).join(", ")}</p>`}
        ${services.deployment === "desktop" ? nothing : html`<p class="host-notice" role="note">${localModelHostCopy(services.deployment)}</p>`}
        ${this.#notice === ""
          ? nothing
          : html`<p class=${`notice${this.#noticeIsError ? " error" : ""}`} role=${this.#noticeIsError ? "alert" : "status"} aria-live="polite">${this.#notice}</p>`}

        ${status.enabled ? html`<section class="settings-card runtime-card" aria-labelledby="local-runtime-title">
          <div class="runtime-copy">
            <strong id="local-runtime-title">llama.cpp runtime — ${status.runtime_installed ? status.runtime_version ?? "Installed" : "Not installed"}${status.runtime_update_available ? " · update available" : ""}</strong>
          ${runtimePending
            ? html`
                <div class="progress">
                  <span>Downloading llama.cpp · ${formatBytes(runtimeReceived)}${runtimeTotal > 0 ? ` of ${formatBytes(runtimeTotal)}` : ""}${runtimeRate === "" ? "" : ` · ${runtimeRate}`}</span>
                  ${runtimeTotal > 0
                    ? html`<small>${Math.min(100, Math.round((runtimeReceived / runtimeProgressMax) * 100))}%</small>`
                    : nothing}
                  <progress max=${runtimeProgressMax} .value=${runtimeReceived} aria-label="llama.cpp runtime install progress"></progress>
                </div>
              `
            : runtimeInstall?.status === "failed"
              ? html`<p class="error-copy" role="alert">Runtime install failed${runtimeInstall.error ? `: ${runtimeInstall.error}` : "."}</p>`
              : nothing}
          </div>
          <div class="actions">
            ${runtimePending
              ? html`<button type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#cancelRuntimeInstall()}>Cancel runtime install</button>`
              : html`
                  ${!status.runtime_installed
                    ? html`<button class="primary" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#installRuntime()}>Install runtime</button>`
                    : nothing}
                  ${status.runtime_update_available
                    ? html`<button class="primary" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#installRuntime()}>Update runtime</button>`
                    : nothing}
                  ${status.runtime_managed
                    ? html`<button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#uninstallRuntime()}>Uninstall runtime</button>`
                    : nothing}
                `}
          </div>
        </section>

        ${status.server_status === "stopped" && !status.running_model
          ? nothing
          : html`<section class="server-card">
              <span>${status.server_status || "running"}${status.running_model ? ` · ${status.running_model}` : ""}</span>
              <button type="button" ?disabled=${this.#busy !== "" || !status.running_model} @click=${() => void this.#restartServer()}>⟳ Restart</button>
              <button type="button" ?disabled=${this.#busy !== "" || status.server_status === "stopped"} @click=${() => void this.#stopServer()}>Stop (free memory)</button>
            </section>`}

        <section class="settings-card model-catalog-card" aria-label="Model catalog">
          <div class="card-list">
            ${status.models.length === 0
              ? html`<div class="empty">No local models are registered.</div>`
              : html`
                  ${modelSections.yours.length === 0 ? nothing : html`
                    <h3 class="model-section-header">YOUR MODELS</h3>
                    ${modelSections.yours.map((model) => this.#renderModel(model))}
                  `}
                  ${modelSections.recommended.length === 0 ? nothing : html`
                    <h3 class="model-section-header">RECOMMENDED</h3>
                    ${modelSections.recommended.map((model) => this.#renderModel(model))}
                  `}
                `}
          </div>
        </section>

        <section class="search-models" aria-labelledby="find-models-title">
          <p id="find-models-title">Add more models — search HuggingFace:</p>
          <form class="inline-form" @submit=${(event: SubmitEvent) => void this.#search(event)}>
            <label>
              Search models
              <input name="query" type="search" minlength="2" required autocomplete="off" spellcheck="false" placeholder="Qwen coder" />
            </label>
            <button class="primary" type="submit" ?disabled=${this.#searching}>${this.#searching ? "Searching…" : "Search"}</button>
          </form>
          <fieldset class="fit-filters">
            <legend>Show:</legend>
            <label>
              <input type="checkbox" .checked=${this.#searchFitGpu} @change=${(event: Event) => this.#setSearchFit("gpu", (event.currentTarget as HTMLInputElement).checked)} />
              fits GPU
            </label>
            <label>
              <input type="checkbox" .checked=${this.#searchFitCpu} @change=${(event: Event) => this.#setSearchFit("cpu", (event.currentTarget as HTMLInputElement).checked)} />
              runs on CPU
            </label>
            <label>
              <input type="checkbox" .checked=${this.#searchFitTooLarge} @change=${(event: Event) => this.#setSearchFit("too-large", (event.currentTarget as HTMLInputElement).checked)} />
              too large for this host
            </label>
          </fieldset>
          ${allSearchResultsHidden
            ? html`<p class="filter-status" role="status">All ${this.#searchResults.length} results are hidden by the fit filters.</p>`
            : nothing}
          <div class="card-list" aria-live="polite">
            ${visibleSearchResults.map((result) => this.#renderSearchResult(result))}
            ${!this.#searching && this.#searchResults.length === 0
              ? html`<div class="empty">Search results will appear here.</div>`
              : nothing}
          </div>
          <details>
            <summary>Add a known GGUF by repository and filename</summary>
            <form @submit=${(event: SubmitEvent) => void this.#addManual(event)}>
              <div class="form-grid">
                <label>Repository <input name="repo" required autocomplete="off" spellcheck="false" placeholder="owner/model-GGUF" /></label>
                <label>GGUF filename <input name="file" required autocomplete="off" spellcheck="false" placeholder="model.Q4_K_M.gguf" /></label>
              </div>
              <label>Display name (optional) <input name="display_name" autocomplete="off" /></label>
              <button type="submit" ?disabled=${this.#busy !== ""}>Add model</button>
            </form>
          </details>
        </section>` : nothing}
      </section>
    `;
  }

  #renderModel(model: ProtocolLocalModelInfo) {
    const deleting = this.#confirmDelete === model.id;
    const pending = model.download_status === "pending";
    const failed = model.download_status === "failed";
    const downloadBytes = model.download_bytes ?? 0;
    const progressMax = Math.max(model.size_bytes, downloadBytes, 1);
    const rate = formatDownloadRate(this.#downloadRates.get(`model:${model.id}`));
    return html`
      <article class="model-row">
        <div class="model-copy">
          <strong>${model.display_name}</strong>
          <small>${[model.params, fitLabel(model.fit), formatBytes(model.size_bytes), `${model.context_window.toLocaleString()} context`].filter(Boolean).join(" · ")}</small>
          ${model.notes ? html`<small>${model.notes}</small>` : nothing}
          ${failed ? html`<small class="error-copy">Download failed. Retry to request the model again.</small>` : nothing}
        </div>
        <div class="actions">
          <span class=${`status-pill ${model.downloaded ? "ready" : pending ? "warning" : failed ? "failed" : ""}`}>
            ${model.downloaded ? "Downloaded" : pending ? "Downloading" : failed ? "Failed" : "Not downloaded"}
          </span>
          ${pending
            ? html`<button type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#cancelDownload(model.id)}>Cancel download</button>`
            : model.downloaded
              ? deleting
                ? html`
                    <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#deleteModel(model.id)}>Confirm delete</button>
                    <button type="button" @click=${() => { this.#confirmDelete = ""; this.requestUpdate(); }}>Cancel</button>
                  `
                : html`<button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => { this.#confirmDelete = model.id; this.requestUpdate(); }}>${model.custom ? "Remove" : "Delete download"}</button>`
              : model.custom
                ? deleting
                  ? html`
                      <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#deleteModel(model.id)}>Confirm remove</button>
                      <button type="button" @click=${() => { this.#confirmDelete = ""; this.requestUpdate(); }}>Cancel</button>
                    `
                  : html`
                      <button class="primary" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#startDownload(model.id)}>Download</button>
                      <button class="danger" type="button" ?disabled=${this.#busy !== ""} @click=${() => { this.#confirmDelete = model.id; this.requestUpdate(); }}>Remove</button>
                    `
                : html`<button class="primary" type="button" ?disabled=${this.#busy !== ""} @click=${() => void this.#startDownload(model.id)}>Download</button>`}
        </div>
        ${pending ? html`
          <div class="progress">
            <span>${formatBytes(downloadBytes)} of ${formatBytes(model.size_bytes)}${rate === "" ? "" : ` · ${rate}`}</span>
            <small>${Math.min(100, Math.round((downloadBytes / progressMax) * 100))}%</small>
            <progress max=${progressMax} .value=${downloadBytes} aria-label=${`${model.display_name} download progress`}></progress>
          </div>
        ` : nothing}
      </article>
    `;
  }

  #renderSearchResult(result: ProtocolLocalSearchResult) {
    const recommended = result.files[result.recommended]?.file ?? result.files[0]?.file ?? "";
    const selected = this.#selectedSearchFiles.get(result.repo) ?? recommended;
    const selectedFile = result.files.find((file) => file.file === selected);
    return html`
      <article class="search-row">
        <div class="model-copy">
          <strong>${result.repo}</strong>
          <small class="search-meta"><span>${result.downloads.toLocaleString()} downloads</span><span>${result.likes.toLocaleString()} likes</span></small>
        </div>
        <form @submit=${(event: SubmitEvent) => { event.preventDefault(); void this.#addSearchResult(result.repo, selected); }}>
          <label>
            GGUF file
            <select
              aria-label=${`GGUF file for ${result.repo}`}
              .value=${selected}
              @change=${(event: Event) => {
                this.#selectedSearchFiles.set(result.repo, (event.currentTarget as HTMLSelectElement).value);
                this.requestUpdate();
              }}
            >
              ${result.files.map((file, index) => html`
                <option value=${file.file}>
                  ${file.quant || file.file} · ${formatBytes(file.size_bytes)} · ${fitLabel(file.fit)}${index === result.recommended ? " · Recommended" : ""}${file.added ? " · Added" : ""}
                </option>
              `)}
            </select>
          </label>
          <button type="submit" ?disabled=${selectedFile === undefined || selectedFile.added || this.#busy !== ""}>
            ${selectedFile?.added ? "Added" : "Add to catalog"}
          </button>
        </form>
      </article>
    `;
  }

  #setSearchFit(fit: "gpu" | "cpu" | "too-large", checked: boolean): void {
    if (fit === "gpu") this.#searchFitGpu = checked;
    else if (fit === "cpu") this.#searchFitCpu = checked;
    else this.#searchFitTooLarge = checked;
    this.requestUpdate();
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const generation = ++this.#loadGeneration;
    this.#loading = true;
    this.requestUpdate();
    try {
      const [status, runtimeInstallStatus] = await Promise.all([
        services.protocol.localStatus(),
        services.protocol.cliInstallStatus("llama-server"),
      ]);
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#updateDownloadRates(status, runtimeInstallStatus);
      this.#status = status;
      this.#runtimeInstallStatus = runtimeInstallStatus;
      this.#loading = false;
      if (shouldRefreshLocalStatus(status, runtimeInstallStatus)) {
        this.#scheduleRefresh();
      } else {
        this.#clearRefreshTimer();
        this.#refreshAttempts = 0;
      }
    } catch {
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#loading = false;
      this.#setNotice("Local-model status could not be loaded. Try again.", true);
      if (this.#status !== undefined && shouldRefreshLocalStatus(this.#status, this.#runtimeInstallStatus)) {
        this.#scheduleRefresh();
      }
    }
    this.requestUpdate();
  }

  #scheduleRefresh(force = false): void {
    if (this.#refreshTimer !== undefined || (!force && this.#status !== undefined && !shouldRefreshLocalStatus(this.#status, this.#runtimeInstallStatus))) return;
    if (this.#refreshAttempts >= MAX_LOCAL_REFRESH_ATTEMPTS) {
      this.#setNotice("Automatic local-model refresh stopped. Use Refresh to check again.", true);
      return;
    }
    this.#refreshAttempts += 1;
    this.#refreshTimer = globalThis.setTimeout(() => {
      this.#refreshTimer = undefined;
      void this.#load();
    }, LOCAL_REFRESH_MS);
  }

  #updateDownloadRates(
    status: ProtocolLocalStatus,
    runtimeInstall: ProtocolCliInstallStatus,
  ): void {
    const active = new Set<string>();
    const observe = (key: string, bytes: number): void => {
      active.add(key);
      const rate = this.#rateTracker.update(key, bytes);
      if (rate !== undefined) this.#downloadRates.set(key, rate);
    };
    if (runtimeInstall.status === "pending") {
      observe("runtime", runtimeInstall.received_bytes ?? 0);
    }
    for (const model of status.models) {
      if (model.download_status === "pending") {
        observe(`model:${model.id}`, model.download_bytes ?? 0);
      }
    }
    for (const key of this.#downloadRates.keys()) {
      if (active.has(key)) continue;
      this.#downloadRates.delete(key);
      this.#rateTracker.delete(key);
    }
    this.#rateTracker.retain(active);
  }

  #clearRefreshTimer(): void {
    if (this.#refreshTimer !== undefined) {
      globalThis.clearTimeout(this.#refreshTimer);
      this.#refreshTimer = undefined;
    }
  }

  #restartRefreshLoop(): void {
    this.#clearRefreshTimer();
    this.#refreshAttempts = 0;
  }

  async #setEnabled(enabled: boolean): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "enabled";
    this.requestUpdate();
    try {
      await services.protocol.setLocalEnabled(enabled);
      this.#setNotice(enabled ? "Local models are enabled." : "Local models are disabled and the server was stopped.", false);
      await this.#load();
    } catch {
      this.#setNotice("Local models could not be updated. Try again.", true);
      await this.#load();
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #installRuntime(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "runtime-install";
    this.#setNotice("Starting the llama.cpp runtime install on the server host…", false);
    this.requestUpdate();
    try {
      await services.protocol.startCliInstall("llama-server");
      this.#runtimeInstallStatus = {
        status: "pending",
        received_bytes: 0,
        total_bytes: 0,
      };
      this.#restartRefreshLoop();
      this.#scheduleRefresh(true);
      await this.#load();
    } catch {
      this.#setNotice("The llama.cpp runtime install could not be started. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #cancelRuntimeInstall(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "runtime-cancel";
    this.requestUpdate();
    try {
      await services.protocol.cancelCliInstall("llama-server");
      this.#setNotice("The llama.cpp runtime install cancellation was requested.", false);
      await this.#load();
    } catch {
      this.#setNotice("The llama.cpp runtime install could not be cancelled. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #uninstallRuntime(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "runtime-uninstall";
    this.requestUpdate();
    try {
      await services.protocol.uninstallCli("llama-server");
      this.#setNotice("The trouve-managed llama.cpp runtime was removed.", false);
      await this.#load();
    } catch {
      this.#setNotice("The llama.cpp runtime could not be removed. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #startDownload(modelId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = `download:${modelId}`;
    this.requestUpdate();
    try {
      await services.protocol.startLocalModelDownload(modelId);
      this.#setNotice("Model download started on the server host.", false);
      this.#restartRefreshLoop();
      this.#scheduleRefresh(true);
      await this.#load();
    } catch {
      this.#setNotice("The model download could not be started. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #cancelDownload(modelId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = `cancel:${modelId}`;
    this.requestUpdate();
    try {
      await services.protocol.cancelLocalModelDownload(modelId);
      this.#setNotice("The download was cancelled and its partial file was removed.", false);
      await this.#load();
    } catch {
      this.#setNotice("The model download could not be cancelled. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #deleteModel(modelId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "" || this.#confirmDelete !== modelId) return;
    this.#busy = `delete:${modelId}`;
    this.requestUpdate();
    try {
      await services.protocol.deleteLocalModel(modelId);
      this.#confirmDelete = "";
      this.#setNotice("The local model was removed from the server host.", false);
      await this.#load();
    } catch {
      this.#setNotice("The local model could not be removed. Stop the server if it is in use, then try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #stopServer(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "stop";
    this.requestUpdate();
    try {
      await services.protocol.stopLocalServer();
      this.#setNotice("The local inference server was stopped.", false);
      await this.#load();
    } catch {
      this.#setNotice("The local inference server could not be stopped. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #restartServer(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "") return;
    this.#busy = "restart";
    this.requestUpdate();
    try {
      await services.protocol.restartLocalServer();
      this.#setNotice("The local inference server is restarting.", false);
      this.#restartRefreshLoop();
      this.#scheduleRefresh(true);
      await this.#load();
    } catch {
      this.#setNotice("The local inference server could not be restarted. Try again.", true);
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  async #search(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const services = this.#services.value;
    const form = event.currentTarget as HTMLFormElement;
    const query = formInput(form, "query").trim();
    if (services === undefined || query === "" || this.#searching) return;
    this.#searching = true;
    this.#searchResults = [];
    this.#selectedSearchFiles.clear();
    this.#setNotice("Searching Hugging Face from the server host…", false);
    this.requestUpdate();
    try {
      this.#searchResults = await services.protocol.searchLocalModels(query);
      this.#setNotice(
        this.#searchResults.length === 0 ? "No single-file GGUF repositories matched." : `${this.#searchResults.length} repositories found.`,
        false,
      );
    } catch {
      this.#setNotice("Model search could not be completed. Check the server connection and try again.", true);
    } finally {
      this.#searching = false;
      this.requestUpdate();
    }
  }

  async #addSearchResult(repo: string, file: string): Promise<void> {
    if (repo === "" || file === "") return;
    const added = await this.#addModel(localModelRequest(repo, file));
    if (!added) return;
    this.#searchResults = this.#searchResults.map((result) =>
      result.repo === repo
        ? { ...result, files: result.files.map((candidate) => candidate.file === file ? { ...candidate, added: true } : candidate) }
        : result,
    );
    this.requestUpdate();
  }

  async #addManual(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const added = await this.#addModel(localModelRequest(
      formInput(form, "repo"),
      formInput(form, "file"),
      formInput(form, "display_name"),
    ));
    if (added) form.reset();
  }

  async #addModel(request: ProtocolAddLocalModelRequest): Promise<boolean> {
    const services = this.#services.value;
    if (services === undefined || this.#busy !== "" || request.repo === "" || request.file === "") return false;
    this.#busy = "add";
    this.requestUpdate();
    try {
      await services.protocol.addLocalModel(request);
      this.#setNotice("The model was added to the server-host catalog. Choose Download when ready.", false);
      await this.#load();
      return true;
    } catch {
      this.#setNotice("The model could not be added. Verify the repository and GGUF filename, then try again.", true);
      return false;
    } finally {
      this.#busy = "";
      this.requestUpdate();
    }
  }

  #setNotice(message: string, error: boolean): void {
    this.#notice = message;
    this.#noticeIsError = error;
    this.requestUpdate();
  }
}

if ("customElements" in globalThis && !customElements.get("trouve-local-model-settings")) {
  customElements.define("trouve-local-model-settings", TrouveLocalModelSettings);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-local-model-settings": TrouveLocalModelSettings;
  }
}
