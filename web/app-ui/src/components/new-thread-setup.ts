import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  hostCapabilitiesContext,
  sessionContext,
  workspaceContext,
} from "../contexts/app-contexts.js";
import {
  AttachmentEncodingError,
  encodeAttachment,
  pendingAttachmentPreviewUrl,
  type PendingAttachment,
} from "../services/attachments.js";
import { readSignal } from "../state/reactivity.js";
import type { ProtocolSubscriptionHealth } from "../services/protocol-client.js";
import { modelHealthPresentations } from "./model-health.js";
import { modelOptionLabel } from "./model-option-controls.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import {
  appendNewThreadAttachment,
  createInitialNewThreadDraft,
  createNewThreadSetupSubmission,
  formatNewThreadAttachmentBytes,
  newThreadAttachmentLimitMessage,
  newThreadSetupControls,
  newThreadThinkingOption,
  selectNewThreadMode,
  selectNewThreadModel,
  type NewThreadPermissionSelection,
  type NewThreadSetupCancelDetail,
  type NewThreadSetupCatalog,
  type NewThreadSetupDraft,
  type NewThreadSetupSubmitDetail,
} from "./new-thread-setup-model.js";
import "./image-preview.js";

const OPTIONS_RETRY_MS = 5_000;
import "./model-picker.js";

export const NEW_THREAD_SETUP_SUBMIT_EVENT = "trouve-new-thread-submit" as const;
export const NEW_THREAD_SETUP_CANCEL_EVENT = "trouve-new-thread-cancel" as const;

export type NewThreadSetupSubmitEvent = CustomEvent<NewThreadSetupSubmitDetail>;
export type NewThreadSetupCancelEvent = CustomEvent<NewThreadSetupCancelDetail>;
export type {
  NewThreadSetupCancelDetail,
  NewThreadSetupSubmitDetail,
} from "./new-thread-setup-model.js";

const emptyCatalog = (): NewThreadSetupCatalog => ({
  modes: [],
  models: [],
  providers: undefined,
});

export class TrouveNewThreadSetup extends LitElement {
  static override properties = {
    workspaceId: { type: String, attribute: "workspace-id" },
    sessionId: { type: String, attribute: "session-id" },
    sessionTitle: { type: String, attribute: "session-title" },
    disabled: { type: Boolean, reflect: true },
    disabledMessage: { type: String, attribute: "disabled-message" },
    busy: { type: Boolean, reflect: true },
    errorMessage: { type: String, attribute: "error-message" },
    catalogModes: { attribute: false },
    catalogModels: { attribute: false },
    subscriptionHealth: { attribute: false },
  };

