import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  appStoreContext,
  hostCapabilitiesContext,
} from "../contexts/app-contexts.js";
import type { ProtocolWorkspace } from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  emptyWorkspaceRegistrationDraft,
  pickAndRegisterWorkspace,
  validateWorkspaceRegistration,
  workspaceRegistrationRequest,
  type WorkspaceRegistrationDraft,
  type WorkspaceRegistrationErrors,
} from "./workspace-settings-model.js";

export class TrouveWorkspaceSettings extends withSignalTracking(LitElement) {
  static override styles = css`
    :host {
      display: block;
      min-width: 0;
      color: var(--trouve-text);
      font: var(--trouve-font-size)/1.4 var(--trouve-font-sans);
    }
    *, *::before, *::after { box-sizing: border-box; }
    h2, h3, p { margin: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 14px; }
    h3 { color: var(--trouve-text-hi); font-size: 12px; }
    button, input { color: inherit; font: inherit; }
    button {
      min-height: 31px;
      cursor: pointer;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 9px;
      background: var(--trouve-control-bg);
    }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button:disabled, input:disabled { cursor: default; opacity: .58; }
    button:focus-visible, input:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    button.primary {
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    button.primary:hover:not(:disabled) { background: var(--trouve-primary-hover); }
    button.danger { color: var(--trouve-err-soft); }
    button.danger:hover:not(:disabled) { background: var(--trouve-err-bg); }
    .workspace-settings { display: grid; gap: 10px; }
    .section-header {
      display: flex;
      align-items: flex-start;
      gap: 10px;
    }
    .section-title { min-width: 0; flex: 1; }
    .section-title p {
      margin-top: 2px;
      color: var(--trouve-text-dim);
      font-size: 10px;
    }
    .notice, .error {
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius-sm);
      padding: 7px 9px;
      font-size: 10px;
    }
    .notice { color: var(--trouve-text-accent-soft); background: var(--trouve-accent-veil); }
    .error { border-color: var(--trouve-err); color: var(--trouve-err-soft); background: var(--trouve-err-bg); }
    .server-path-copy {
      display: grid;
      grid-template-columns: auto minmax(0, 1fr);
      gap: 8px;
      border: 1px solid var(--trouve-warn-border);
      border-radius: var(--trouve-radius);
      padding: 8px 9px;
      color: var(--trouve-text-mid);
      background: var(--trouve-warn-bg);
      font-size: 10px;
    }
    .server-path-copy strong { color: var(--trouve-warn); }
    .server-path-copy span:first-child {
      display: grid;
      place-content: center;
      width: 20px;
      height: 20px;
      border: 1px solid var(--trouve-warn-border);
      border-radius: 50%;
      color: var(--trouve-warn);
      font-weight: 700;
    }
    .card {
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      background: var(--trouve-surface);
    }
    .card > header {
      display: flex;
      align-items: center;
      gap: 8px;
      min-height: 36px;
      padding: 6px 9px;
      border-bottom: 1px solid var(--trouve-rule);
    }
    .card > header h3 { flex: 1; }
    .count {
      min-width: 22px;
      border-radius: 999px;
      padding: 1px 6px;
      color: var(--trouve-text-dim);
      background: var(--trouve-pill-bg);
      text-align: center;
      font-size: 10px;
    }
    .workspace-list { display: grid; gap: 5px; margin: 0; padding: 7px; list-style: none; }
    .workspace-row {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: center;
      gap: 9px;
      min-height: 52px;
      border: 1px solid var(--trouve-border);
      border-radius: var(--trouve-radius-sm);
      padding: 7px 8px;
      background: var(--trouve-panel-bg);
    }
    .workspace-copy { min-width: 0; }
    .workspace-copy strong, .workspace-copy code { display: block; }
    .workspace-copy strong {
      overflow: hidden;
      color: var(--trouve-text-hi);
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 11px;
    }
    .workspace-copy code {
      margin-top: 2px;
      overflow: hidden;
      color: var(--trouve-text-dim);
      font: 10px/1.4 var(--trouve-font-mono);
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .close-confirmation {
      grid-column: 1 / -1;
      display: grid;
      gap: 6px;
      border-top: 1px solid var(--trouve-err);
      margin: 2px -8px -7px;
      padding: 8px;
      color: var(--trouve-err-soft);
      background: var(--trouve-err-bg);
    }
    .close-confirmation p { font-size: 10px; }
    .confirmation-actions, .form-actions {
      display: flex;
      justify-content: flex-end;
      flex-wrap: wrap;
      gap: 6px;
    }
    .empty-state, .loading-state {
      display: grid;
      place-items: center;
      gap: 5px;
      min-height: 90px;
      padding: 16px;
      color: var(--trouve-text-dim);
      text-align: center;
      font-size: 10px;
    }
    .empty-state strong, .loading-state strong { color: var(--trouve-text-hi); font-size: 11px; }
    .registration-form { display: grid; gap: 9px; padding: 9px; }
    .form-grid {
      display: grid;
      grid-template-columns: minmax(0, 1.55fr) minmax(180px, .75fr);
      gap: 9px;
    }
    label { display: grid; gap: 3px; min-width: 0; }
    label > span:first-child { color: var(--trouve-text-mid); font-weight: 600; font-size: 10px; }
    input {
      width: 100%;
      min-height: 32px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 7px;
      background: var(--trouve-control-bg);
    }
    input[aria-invalid="true"] { border-color: var(--trouve-err); }
    .field-note { color: var(--trouve-text-dim); font-size: 9px; }
    .field-error { color: var(--trouve-err-soft); font-size: 10px; }
    @media (max-width: 620px) {
      .section-header { flex-direction: column; }
      .section-header > button { width: 100%; }
      .form-grid { grid-template-columns: 1fr; }
      .workspace-row { grid-template-columns: minmax(0, 1fr); }
      .workspace-row > button { justify-self: stretch; }
      .close-confirmation { grid-column: 1; }
      .form-actions > button { flex: 1; }
    }
    @media (pointer: coarse) {
      button, input { min-height: 44px; }
      .workspace-row { min-height: 60px; }
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
  readonly #capabilities = new ContextConsumer(this, {
    context: hostCapabilitiesContext,
    subscribe: true,
  });

  #draft: WorkspaceRegistrationDraft = emptyWorkspaceRegistrationDraft();
  #draftErrors: WorkspaceRegistrationErrors = {};
  #confirmCloseId = "";
  #busyId = "";
  #loading = true;
  #refreshing = false;
  #error = "";
  #notice = "";

  protected override firstUpdated(): void {
    void this.refresh();
  }

  /** Replaces the shared workspace projection with a fresh protocol snapshot. */
  async refresh(): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) {
      this.#loading = false;
      this.#error = "Workspace services are unavailable.";
      this.requestUpdate();
      return;
    }
    this.#refreshing = true;
    this.#error = "";
    this.requestUpdate();
    try {
      await this.#refreshStore();
    } catch {
      this.#error = "Workspaces could not be refreshed. Check the connection and retry.";
    } finally {
      this.#loading = false;
      this.#refreshing = false;
      this.requestUpdate();
    }
  }

  override render() {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) {
      return html`<div class="loading-state" role="status"><strong>Loading workspaces…</strong></div>`;
    }
    const workspaces = readSignal(store.workspaces);
    const directoryPickerAvailable =
      services.nativeHost !== undefined &&
      this.#capabilities.value !== undefined &&
      readSignal(this.#capabilities.value.current).directoryPicker;
    const pathError = this.#draftErrors.path;
    const mutationBusy = this.#busyId !== "";
    return html`
      <section class="workspace-settings" aria-labelledby="workspace-settings-title">
        <header class="section-header">
          <div class="section-title">
            <h2 id="workspace-settings-title">Workspaces</h2>
            <p>Repositories available for sessions, worktrees, modes, and automations.</p>
          </div>
          <button type="button" aria-label="Refresh workspaces" ?disabled=${this.#refreshing || mutationBusy} @click=${() => void this.refresh()}>
            ${this.#refreshing ? "Refreshing…" : "Refresh"}
          </button>
        </header>
        ${services.deployment === "pwa"
          ? html`
              <p class="server-path-copy">
                <span aria-hidden="true">i</span>
                <span><strong>PWA server path</strong><br />The path is on the machine running trouve-server, not on this device. Enter an absolute repository path already available to the server; this PWA cannot browse the server filesystem.</span>
              </p>
            `
          : html`
              <p class="server-path-copy">
                <span aria-hidden="true">i</span>
                <span><strong>Server path</strong><br />Enter an absolute repository path on the machine running trouve-server. The server validates and opens it.</span>
              </p>
            `}
        ${this.#error === ""
          ? this.#notice === ""
            ? nothing
            : html`<p class="notice" role="status" aria-live="polite">${this.#notice}</p>`
          : html`<p class="error" role="alert">${this.#error}</p>`}
        <section class="card" aria-labelledby="current-workspaces-title">
          <header>
            <h3 id="current-workspaces-title">Current workspaces</h3>
            <span class="count" aria-label=${`${workspaces.length} workspaces`}>${workspaces.length}</span>
          </header>
          ${this.#loading && workspaces.length === 0
            ? html`<div class="loading-state" role="status"><strong>Loading workspaces…</strong></div>`
            : workspaces.length === 0
              ? html`<div class="empty-state"><strong>No workspaces registered</strong><span>Add a repository path below to get started.</span></div>`
              : html`
                  <ul class="workspace-list">
                    ${workspaces.map((workspace) => this.#renderWorkspace(workspace))}
                  </ul>
                `}
        </section>
        <section class="card" aria-labelledby="register-workspace-title">
          <header><h3 id="register-workspace-title">Register a workspace</h3></header>
          <form class="registration-form" novalidate @submit=${this.#registerWorkspace}>
            <div class="form-grid">
              <label for="workspace-path">
                <span>Repository path on server host</span>
                <input
                  id="workspace-path"
                  name="path"
                  required
                  autocomplete="off"
                  spellcheck="false"
                  placeholder="/srv/repos/project"
                  .value=${this.#draft.path}
                  aria-invalid=${pathError === undefined ? "false" : "true"}
                  aria-describedby=${pathError === undefined ? "workspace-path-note" : "workspace-path-note workspace-path-error"}
                  ?disabled=${mutationBusy}
                  @input=${(event: Event) => this.#updateDraft({ path: (event.currentTarget as HTMLInputElement).value })}
                />
                <span class="field-note" id="workspace-path-note">Must point to a Git repository accessible to trouve-server.</span>
                ${pathError === undefined ? nothing : html`<span class="field-error" id="workspace-path-error">${pathError}</span>`}
              </label>
              <label for="workspace-name">
                <span>Display name (optional)</span>
                <input
                  id="workspace-name"
                  name="name"
                  autocomplete="off"
                  maxlength="200"
                  placeholder="Derived from folder name"
                  .value=${this.#draft.name}
                  ?disabled=${mutationBusy}
                  @input=${(event: Event) => this.#updateDraft({ name: (event.currentTarget as HTMLInputElement).value })}
                />
                <span class="field-note">Leave blank to use the repository folder name.</span>
              </label>
            </div>
            <div class="form-actions">
              ${directoryPickerAvailable
                ? html`<button type="button" ?disabled=${mutationBusy} @click=${() => void this.#pickAndRegisterWorkspace()}>
                    ${this.#busyId === "pick" ? "Choosing…" : "Choose folder & register"}
                  </button>`
                : nothing}
              <button class="primary" type="submit" ?disabled=${mutationBusy}>
                ${this.#busyId === "register" ? "Registering…" : "Register workspace"}
              </button>
            </div>
          </form>
        </section>
      </section>
    `;
  }

  #renderWorkspace(workspace: ProtocolWorkspace) {
    const busy = this.#busyId === workspace.id;
    const confirming = this.#confirmCloseId === workspace.id;
    return html`
      <li class="workspace-row">
        <span class="workspace-copy">
          <strong>${workspace.name}</strong>
          <code title=${workspace.path}>${workspace.path}</code>
        </span>
        <button class="danger" type="button" ?disabled=${this.#busyId !== ""} aria-expanded=${confirming ? "true" : "false"} @click=${() => this.#confirmClose(workspace.id)}>
          Close
        </button>
        ${confirming
          ? html`
              <section class="close-confirmation" role="alertdialog" aria-labelledby=${`close-workspace-${workspace.id}`} aria-describedby=${`close-workspace-copy-${workspace.id}`}>
                <h3 id=${`close-workspace-${workspace.id}`}>Close “${workspace.name}”?</h3>
                <p id=${`close-workspace-copy-${workspace.id}`}>This removes it from the active workspace list and stops its live terminals. Stored sessions and worktrees are kept; registering the same path later reopens it.</p>
                <div class="confirmation-actions">
                  <button type="button" ?disabled=${busy} @click=${this.#cancelClose}>Cancel</button>
                  <button class="danger" type="button" ?disabled=${busy} @click=${() => void this.#closeWorkspace(workspace)}>${busy ? "Closing…" : "Close workspace"}</button>
                </div>
              </section>
            `
          : nothing}
      </li>
    `;
  }

  #updateDraft(update: Partial<WorkspaceRegistrationDraft>): void {
    this.#draft = { ...this.#draft, ...update };
    this.#draftErrors = {};
    this.#error = "";
    this.requestUpdate();
  }

  readonly #registerWorkspace = (event: SubmitEvent): void => {
    event.preventDefault();
    const errors = validateWorkspaceRegistration(this.#draft);
    if (errors.path !== undefined) {
      this.#draftErrors = errors;
      this.requestUpdate();
      void this.updateComplete.then(() => {
        this.renderRoot.querySelector<HTMLInputElement>("#workspace-path")?.focus();
      });
      return;
    }
    void this.#performRegistration();
  };

  async #performRegistration(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#store.value === undefined) return;
    this.#busyId = "register";
    this.#error = "";
    this.#notice = "";
    this.requestUpdate();
    let workspace: ProtocolWorkspace;
    try {
      workspace = await services.protocol.registerWorkspace(
        workspaceRegistrationRequest(this.#draft),
      );
    } catch {
      this.#busyId = "";
      this.#error = "Workspace could not be registered. Verify the server-host path and try again.";
      this.requestUpdate();
      return;
    }

    await this.#finishRegistration(workspace, true);
  }

  async #pickAndRegisterWorkspace(): Promise<void> {
    const services = this.#services.value;
    if (
      services?.nativeHost === undefined ||
      this.#store.value === undefined ||
      this.#busyId !== ""
    ) {
      return;
    }
    this.#busyId = "pick";
    this.#error = "";
    this.#notice = "";
    this.requestUpdate();
    let workspace: ProtocolWorkspace | undefined;
    try {
      workspace = await pickAndRegisterWorkspace(
        services.nativeHost,
        services.protocol,
      );
    } catch {
      this.#busyId = "";
      this.#error =
        "Workspace could not be opened. Verify the selected repository and try again.";
      this.requestUpdate();
      return;
    }
    if (workspace === undefined) {
      this.#busyId = "";
      this.requestUpdate();
      return;
    }
    await this.#finishRegistration(workspace, false);
  }

  async #finishRegistration(
    workspace: ProtocolWorkspace,
    clearDraft: boolean,
  ): Promise<void> {
    const store = this.#store.value;
    if (store === undefined) return;

    const current = readSignal(store.workspaces);
    store.replaceWorkspaces([
      ...current.filter((candidate) => candidate.id !== workspace.id),
      workspace,
    ]);
    if (clearDraft) {
      this.#draft = emptyWorkspaceRegistrationDraft();
      this.#draftErrors = {};
    }
    this.#notice = `${workspace.name} is ready for new sessions.`;
    try {
      await this.#refreshStore();
    } catch {
      this.#notice = `${workspace.name} was registered, but the workspace list could not be refreshed.`;
    } finally {
      this.#busyId = "";
      this.requestUpdate();
    }
  }

  #confirmClose(workspaceId: string): void {
    this.#confirmCloseId = workspaceId;
    this.#error = "";
    this.#notice = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.renderRoot.querySelector<HTMLElement>(".close-confirmation button")?.focus();
    });
  }

  readonly #cancelClose = (): void => {
    this.#confirmCloseId = "";
    this.requestUpdate();
  };

  async #closeWorkspace(workspace: ProtocolWorkspace): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (
      services === undefined ||
      store === undefined ||
      this.#confirmCloseId !== workspace.id
    ) return;
    this.#busyId = workspace.id;
    this.#error = "";
    this.requestUpdate();
    try {
      await services.protocol.closeWorkspace(workspace.id);
    } catch {
      this.#busyId = "";
      this.#error = "Workspace could not be closed. Try again.";
      this.requestUpdate();
      return;
    }

    store.replaceWorkspaces(
      readSignal(store.workspaces).filter((candidate) => candidate.id !== workspace.id),
    );
    this.#confirmCloseId = "";
    const route = readSignal(services.router.route);
    if (route.kind === "session" && route.workspaceId === workspace.id) {
      services.router.navigate({ kind: "inbox" }, true);
    }
    this.#notice = `${workspace.name} was closed. Its stored sessions and worktrees were kept.`;
    try {
      await this.#refreshStore();
    } catch {
      this.#notice = `${workspace.name} was closed, but the workspace list could not be refreshed.`;
    } finally {
      this.#busyId = "";
      this.requestUpdate();
    }
  }

  async #refreshStore(): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) throw new TypeError("missing context");
    const workspaces = await services.protocol.workspaces();
    store.replaceWorkspaces(workspaces);
    if (!workspaces.some((workspace) => workspace.id === this.#confirmCloseId)) {
      this.#confirmCloseId = "";
    }
  }
}

customElements.define("trouve-workspace-settings", TrouveWorkspaceSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-workspace-settings": TrouveWorkspaceSettings;
  }
}
