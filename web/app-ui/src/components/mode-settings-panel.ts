import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { thinkingOption } from "../app/new-session-model.js";
import { appServicesContext } from "../contexts/app-contexts.js";
import type {
  ProtocolModeInfo,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
  ProtocolUpsertModeRequest,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  modelOptionLabel,
  modelSelectorLabel,
} from "./model-option-controls.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

type PermissionMode = "ask" | "allow_list" | "yolo";

const thinkingOptions = (model: ProtocolModelInfo | undefined): readonly string[] =>
  thinkingOption(model)?.values ?? [];

const splitTools = (value: string): string[] =>
  value.split(/[\n,]/u).map((tool) => tool.trim()).filter(Boolean);

export class TrouveModeSettings extends withSignalTracking(LitElement) {
  static override styles = css`
    :host { display: block; color: var(--trouve-text); font: var(--trouve-font-size, 13px)/1.35 var(--trouve-font-sans, system-ui); }
    h2, h3, p { margin-block: 0; }
    h2 { color: var(--trouve-text-hi); font-size: 16px; } h3 { color: var(--trouve-text-hi); font-size: 13px; }
    h3.section-subtitle { font-size: 14px; }
    .stack { display: grid; gap: 12px; }
    .card, .mode-editor { display: grid; gap: 8px; padding: 12px; border: 0; border-radius: 7px; background: var(--trouve-surface); }
    .row { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
    .grow { flex: 1 1 12rem; min-width: 0; }
    .identity-row > label { flex: 1 1 12rem; }
    .identity-row > .mode-id-field { flex: 0 0 150px; }
    label { display: grid; gap: 4px; color: var(--trouve-muted); }
    label > span { font-size: 0.82rem; font-weight: 600; }
    button, input, select, textarea { box-sizing: border-box; font: inherit; color: var(--trouve-text); border: 1px solid var(--trouve-border); border-radius: 5px; background: var(--trouve-control-bg, var(--trouve-surface)); }
    input, select, textarea { width: 100%; min-height: 30px; padding: 4px 8px; }
    textarea { min-height: 82px; resize: vertical; }
    button { min-height: 30px; padding: 4px 9px; cursor: pointer; }
    button.primary { color: var(--trouve-on-accent, white); border-color: var(--trouve-primary-border, var(--trouve-accent)); background: var(--trouve-primary-bg, var(--trouve-accent)); }
    button.primary:hover:not(:disabled) { background: var(--trouve-primary-hover, var(--trouve-primary-bg)); }
    button.danger { color: var(--trouve-err); }
    button:disabled { cursor: default; opacity: 0.55; }
    button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible { outline: 2px solid var(--trouve-focus, var(--trouve-accent)); outline-offset: 2px; }
    .meta, .status { color: var(--trouve-muted); font-size: 0.83rem; }
    .status { min-height: 1.4em; } .status.error { color: var(--trouve-err); }
    .defaults-form { display: grid; gap: 8px; }
    .global-default-grid { display: grid; grid-template-columns: minmax(0, 2fr) minmax(140px, 1fr) minmax(150px, 1fr); align-items: end; gap: 10px; }
    .modes-copy { color: var(--trouve-muted); font-size: 11px; }
    .no-models { display: flex; align-items: center; gap: 10px; border-radius: 6px; padding: 10px; color: var(--trouve-warn); background: var(--trouve-surface); font-size: 12px; }
    .no-models span { min-width: 0; flex: 1; }
    .mode-list { height: 320px; overflow: auto; border-radius: 7px; background: var(--trouve-surface); }
    .mode-row { box-sizing: border-box; height: 52px; display: grid; grid-template-columns: minmax(0, 1fr) 190px auto; align-items: center; gap: 8px; padding: 0 6px 0 10px; }
    .mode-row-copy { min-width: 0; line-height: 1.2; }
    .mode-row-copy > span { display: flex; align-items: center; gap: 6px; min-width: 0; }
    .mode-row-copy strong, .mode-row-copy small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .mode-row-copy strong { color: var(--trouve-text-hi); font-size: 13px; }
    .mode-row-copy small { color: var(--trouve-muted); font-size: 11px; }
    .mode-row-defaults { display: grid; gap: 3px; }
    .mode-row-defaults select { min-height: 28px; }
    .mode-row-actions { display: flex; gap: 5px; }
    button.icon-button { display: inline-grid; width: 30px; padding: 0; place-items: center; }
    .visually-hidden { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; }
    @media (max-width: 620px) {
      .global-default-grid { grid-template-columns: 1fr; }
      .row > button { flex: 1 1 auto; }
      .mode-list { height: auto; max-height: 420px; }
      .mode-row { height: auto; min-height: 68px; grid-template-columns: minmax(0, 1fr); align-items: stretch; padding: 8px; }
      .mode-row-actions { justify-content: end; }
    }
  `;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  #providers: ProtocolProvidersResponse | undefined;
  #models: readonly ProtocolModelInfo[] = [];
  #modes: readonly ProtocolModeInfo[] = [];
  #busy = false;
  #message = "";
  #error = false;
  #editingModeId = "";
  #defaultModelDraft = "";
  #defaultThinkingDraft = "";