  static override styles = css`
    :host {
      display: block;
      width: 100%;
      height: 100%;
      min-width: 0;
      min-height: 0;
      overflow: auto;
      color: var(--trouve-text);
      background: var(--trouve-win-bg);
      font: var(--trouve-font-size)/var(--trouve-line-height) var(--trouve-font-sans);
    }
    * { box-sizing: border-box; }
    button, input, select, textarea { font: inherit; }
    button { color: inherit; }
    button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 1px;
    }
    button:disabled, input:disabled, select:disabled, textarea:disabled {
      cursor: not-allowed;
      opacity: .56;
    }
    form {
      width: min(760px, 100%);
      min-height: 100%;
      display: grid;
      align-content: center;
      gap: 14px;
      margin-inline: auto;
      padding: clamp(20px, 5vw, 40px);
    }
    header { display: grid; gap: 4px; }
    h2 { margin: 0; color: var(--trouve-text-hi); font-size: 22px; line-height: 1.2; }
    header p { margin: 0; color: var(--trouve-text-dim); font-size: 13px; }
    .option-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
    .option-grid.no-thinking .permission-field { grid-column: 1 / -1; }
    label, .field-label { min-width: 0; display: grid; gap: 6px; color: var(--trouve-text-mid); font-size: 12px; font-weight: 700; }
    select, textarea {
      width: 100%;
      min-width: 0;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 6px 8px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font-weight: 400;
    }
    select { min-height: 30px; }
    trouve-model-picker { display: block; width: 100%; }
    .model-picker { position: relative; display: block; width: 100%; font-weight: 400; }
    .model-picker-trigger { width: 100%; min-height: 30px; display: grid; grid-template-columns: minmax(0, 1fr) auto auto; align-items: center; gap: 6px; border: 1px solid var(--trouve-border-strong); border-radius: var(--trouve-radius-sm); padding: 4px 8px; color: var(--trouve-text); background: var(--trouve-control-bg); text-align: start; }
    .model-picker-trigger > span:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model-picker-popup { position: absolute; z-index: 8; inset-block-start: calc(100% + 4px); inset-inline-start: 0; width: min(480px, calc(100vw - 32px)); display: grid; grid-template-rows: auto minmax(0, 280px); gap: 4px; padding: 5px; border: 1px solid var(--trouve-border-strong); border-radius: 7px; color: var(--trouve-text); background: var(--trouve-popup-bg); box-shadow: 0 10px 30px var(--trouve-scrim); }
    .model-picker-popup > input { width: 100%; min-height: 32px; border: 1px solid var(--trouve-border); border-radius: var(--trouve-radius-sm); padding: 5px 8px; color: var(--trouve-text); background: var(--trouve-control-bg); }
    .model-picker-options { display: block; min-height: 28px; overflow: auto; }
    .model-picker-options > button { width: 100%; min-height: 32px; display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 202px); align-items: center; gap: 8px; border: 0; border-radius: var(--trouve-radius-sm); padding: 4px 7px; color: var(--trouve-text); background: transparent; text-align: start; }
    .model-picker-options > button:hover, .model-picker-options > button.active { background: var(--trouve-hover-bg); }
    .model-picker-options > button[aria-selected="true"] { background: var(--trouve-accent-bg); }
    .model-picker-options > button > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .model-picker-options small { min-width: 0; display: flex; align-items: center; gap: 5px; overflow: hidden; color: var(--trouve-text-dim); text-overflow: ellipsis; white-space: nowrap; }
    .model-picker-empty { min-height: 32px; display: flex; align-items: center; padding: 4px 7px; color: var(--trouve-text-disabled); }
    .model-health-dot { width: 7px; height: 7px; display: inline-block; flex: none; border-radius: 50%; background: var(--trouve-text-faint); }
    .model-health-dot.tone-ok { background: var(--trouve-ok); }
    .model-health-dot.tone-warning { background: var(--trouve-warn); }
    .model-health-dot.tone-error { background: var(--trouve-err); }
    .tone-warning { color: var(--trouve-warn) !important; }
    .tone-error { color: var(--trouve-err) !important; }
    textarea { height: 34px; min-height: 34px; max-height: 144px; resize: vertical; line-height: 1.45; }
    textarea::placeholder { color: var(--trouve-text-faint); }
    .permission-yolo > span, select.permission-yolo { color: var(--trouve-err); }
    select.permission-yolo { border-color: var(--trouve-err); font-weight: 700; }
    .yolo-warning {
      display: grid;
      gap: 3px;
      border: 1px solid var(--trouve-err);
      border-radius: var(--trouve-radius);
      padding: 10px;
      color: var(--trouve-err);
      background: var(--trouve-err-bg);
      font-size: 11px;
    }
    .yolo-warning strong { font-size: 12px; }
    .attachment-list { display: flex; flex-wrap: wrap; gap: 6px; margin: 0; padding: 0; list-style: none; }
    .attachment-list li {
      width: min(100%, 310px);
      min-width: 170px;
      display: grid;
      grid-template-columns: auto minmax(0, 1fr) auto;
      align-items: center;
      gap: 7px;
      overflow: hidden;
      border: 1px solid var(--trouve-border);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 7px;
      color: var(--trouve-text-mid);
      background: var(--trouve-surface);
      font-size: 10px;
    }
    .attachment-icon {
      width: 64px;
      height: 48px;
      border-radius: 3px;
      background: var(--trouve-code-bg);
    }
    .attachment-icon { display: grid; place-items: center; color: var(--trouve-text-faint); font-size: 17px; }
    .attachment-details { min-width: 0; display: grid; gap: 2px; }
    .attachment-details strong, .attachment-details small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .attachment-details strong { color: var(--trouve-text-mid); }
    .attachment-details small { color: var(--trouve-text-dim); }
    .attachment-list li button {
      width: 24px;
      height: 24px;
      flex: none;
      border: 0;
      border-radius: 50%;
      padding: 0;
      color: var(--trouve-text-dim);
      background: transparent;
    }
    .attachment-list li button:hover { color: var(--trouve-text-hi); background: var(--trouve-hover-bg); }
    .notice { margin: 0; color: var(--trouve-text-dim); font-size: 11px; }
    .notice.error { color: var(--trouve-err); }
    .notice.warning { color: var(--trouve-warn); }
    .notice-row { display: flex; align-items: center; gap: 8px; }
    .notice-row .notice { min-width: 0; flex: 1; }
    .notice-row button {
      min-height: 28px;
      border: 1px solid var(--trouve-border);
      border-radius: var(--trouve-radius-sm);
      padding: 3px 8px;
      background: var(--trouve-control-bg);
    }
    footer { display: flex; align-items: center; gap: 8px; }
    footer button, .attachment-picker {
      min-height: 30px;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 5px 10px;
      color: var(--trouve-text);
      background: var(--trouve-control-bg);
      font-weight: 400;
    }
    footer button:hover, .attachment-picker:hover { background: var(--trouve-hover-bg); }
    footer button.primary { border-color: var(--trouve-accent); color: var(--trouve-accent-fg, white); background: var(--trouve-accent); }
    .attachment-picker { position: relative; gap: 5px; cursor: pointer; }
    .attachment-picker.icon-only { width: 30px; padding: 0; font-size: 15px; }
    .visually-hidden { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; }
    .attachment-picker:focus-within { outline: 2px solid var(--trouve-accent); outline-offset: 1px; }
    .attachment-picker.disabled { cursor: not-allowed; opacity: .56; }
    .attachment-picker input { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); }
    .spacer { flex: 1; }
    @media (max-width: 560px) {
      form { align-content: start; padding: 18px 12px max(18px, env(safe-area-inset-bottom)); }
      .option-grid { grid-template-columns: 1fr; }
      footer { flex-wrap: wrap; }
      footer button, .attachment-picker { min-height: 42px; }
      footer .spacer { display: none; }
    }
    @media (forced-colors: active) {
      .yolo-warning { border: 2px solid Mark; }
      footer button.primary { color: HighlightText; background: Highlight; }
    }
  `;

