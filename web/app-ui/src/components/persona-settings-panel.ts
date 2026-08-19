import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import { appServicesContext } from "../contexts/app-contexts.js";
import type {
  ProtocolPersonaInfo,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
  ProtocolUpsertPersonaRequest,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  modelOptionLabel,
  modelSelectorLabel,
} from "./model-option-controls.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

type PermissionMode = "ask" | "allow_list" | "yolo";

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null ? value as Record<string, unknown> : undefined;

const thinkingOptions = (model: ProtocolModelInfo | undefined): readonly string[] => {
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  for (const key of ["thinking_level", "effort"]) {
    const property = asRecord(properties?.[key]);
    const values = property?.["enum"];
    if (Array.isArray(values) && values.every((value) => typeof value === "string")) {
      return values as string[];
    }
  }
  return [];
};

const splitTools = (value: string): string[] =>
  value.split(/[\n,]/u).map((tool) => tool.trim()).filter(Boolean);

export class TrouvePersonaSettings extends withSignalTracking(LitElement) {
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
    .grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
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
    .defaults-form > .row > label:first-child { flex: 1 1 100%; }
    .permission-default { width: 150px; }
    .modes-copy { color: var(--trouve-muted); font-size: 11px; }
    .no-models { display: flex; align-items: center; gap: 10px; border-radius: 6px; padding: 10px; color: var(--trouve-warn); background: var(--trouve-surface); font-size: 12px; }
    .no-models span { min-width: 0; flex: 1; }
    .mode-list { height: 320px; overflow: auto; border-radius: 7px; background: var(--trouve-surface); }
    .mode-row { box-sizing: border-box; height: 52px; display: grid; grid-template-columns: minmax(0, 1fr) 190px auto; align-items: center; gap: 8px; padding: 0 6px 0 10px; }
    .mode-row-copy { min-width: 0; line-height: 1.2; }
    .mode-row-copy > span { display: flex; align-items: center; gap: 6px; min-width: 0; }
    .mode-row-copy strong, .mode-row-copy small, .mode-row-copy p { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .mode-row-copy strong { color: var(--trouve-text-hi); font-size: 13px; }
    .mode-row-copy small, .mode-row-copy p { color: var(--trouve-muted); font-size: 11px; }
    .mode-row-copy p { margin-top: 2px; }
    .mode-row-defaults { display: grid; gap: 3px; }
    .mode-row-defaults select { min-height: 28px; }
    .mode-row-actions { display: flex; gap: 5px; }
    .mode-default-grid { grid-template-columns: 150px minmax(0, 1fr) 190px; align-items: end; }
    .visually-hidden { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; }
    @media (max-width: 620px) {
      .grid, .mode-default-grid { grid-template-columns: 1fr; }
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
  #modes: readonly ProtocolPersonaInfo[] = [];
  #busy = false;
  #message = "";
  #error = false;
  #editingModeId = "";
  #defaultModelDraft = "";
  #defaultThinkingDraft = "";
  #modeFormModelId: string | undefined;
  #modeFormThinkingDraft: string | undefined;

  async #restorePersonaFocus(id: string): Promise<void> {
    await this.updateComplete;
    const button = [...this.renderRoot.querySelectorAll<HTMLButtonElement>("button")]
      .find((candidate) => candidate.dataset["personaFocus"] === id);
    (button ?? this.renderRoot.querySelector<HTMLElement>(".stack"))?.focus();
  }

  async #reloadAfterMutation(success: string): Promise<boolean> {
    if (await this.#load()) return true;
    this.#message = `${success} The updated persona list could not be refreshed.`;
    this.#error = true;
    this.requestUpdate();
    return false;
  }

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

  async #load(): Promise<boolean> {
    const services = this.#services.value;
    if (services === undefined) return false;
    const { protocol } = services;
    this.#busy = true;
    this.#message = "Loading personas and models…";
    this.#error = false;
    this.requestUpdate();
    try {
      [this.#providers, this.#models, this.#modes] = await Promise.all([
        protocol.providers(),
        services.modelCatalog.refresh("if-stale"),
        protocol.personaInfos(),
      ]);
      this.#defaultModelDraft = this.#providers.default_model ?? "";
      this.#defaultThinkingDraft = this.#providers.default_thinking_level ?? "";
      this.#message = "";
      return true;
    } catch {
      this.#message = "Personas and model defaults could not be loaded.";
      this.#error = true;
      return false;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #saveDefaults(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const data = new FormData(event.currentTarget as HTMLFormElement);
    const model = String(data.get("model") ?? "");
    const permission = String(data.get("permission_mode") ?? "ask") as PermissionMode;
    const thinking = String(data.get("thinking") ?? "");
    if (!["ask", "allow_list", "yolo"].includes(permission)) return;
    this.#busy = true;
    this.#message = "Saving defaults…";
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.setGlobalDefaults({
        model,
        default_thinking_level: thinking || null,
        permission_mode: permission,
      });
      const success = "Defaults saved for new threads.";
      if (!await this.#reloadAfterMutation(success)) return;
      this.#message = success;
      this.requestUpdate();
    } catch {
      await this.#load();
      this.#message = "Defaults could not be saved.";
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #saveMode(event: SubmitEvent, existing?: ProtocolPersonaInfo): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const id = String(data.get("id") ?? existing?.persona.id ?? "").trim();
    const displayName = String(data.get("display_name") ?? "").trim();
    const systemPrompt = String(data.get("system_prompt") ?? "").trim();
    const group = String(data.get("group") ?? "general") as "general" | "reviewer";
    const permission = String(data.get("default_permission_mode") ?? "");
    if ((existing === undefined && !/^[a-z0-9][a-z0-9_-]*$/u.test(id)) || displayName === "") {
      this.#message = "Persona IDs use lowercase letters, digits, underscore, or dash; a display name is required.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    if (
      existing === undefined
      && this.#modes.some(({ persona }) => persona.id === id)
    ) {
      this.#message = `A persona with the ID ${id} already exists.`;
      this.#error = true;
      this.requestUpdate();
      return;
    }
    const modelValue = data.get("default_model");
    const thinkingValue = data.get("default_thinking_level");
    const request: ProtocolUpsertPersonaRequest = {
      display_name: displayName,
      group,
      system_prompt: systemPrompt,
      allowed_tools: splitTools(String(data.get("allowed_tools") ?? "")),
      read_only: data.get("read_only") === "on",
      default_model: modelValue === null
        ? existing?.persona.default_model ?? null
        : String(modelValue) || null,
      default_permission_mode: permission === "" ? null : permission as PermissionMode,
      default_thinking_level: thinkingValue === null
        ? existing?.persona.default_thinking_level ?? null
        : String(thinkingValue) || null,
    };
    this.#busy = true;
    this.#message = `Saving ${id}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.upsertPersona(id, request);
      if (existing === undefined) form.reset();
      this.#editingModeId = "";
      this.#modeFormModelId = undefined;
      this.#modeFormThinkingDraft = undefined;
      const success = `Saved persona ${id}.`;
      if (!await this.#reloadAfterMutation(success)) {
        void this.#restorePersonaFocus(existing === undefined ? "__add__" : id);
        return;
      }
      this.#message = success;
      this.requestUpdate();
      void this.#restorePersonaFocus(existing === undefined ? "__add__" : id);
    } catch (error) {
      this.#message = error instanceof Error && error.message !== ""
        ? error.message
        : `Persona ${id} could not be saved.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #resetMode(info: ProtocolPersonaInfo): Promise<void> {
    const verb = info.origin === "custom" ? "Delete" : "Reset";
    if (!globalThis.confirm(`${verb} persona “${info.persona.display_name}”?`)) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      await protocol.deletePersona(info.persona.id);
      const success = info.origin === "custom"
        ? `Deleted persona ${info.persona.id}.`
        : `Reset persona ${info.persona.id} to its built-in definition.`;
      const focusId = info.origin === "custom" ? "__add__" : info.persona.id;
      if (info.origin === "custom") {
        this.#modes = this.#modes.filter((candidate) => candidate.persona.id !== info.persona.id);
      }
      if (!await this.#reloadAfterMutation(success)) {
        void this.#restorePersonaFocus(focusId);
        return;
      }
      this.#message = success;
      this.requestUpdate();
      void this.#restorePersonaFocus(focusId);
    } catch {
      this.#message = `Persona ${info.persona.id} could not be ${info.origin === "custom" ? "deleted" : "reset"}.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #modeForm(info?: ProtocolPersonaInfo) {
    const mode = info?.persona;
    const readOnly = info?.origin === "workspace";
    const configuredModelId = this.#modeFormModelId ?? mode?.default_model ?? "";
    const effectiveModelId = configuredModelId || this.#providers?.default_model || "";
    const editorThinking = thinkingOptions(
      this.#availableModels().find((candidate) => candidate.id === effectiveModelId),
    );
    return html`
      <form class="mode-editor" @submit=${(event: SubmitEvent) => void this.#saveMode(event, info)}>
        <div class="row"><h3 class="grow">${mode === undefined ? "Add persona" : `Edit persona \"${mode.id}\"`}</h3></div>
        <div class="row identity-row">
          ${mode === undefined ? html`<label class="mode-id-field"><span class="visually-hidden">Persona ID</span><input name="id" required placeholder="id (e.g. docs)" /></label>` : html`<input type="hidden" name="id" .value=${mode.id} />`}
          <label><span class="visually-hidden">Display name</span><input name="display_name" required placeholder="display name" .value=${mode?.display_name ?? ""} ?disabled=${readOnly} /></label>
        </div>
        <label><span>Persona group</span><select name="group" .value=${mode?.group ?? "general"} ?disabled=${readOnly}><option value="general">General persona</option><option value="reviewer">Reviewer persona</option></select></label>
        <label><span>System prompt (appended to the base prompt):</span><textarea name="system_prompt" .value=${mode?.system_prompt ?? ""} ?disabled=${readOnly}></textarea></label>
        <label><span class="visually-hidden">Allowed tools</span><input name="allowed_tools" placeholder="allowed tools, comma-separated (empty = all tools)" .value=${(mode?.allowed_tools ?? []).join(", ")} ?disabled=${readOnly} /></label>
        <label class="row"><input style="width:auto" type="checkbox" name="read_only" .checked=${mode?.read_only ?? false} ?disabled=${readOnly} /><span>Read-only (never mutates the worktree)</span></label>
        <div class="grid mode-default-grid">
          <label><span>Default permissions</span><select name="default_permission_mode" .value=${mode?.default_permission_mode ?? ""} ?disabled=${readOnly}><option value="">Global default</option><option value="ask">Ask</option><option value="allow_list">Allow list</option><option value="yolo">Yolo</option></select></label>
          <label><span>Default model</span><select name="default_model" .value=${configuredModelId} ?disabled=${readOnly || this.#availableModels().length === 0} @change=${(event: Event) => {
            const configuredModelId = (event.currentTarget as HTMLSelectElement).value;
            this.#modeFormModelId = configuredModelId;
            const effectiveModelId = configuredModelId || this.#providers?.default_model || "";
            const options = thinkingOptions(this.#availableModels().find((candidate) => candidate.id === effectiveModelId));
            const current = this.#modeFormThinkingDraft ?? mode?.default_thinking_level ?? "";
            this.#modeFormThinkingDraft = options.includes(current) ? current : "";
            this.requestUpdate();
          }}><option value="">Global default</option>${this.#availableModels().map((candidate) => html`<option value=${candidate.id}>${modelSelectorLabel(candidate)}</option>`)}</select></label>
          ${editorThinking.length === 0
            ? html`<input type="hidden" name="default_thinking_level" .value=${this.#modeFormThinkingDraft ?? mode?.default_thinking_level ?? ""} />`
            : html`<label><span>Default thinking level</span><select name="default_thinking_level" .value=${this.#modeFormThinkingDraft ?? mode?.default_thinking_level ?? ""} ?disabled=${readOnly} @change=${(event: Event) => { this.#modeFormThinkingDraft = (event.currentTarget as HTMLSelectElement).value; }}><option value="">Global default</option>${editorThinking.map((value) => html`<option value=${value}>${modelOptionLabel(value)}</option>`)}</select></label>`}
        </div>
        ${readOnly
          ? html`<p class="meta">Workspace personas are managed by the repository’s .agents configuration.</p>`
          : html`<div class="row"><button class="primary" type="submit" ?disabled=${this.#busy}>${info === undefined ? "Add persona" : "Save persona"}</button><button type="button" @click=${() => { const focusId = mode?.id ?? "__add__"; this.#editingModeId = ""; this.#modeFormModelId = undefined; this.#modeFormThinkingDraft = undefined; this.requestUpdate(); void this.#restorePersonaFocus(focusId); }}>Cancel</button></div>`}
      </form>
    `;
  }

  async #updateModeDefaults(
    info: ProtocolPersonaInfo,
    update: { readonly model?: string | null; readonly thinking?: string | null },
  ): Promise<void> {
    if (info.origin === "workspace" || this.#busy) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    const mode = info.persona;
    this.#busy = true;
    this.#message = `Saving ${mode.id}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.upsertPersona(mode.id, {
        display_name: mode.display_name,
        group: mode.group ?? "general",
        system_prompt: mode.system_prompt,
        allowed_tools: [...(mode.allowed_tools ?? [])],
        ...(mode.read_only === undefined ? {} : { read_only: mode.read_only }),
        default_permission_mode: mode.default_permission_mode ?? null,
        default_model: update.model === undefined ? mode.default_model ?? null : update.model,
        default_thinking_level: update.thinking === undefined
          ? mode.default_thinking_level ?? null
          : update.thinking,
      });
      const success = `Saved persona ${mode.id}.`;
      if (!await this.#reloadAfterMutation(success)) return;
      this.#message = success;
      this.requestUpdate();
    } catch {
      this.#message = `Persona ${mode.id} could not be saved.`;
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #modeRow(info: ProtocolPersonaInfo) {
    const mode = info.persona;
    const modelId = mode.default_model ?? "";
    const thinkingModel = this.#availableModels().find((model) => model.id === (modelId || this.#providers?.default_model));
    const thinking = thinkingOptions(thinkingModel);
    const readOnly = info.origin === "workspace";
    return html`
      <article class="mode-row">
        <div class="mode-row-copy">
          <span><strong>${mode.display_name}</strong><small>${mode.id}${info.origin === "builtin" ? "" : ` · ${info.origin}`}${mode.read_only ? " · read-only" : ""}</small></span>
          <p title=${mode.system_prompt}>${mode.system_prompt}</p>
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
          ${readOnly ? nothing : html`<button type="button" data-persona-focus=${mode.id} aria-label=${`Edit ${mode.display_name}`} @click=${() => { this.#editingModeId = mode.id; this.#modeFormModelId = mode.default_model ?? ""; this.#modeFormThinkingDraft = mode.default_thinking_level ?? ""; this.requestUpdate(); }}>Edit</button>`}
          ${info.origin === "customized" || info.origin === "custom"
            ? html`<button class="danger" type="button" aria-label=${`${info.origin === "custom" ? "Remove" : "Reset"} ${mode.display_name}`} ?disabled=${this.#busy} @click=${() => void this.#resetMode(info)}>${info.origin === "custom" ? "Remove" : "Reset"}</button>`
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
      <div class="stack" tabindex="-1">
        <h2>Personas &amp; Models</h2>
        ${models.length === 0 && !this.#busy
          ? html`<div class="no-models"><span>No models available — configure a provider to enable the model selectors.</span><button class="primary" type="button" @click=${() => this.#services.value?.router.navigate({ kind: "settings", section: "providers" })}>Configure providers</button></div>`
          : nothing}
        <form class="defaults-form" @submit=${(event: SubmitEvent) => void this.#saveDefaults(event)}>
          <p class="meta">Global default model — used by new threads whose persona has no default of its own.</p>
          <div class="row">
            <label><span class="visually-hidden">Default model</span><select required name="model" .value=${this.#defaultModelDraft || this.#providers?.default_model || ""} ?disabled=${this.#busy || models.length === 0} @change=${(event: Event) => {
              this.#defaultModelDraft = (event.currentTarget as HTMLSelectElement).value;
              const options = thinkingOptions(this.#availableModels().find((model) => model.id === this.#defaultModelDraft));
              if (!options.includes(this.#defaultThinkingDraft)) this.#defaultThinkingDraft = "";
              this.requestUpdate();
            }}><option value="" disabled>Choose model</option>${models.map((model) => html`<option value=${model.id}>${modelSelectorLabel(model)}${model.supports_tools ? "" : " · no tools"}</option>`)}</select></label>
          </div>
          ${thinking.length === 0
            ? html`<input name="thinking" type="hidden" .value=${this.#defaultThinkingDraft ?? ""} />`
            : html`<label><span>Global default thinking level</span><select name="thinking" .value=${this.#defaultThinkingDraft} ?disabled=${this.#busy} @change=${(event: Event) => { this.#defaultThinkingDraft = (event.currentTarget as HTMLSelectElement).value; }}><option value="">Model default</option>${thinking.map((value) => html`<option value=${value}>${modelOptionLabel(value)}</option>`)}</select></label>`}
          <p class="meta">Global default permissions — used by new threads whose persona has no default of its own.</p>
          <label class="permission-default"><span class="visually-hidden">Default permission</span><select name="permission_mode" .value=${this.#providers?.default_permission_mode ?? "ask"} ?disabled=${this.#busy}><option value="ask">Ask</option><option value="allow_list">Allow list</option><option value="yolo">Yolo</option></select></label>
          <div class="row"><button type="submit" ?disabled=${this.#busy || models.length === 0}>Set defaults</button></div>
        </form>
        <h3 class="section-subtitle">General personas</h3>
        <p class="modes-copy">General personas are available to sessions and threads.</p>
        <section class="mode-list" aria-label="General personas">${this.#modes.filter((info) => (info.persona.group ?? "general") === "general").map((info) => this.#modeRow(info))}</section>
        <h3 class="section-subtitle">Reviewer personas</h3>
        <p class="modes-copy">Reviewer personas are available to sessions and threads and may also be selected by code review.</p>
        <section class="mode-list" aria-label="Reviewer personas">${this.#modes.filter((info) => info.persona.group === "reviewer").map((info) => this.#modeRow(info))}</section>
        ${this.#editingModeId === ""
          ? html`<div class="row"><button type="button" data-persona-focus="__add__" aria-label="Add persona" @click=${() => { this.#editingModeId = "__new__"; this.#modeFormModelId = ""; this.#modeFormThinkingDraft = ""; this.requestUpdate(); }}>${fontAwesomeIcon("plus")} Add persona</button></div>`
          : this.#editingModeId === "__new__"
            ? this.#modeForm()
            : this.#modeForm(this.#modes.find((info) => info.persona.id === this.#editingModeId))}
        ${this.#message === "" ? nothing : html`<p class="status ${this.#error ? "error" : ""}" role="status" aria-live="polite">${this.#message}</p>`}
      </div>
    `;
  }
}

customElements.define("trouve-persona-settings", TrouvePersonaSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-persona-settings": TrouvePersonaSettings;
  }
}