  #availableModels(): readonly ProtocolModelInfo[] {
    const catalog = this.#services.value?.modelCatalog.current;
    if (catalog === undefined) return this.#models;
    const models = readSignal(catalog);
    return models.length === 0 ? this.#models : models;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    queueMicrotask(() => void this.#load());
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const { protocol } = services;
    this.#busy = true;
    this.#message = "Loading modes and models…";
    this.#error = false;
    this.requestUpdate();
    try {
      [this.#providers, this.#models, this.#modes] = await Promise.all([
        protocol.providers(),
        services.modelCatalog.refresh("if-stale"),
        protocol.modeInfos(),
      ]);
      this.#defaultModelDraft = this.#providers.default_model ?? "";
      this.#defaultThinkingDraft = this.#providers.default_thinking_level ?? "";
      this.#message = "";
    } catch {
      this.#message = "Modes and model defaults could not be loaded.";
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #saveDefaultModel(model: string, thinking: string | undefined): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    if (model === "") return;
    this.#busy = true;
    this.#message = "Saving model defaults…";
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.setDefaultModel({
        model,
        ...(thinking === undefined
          ? {}
          : { default_thinking_level: thinking === "" ? null : thinking }),
      });
      await this.#load();
      this.#message = "Model defaults saved for new threads.";
      this.requestUpdate();
    } catch {
      this.#message = "Model defaults could not be saved.";
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #saveDefaultPermission(permission: PermissionMode): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    if (!["ask", "allow_list", "yolo"].includes(permission)) return;
    this.#busy = true;
    this.#message = "Saving permission default…";
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.setDefaultPermissionMode({ permission_mode: permission });
      await this.#load();
      this.#message = "Permission default saved for new threads.";
      this.requestUpdate();
    } catch {
      this.#message = "Permission default could not be saved.";
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #saveMode(event: SubmitEvent, existing?: ProtocolModeInfo): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const id = String(data.get("id") ?? existing?.mode.id ?? "").trim();
    const displayName = String(data.get("display_name") ?? "").trim();
    const systemPrompt = String(data.get("system_prompt") ?? "").trim();
    if (!/^[a-z0-9][a-z0-9_-]*$/u.test(id) || displayName === "") {
      this.#message = "Mode IDs use lowercase letters, digits, underscore, or dash; a display name is required.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    const request: ProtocolUpsertModeRequest = {
      display_name: displayName,
      system_prompt: systemPrompt,
      allowed_tools: splitTools(String(data.get("allowed_tools") ?? "")),
      read_only: data.get("read_only") === "on",
      default_model: existing?.mode.default_model ?? null,
      default_permission_mode: existing?.mode.default_permission_mode ?? null,
      default_thinking_level: existing?.mode.default_thinking_level ?? null,
    };
    this.#busy = true;
    this.#message = `Saving ${id}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.upsertMode(id, request);
      if (existing === undefined) form.reset();
      this.#editingModeId = "";
      await this.#load();
      this.#message = `Saved mode ${id}.`;
      this.requestUpdate();
    } catch {
      this.#message = `Mode ${id} could not be saved.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #resetMode(info: ProtocolModeInfo): Promise<void> {
    const verb = info.origin === "custom" ? "Delete" : "Reset";
    if (!globalThis.confirm(`${verb} mode “${info.mode.display_name}”?`)) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      await protocol.deleteMode(info.mode.id);
      const success = info.origin === "custom"
        ? `Deleted mode ${info.mode.id}.`
        : `Reset mode ${info.mode.id} to its built-in definition.`;
      await this.#load();
      this.#message = success;
      this.requestUpdate();
    } catch {
      this.#message = `Mode ${info.mode.id} could not be ${info.origin === "custom" ? "deleted" : "reset"}.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #modeForm(info?: ProtocolModeInfo) {
    const mode = info?.mode;
    const readOnly = info?.origin === "workspace";
    return html`
      <form class="mode-editor" @submit=${(event: SubmitEvent) => void this.#saveMode(event, info)}>
        <div class="row"><h3 class="grow">${mode === undefined ? "Add mode" : `Edit mode \"${mode.id}\"`}</h3></div>
        <div class="row identity-row">
          ${mode === undefined ? html`<label class="mode-id-field"><span class="visually-hidden">Mode ID</span><input name="id" required placeholder="id (e.g. docs)" /></label>` : html`<input type="hidden" name="id" .value=${mode.id} />`}
          <label><span class="visually-hidden">Display name</span><input name="display_name" required placeholder="display name" .value=${mode?.display_name ?? ""} ?disabled=${readOnly} /></label>
        </div>
        <label><span>System prompt (appended to the base prompt):</span><textarea name="system_prompt" .value=${mode?.system_prompt ?? ""} ?disabled=${readOnly}></textarea></label>
        <label><span class="visually-hidden">Allowed tools</span><input name="allowed_tools" placeholder="allowed tools, comma-separated (empty = all tools)" .value=${(mode?.allowed_tools ?? []).join(", ")} ?disabled=${readOnly} /></label>
        <label class="row"><input style="width:auto" type="checkbox" name="read_only" .checked=${mode?.read_only ?? false} ?disabled=${readOnly} /><span>Read-only (never mutates the worktree)</span></label>
        ${readOnly
          ? html`<p class="meta">Workspace modes are managed by the repository’s .agents configuration.</p>`
          : html`<div class="row"><button class="primary" type="submit" ?disabled=${this.#busy}>${info === undefined ? "Add mode" : "Save mode"}</button><button type="button" @click=${() => { this.#editingModeId = ""; this.requestUpdate(); }}>Cancel</button></div>`}
      </form>
    `;
  }

  async #updateModeDefaults(
    info: ProtocolModeInfo,
    update: { readonly model?: string | null; readonly thinking?: string | null },
  ): Promise<void> {
    if (info.origin === "workspace" || this.#busy) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    const mode = info.mode;
    this.#busy = true;
    this.#message = `Saving ${mode.id}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.upsertMode(mode.id, {
        display_name: mode.display_name,
        system_prompt: mode.system_prompt,
        allowed_tools: [...(mode.allowed_tools ?? [])],
        ...(mode.read_only === undefined ? {} : { read_only: mode.read_only }),
        default_permission_mode: mode.default_permission_mode ?? null,
        default_model: update.model === undefined ? mode.default_model ?? null : update.model,
        default_thinking_level: update.thinking === undefined
          ? mode.default_thinking_level ?? null
          : update.thinking,
      });
      await this.#load();
      this.#message = `Saved mode ${mode.id}.`;
      this.requestUpdate();
    } catch {
      this.#message = `Mode ${mode.id} could not be saved.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #modeRow(info: ProtocolModeInfo) {
    const mode = info.mode;
    const modelId = mode.default_model ?? "";
    const thinkingModel = this.#availableModels().find((model) => model.id === (modelId || this.#providers?.default_model));
    const thinking = thinkingOptions(thinkingModel);
    const readOnly = info.origin === "workspace";
    return html`
      <article class="mode-row">
        <div class="mode-row-copy">
          <span><strong>${mode.display_name}</strong><small>${mode.id}${info.origin === "builtin" ? "" : ` · ${info.origin}`}${mode.read_only ? " · read-only" : ""}</small></span>
        </div>
        <div class="mode-row-defaults">
          <select
            aria-label=${`Default model for ${mode.display_name}`}
            .value=${modelId}
            ?disabled=${readOnly || this.#busy || this.#availableModels().length === 0}
            @change=${(event: Event) => void this.#updateModeDefaults(info, {
              model: (event.currentTarget as HTMLSelectElement).value || null,
              thinking: null,
            })}
          ><option value="">Global default</option>${this.#availableModels().map((model) => html`<option value=${model.id}>${modelSelectorLabel(model)}</option>`)}</select>
          ${thinking.length === 0
            ? nothing
            : html`<select
                aria-label=${`Default thinking level for ${mode.display_name}`}
                .value=${mode.default_thinking_level ?? ""}
                ?disabled=${readOnly || this.#busy}
                @change=${(event: Event) => void this.#updateModeDefaults(info, {
                  thinking: (event.currentTarget as HTMLSelectElement).value || null,
                })}
              ><option value="">Model default</option>${thinking.map((value) => html`<option value=${value}>${modelOptionLabel(value)}</option>`)}</select>`}
        </div>
        <div class="mode-row-actions">
          ${readOnly ? nothing : html`<button class="icon-button" type="button" title=${`Edit ${mode.display_name}`} aria-label=${`Edit ${mode.display_name}`} ?disabled=${this.#busy} @click=${() => { this.#editingModeId = mode.id; this.requestUpdate(); }}>${fontAwesomeIcon("pen")}</button>`}
          ${info.origin === "customized" || info.origin === "custom"
            ? html`<button class="danger" type="button" ?disabled=${this.#busy} @click=${() => void this.#resetMode(info)}>${info.origin === "custom" ? "Remove" : "Reset"}</button>`
            : nothing}
        </div>
      </article>
    `;
  }

  override render() {
    const models = this.#availableModels();
    const selected = models.find((model) => model.id === (this.#defaultModelDraft || this.#providers?.default_model));
    const thinking = thinkingOptions(selected);
    return html`
      <div class="stack">
        <h2>Modes &amp; Models</h2>
        ${models.length === 0 && !this.#busy
          ? html`<div class="no-models"><span>No models available — configure a provider to enable the model selectors.</span><button class="primary" type="button" @click=${() => this.#services.value?.router.navigate({ kind: "settings", section: "providers" })}>Configure providers</button></div>`
          : nothing}
        <section class="defaults-form" aria-label="Global defaults">
          <p class="meta">Used by new threads whose mode has no default of its own. Changes save automatically.</p>
          <div class="global-default-grid">
            <label><span>Global Default Model</span><select required .value=${this.#defaultModelDraft || this.#providers?.default_model || ""} ?disabled=${this.#busy || models.length === 0} @change=${(event: Event) => {
              const model = (event.currentTarget as HTMLSelectElement).value;
              const options = thinkingOptions(this.#availableModels().find((candidate) => candidate.id === model));
              const thinkingDraft = options.includes(this.#defaultThinkingDraft) ? this.#defaultThinkingDraft : "";
              this.#defaultModelDraft = model;
              this.#defaultThinkingDraft = thinkingDraft;
              this.requestUpdate();
              void this.#saveDefaultModel(model, options.length === 0 ? undefined : thinkingDraft);
            }}><option value="" disabled>Choose model</option>${models.map((model) => html`<option value=${model.id}>${modelSelectorLabel(model)}${model.supports_tools ? "" : " · no tools"}</option>`)}</select></label>
            <label><span>Global Default Thinking</span>${thinking.length === 0
              ? html`<select disabled><option>Not available</option></select>`
              : html`<select .value=${this.#defaultThinkingDraft} ?disabled=${this.#busy} @change=${(event: Event) => {
                const thinkingDraft = (event.currentTarget as HTMLSelectElement).value;
                this.#defaultThinkingDraft = thinkingDraft;
                void this.#saveDefaultModel(this.#defaultModelDraft || this.#providers?.default_model || "", thinkingDraft);
              }}><option value="" .selected=${this.#defaultThinkingDraft === ""}>Model default</option>${thinking.map((value) => html`<option value=${value} .selected=${this.#defaultThinkingDraft === value}>${modelOptionLabel(value)}</option>`)}</select>`}</label>
            <label><span>Global Default Permissions</span><select .value=${this.#providers?.default_permission_mode ?? "ask"} ?disabled=${this.#busy} @change=${(event: Event) => void this.#saveDefaultPermission((event.currentTarget as HTMLSelectElement).value as PermissionMode)}><option value="ask">Ask</option><option value="allow_list">Allow list</option><option value="yolo">Yolo</option></select></label>
          </div>
        </section>
        <h3 class="section-subtitle">Modes</h3>
        <p class="modes-copy">A mode combines a prompt, tool policy, permissions, model, and thinking defaults. Editing a built-in saves an override in ~/.config/trouve/modes/; Reset removes it. Workspace modes (.agents/modes/) are file-managed and read-only here.</p>
        <section class="mode-list" aria-label="Modes">${this.#modes.map((info) => this.#modeRow(info))}</section>
        ${this.#editingModeId === ""
          ? html`<div class="row"><button type="button" @click=${() => { this.#editingModeId = "__new__"; this.requestUpdate(); }}>${fontAwesomeIcon("plus")} Add mode</button></div>`
          : this.#editingModeId === "__new__"
            ? this.#modeForm()
            : this.#modeForm(this.#modes.find((info) => info.mode.id === this.#editingModeId))}
        ${this.#message === "" ? nothing : html`<p class="status ${this.#error ? "error" : ""}" role="status" aria-live="polite">${this.#message}</p>`}
      </div>
    `;
  }
}

customElements.define("trouve-mode-settings", TrouveModeSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-mode-settings": TrouveModeSettings;
  }
}