  workspaceId = "";
  sessionId = "";
  sessionTitle = "";
  disabled = false;
  disabledMessage = "";
  busy = false;
  errorMessage = "";
  catalogModes: NewThreadSetupCatalog["modes"] = [];
  catalogModels: NewThreadSetupCatalog["models"] = [];
  subscriptionHealth: readonly ProtocolSubscriptionHealth[] = [];

  #catalog = emptyCatalog();
  #draft: NewThreadSetupDraft = createInitialNewThreadDraft(this.#catalog);
  #optionsLoading = false;
  #optionsError = "";
  #attachmentLoading = false;
  #attachmentError = "";
  #internalError = "";
  #loadedWorkspaceId = "";
  #observedSessionId = "";
  #loadGeneration = 0;
  #attachmentGeneration = 0;
  #subscriptionHealth: readonly ProtocolSubscriptionHealth[] = [];
  #optionsRetryTimer: ReturnType<typeof setTimeout> | undefined;
  #optionsTouched = false;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #capabilities = new ContextConsumer(this, {
    context: hostCapabilitiesContext,
    subscribe: true,
  });
  readonly #workspaceScope = new ContextConsumer(this, {
    context: workspaceContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });

  get #effectiveWorkspaceId(): string {
    return this.workspaceId || this.#workspaceScope.value?.workspaceId || "";
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override disconnectedCallback(): void {
    this.#loadGeneration += 1;
    this.#attachmentGeneration += 1;
    this.#loadedWorkspaceId = "";
    this.#optionsLoading = false;
    if (this.#optionsRetryTimer !== undefined) {
      globalThis.clearTimeout(this.#optionsRetryTimer);
      this.#optionsRetryTimer = undefined;
    }
    super.disconnectedCallback();
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    const sessionId = this.#effectiveSessionId;
    if (changed.has("sessionId") || sessionId !== this.#observedSessionId) {
      this.#attachmentGeneration += 1;
      this.#observedSessionId = sessionId;
      this.#draft = createInitialNewThreadDraft(this.#catalog);
      this.#optionsTouched = false;
      this.#attachmentLoading = false;
      this.#attachmentError = "";
      this.#internalError = "";
    }
    if (changed.has("catalogModes") || changed.has("catalogModels")) {
      this.#adoptCatalog({
        modes: this.catalogModes,
        models: this.catalogModels,
        providers: this.#catalog.providers,
      });
    }
    if (changed.has("subscriptionHealth")) {
      this.#subscriptionHealth = this.subscriptionHealth;
    }
  }

  protected override updated(): void {
    const workspaceId = this.#effectiveWorkspaceId;
    if (workspaceId !== "" && this.#loadedWorkspaceId !== workspaceId) {
      void this.#loadOptions();
    }
  }

  override render() {
    const unavailable = this.#services.value === undefined
      || this.#effectiveWorkspaceId.trim() === ""
      || this.#effectiveSessionId.trim() === "";
    const controls = newThreadSetupControls({
      sessionId: this.#effectiveSessionId,
      workspaceId: this.#effectiveWorkspaceId,
      disabled: this.disabled || unavailable,
      busy: this.busy,
      attachmentLoading: this.#attachmentLoading,
    });
    const thinking = newThreadThinkingOption(this.#draft, this.#catalog);
    const describedBy = [
      this.busy ? "new-thread-progress" : "",
      this.disabled || unavailable ? "new-thread-disabled" : "",
      this.#optionsError === "" ? "" : "new-thread-options-error",
      this.#attachmentError === "" && this.#internalError === "" && this.errorMessage === ""
        ? ""
        : "new-thread-error",
    ].filter((id) => id !== "").join(" ");
    const selectedMode = this.#catalog.modes.find(
      (mode) => mode.id === this.#draft.modeId,
    );
    const modelDefault = selectedMode?.default_model
      ?? this.#catalog.providers?.default_model
      ?? "";
    const modelHealth = modelHealthPresentations(
      this.#catalog.models,
      this.#subscriptionHealth,
    );
    const hasInitialMessage = this.#draft.prompt.trim() !== ""
      || this.#draft.attachments.length > 0;

    return html`
      <form
        aria-label="New thread setup (provisional)"
        aria-busy=${this.busy || this.#attachmentLoading}
        aria-describedby=${describedBy === "" ? nothing : describedBy}
        @submit=${this.#submit}
      >
        <header>
          <h2>New thread</h2>
          <p>Threads share the session worktree; pick the agent setup for this one.</p>
        </header>

        ${this.busy
          ? html`<p id="new-thread-progress" class="notice" role="status">Starting thread…</p>`
          : nothing}

        <div class="option-grid">
          <label>
            <span>Agent mode</span>
            <select
              name="mode"
              .value=${this.#draft.modeId}
              ?disabled=${controls.optionControlsDisabled}
              @change=${this.#modeChanged}
            >
              <option value="">Default mode</option>
              ${this.#catalog.modes.map(
                (mode) => html`<option value=${mode.id}>${mode.display_name || mode.id}</option>`,
              )}
            </select>
          </label>
          <div class="field-label">
            <span>Model</span>
            <trouve-model-picker
              accessible-label="Model"
              placement="down"
              placeholder=${`Mode or server default${modelDefault === "" ? "" : ` · ${modelDefault}`}`}
              empty-label=${`Mode or server default${modelDefault === "" ? "" : ` · ${modelDefault}`}`}
              .value=${this.#draft.modelId}
              .models=${this.#catalog.models}
              .health=${modelHealth}
              .disabled=${controls.optionControlsDisabled}
              @trouve-model-picked=${this.#modelPicked}
            ></trouve-model-picker>
          </div>
          <label>
            <span>Thinking level</span>
            <select
              name="thinking"
              .value=${thinking === undefined ? "" : this.#draft.thinking}
              ?disabled=${controls.optionControlsDisabled}
              @change=${this.#thinkingChanged}
            >
              <option value="">Model default</option>
              ${(thinking?.values ?? []).map(
                (value) => html`<option value=${value}>${modelOptionLabel(value)}</option>`,
              )}
            </select>
          </label>
          <label class=${`permission-field ${this.#draft.permissionMode === "yolo" ? "permission-yolo" : ""}`}>
            <span>${this.#draft.permissionMode === "yolo"
              ? fontAwesomeIcon("triangle-exclamation")
              : nothing}Permission mode</span>
            <select
              name="permission_mode"
              class=${this.#draft.permissionMode === "yolo" ? "permission-yolo" : ""}
              .value=${this.#draft.permissionMode}
              ?disabled=${controls.optionControlsDisabled}
              @change=${this.#permissionChanged}
            >
              <option value="">Mode or server default</option>
              <option value="ask">Ask</option>
              <option value="allow_list">Allow list</option>
              <option value="yolo">Yolo</option>
            </select>
          </label>
        </div>

        ${this.#draft.permissionMode === "yolo"
          ? html`
              <div class="yolo-warning" role="note">
                <strong>${fontAwesomeIcon("triangle-exclamation")} Unattended execution (YOLO) is dangerous</strong>
                <span>The agent can run commands and change or delete files without asking for approval.</span>
              </div>
            `
          : nothing}

        <label>
          <span>First message</span>
          <textarea
            name="prompt"
            maxlength="100000"
            rows="1"
            autocomplete="off"
            .value=${this.#draft.prompt}
            ?disabled=${controls.formDisabled}
            placeholder="What should the agent do?  (Shift+Enter for a new line)"
            @input=${this.#promptChanged}
            @paste=${this.#promptPaste}
          ></textarea>
        </label>

        ${this.#draft.attachments.length === 0
          ? nothing
          : html`
              <ul class="attachment-list" aria-label="Initial message attachments">
                ${this.#draft.attachments.map(
                  (attachment, index) => {
                    const preview = pendingAttachmentPreviewUrl(attachment);
                    return html`
                      <li class=${preview === undefined ? "file-attachment" : "image-attachment"}>
                        ${preview === undefined
                          ? html`<span class="attachment-icon">${fontAwesomeIcon("file")}</span>`
                          : html`<trouve-image-preview
                              .source=${preview}
                              .name=${attachment.upload.name}
                            ></trouve-image-preview>`}
                        <div class="attachment-details">
                          <strong title=${attachment.upload.name}>${attachment.upload.name}</strong>
                          <small>${attachment.upload.mime} · ${formatNewThreadAttachmentBytes(attachment.size)}</small>
                        </div>
                        <button
                          type="button"
                          aria-label=${`Remove ${attachment.upload.name}`}
                          ?disabled=${controls.formDisabled}
                          @click=${() => this.#removeAttachment(index)}
                        >${fontAwesomeIcon("xmark")}</button>
                      </li>
                    `;
                  },
                )}
              </ul>
            `}

        ${this.disabled || unavailable
          ? html`<p id="new-thread-disabled" class="notice warning" role="status">${this.#disabledText(unavailable)}</p>`
          : nothing}
        ${this.#optionsError === ""
          ? nothing
          : html`<p id="new-thread-options-error" class="notice warning" role="status">${this.#optionsError}</p>`}
        ${this.#attachmentError === "" && this.#internalError === "" && this.errorMessage === ""
          ? nothing
          : html`<p id="new-thread-error" class="notice error" role="alert">${this.errorMessage || this.#internalError || this.#attachmentError}</p>`}

        <footer>
          <label
            class=${`attachment-picker icon-only ${controls.formDisabled || this.#attachmentLoading ? "disabled" : ""}`}
            title="Attach files to the optional first message"
          >
            ${fontAwesomeIcon("paperclip")}
            <span class="visually-hidden">${this.#attachmentLoading ? "Reading files…" : "Attach files"}</span>
            <input
              type="file"
              multiple
              ?disabled=${controls.formDisabled || this.#attachmentLoading}
              @click=${this.#attachmentPickerClicked}
              @change=${this.#filesSelected}
            />
          </label>
          <button class="primary" type="submit" ?disabled=${!controls.canSubmit || !hasInitialMessage}>${controls.submitLabel}</button>
          <button type="button" ?disabled=${!controls.canCancel} @click=${this.#cancel}>Cancel</button>
          <span class="spacer"></span>
        </footer>
      </form>
    `;
  }

  async #loadOptions(): Promise<void> {
    const services = this.#services.value;
    const workspaceId = this.#effectiveWorkspaceId;
    if (services === undefined || workspaceId === "") return;
    if (this.#optionsRetryTimer !== undefined) {
      globalThis.clearTimeout(this.#optionsRetryTimer);
      this.#optionsRetryTimer = undefined;
    }
    const generation = ++this.#loadGeneration;
    this.#loadedWorkspaceId = workspaceId;
    this.#optionsLoading = true;
    this.#optionsError = "";
    this.#subscriptionHealth = readSignal(services.subscriptionHealth.current);
    this.requestUpdate();

    // Subscription health only decorates model choices. Provider probes may
    // launch vendor helpers and take an unbounded amount of time, so they must
    // never keep the mode and model controls disabled while the required
    // catalog requests have already completed.
    void services.subscriptionHealth.refresh("if-stale").then(
      (subscriptionHealth) => {
        if (
          generation !== this.#loadGeneration
          || workspaceId !== this.#effectiveWorkspaceId
        ) return;
        this.#subscriptionHealth = subscriptionHealth;
        this.requestUpdate();
      },
      () => {
        // Health is optional presentation data; catalog errors are reported
        // independently below.
      },
    );
    try {
      const [modes, models, providers] = await Promise.all([
        services.protocol.modes(workspaceId),
        services.modelCatalog.refresh("if-stale"),
        services.protocol.providers(),
      ]);
      if (generation !== this.#loadGeneration || workspaceId !== this.#effectiveWorkspaceId) return;
      this.#adoptCatalog({ modes, models, providers });
    } catch {
      if (generation !== this.#loadGeneration || workspaceId !== this.#effectiveWorkspaceId) return;
      this.#optionsError =
        "Mode and model choices could not be loaded. Server defaults remain available while trouve retries automatically.";
      this.#scheduleOptionsRetry();
    } finally {
      if (generation === this.#loadGeneration && workspaceId === this.#effectiveWorkspaceId) {
        this.#optionsLoading = false;
        this.requestUpdate();
      }
    }
  }

  #adoptCatalog(catalog: NewThreadSetupCatalog): void {
    const draft = this.#draft;
    this.#catalog = catalog;
    const refreshedInitial = createInitialNewThreadDraft(catalog);
    this.#draft = this.#optionsTouched
      ? {
          ...draft,
          modeId: catalog.modes.some((mode) => mode.id === draft.modeId)
            ? draft.modeId
            : "",
          modelId: catalog.models.some((model) => model.id === draft.modelId)
            ? draft.modelId
            : "",
          thinking: newThreadThinkingOption(draft, catalog)?.values.includes(draft.thinking)
            ? draft.thinking
            : "",
        }
      : {
          ...refreshedInitial,
          permissionMode: draft.permissionMode,
          prompt: draft.prompt,
          attachments: draft.attachments,
        };
  }

  #scheduleOptionsRetry(): void {
    if (!this.isConnected || this.#optionsRetryTimer !== undefined) return;
    this.#optionsRetryTimer = globalThis.setTimeout(() => {
      this.#optionsRetryTimer = undefined;
      if (typeof document !== "undefined" && document.visibilityState === "hidden") {
        this.#scheduleOptionsRetry();
        return;
      }
      this.#loadedWorkspaceId = "";
      void this.#loadOptions();
    }, OPTIONS_RETRY_MS);
  }

  readonly #modeChanged = (event: Event): void => {
    this.#optionsTouched = true;
    this.#draft = selectNewThreadMode(
      this.#draft,
      (event.currentTarget as HTMLSelectElement).value,
      this.#catalog,
    );
    this.#internalError = "";
    this.requestUpdate();
  };

  readonly #modelPicked = (event: CustomEvent<{ readonly modelId: string }>): void => {
    this.#optionsTouched = true;
    this.#draft = selectNewThreadModel(
      this.#draft,
      event.detail.modelId,
      this.#catalog,
    );
    this.#internalError = "";
    this.requestUpdate();
  };

  readonly #thinkingChanged = (event: Event): void => {
    this.#optionsTouched = true;
    this.#draft = {
      ...this.#draft,
      thinking: (event.currentTarget as HTMLSelectElement).value,
    };
    this.requestUpdate();
  };

  readonly #permissionChanged = (event: Event): void => {
    this.#optionsTouched = true;
    const value = (event.currentTarget as HTMLSelectElement).value;
    const permissionMode: NewThreadPermissionSelection =
      value === "ask" || value === "allow_list" || value === "yolo" ? value : "";
    this.#draft = { ...this.#draft, permissionMode };
    this.requestUpdate();
  };

  readonly #promptChanged = (event: InputEvent): void => {
    this.#draft = {
      ...this.#draft,
      prompt: (event.currentTarget as HTMLTextAreaElement).value,
    };
    this.requestUpdate();
  };

  readonly #filesSelected = (event: Event): void => {
    const input = event.currentTarget as HTMLInputElement;
    const files = input.files === null ? [] : [...input.files];
    input.value = "";
    void this.#addAttachments(files);
  };

  readonly #attachmentPickerClicked = (event: MouseEvent): void => {
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (
      services?.nativeHost === undefined ||
      capabilities === undefined ||
      !readSignal(capabilities.current).filePicker
    ) {
      return;
    }
    event.preventDefault();
    void this.#pickNativeAttachments();
  };

  readonly #promptPaste = (event: ClipboardEvent): void => {
    if (event.clipboardData?.types.includes("text/plain") === true) return;
    const files = event.clipboardData?.files;
    if (files !== undefined && files.length > 0) {
      event.preventDefault();
      void this.#addAttachments([...files]);
      return;
    }
    const services = this.#services.value;
    const capabilities = this.#capabilities.value;
    if (
      services?.nativeHost === undefined ||
      capabilities === undefined ||
      !readSignal(capabilities.current).clipboardImage
    ) {
      return;
    }
    event.preventDefault();
    void this.#readNativeClipboardImage();
  };

  async #pickNativeAttachments(): Promise<void> {
    const nativeHost = this.#services.value?.nativeHost;
    if (nativeHost === undefined || this.#attachmentLoading) return;
    const generation = ++this.#attachmentGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#attachmentLoading = true;
    this.#attachmentError = "";
    this.requestUpdate();
    try {
      const attachments = await nativeHost.pickFiles();
      if (
        generation !== this.#attachmentGeneration ||
        sessionId !== this.#effectiveSessionId
      ) return;
      for (const attachment of attachments) {
        if (!this.#stageNativeAttachment(attachment)) break;
      }
    } catch {
      if (
        generation === this.#attachmentGeneration &&
        sessionId === this.#effectiveSessionId
      ) {
        this.#attachmentError = "Files could not be read from the desktop picker.";
      }
    } finally {
      if (generation === this.#attachmentGeneration) {
        this.#attachmentLoading = false;
        this.requestUpdate();
      }
    }
  }

  async #readNativeClipboardImage(): Promise<void> {
    const nativeHost = this.#services.value?.nativeHost;
    if (nativeHost === undefined || this.#attachmentLoading) return;
    const generation = ++this.#attachmentGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#attachmentLoading = true;
    this.#attachmentError = "";
    this.requestUpdate();
    try {
      const attachment = await nativeHost.readClipboardImage();
      if (
        generation !== this.#attachmentGeneration ||
        sessionId !== this.#effectiveSessionId
      ) return;
      if (attachment !== undefined) this.#stageNativeAttachment(attachment);
    } catch {
      if (
        generation === this.#attachmentGeneration &&
        sessionId === this.#effectiveSessionId
      ) {
        this.#attachmentError = "The desktop clipboard image could not be read.";
      }
    } finally {
      if (generation === this.#attachmentGeneration) {
        this.#attachmentLoading = false;
        this.requestUpdate();
      }
    }
  }

  #stageNativeAttachment(attachment: PendingAttachment): boolean {
    const appended = appendNewThreadAttachment(this.#draft.attachments, attachment);
    if (!appended.accepted) {
      this.#attachmentError = newThreadAttachmentLimitMessage(
        appended.limit ?? "total-too-large",
        attachment.upload.name,
      );
      return false;
    }
    this.#draft = { ...this.#draft, attachments: appended.attachments };
    return true;
  }

  async #addAttachments(files: readonly File[]): Promise<void> {
    if (files.length === 0 || this.#attachmentLoading) return;
    const generation = ++this.#attachmentGeneration;
    const sessionId = this.#effectiveSessionId;
    this.#attachmentLoading = true;
    this.#attachmentError = "";
    this.requestUpdate();
    try {
      for (const [index, file] of files.entries()) {
        let attachment: PendingAttachment;
        try {
          attachment = await encodeAttachment(
            file,
            `new-thread-${Date.now()}-${index + 1}.bin`,
          );
          if (
            generation !== this.#attachmentGeneration ||
            sessionId !== this.#effectiveSessionId
          ) return;
        } catch (error) {
          if (
            generation !== this.#attachmentGeneration ||
            sessionId !== this.#effectiveSessionId
          ) return;
          const kind = error instanceof AttachmentEncodingError
            ? error.kind
            : "read-failed";
          const name = file.name || "Attachment";
          this.#attachmentError = kind === "too-large"
            ? newThreadAttachmentLimitMessage("item-too-large", name)
            : kind === "empty"
              ? `${name} is empty.`
              : `${name} could not be read.`;
          continue;
        }
        const appended = appendNewThreadAttachment(this.#draft.attachments, attachment);
        if (!appended.accepted) {
          this.#attachmentError = newThreadAttachmentLimitMessage(
            appended.limit ?? "total-too-large",
            attachment.upload.name,
          );
          break;
        }
        this.#draft = { ...this.#draft, attachments: appended.attachments };
      }
    } finally {
      if (generation === this.#attachmentGeneration) {
        this.#attachmentLoading = false;
        this.requestUpdate();
      }
    }
  }

  #removeAttachment(index: number): void {
    this.#draft = {
      ...this.#draft,
      attachments: this.#draft.attachments.filter(
        (_, candidate) => candidate !== index,
      ),
    };
    this.#attachmentError = "";
    this.requestUpdate();
  }

  readonly #submit = (event: SubmitEvent): void => {
    event.preventDefault();
    const controls = newThreadSetupControls({
      sessionId: this.#effectiveSessionId,
      workspaceId: this.#effectiveWorkspaceId,
      disabled: this.disabled || this.#services.value === undefined,
      busy: this.busy,
      attachmentLoading: this.#attachmentLoading,
    });
    if (
      !controls.canSubmit
      || (this.#draft.prompt.trim() === "" && this.#draft.attachments.length === 0)
    ) return;
    let detail: NewThreadSetupSubmitDetail;
    try {
      detail = createNewThreadSetupSubmission({
        workspaceId: this.#effectiveWorkspaceId,
        sessionId: this.#effectiveSessionId,
        draft: this.#draft,
        catalog: this.#catalog,
      });
    } catch {
      this.#internalError = "The new thread setup is incomplete or invalid.";
      this.requestUpdate();
      return;
    }
    this.#internalError = "";
    this.dispatchEvent(
      new CustomEvent<NewThreadSetupSubmitDetail>(NEW_THREAD_SETUP_SUBMIT_EVENT, {
        detail,
        bubbles: true,
        composed: true,
        cancelable: true,
      }),
    );
  };

  readonly #cancel = (): void => {
    if (this.busy) return;
    this.dispatchEvent(
      new CustomEvent<NewThreadSetupCancelDetail>(NEW_THREAD_SETUP_CANCEL_EVENT, {
        detail: {
          workspaceId: this.#effectiveWorkspaceId,
          sessionId: this.#effectiveSessionId,
        },
        bubbles: true,
        composed: true,
        cancelable: true,
      }),
    );
  };

  #disabledText(unavailable: boolean): string {
    if (this.disabledMessage !== "") return this.disabledMessage;
    if (unavailable) return "Select a connected session before starting a thread.";
    return "New-thread setup is currently unavailable.";
  }
}

customElements.define("trouve-new-thread-setup", TrouveNewThreadSetup);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-new-thread-setup": TrouveNewThreadSetup;
  }

  interface HTMLElementEventMap {
    "trouve-new-thread-submit": NewThreadSetupSubmitEvent;
    "trouve-new-thread-cancel": NewThreadSetupCancelEvent;
  }
}
