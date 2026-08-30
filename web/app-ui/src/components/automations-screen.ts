import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { appServicesContext, appStoreContext } from "../contexts/app-contexts.js";
import type {
  ProtocolAgentPersona,
  ProtocolAutomation,
  ProtocolAutomationTemplate,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
  ProtocolWorkspace,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  changeModelOption,
  modelOptionControls,
  type ModelOptionChangeDetail,
} from "./model-option-controls.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

const AUTOMATION_RETRY_MS = 5_000;
import {
  AUTOMATION_DAY_NAMES,
  automationDraftFrom,
  automationDraftFromTemplate,
  automationRequestFromDraft,
  automationScheduleSummary,
  emptyAutomationDraft,
  hasAutomationDraftErrors,
  modelOptionsAfterEffectiveModelChange,
  validateAutomationDraft,
  type AutomationDraft,
  type AutomationDraftErrors,
  type AutomationPermissionMode,
  type AutomationScheduleKind,
} from "./automations-model.js";
import "./model-picker.js";
import "./model-options-editor.js";

type EditorMode = "" | "create" | "edit";

const formatTimestamp = (value: string | null | undefined): string => {
  if (value == null || value === "") return "Not yet";
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
};

export class TrouveAutomationsScreen extends withSignalTracking(LitElement) {
  static override styles = css`
    :host {
      display: block;
      min-width: 0;
      min-height: 0;
      height: 100%;
      overflow: hidden;
      color: var(--trouve-text);
      background: var(--trouve-win-bg);
      font: var(--trouve-font-size)/1.4 var(--trouve-font-sans);
    }
    *, *::before, *::after { box-sizing: border-box; }
    button, input, select, textarea { color: inherit; font: inherit; }
    button, select, input[type="checkbox"] { cursor: pointer; }
    button:disabled, input:disabled, select:disabled, textarea:disabled {
      cursor: default;
      opacity: .58;
    }
    button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 9px;
      background: var(--trouve-control-bg);
    }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.primary:hover:not(:disabled) { background: var(--trouve-primary-hover); }
    button.danger { color: var(--trouve-err-soft); }
    button.danger:hover:not(:disabled) { background: var(--trouve-err-bg); }
    .screen {
      display: grid;
      grid-template-rows: 52px minmax(0, 1fr);
      height: 100%;
    }
    .page-header {
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 0 16px;
      background: var(--trouve-sidebar-bg);
    }
    h1, h2, h3, p { margin: 0; }
    h1 { min-width: 0; flex: 1; color: var(--trouve-text-hi); font-size: 18px; }
    h2 { color: var(--trouve-text-hi); font-size: 14px; }
    h3 { color: var(--trouve-text-hi); font-size: 12px; }
    .body-scroll { min-width: 0; min-height: 0; overflow: auto; }
    .body-column {
      width: min(680px, 100%);
      min-height: 100%;
      display: grid;
      align-content: start;
      gap: 12px;
      margin-inline: auto;
      padding: 16px;
    }
    .automation-heading {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    .automation-heading h2 { min-width: 0; flex: 1; font-size: 16px; }
    .heading-actions, .row-actions, .form-actions, .confirmation-actions {
      display: flex;
      align-items: center;
      flex-wrap: wrap;
      gap: 6px;
    }
    .description, .template-description { color: var(--trouve-text-soft); font-size: 11px; }
    .inline-warning { color: var(--trouve-warn); font-size: 12px; }
    .banner { border-radius: 6px; padding: 8px 10px; color: var(--trouve-text-mid); background: var(--trouve-surface); font-size: 11px; }
    .banner.notice { color: var(--trouve-text-accent-soft); background: var(--trouve-accent-veil); }
    .banner.warning { border: 1px solid var(--trouve-warn-border); color: var(--trouve-warn); background: var(--trouve-warn-bg); }
    .banner.error { border: 1px solid var(--trouve-err); color: var(--trouve-err-soft); background: var(--trouve-err-bg); }
    .empty-card, .loading-card { height: 72px; display: grid; place-items: center; border-radius: 6px; color: var(--trouve-text-dim); background: var(--trouve-surface); font-size: 13px; }
    .automation-list {
      display: grid;
      gap: 12px;
      margin: 0;
      padding: 0;
      list-style: none;
    }
    .automation-card {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: center;
      gap: 10px;
      width: 100%;
      border-radius: 6px;
      padding: 10px;
      background: var(--trouve-surface);
    }
    .automation-copy { min-width: 0; display: grid; gap: 3px; }
    .automation-title { min-width: 0; display: flex; align-items: baseline; gap: 8px; }
    .automation-title strong {
      overflow: hidden;
      color: var(--trouve-text-hi);
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 13px;
    }
    .automation-title span { flex: none; color: var(--trouve-text-faint); font-size: 10px; }
    .automation-title .yolo { color: var(--trouve-warn); font-weight: 700; }
    .automation-meta {
      overflow: hidden;
      color: var(--trouve-text-mid);
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 11px;
    }
    .automation-meta.failure { color: var(--trouve-err-soft); }
    .delete-confirmation {
      grid-column: 1 / -1;
      display: grid;
      gap: 7px;
      padding: 9px;
      border: 1px solid var(--trouve-err);
      border-radius: 6px;
      border-color: var(--trouve-err);
      background: var(--trouve-err-bg);
    }
    .delete-confirmation p { color: var(--trouve-err-soft); }
    .confirmation-actions { justify-content: flex-end; }
    .editor { display: grid; gap: 10px; border: 1px solid var(--trouve-border); border-radius: 6px; padding: 14px; background: var(--trouve-raised-bg); }
    .editor-title { color: var(--trouve-text-hi); font-size: 14px; }
    .editor > input, .editor > textarea, .editor-inline select, .schedule-row select, .schedule-row input {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 7px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font: inherit;
    }
    .editor > textarea { min-height: 90px; resize: vertical; }
    .editor-inline, .schedule-row { display: flex; align-items: center; gap: 8px; min-width: 0; }
    .editor-inline > span, .schedule-row > span { flex: none; color: var(--trouve-text-mid); font-size: 12px; }
    .editor-inline select { min-width: 0; flex: 1; }
    .editor-inline trouve-model-picker { min-width: 0; flex: 1; }
    .model-picker { position: relative; display: block; width: 100%; }
    .model-picker-trigger { width: 100%; min-height: 30px; display: grid; grid-template-columns: minmax(0, 1fr) auto auto; align-items: center; gap: 6px; border: 1px solid var(--trouve-border-strong); border-radius: var(--trouve-radius-sm); padding: 4px 8px; color: var(--trouve-text); background: var(--trouve-control-bg); text-align: start; }
    .model-picker-trigger > span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model-picker-popup { position: absolute; z-index: 8; inset-block-start: calc(100% + 4px); inset-inline-start: 0; width: min(480px, calc(100vw - 32px)); display: grid; grid-template-rows: auto minmax(0, 280px); gap: 4px; padding: 5px; border: 1px solid var(--trouve-border-strong); border-radius: 7px; color: var(--trouve-text); background: var(--trouve-popup-bg); box-shadow: 0 10px 30px var(--trouve-scrim); }
    .model-picker-popup > input { width: 100%; min-height: 32px; border: 1px solid var(--trouve-border); border-radius: var(--trouve-radius-sm); padding: 5px 8px; color: var(--trouve-text); background: var(--trouve-control-bg); }
    .model-picker-options { display: block; min-height: 28px; overflow: auto; }
    .model-picker-options > button { width: 100%; min-height: 32px; display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 202px); align-items: center; gap: 8px; border: 0; border-radius: var(--trouve-radius-sm); padding: 4px 7px; color: var(--trouve-text); background: transparent; text-align: start; }
    .model-picker-options > button:hover, .model-picker-options > button.active { background: var(--trouve-hover-bg); }
    .model-picker-options > button[aria-selected="true"] { background: var(--trouve-accent-bg); }
    .model-picker-options > button > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model-picker-empty { min-height: 32px; display: flex; align-items: center; padding: 4px 7px; color: var(--trouve-text-disabled); }
    .schedule-row select { width: 120px; }
    .schedule-row input { width: 84px; }
    .schedule-row input.minute { width: 64px; }
    .schedule-spacer { flex: 1; }
    .yolo-warning { display: grid; gap: 8px; border: 1px solid var(--trouve-warn-border); border-radius: 6px; padding: 10px; color: var(--trouve-text-mid); background: var(--trouve-warn-bg); font-size: 11px; }
    .yolo-warning strong { color: var(--trouve-warn); font-size: 12px; }
    .yolo-warning label { display: flex; grid-auto-flow: initial; align-items: center; gap: 8px; color: var(--trouve-text-mid); font-weight: 400; }
    .yolo-warning input { width: auto; min-height: auto; margin: 0; }
    .templates-section { display: grid; gap: 8px; }
    .templates-list {
      display: grid;
      gap: 8px;
    }
    .template-card {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: center;
      gap: 10px;
      border-radius: 6px;
      padding: 10px;
      background: var(--trouve-surface);
    }
    .template-copy { min-width: 0; display: grid; gap: 3px; }
    .template-title { min-width: 0; display: flex; align-items: baseline; gap: 8px; }
    .template-title strong { overflow: hidden; color: var(--trouve-text-hi); text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
    .template-title span, .template-copy > span { color: var(--trouve-text-dim); font-size: 11px; }
    .template-copy > span { color: var(--trouve-text-mid); }
    .form-section { display: grid; gap: 7px; }
    .form-section > header { padding-top: 2px; }
    .form-grid {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: 9px 10px;
    }
    .field { display: grid; align-content: start; gap: 3px; min-width: 0; }
    .field.wide { grid-column: 1 / -1; }
    .field > label, fieldset > legend {
      color: var(--trouve-text-mid);
      font-weight: 600;
      font-size: 10px;
    }
    .field input:not([type="checkbox"]), .field select, .field textarea {
      width: 100%;
      min-height: 31px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 7px;
      background: var(--trouve-control-bg);
    }
    .field textarea { min-height: 90px; resize: vertical; line-height: 1.45; }
    .field [aria-invalid="true"] { border-color: var(--trouve-err); }
    .field-note { color: var(--trouve-text-dim); font-size: 9px; }
    .field-error { color: var(--trouve-err-soft); font-size: 10px; }
    fieldset { min-width: 0; margin: 0; border: 0; padding: 0; }
    .day-grid {
      display: grid;
      grid-template-columns: repeat(7, minmax(0, 1fr));
      gap: 4px;
      margin-top: 4px;
    }
    .day-option {
      display: grid;
      place-items: center;
      min-height: 26px;
      border: 1px solid var(--trouve-border);
      border-radius: 6px;
      padding: 2px 4px;
      background: var(--trouve-control-bg);
      font-size: 11px;
    }
    .day-option.selected {
      border-color: var(--trouve-primary-border);
      background: var(--trouve-accent-bg);
    }
    .day-option input { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); }
    .permission-warning {
      margin-top: 3px;
      border-left: 2px solid var(--trouve-warn);
      padding-left: 6px;
      color: var(--trouve-warn);
      font-size: 10px;
    }
    .form-summary {
      margin: 0;
      border: 1px solid var(--trouve-err);
      border-radius: var(--trouve-radius-sm);
      padding: 7px 8px;
      color: var(--trouve-err-soft);
      background: var(--trouve-err-bg);
      font-size: 10px;
    }
    .form-actions { justify-content: flex-end; }
    @media (max-width: 620px) {
      .screen { grid-template-rows: calc(52px + env(safe-area-inset-top)) minmax(0, 1fr); }
      .page-header {
        padding-top: env(safe-area-inset-top);
        padding-inline: max(12px, env(safe-area-inset-left)) max(12px, env(safe-area-inset-right));
      }
      .body-column {
        padding: 12px max(12px, env(safe-area-inset-right)) max(12px, env(safe-area-inset-bottom)) max(12px, env(safe-area-inset-left));
      }
      .automation-heading { align-items: stretch; flex-direction: column; }
      .heading-actions { width: 100%; }
      .heading-actions button { flex: 1; }
      .automation-card, .template-card { grid-template-columns: minmax(0, 1fr); }
      .row-actions { justify-content: flex-end; }
      .editor-title { flex-direction: column; }
      .form-grid { grid-template-columns: 1fr; }
      .field.wide { grid-column: auto; }
    }
    @media (max-width: 440px) {
      .status-grid { grid-template-columns: 1fr; }
      .day-grid { grid-template-columns: repeat(4, minmax(0, 1fr)); }
      .form-actions > button { flex: 1; }
    }
    @media (pointer: coarse) {
      button, .switch { min-height: 44px; }
      .field input:not([type="checkbox"]), .field select { min-height: 44px; }
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

  #automations: readonly ProtocolAutomation[] = [];
  #templates: readonly ProtocolAutomationTemplate[] = [];
  #workspaces: readonly ProtocolWorkspace[] = [];
  #modes: readonly ProtocolAgentPersona[] = [];
  #models: readonly ProtocolModelInfo[] = [];
  #providers: ProtocolProvidersResponse | undefined;
  #modesLoading = false;
  #modesError = "";
  #modesGeneration = 0;
  #modesWorkspaceId = "";
  #modelsLoading = true;
  #modelsError = "";
  #selectedId = "";
  #editorMode: EditorMode = "";
  #draft: AutomationDraft = emptyAutomationDraft();
  #draftErrors: AutomationDraftErrors = {};
  #deleteConfirmId = "";
  #busyId = "";
  #loading = true;
  #refreshing = false;
  #polling = false;
  #yoloConfirmed = false;
  #actionError = "";
  #liveError = "";
  #notice = "";
  #loadGeneration = 0;
  #renderedAutomationRevision = 0;
  #refreshedAutomationRevision = 0;
  #pollTimer: ReturnType<typeof setInterval> | undefined;
  #deferredRefreshTimer: ReturnType<typeof setTimeout> | undefined;
  #loadRetryTimer: ReturnType<typeof setTimeout> | undefined;

  #availableModels(): readonly ProtocolModelInfo[] {
    const catalog = this.#services.value?.modelCatalog.current;
    if (catalog === undefined) return this.#models;
    const models = readSignal(catalog);
    return models.length === 0 ? this.#models : models;
  }

  async #loadModes(workspaceId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || workspaceId === "") {
      this.#modesGeneration += 1;
      this.#modes = [];
      this.#modesWorkspaceId = "";
      this.#modesLoading = false;
      return;
    }
    const generation = ++this.#modesGeneration;
    if (workspaceId !== this.#modesWorkspaceId) {
      this.#modes = [];
      this.#modesWorkspaceId = workspaceId;
    }
    this.#modesLoading = true;
    this.#modesError = "";
    this.requestUpdate();
    try {
      const modes = await services.protocol.personas(workspaceId);
      if (
        generation !== this.#modesGeneration
        || workspaceId !== this.#modesWorkspaceId
        || !this.isConnected
      ) return;
      this.#modes = modes;
    } catch {
      if (
        generation !== this.#modesGeneration
        || workspaceId !== this.#modesWorkspaceId
        || !this.isConnected
      ) return;
      this.#modes = [];
      this.#modesError = "Mode choices could not be loaded. The saved value is preserved.";
    } finally {
      if (generation === this.#modesGeneration) {
        this.#modesLoading = false;
        this.requestUpdate();
      }
    }
  }

  override connectedCallback(): void {
    super.connectedCallback();
    if (this.#pollTimer === undefined) {
      this.#pollTimer = globalThis.setInterval(() => {
        if (globalThis.document.visibilityState === "visible") {
          void this.#refreshAutomations();
        }
      }, 15_000);
    }
  }

  protected override firstUpdated(): void {
    void this.refresh();
  }

  protected override updated(): void {
    if (
      this.#renderedAutomationRevision <= this.#refreshedAutomationRevision
      || this.#loading
      || this.#polling
      || this.#busyId !== ""
    ) return;
    this.#refreshedAutomationRevision = this.#renderedAutomationRevision;
    void this.#refreshAutomations();
  }

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#modesGeneration += 1;
    if (this.#pollTimer !== undefined) globalThis.clearInterval(this.#pollTimer);
    if (this.#deferredRefreshTimer !== undefined) {
      globalThis.clearTimeout(this.#deferredRefreshTimer);
    }
    if (this.#loadRetryTimer !== undefined) globalThis.clearTimeout(this.#loadRetryTimer);
    this.#pollTimer = undefined;
    this.#deferredRefreshTimer = undefined;
    this.#loadRetryTimer = undefined;
    super.disconnectedCallback();
  }

  /** Refreshes the automation snapshot and the templates used by this screen. */
  async refresh(): Promise<void> {
    this.#clearLoadRetry();
    const services = this.#services.value;
    if (services === undefined) {
      this.#loading = false;
      this.#actionError = "Automation services are unavailable.";
      this.#scheduleLoadRetry();
      this.requestUpdate();
      return;
    }
    const generation = ++this.#loadGeneration;
    this.#loading = this.#automations.length === 0;
    this.#refreshing = !this.#loading;
    this.#actionError = "";
    this.#modelsLoading = true;
    this.#modelsError = "";
    this.requestUpdate();
    void services.modelCatalog.refresh("if-stale").then(
      (models) => {
        if (generation !== this.#loadGeneration || !this.isConnected) return;
        this.#models = models;
      },
      () => {
        if (generation !== this.#loadGeneration || !this.isConnected) return;
        this.#modelsError = "Model choices could not be loaded. Existing defaults are preserved.";
        this.#scheduleLoadRetry();
      },
    ).finally(() => {
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#modelsLoading = false;
      this.requestUpdate();
    });
    try {
      const [automations, templates, workspaces, providers] = await Promise.all([
        services.protocol.automations(),
        services.protocol.automationTemplates(),
        services.protocol.workspaces(),
        services.protocol.providers(),
      ]);
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      const selected = automations.find((automation) => automation.id === this.#selectedId);
      const workspaceId =
        this.#editorMode === ""
          ? (selected?.workspace_id ?? workspaces[0]?.id ?? "")
          : (this.#draft.workspaceId || workspaces[0]?.id || "");
      this.#automations = automations;
      this.#templates = templates;
      this.#workspaces = workspaces;
      this.#providers = providers;
      if (!automations.some((automation) => automation.id === this.#selectedId)) {
        this.#selectedId = automations[0]?.id ?? "";
        if (this.#editorMode === "edit") this.#editorMode = "";
      }
      if (this.#editorMode === "create" && this.#draft.workspaceId === "" && workspaceId !== "") {
        this.#draft = { ...this.#draft, workspaceId };
      }
      if (this.#editorMode !== "") void this.#loadModes(workspaceId);
      this.#liveError = "";
    } catch {
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#actionError = "Automations could not be loaded. Retrying automatically.";
      this.#liveError = "Reconnect needed";
      this.#scheduleLoadRetry();
    } finally {
      if (generation === this.#loadGeneration) {
        this.#loading = false;
        this.#refreshing = false;
        this.requestUpdate();
      }
    }
  }

  override render() {
    const store = this.#store.value;
    const server = store === undefined ? undefined : readSignal(store.serverInfo);
    this.#renderedAutomationRevision = store === undefined
      ? 0
      : readSignal(store.automationRevision);
    const offline = server?.online === false;

    return html`
      <section class="screen" aria-labelledby="automations-title">
        <header class="page-header">
          <h1 id="automations-title">Automations</h1>
          <button type="button" @click=${this.#close}>${fontAwesomeIcon("xmark")} Close</button>
        </header>
        <main class="body-scroll">
          <div class="body-column">
            <div class="automation-heading">
              <h2>Scheduled prompts</h2>
              <div class="heading-actions">
                <button
                  class="primary"
                  type="button"
                  ?disabled=${this.#workspaces.length === 0 || offline}
                  @click=${this.#startCreate}
                >${fontAwesomeIcon("plus")} New automation</button>
              </div>
            </div>
            <p class="description">Each run creates a fresh session in the chosen workspace and sends the prompt — exactly as if you had typed it. Times follow this machine's clock; runs missed while trouve was closed are skipped.</p>
            ${this.#workspaces.length === 0 && !this.#loading
              ? html`<p class="inline-warning">Open a workspace first — automations run their prompts inside one.</p>`
              : nothing}
            ${offline
              ? html`<p class="banner warning" role="status">The server is offline. Saved schedules remain visible, but changes and manual runs are unavailable.</p>`
              : this.#liveError === ""
                ? nothing
                : html`<p class="banner warning" role="status">${this.#liveError}</p>`}
            ${this.#actionError === ""
              ? this.#notice === ""
                ? nothing
                : html`<p class="banner notice" role="status" aria-live="polite">${this.#notice}</p>`
              : html`<p class="banner error" role="alert">${this.#actionError}</p>`}
            ${this.#loading && this.#automations.length === 0
              ? html`<div class="loading-card" role="status">Loading automations…</div>`
              : this.#renderList()}
            ${this.#editorMode === "" ? this.#renderTemplates() : this.#renderEditor()}
          </div>
        </main>
      </section>
    `;
  }

  #renderList() {
    if (this.#automations.length === 0) {
      return this.#editorMode === ""
        ? html`<div class="empty-card">No automations yet.</div>`
        : nothing;
    }
    return html`
      <ol class="automation-list">
        ${this.#automations.map((automation) => {
          const busy = this.#busyId === automation.id;
          const permission = automation.permission_mode ?? "ask";
          return html`
            <li>
              <article class="automation-card">
                <div class="automation-copy">
                  <div class="automation-title">
                    <strong>${automation.name}</strong>
                    ${automation.enabled ? nothing : html`<span>paused</span>`}
                    <span class=${permission === "yolo" ? "yolo" : ""}>${permission === "yolo" ? "YOLO" : permission === "allow_list" ? "allow-list" : "Ask"}</span>
                  </div>
                  <span class="automation-meta">${automationScheduleSummary(automation.schedule)}</span>
                  ${automation.enabled && automation.next_run_at != null
                    ? html`<span class="automation-meta">Next: ${formatTimestamp(automation.next_run_at)}</span>`
                    : nothing}
                  ${automation.last_error
                    ? html`<span class="automation-meta failure">Last run failed: ${automation.last_error}</span>`
                    : automation.last_run_at == null
                      ? nothing
                      : html`<span class="automation-meta">Last: ${formatTimestamp(automation.last_run_at)}</span>`}
                </div>
                <div class="row-actions">
                  <button type="button" ?disabled=${busy} @click=${() => void this.#toggleEnabled(automation, !automation.enabled)}>${automation.enabled ? "Pause" : "Resume"}</button>
                  <button type="button" ?disabled=${busy} @click=${() => void this.#runNow(automation)}>Run now</button>
                  <button type="button" ?disabled=${busy} @click=${() => this.#startEdit(automation)}>Edit</button>
                  <button class="danger" type="button" ?disabled=${busy} @click=${() => this.#confirmDelete(automation.id)}>Delete</button>
                </div>
                ${this.#deleteConfirmId === automation.id
                  ? html`
                      <section class="delete-confirmation" role="alertdialog" aria-labelledby=${`delete-automation-title-${automation.id}`} aria-describedby=${`delete-automation-copy-${automation.id}`}>
                        <h3 id=${`delete-automation-title-${automation.id}`}>Delete “${automation.name}”?</h3>
                        <p id=${`delete-automation-copy-${automation.id}`}>This permanently removes the schedule. Sessions created by earlier runs are kept.</p>
                        <div class="confirmation-actions">
                          <button type="button" @click=${this.#cancelDelete}>Cancel</button>
                          <button class="danger" type="button" ?disabled=${busy} @click=${() => void this.#deleteAutomation(automation)}>Delete automation</button>
                        </div>
                      </section>
                    `
                  : nothing}
              </article>
            </li>
          `;
        })}
      </ol>
    `;
  }

  #renderTemplates() {
    if (this.#templates.length === 0 || this.#workspaces.length === 0) return nothing;
    return html`
      <section class="templates-section" aria-labelledby="automation-template-title">
        <h2 id="automation-template-title">Start from a template</h2>
        <p class="template-description">Common development chores, ready to schedule. Picking one just pre-fills the form — edit anything before saving.</p>
        <div class="templates-list">
          ${this.#templates.map((template) => html`
            <article class="template-card">
              <div class="template-copy">
                <div class="template-title">
                  <strong>${template.name}</strong>
                  <span>${automationScheduleSummary(template.schedule)}</span>
                </div>
                <span>${template.description}</span>
              </div>
              <button type="button" aria-label=${`Use ${template.name} template`} @click=${() => this.#useTemplate(template)}>Use</button>
            </article>
          `)}
        </div>
      </section>
    `;
  }

  #renderEditor() {
    const models = this.#availableModels();
    const modes = this.#modesWorkspaceId === this.#draft.workspaceId ? this.#modes : [];
    const editing = this.#editorMode === "edit";
    const selectedModel = this.#effectiveAutomationModel(this.#draft, modes);
    const effectiveModelId = selectedModel?.id
      ?? this.#draft.model
      ?? "";
    const modelControls = modelOptionControls(selectedModel, this.#draft.modelOptions);
    const nameError = this.#draftErrors.name;
    const promptError = this.#draftErrors.prompt;
    const workspaceError = this.#draftErrors.workspaceId;
    const scheduleError = this.#draftErrors.schedule;
    const invalid = hasAutomationDraftErrors(this.#draftErrors);
    const busy = this.#busyId === (editing ? this.#selectedId : "new");
    const currentErrors = validateAutomationDraft(this.#draft);
    const yoloNeedsConfirmation = this.#draft.permissionMode === "yolo" && !this.#yoloConfirmed;
    const canSave = !hasAutomationDraftErrors(currentErrors) && !yoloNeedsConfirmation && !busy;
    return html`
      <form class="editor" novalidate @submit=${this.#saveAutomation}>
        <h2 class="editor-title">${editing ? "Edit automation" : "New automation"}</h2>
        <input id="automation-name" name="name" maxlength="200" required placeholder="name (e.g. Nightly dependency audit)" .value=${this.#draft.name} aria-label="Automation name" aria-invalid=${nameError === undefined ? "false" : "true"} @input=${(event: Event) => this.#updateDraft({ name: (event.currentTarget as HTMLInputElement).value })} />
        ${nameError === undefined ? nothing : html`<span class="field-error" id="automation-name-error">${nameError}</span>`}
        <textarea id="automation-prompt" name="prompt" required placeholder="prompt to send" .value=${this.#draft.prompt} aria-label="Prompt to send" aria-invalid=${promptError === undefined ? "false" : "true"} @input=${(event: Event) => this.#updateDraft({ prompt: (event.currentTarget as HTMLTextAreaElement).value })}></textarea>
        ${promptError === undefined ? nothing : html`<span class="field-error" id="automation-prompt-error">${promptError}</span>`}
        <label class="editor-inline" for="automation-workspace"><span>Workspace</span><select id="automation-workspace" name="workspace" required aria-invalid=${workspaceError === undefined ? "false" : "true"} @change=${this.#workspaceChanged}><option value="" ?selected=${this.#draft.workspaceId === ""}>Choose a workspace</option>${this.#workspaces.map((workspace) => html`<option value=${workspace.id} ?selected=${workspace.id === this.#draft.workspaceId}>${workspace.name}</option>`)}</select></label>
        ${workspaceError === undefined ? nothing : html`<span class="field-error" id="automation-workspace-error">${workspaceError}</span>`}
        <label class="editor-inline" for="automation-mode">
          <span>Mode</span>
          <select
            id="automation-mode"
            name="mode"
            .value=${this.#draft.mode}
            ?disabled=${busy || this.#modesLoading}
            @change=${this.#modeChanged}
          >
            <option value="">Server default</option>
            ${this.#draft.mode !== "" && !modes.some((mode) => mode.id === this.#draft.mode)
              ? html`<option value=${this.#draft.mode}>${this.#draft.mode}</option>`
              : nothing}
            ${modes.map(
              (mode) => html`<option value=${mode.id}>${mode.display_name || mode.id}</option>`,
            )}
          </select>
        </label>
        <div class="editor-inline">
          <span>Model</span>
          <trouve-model-picker
            accessible-label="Automation model"
            placement="down"
            placeholder=${`Mode or server default${effectiveModelId === "" ? "" : ` · ${effectiveModelId}`}`}
            empty-label=${`Mode or server default${effectiveModelId === "" ? "" : ` · ${effectiveModelId}`}`}
            .value=${this.#draft.model}
            .models=${models}
            .disabled=${busy || this.#modelsLoading}
            @trouve-model-picked=${this.#modelPicked}
          ></trouve-model-picker>
        </div>
        ${modelControls.length === 0
          ? nothing
          : html`<trouve-model-options-editor
              class="automation-model-options"
              .controls=${modelControls}
              .disabled=${busy || this.#modelsLoading}
              @trouve-model-option-changed=${this.#modelOptionChanged}
            ></trouve-model-options-editor>`}
        ${this.#modelsError === "" && this.#modesError === ""
          ? nothing
          : html`<span class="field-note">${[this.#modesError, this.#modelsError].filter(Boolean).join(" ")}</span>`}
        <label class="editor-inline" for="automation-permission"><span>Permissions</span><select id="automation-permission" name="permission" @change=${(event: Event) => { const permissionMode = (event.currentTarget as HTMLSelectElement).value as AutomationPermissionMode; this.#yoloConfirmed = false; this.#updateDraft({ permissionMode }); }}><option value="ask" ?selected=${this.#draft.permissionMode === "ask"}>Ask before changes (safe)</option><option value="allow_list" ?selected=${this.#draft.permissionMode === "allow_list"}>Allow-list (approve as needed)</option><option value="yolo" ?selected=${this.#draft.permissionMode === "yolo"}>Unattended (YOLO)</option></select></label>
        ${this.#draft.permissionMode === "yolo" ? html`<section class="yolo-warning"><strong>Unattended execution is dangerous</strong><span>The agent can run shell commands and edit or delete files without asking. Repository content can influence those actions. This permission applies only to fresh sessions created by this automation and does not change global defaults.</span><label><input type="checkbox" .checked=${this.#yoloConfirmed} @change=${(event: Event) => { this.#yoloConfirmed = (event.currentTarget as HTMLInputElement).checked; this.requestUpdate(); }} />I understand and want this automation to run without approval</label></section>` : nothing}
        <div class="schedule-row"><span>Runs</span><select aria-label="Frequency" @change=${(event: Event) => this.#updateDraft({ scheduleKind: (event.currentTarget as HTMLSelectElement).value as AutomationScheduleKind })}><option value="hourly" ?selected=${this.#draft.scheduleKind === "hourly"}>Hourly</option><option value="daily" ?selected=${this.#draft.scheduleKind === "daily"}>Daily</option><option value="weekly" ?selected=${this.#draft.scheduleKind === "weekly"}>Weekly</option></select>${this.#draft.scheduleKind === "hourly" ? html`<span>at minute</span><input class="minute" aria-label="Minute of the hour" type="number" min="0" max="59" step="1" .value=${this.#draft.minute} @input=${(event: Event) => this.#updateDraft({ minute: (event.currentTarget as HTMLInputElement).value })} />` : html`<span>at</span><input aria-label="Time of day" type="time" step="60" .value=${this.#draft.time} @input=${(event: Event) => this.#updateDraft({ time: (event.currentTarget as HTMLInputElement).value })} />`}<span class="schedule-spacer"></span></div>
        ${this.#draft.scheduleKind === "weekly" ? html`<div class="day-grid" role="group" aria-label="Days of the week">${AUTOMATION_DAY_NAMES.map((name, day) => html`<label class=${`day-option ${this.#draft.days.includes(day) ? "selected" : ""}`}><input type="checkbox" .checked=${this.#draft.days.includes(day)} aria-label=${name} @change=${() => this.#toggleDay(day)} />${name.slice(0, 3)}</label>`)}</div>` : nothing}
        ${scheduleError === undefined ? nothing : html`<span class="field-error" id="automation-schedule-error">${scheduleError}</span>`}
        ${invalid ? html`<p class="form-summary" role="alert">Correct the highlighted fields before saving.</p>` : nothing}
        <div class="form-actions"><button type="button" ?disabled=${busy} @click=${this.#cancelEditor}>Cancel</button><button class="primary" type="submit" ?disabled=${!canSave}>${busy ? "Saving…" : editing ? "Save" : "Create"}</button></div>
      </form>
    `;
  }

  readonly #startCreate = (): void => {
    const workspaceId =
      this.#automations.find((automation) => automation.id === this.#selectedId)?.workspace_id ??
      this.#workspaces[0]?.id ??
      "";
    this.#editorMode = "create";
    this.#draft = emptyAutomationDraft(workspaceId);
    this.#yoloConfirmed = false;
    this.#draftErrors = {};
    this.#deleteConfirmId = "";
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    void this.#loadModes(workspaceId);
  };

  #startEdit(automation: ProtocolAutomation): void {
    this.#selectedId = automation.id;
    this.#editorMode = "edit";
    this.#draft = automationDraftFrom(automation);
    this.#yoloConfirmed = false;
    this.#draftErrors = {};
    this.#deleteConfirmId = "";
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    void this.#loadModes(automation.workspace_id);
  }

  readonly #cancelEditor = (): void => {
    this.#editorMode = "";
    this.#draftErrors = {};
    this.#actionError = "";
    this.requestUpdate();
  };

  #useTemplate(template: ProtocolAutomationTemplate): void {
    const workspaceId = this.#draft.workspaceId || this.#workspaces[0]?.id || "";
    this.#editorMode = "create";
    this.#draft = automationDraftFromTemplate(template, workspaceId);
    this.#yoloConfirmed = false;
    this.#draftErrors = {};
    this.#notice = `Applied the ${template.name} template. Review it before saving.`;
    this.requestUpdate();
    void this.#loadModes(workspaceId);
  }

  #updateDraft(update: Partial<AutomationDraft>): void {
    this.#draft = { ...this.#draft, ...update };
    this.#draftErrors = {};
    this.#actionError = "";
    this.requestUpdate();
  }

  readonly #workspaceChanged = (event: Event): void => {
    const workspaceId = (event.currentTarget as HTMLSelectElement).value;
    this.#updateDraft({ workspaceId, mode: "", model: "", modelOptions: {} });
    void this.#loadModes(workspaceId);
  };

  readonly #modeChanged = (event: Event): void => {
    const modeId = (event.currentTarget as HTMLSelectElement).value;
    const modes = this.#modesWorkspaceId === this.#draft.workspaceId ? this.#modes : [];
    const previousModel = this.#effectiveAutomationModel(this.#draft, modes);
    const nextModel = this.#effectiveAutomationModel(
      { ...this.#draft, mode: modeId, model: "" },
      modes,
    );
    this.#updateDraft({
      mode: modeId,
      model: "",
      modelOptions: modelOptionsAfterEffectiveModelChange(
        this.#draft.modelOptions,
        previousModel?.id,
        nextModel?.id,
      ),
    });
  };

  readonly #modelPicked = (event: CustomEvent<{ readonly modelId: string }>): void => {
    const modes = this.#modesWorkspaceId === this.#draft.workspaceId ? this.#modes : [];
    const previousModel = this.#effectiveAutomationModel(this.#draft, modes);
    const nextDraft = { ...this.#draft, model: event.detail.modelId };
    const nextModel = this.#effectiveAutomationModel(nextDraft, modes);
    const previousModelId = previousModel?.id ?? this.#draft.model.trim();
    const nextModelId = nextModel?.id ?? event.detail.modelId.trim();
    this.#updateDraft({
      model: event.detail.modelId,
      modelOptions: modelOptionsAfterEffectiveModelChange(
        this.#draft.modelOptions,
        previousModelId,
        nextModelId,
      ),
    });
  };

  readonly #modelOptionChanged = (event: CustomEvent<ModelOptionChangeDetail>): void => {
    this.#updateDraft({
      modelOptions: changeModelOption(this.#draft.modelOptions, event.detail),
    });
  };

  #toggleDay(day: number): void {
    const days = this.#draft.days.includes(day)
      ? this.#draft.days.filter((candidate) => candidate !== day)
      : [...this.#draft.days, day];
    this.#updateDraft({ days });
  }

  readonly #saveAutomation = (event: SubmitEvent): void => {
    event.preventDefault();
    if (this.#draft.permissionMode === "yolo" && !this.#yoloConfirmed) {
      this.#actionError = "Confirm that you understand unattended execution before saving.";
      this.requestUpdate();
      return;
    }
    const errors = validateAutomationDraft(this.#draft);
    if (hasAutomationDraftErrors(errors)) {
      this.#draftErrors = errors;
      this.requestUpdate();
      void this.updateComplete.then(() => {
        this.renderRoot.querySelector<HTMLElement>("[aria-invalid=\"true\"]")?.focus();
      });
      return;
    }
    void this.#persistAutomation();
  };

  async #persistAutomation(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const editing = this.#editorMode === "edit";
    const selectedId = this.#selectedId;
    const draft: AutomationDraft = {
      ...this.#draft,
      modelOptions: { ...this.#draft.modelOptions },
      days: [...this.#draft.days],
    };
    const busyId = editing ? selectedId : "new";
    this.#busyId = busyId;
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    try {
      const model = await this.#modelForMutation(draft);
      if (model === undefined) return;
      const request = automationRequestFromDraft(
        draft,
        model,
      );
      const automation = editing
        ? await services.protocol.updateAutomation(selectedId, request)
        : await services.protocol.createAutomation(request);
      this.#replaceAutomation(automation);
      this.#selectedId = automation.id;
      this.#editorMode = "";
      this.#notice = editing ? "Automation updated." : "Automation created.";
    } catch {
      this.#actionError = editing
        ? "Automation changes could not be saved."
        : "Automation could not be created.";
    } finally {
      if (this.#busyId === busyId) this.#busyId = "";
      this.requestUpdate();
    }
  }

  async #toggleEnabled(automation: ProtocolAutomation, enabled: boolean): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    this.#busyId = automation.id;
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    try {
      this.#replaceAutomation(
        await services.protocol.setAutomationEnabled(
          automation.id,
          { enabled },
        ),
      );
      this.#notice = enabled ? "Automation enabled." : "Automation paused.";
    } catch {
      this.#actionError = enabled
        ? "Automation could not be enabled."
        : "Automation could not be paused.";
    } finally {
      this.#busyId = "";
      this.requestUpdate();
    }
  }

  async #runNow(automation: ProtocolAutomation): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    this.#busyId = automation.id;
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    try {
      await services.protocol.runAutomation(automation.id);
      this.#notice = `Run started for ${automation.name}. Live status will refresh automatically.`;
      if (this.#deferredRefreshTimer !== undefined) {
        globalThis.clearTimeout(this.#deferredRefreshTimer);
      }
      this.#deferredRefreshTimer = globalThis.setTimeout(() => {
        this.#deferredRefreshTimer = undefined;
        void this.#refreshAutomations();
      }, 1_000);
    } catch {
      this.#actionError = "The automation could not be started.";
    } finally {
      this.#busyId = "";
      this.requestUpdate();
    }
  }

  #confirmDelete(id: string): void {
    this.#deleteConfirmId = id;
    this.#actionError = "";
    this.#notice = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.renderRoot.querySelector<HTMLElement>(".delete-confirmation button")?.focus();
    });
  }

  readonly #cancelDelete = (): void => {
    this.#deleteConfirmId = "";
    this.requestUpdate();
  };

  async #deleteAutomation(automation: ProtocolAutomation): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#deleteConfirmId !== automation.id) return;
    this.#busyId = automation.id;
    this.#actionError = "";
    this.requestUpdate();
    try {
      await services.protocol.deleteAutomation(automation.id);
      this.#automations = this.#automations.filter((candidate) => candidate.id !== automation.id);
      this.#selectedId = this.#automations[0]?.id ?? "";
      this.#deleteConfirmId = "";
      this.#notice = `${automation.name} was deleted.`;
    } catch {
      this.#actionError = "Automation could not be deleted.";
    } finally {
      this.#busyId = "";
      this.requestUpdate();
    }
  }


  async #refreshAutomations(): Promise<void> {
    const services = this.#services.value;
    if (
      services === undefined
      || this.#polling
      || this.#refreshing
      || this.#loading
      || this.#busyId !== ""
    ) return;
    const generation = this.#loadGeneration;
    this.#polling = true;
    try {
      const automations = await services.protocol.automations();
      if (generation !== this.#loadGeneration || !this.isConnected) return;
      this.#automations = automations;
      if (!automations.some((automation) => automation.id === this.#selectedId)) {
        this.#selectedId = automations[0]?.id ?? "";
        if (this.#editorMode === "edit") this.#editorMode = "";
      }
      this.#liveError = "";
    } catch {
      if (generation === this.#loadGeneration && this.isConnected) {
        this.#liveError = "Live refresh paused";
      }
    } finally {
      if (generation === this.#loadGeneration) {
        this.#polling = false;
        this.requestUpdate();
      }
    }
  }

  #replaceAutomation(automation: ProtocolAutomation): void {
    const index = this.#automations.findIndex((candidate) => candidate.id === automation.id);
    this.#automations = index < 0
      ? [...this.#automations, automation]
      : this.#automations.map((candidate, candidateIndex) =>
          candidateIndex === index ? automation : candidate,
        );
  }

  #effectiveAutomationModel(
    draft: AutomationDraft = this.#draft,
    modes: readonly ProtocolAgentPersona[] = this.#modesWorkspaceId === draft.workspaceId
      ? this.#modes
      : [],
    models: readonly ProtocolModelInfo[] = this.#availableModels(),
    providers: ProtocolProvidersResponse | undefined = this.#providers,
  ): ProtocolModelInfo | undefined {
    const mode = modes.find((candidate) => candidate.id === (draft.mode || "code"));
    const modelId = draft.model.trim()
      || mode?.default_model?.trim()
      || providers?.default_model.trim()
      || "";
    return models.find((model) => model.id === modelId);
  }

  async #modelForMutation(
    draft: AutomationDraft,
  ): Promise<ProtocolModelInfo | null | undefined> {
    if (Object.keys(draft.modelOptions).length === 0) return null;
    const services = this.#services.value;
    if (services === undefined || draft.workspaceId === "") {
      this.#actionError = "Mode and model metadata are unavailable. No changes were saved.";
      this.requestUpdate();
      return undefined;
    }
    try {
      const [modes, models, providers] = await Promise.all([
        services.protocol.personas(draft.workspaceId),
        services.modelCatalog.refresh("if-stale"),
        services.protocol.providers(),
      ]);
      const modeId = draft.mode || "code";
      if (!modes.some((mode) => mode.id === modeId)) {
        throw new Error(`mode ${modeId} is unavailable`);
      }
      const model = this.#effectiveAutomationModel(draft, modes, models, providers);
      if (model === undefined) throw new Error("effective model metadata is unavailable");
      return model;
    } catch {
      this.#actionError = "Mode and model metadata could not be loaded. No changes were saved.";
      this.requestUpdate();
      return undefined;
    }
  }

  #scheduleLoadRetry(): void {
    if (!this.isConnected || this.#loadRetryTimer !== undefined) return;
    this.#loadRetryTimer = globalThis.setTimeout(() => {
      this.#loadRetryTimer = undefined;
      if (typeof document !== "undefined" && document.visibilityState === "hidden") {
        this.#scheduleLoadRetry();
        return;
      }
      void this.refresh();
    }, AUTOMATION_RETRY_MS);
  }

  #clearLoadRetry(): void {
    if (this.#loadRetryTimer === undefined) return;
    globalThis.clearTimeout(this.#loadRetryTimer);
    this.#loadRetryTimer = undefined;
  }
}

customElements.define("trouve-automations-screen", TrouveAutomationsScreen);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-automations-screen": TrouveAutomationsScreen;
  }
}
