import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing } from "lit";

import {
  appServicesContext,
  appStoreContext,
} from "../contexts/app-contexts.js";
import type {
  ProtocolGithubIntegration,
  ProtocolGitWorktreeSettings,
  ProtocolMcpLogs,
  ProtocolMcpServerInfo,
  ProtocolUpsertMcpServerRequest,
} from "../services/protocol-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  MCP_CONFIG_CHANGED_EVENT,
  parseMcpCommandLine,
  parseMcpConfigJson,
  sessionMcpCommandLine,
  sessionMcpEnvironmentLines,
} from "./session-mcp-model.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

const MCP_REFRESH_MS = 30_000;
const MCP_LOG_REFRESH_MS = 2_000;

const panelStyles = css`
  :host {
    display: block;
    color: var(--trouve-text);
    font: var(--trouve-font-size, 13px) / 1.35 var(--trouve-font-sans, system-ui);
  }
  h2, h3, p { margin-block: 0; }
  h2 { color: var(--trouve-text-hi); font-size: 16px; }
  h3 { color: var(--trouve-text-hi); font-size: 13px; }
  button, input, select, textarea { font: inherit; }
  button, input, select, textarea {
    border: 1px solid var(--trouve-border);
    border-radius: 5px;
    color: var(--trouve-text);
    background: var(--trouve-control-bg, var(--trouve-surface));
  }
  button { min-height: 30px; padding: 4px 9px; cursor: pointer; }
  button.primary { color: var(--trouve-on-accent, white); background: var(--trouve-accent); border-color: var(--trouve-accent); }
  button.danger { color: var(--trouve-err); }
  button:disabled { cursor: default; opacity: 0.55; }
  button:focus-visible, input:focus-visible, select:focus-visible, textarea:focus-visible {
    outline: 2px solid var(--trouve-focus, var(--trouve-accent));
    outline-offset: 2px;
  }
  input, select, textarea { box-sizing: border-box; width: 100%; min-height: 30px; padding: 4px 8px; }
  textarea { min-height: 72px; resize: vertical; }
  label { display: grid; gap: 4px; color: var(--trouve-muted); }
  label > span { font-size: 0.82rem; font-weight: 600; }
  .stack { display: grid; gap: 12px; }
  .card {
    display: grid;
    gap: 10px;
    padding: 12px;
    border: 0;
    border-radius: 7px;
    background: var(--trouve-surface);
  }
  .row { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
  .row > .grow { flex: 1 1 12rem; min-width: 0; }
  .grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
  .mcp-command-grid { grid-template-columns: 160px minmax(0, 1fr); }
  .meta { color: var(--trouve-muted); font-size: var(--trouve-settings-info-font-size, 11px); overflow-wrap: anywhere; }
  .status { min-height: 1.4em; color: var(--trouve-muted); }
  .status.error, .health-error { color: var(--trouve-err); }
  .health-ok { color: var(--trouve-ok); }
  .health-untrusted { color: var(--trouve-warn); }
  .badge { border-radius: 999px; padding: 2px 7px; background: var(--trouve-control-bg); font-size: 0.78rem; }
  .naming-card { padding: 14px; border: 0; gap: 10px; }
  .naming-card hr { width: 100%; height: 1px; margin: 0; border: 0; background: var(--trouve-border); }
  .check-row {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    color: var(--trouve-text);
    cursor: pointer;
  }
  .check-row input[type="checkbox"] {
    width: 16px;
    min-height: 16px;
    margin: 0;
    padding: 0;
    accent-color: var(--trouve-accent);
  }
  .check-row > span { font-size: 12px; }
  code { color: var(--trouve-text-mid); font: 0.95em var(--trouve-font-mono, monospace); }
  .visually-hidden { position: absolute; width: 1px; height: 1px; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; }
  .visually-hidden-focusable { position: absolute; width: 1px; height: 1px; min-height: 0; overflow: hidden; padding: 0; clip: rect(0, 0, 0, 0); }
  .visually-hidden-focusable:focus-visible { position: static; width: auto; height: 30px; min-height: 30px; overflow: visible; padding: 4px 9px; clip: auto; }
  .additive-action { position: absolute; width: 1px; height: 1px; min-height: 0; overflow: hidden; padding: 0; clip: rect(0, 0, 0, 0); }
  .additive-action:focus-visible { position: static; width: auto; height: 30px; min-height: 30px; overflow: visible; padding: 4px 9px; clip: auto; }
  .mcp-list { height: 180px; overflow: auto; border-radius: 7px; background: var(--trouve-surface); }
  .mcp-empty { height: 100%; display: grid; place-items: center; color: var(--trouve-muted); }
  .mcp-row { min-height: 40px; display: grid; grid-template-columns: 12px minmax(0, 1fr) auto auto; align-items: center; gap: 8px; padding: 4px 6px 4px 10px; }
  .mcp-health { color: var(--trouve-muted); font-size: 12px; }
  .mcp-health.ok { color: var(--trouve-ok); }
  .mcp-health.error { color: var(--trouve-err); }
  .mcp-health.disabled { color: var(--trouve-muted); }
  .mcp-copy { min-width: 0; }
  .mcp-copy > span { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .mcp-copy strong, .mcp-copy small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .mcp-copy strong { color: var(--trouve-text-hi); font-size: 13px; }
  .mcp-copy small { display: block; color: var(--trouve-muted); font-size: 11px; }
  .mcp-scope { color: var(--trouve-accent); font-size: 10px; }
  .mcp-toggle { position: relative; display: inline-flex; align-items: center; cursor: pointer; }
  .mcp-toggle input { position: absolute; width: 1px; min-height: 1px; margin: -1px; padding: 0; opacity: 0; }
  .mcp-toggle input:focus-visible { outline: none; }
  .mcp-toggle-track { position: relative; display: block; width: 30px; height: 17px; border: 1px solid var(--trouve-border-strong, var(--trouve-border)); border-radius: 999px; background: var(--trouve-control-bg, var(--trouve-surface)); transition: background-color 120ms ease, border-color 120ms ease; }
  .mcp-toggle-track::after { content: ""; position: absolute; top: 2px; left: 2px; width: 11px; height: 11px; border-radius: 50%; background: var(--trouve-muted); transition: transform 120ms ease, background-color 120ms ease; }
  .mcp-toggle input:checked + .mcp-toggle-track { border-color: var(--trouve-accent); background: color-mix(in srgb, var(--trouve-accent) 35%, var(--trouve-control-bg, var(--trouve-surface))); }
  .mcp-toggle input:checked + .mcp-toggle-track::after { transform: translateX(13px); background: var(--trouve-accent); }
  .mcp-toggle input:focus-visible + .mcp-toggle-track { outline: 2px solid var(--trouve-focus, var(--trouve-accent)); outline-offset: 2px; }
  .mcp-toggle input:disabled + .mcp-toggle-track { cursor: default; opacity: .55; }
  .mcp-actions { display: flex; gap: 5px; }
  .mcp-form { border: 0; padding: 14px; }
  .mcp-import textarea { min-height: 150px; font-family: var(--trouve-font-mono, monospace); }
  .mcp-import input[type="file"] { padding: 3px; }
  .integration-host, .integration-add { border: 0; padding: 14px; }
  .integration-add-fields {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr)) auto;
    align-items: end;
    gap: 8px;
  }
  .integration-add-fields > button { align-self: end; }
  .integration-status { color: var(--trouve-muted); font-size: 12px; }
  .integration-status.connected { color: var(--trouve-ok); }
  input[type="hidden"] { display: none; }
  progress { width: 100%; accent-color: var(--trouve-accent); }
  pre {
    max-height: 18rem;
    margin: 0;
    overflow: auto;
    padding: 10px;
    color: var(--trouve-code-fg, var(--trouve-text));
    background: var(--trouve-code-bg, var(--trouve-control-bg));
    border-radius: 5px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  @media (max-width: 620px) {
    .grid { grid-template-columns: 1fr; }
    .integration-add-fields { grid-template-columns: 1fr; }
    .integration-add-fields > button { width: 100%; }
    .row > button { flex: 1 1 auto; }
  }
`;

const genericFailure = (action: string): string =>
  `${action} failed. Check server connectivity and configuration, then retry.`;

type TitleModelLoadBehavior =
  ProtocolGitWorktreeSettings["title_model_load_behavior"];
type TitleModelResourcePolicy = NonNullable<
  ProtocolGitWorktreeSettings["title_model_resource_policy"]
>;

interface TitleModelOption<T extends string> {
  readonly value: T;
  readonly label: string;
  readonly description: string;
}

const TITLE_MODEL_LOAD_OPTIONS = [
  {
    value: "auto",
    label: "Adaptive (Recommended)",
    description: "Keeps the naming model ready when this computer has comfortable memory headroom; otherwise loads it only when needed.",
  },
  {
    value: "always",
    label: "Keep Ready",
    description: "Loads the naming model at startup and keeps it in memory for the fastest new-session creation.",
  },
  {
    value: "on_demand",
    label: "Load When Needed",
    description: "Loads the naming model when a session is created, then releases it after a short idle period.",
  },
  {
    value: "off",
    label: "Rules Only",
    description: "Uses fast built-in heuristics and never loads the optional naming model.",
  },
] as const satisfies readonly TitleModelOption<TitleModelLoadBehavior>[];

const TITLE_MODEL_RESOURCE_OPTIONS = [
  {
    value: "adaptive",
    label: "Adaptive (Recommended)",
    description: "Uses GPU, CPU, and RAM when no local coding model is active; otherwise uses CPU and RAM only.",
  },
  {
    value: "gpu_cpu_ram",
    label: "GPU, CPU, & RAM",
    description: "Lets llama.cpp use available GPU memory and spill remaining work to CPU and system RAM.",
  },
  {
    value: "gpu_only",
    label: "GPU Only",
    description: "Requires every model layer to fit on a detected GPU; naming falls back to rules when it cannot.",
  },
  {
    value: "cpu_ram_only",
    label: "CPU & RAM Only",
    description: "Keeps session naming entirely off the GPU and uses CPU plus system RAM.",
  },
] as const satisfies readonly TitleModelOption<TitleModelResourcePolicy>[];

const isTitleModelLoadBehavior = (value: unknown): value is TitleModelLoadBehavior =>
  typeof value === "string" &&
  TITLE_MODEL_LOAD_OPTIONS.some((option) => option.value === value);

const isTitleModelResourcePolicy = (value: unknown): value is TitleModelResourcePolicy =>
  typeof value === "string" &&
  TITLE_MODEL_RESOURCE_OPTIONS.some((option) => option.value === value);

const titleModelOptionDescription = <T extends string>(
  options: readonly TitleModelOption<T>[],
  value: T,
): string => options.find((option) => option.value === value)?.description ?? "";

const titleModelLoadDescription = (
  value: TitleModelLoadBehavior,
): string => titleModelOptionDescription(TITLE_MODEL_LOAD_OPTIONS, value);

const titleModelResourceDescription = (
  value: TitleModelResourcePolicy,
): string => titleModelOptionDescription(TITLE_MODEL_RESOURCE_OPTIONS, value);

const isSafeHttps = (value: string): boolean => {
  try {
    const url = new URL(value);
    return url.protocol === "https:" &&
      url.host !== "" &&
      url.username === "" &&
      url.password === "" &&
      !/[\u0000-\u001f\u007f]/u.test(value) &&
      url.href.length <= 8_000;
  } catch {
    return false;
  }
};

const openExternal = (target: HTMLElement, href: string): void => {
  if (!isSafeHttps(href)) return;
  target.dispatchEvent(new CustomEvent("trouve-open-external", {
    detail: { href },
    bubbles: true,
    composed: true,
  }));
};

const githubConnectionSource = (source: string): string => {
  if (source === "environment") return "environment";
  if (source === "oauth") return "GitHub sign-in";
  if (source === "gh-cli") return "gh CLI";
  return "token";
};

export class TrouveGitWorktreeSettings extends withSignalTracking(LitElement) {
  static override styles = panelStyles;

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  #settings: ProtocolGitWorktreeSettings | undefined;
  #busy = false;
  #message = "";
  #error = false;
  #draftDeriveBranchName: boolean | undefined;
  #draftLoadBehavior: TitleModelLoadBehavior | undefined;
  #draftResourcePolicy: TitleModelResourcePolicy | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    queueMicrotask(() => void this.#load());
  }

  async #load(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#busy = true;
    this.#message = "Loading session naming settings…";
    this.#error = false;
    this.requestUpdate();
    try {
      const snapshot = await protocol.gitWorktreeSettingsSnapshot();
      this.#store.value?.replaceGitWorktreeSettings(snapshot.cursor, snapshot.value);
      this.#settings = snapshot.value;
      this.#clearDraft();
      this.#message = "";
    } catch {
      this.#message = genericFailure("Loading settings");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const current = this.#currentSettings();
    const data = new FormData(event.currentTarget as HTMLFormElement);
    const deriveBranchName = this.#draftDeriveBranchName ??
      data.has("derive_branch_name");
    const submittedBehavior = data.get("load_behavior");
    const submittedResources = data.get("resource_policy");
    const behavior = this.#draftLoadBehavior ?? (
      isTitleModelLoadBehavior(submittedBehavior)
        ? submittedBehavior
        : current?.title_model_load_behavior ?? "auto"
    );
    // The resource picker is disabled in Rules Only mode, so FormData omits
    // it. Preserve the selected resource policy for switching the model back
    // on instead of silently discarding the submit.
    const resources = this.#draftResourcePolicy ?? (
      isTitleModelResourcePolicy(submittedResources)
        ? submittedResources
        : current?.title_model_resource_policy ?? "adaptive"
    );
    this.#busy = true;
    this.#message = "Saving…";
    this.#error = false;
    this.requestUpdate();
    try {
      const snapshot = await protocol.setGitWorktreeSettingsSnapshot({
        derive_branch_name_from_session_title: deriveBranchName,
        title_model_load_behavior: behavior,
        title_model_resource_policy: resources,
      });
      this.#store.value?.replaceGitWorktreeSettings(snapshot.cursor, snapshot.value);
      this.#settings = snapshot.value;
      this.#clearDraft();
      this.#message = "Session naming settings saved.";
    } catch {
      this.#clearDraft();
      this.#message = genericFailure("Saving settings");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #currentSettings(): ProtocolGitWorktreeSettings | undefined {
    return this.#store.value === undefined
      ? this.#settings
      : readSignal(this.#store.value.gitWorktreeSettings)?.settings ?? this.#settings;
  }

  #selectionChanged(event: Event): void {
    const form = (event.currentTarget as HTMLSelectElement).form;
    const behaviorSelect = form?.elements.namedItem("load_behavior");
    const resourceSelect = form?.elements.namedItem("resource_policy");
    const deriveBranchName = form?.elements.namedItem("derive_branch_name");
    if (
      form === null ||
      !(behaviorSelect instanceof HTMLSelectElement) ||
      !(resourceSelect instanceof HTMLSelectElement) ||
      !(deriveBranchName instanceof HTMLInputElement) ||
      !isTitleModelLoadBehavior(behaviorSelect.value) ||
      !isTitleModelResourcePolicy(resourceSelect.value)
    ) return;
    // Keep both selections locally while the auto-save is in flight. This
    // makes the selected option and its explanation update in the same frame.
    this.#draftLoadBehavior = behaviorSelect.value;
    this.#draftResourcePolicy = resourceSelect.value;
    this.#draftDeriveBranchName = deriveBranchName.checked;
    this.requestUpdate();
    form.requestSubmit();
  }

  #branchNamingChanged(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    this.#draftDeriveBranchName = input.checked;
    this.requestUpdate();
    input.form?.requestSubmit();
  }

  #clearDraft(): void {
    this.#draftDeriveBranchName = undefined;
    this.#draftLoadBehavior = undefined;
    this.#draftResourcePolicy = undefined;
  }

  async #install(cancel: boolean): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    this.#busy = true;
    this.#message = cancel ? "Cancelling title model install…" : "Starting title model install…";
    this.#error = false;
    this.requestUpdate();
    try {
      if (cancel) await protocol.cancelTitleModelInstall();
      else await protocol.installTitleModel();
      await this.#load();
      this.#message = cancel
        ? "Title model install cancelled."
        : "Title model install started.";
      this.requestUpdate();
    } catch {
      this.#message = genericFailure(cancel ? "Cancelling install" : "Starting install");
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  override render() {
    const settings = this.#currentSettings();
    const model = settings?.title_model;
    const total = model?.install_total ?? 0;
    const value = model?.install_bytes ?? 0;
    const deriveBranchName = this.#draftDeriveBranchName ??
      settings?.derive_branch_name_from_session_title ?? false;
    const behavior = this.#draftLoadBehavior ??
      settings?.title_model_load_behavior ?? "auto";
    const resources = this.#draftResourcePolicy ??
      settings?.title_model_resource_policy ?? "adaptive";
    return html`
      <div class="stack">
        <h2>Session naming</h2>
        <p class="meta">trouve derives a concise title for each session before creating its worktree.</p>
        <form class="card naming-card" @submit=${(event: SubmitEvent) => void this.#save(event)}>
          <label class="check-row">
            <input
              name="derive_branch_name"
              type="checkbox"
              .checked=${deriveBranchName}
              ?disabled=${this.#busy}
              @change=${this.#branchNamingChanged}
            />
            <span>Use session names in branch names</span>
          </label>
          <p class="meta">Off by default: new branches use a compact name such as <code>trouve/abc123</code>. Turn this on to use names such as <code>trouve/session-name-abc123</code>. Existing branches are not renamed.</p>
          <hr />
          <h3>Optional naming model</h3>
          <label><span class="visually-hidden">Load behavior</span><select name="load_behavior" .value=${behavior} ?disabled=${this.#busy} @change=${this.#selectionChanged}>
              ${TITLE_MODEL_LOAD_OPTIONS.map((option) => html`<option value=${option.value}>${option.label}</option>`)}
            </select></label>
          <p class="meta">${titleModelLoadDescription(behavior)}</p>
          <label><span>Compute resources</span><select name="resource_policy" .value=${resources} ?disabled=${this.#busy || behavior === "off"} @change=${this.#selectionChanged}>
              ${TITLE_MODEL_RESOURCE_OPTIONS.map((option) => html`<option value=${option.value}>${option.label}</option>`)}
            </select></label>
          <p class="meta">${titleModelResourceDescription(resources)}</p>
          <p class="meta">The optional model is about 640 MB to download. Changing compute resources restarts it when it is kept ready.</p>
          <button class="visually-hidden-focusable" type="submit" ?disabled=${this.#busy || settings === undefined}>Save naming settings</button>
          <hr />
          <div class="row"><strong class=${model?.state === "error" ? "health-error" : model?.state === "ready" ? "health-ok" : ""}>${model?.state === "ready" ? "Ready" : model?.state === "loading" ? "Loading" : model?.state === "installing" ? "Installing" : model?.state === "stopped" ? "Available" : model?.state === "error" ? "Needs attention" : "Optional model not installed"}</strong><span class="grow"></span>
          ${model?.state === "installing"
            ? html`<button type="button" @click=${() => void this.#install(true)} ?disabled=${this.#busy}>Cancel</button>`
            : model?.runtime_installed === true && model.model_downloaded
              ? nothing
              : behavior === "off"
                ? nothing
                : html`<button class="primary" type="button" @click=${() => void this.#install(false)} ?disabled=${this.#busy}>Install naming model</button>`}
          </div>
          ${model?.detail ? html`<p class="meta">${model.detail}</p>` : nothing}
          ${model?.state === "installing" && total > 0 ? html`<progress max=${total} value=${value}>${value} / ${total}</progress>` : nothing}
        </form>
        ${this.#message === "" ? nothing : html`<p class="status ${this.#error ? "error" : ""}" role="status" aria-live="polite">${this.#message}</p>`}
      </div>
    `;
  }
}

export class TrouveMcpSettings extends withSignalTracking(LitElement) {
  static override styles = panelStyles;

  readonly #services = new ContextConsumer(this, { context: appServicesContext, subscribe: true });
  readonly #store = new ContextConsumer(this, { context: appStoreContext, subscribe: true });
  #servers: readonly ProtocolMcpServerInfo[] = [];
  #logs: ProtocolMcpLogs | undefined;
  #logsName = "";
  #workspaceId = "";
  #busy = false;
  #message = "";
  #error = false;
  #formOpen = false;
  #importOpen = false;
  #importJson = "";
  #formScope: "user" | "workspace" = "user";
  #editingServer: ProtocolMcpServerInfo | undefined;
  #togglePending = new Set<string>();
  #refreshTimer: ReturnType<typeof setInterval> | undefined;
  #logsRefreshTimer: ReturnType<typeof setInterval> | undefined;
  #logsLoading = false;

  override connectedCallback(): void {
    super.connectedCallback();
    queueMicrotask(() => void this.#load());
    this.#refreshTimer ??= globalThis.setInterval(() => {
      if (
        this.#busy
        || this.#togglePending.size > 0
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#load(true);
    }, MCP_REFRESH_MS);
    this.#logsRefreshTimer ??= globalThis.setInterval(() => {
      if (
        this.#logsName === ""
        || this.#logsLoading
        || (typeof document !== "undefined" && document.visibilityState === "hidden")
      ) return;
      void this.#showLogs(this.#logsName, true);
    }, MCP_LOG_REFRESH_MS);
  }

  override disconnectedCallback(): void {
    if (this.#refreshTimer !== undefined) globalThis.clearInterval(this.#refreshTimer);
    if (this.#logsRefreshTimer !== undefined) globalThis.clearInterval(this.#logsRefreshTimer);
    this.#refreshTimer = undefined;
    this.#logsRefreshTimer = undefined;
    super.disconnectedCallback();
  }

  async #load(silent = false, probe = true): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || (silent && this.#busy)) return;
    if (!silent) this.#busy = true;
    if (!silent) {
      this.#message = "Checking MCP servers…";
      this.#error = false;
      this.requestUpdate();
    }
    try {
      this.#servers = await protocol.mcpServers(this.#workspaceId || undefined, probe);
      this.#message = "";
    } catch {
      this.#message = "MCP servers could not be loaded. Retrying automatically.";
      this.#error = true;
    } finally {
      if (!silent) this.#busy = false;
      this.requestUpdate();
    }
  }

  async #save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const name = String(data.get("name") ?? "").trim();
    const scope = String(data.get("scope") ?? "user");
    const workspaceId = String(data.get("workspace_id") ?? "");
    const commandLine = String(data.get("command_line") ?? "").trim();
    const parsedCommand = parseMcpCommandLine(commandLine);
    const env: Record<string, string> = {};
    for (const rawLine of String(data.get("env") ?? "").split("\n")) {
      const line = rawLine.trim();
      if (line === "") continue;
      const equals = line.indexOf("=");
      if (equals < 1) {
        this.#message = "Environment entries must use KEY=VALUE, one per line.";
        this.#error = true;
        this.requestUpdate();
        return;
      }
      env[line.slice(0, equals).trim()] = line.slice(equals + 1);
    }
    if (name === "" || commandLine === "" || !["user", "workspace"].includes(scope)) return;
    if (parsedCommand === undefined) {
      this.#message = "The command line is empty or contains an unfinished quote or escape.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    if (scope === "workspace" && workspaceId === "") {
      this.#message = "Choose a workspace for a workspace-scoped server.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    const request: ProtocolUpsertMcpServerRequest = {
      scope,
      command: parsedCommand.command,
      args: [...parsedCommand.args],
      env,
      enabled: this.#editingServer?.enabled ?? true,
      ...(scope === "workspace" ? { workspace_id: workspaceId } : {}),
    };
    this.#busy = true;
    this.#message = "Saving MCP server…";
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.upsertMcpServer(name, request);
      form.reset();
      this.#formOpen = false;
      this.#editingServer = undefined;
      this.#notifyMcpConfigChanged();
      await this.#load();
      this.#message = `Saved ${name}.`;
      this.requestUpdate();
    } catch {
      this.#message = genericFailure("Saving MCP server");
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #remove(server: ProtocolMcpServerInfo): Promise<void> {
    if (!globalThis.confirm(`Remove MCP server “${server.name}” from ${server.scope} settings?`)) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      await protocol.deleteMcpServer(server.name, server.scope, server.workspace_id || undefined);
      this.#notifyMcpConfigChanged();
      await this.#load();
      this.#message = `Removed ${server.name}.`;
      this.requestUpdate();
    } catch {
      this.#message = genericFailure("Removing MCP server");
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #serverKey(server: Pick<ProtocolMcpServerInfo, "name" | "scope" | "workspace_id">): string {
    return `${server.scope}\u0000${server.workspace_id ?? ""}\u0000${server.name}`;
  }

  #notifyMcpConfigChanged(): void {
    globalThis.dispatchEvent(new Event(MCP_CONFIG_CHANGED_EVENT));
  }

  async #setEnabled(server: ProtocolMcpServerInfo, enabled: boolean): Promise<void> {
    const protocol = this.#services.value?.protocol;
    const key = this.#serverKey(server);
    if (protocol === undefined || this.#busy || this.#togglePending.has(key)) return;
    const previous = this.#servers;
    this.#togglePending.add(key);
    this.#servers = this.#servers.map((candidate) =>
      this.#serverKey(candidate) === key
        ? {
            ...candidate,
            enabled,
            health: enabled
              ? candidate.scope === "workspace" ? "untrusted" : "unknown"
              : "disabled",
            detail: enabled ? "" : "disabled in this scope",
          }
        : candidate
    );
    this.#message = `${enabled ? "Enabling" : "Disabling"} ${server.name}…`;
    this.#error = false;
    this.requestUpdate();
    try {
      await protocol.setMcpServerEnabled(server.name, {
        scope: server.scope,
        enabled,
        ...(server.scope === "workspace" && server.workspace_id
          ? { workspace_id: server.workspace_id }
          : {}),
      });
      this.#notifyMcpConfigChanged();
      await this.#load(true, false);
      this.#message = `${enabled ? "Enabled" : "Disabled"} ${server.name}.`;
      this.#error = false;
    } catch {
      this.#servers = previous;
      this.#message = genericFailure(`${enabled ? "Enabling" : "Disabling"} MCP server`);
      this.#error = true;
    } finally {
      this.#togglePending.delete(key);
      this.requestUpdate();
    }
  }

  async #loadImportFile(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (file === undefined) return;
    try {
      this.#importJson = await file.text();
      this.#message = `Loaded ${file.name}. Review the JSON, then import it.`;
      this.#error = false;
    } catch {
      this.#message = `Could not read ${file.name}.`;
      this.#error = true;
    }
    this.requestUpdate();
  }

  async #import(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const data = new FormData(event.currentTarget as HTMLFormElement);
    const scope = String(data.get("scope") ?? "user");
    const workspaceId = String(data.get("workspace_id") ?? "");
    if (scope === "workspace" && workspaceId === "") {
      this.#message = "Choose a workspace for the imported servers.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    try {
      const servers = parseMcpConfigJson(this.#importJson);
      this.#busy = true;
      this.#message = `Importing ${servers.length} MCP server${servers.length === 1 ? "" : "s"}…`;
      this.#error = false;
      this.requestUpdate();
      for (const server of servers) {
        await protocol.upsertMcpServer(server.name, {
          scope,
          command: server.command,
          args: [...server.args],
          env: { ...server.env },
          enabled: server.enabled,
          ...(scope === "workspace" ? { workspace_id: workspaceId } : {}),
        });
      }
      this.#importOpen = false;
      this.#importJson = "";
      this.#notifyMcpConfigChanged();
      await this.#load();
      this.#message = `Imported ${servers.length} MCP server${servers.length === 1 ? "" : "s"}.`;
    } catch (error) {
      this.#message = error instanceof Error ? error.message : genericFailure("Importing MCP config");
      this.#error = true;
      this.#busy = false;
    }
    this.requestUpdate();
  }

  async #showLogs(name: string, silent = false): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#logsLoading) return;
    this.#logsName = name;
    this.#logsLoading = true;
    if (!silent) {
      this.#logs = undefined;
      this.requestUpdate();
    }
    try {
      this.#logs = await protocol.mcpServerLogs(name);
    } catch {
      this.#message = "MCP logs are temporarily unavailable. Retrying automatically.";
      this.#error = true;
    } finally {
      this.#logsLoading = false;
    }
    this.requestUpdate();
  }

  #openMcpForm(scope: "user" | "workspace", server?: ProtocolMcpServerInfo): void {
    this.#formScope = server?.scope === "workspace" ? "workspace" : scope;
    this.#editingServer = server;
    this.#formOpen = true;
    this.#message = "";
    this.requestUpdate();
  }

  override render() {
    const workspaces = this.#store.value === undefined
      ? []
      : readSignal(this.#store.value.workspaces);
    return html`
      <div class="stack">
        <div class="row"><h2 class="grow">MCP Servers</h2></div>
        <p class="meta">External tool servers offered to the agent as mcp__&lt;server&gt;__&lt;tool&gt;. In ask and allow-list modes each one needs first-use approval per session; read-only modes block MCP tools entirely; yolo skips prompts. trouve's own built-in servers are not listed.</p>
        <p class="meta">Sessions merge these by name: the app-wide list (applies to every workspace), then the session workspace's list. A session branch can override or disable entries via its own committed .agents/.mcp.json — those per-branch files are managed in git, not here.</p>
        ${this.#message === "" ? nothing : html`<p class="status ${this.#error ? "error" : ""}" role="status" aria-live="polite">${this.#message}</p>`}
        <section class="mcp-list" aria-label="Configured MCP servers">
          ${this.#servers.length === 0
            ? html`<p class="mcp-empty">No MCP servers configured.</p>`
            : this.#servers.map((server) => html`
                <article class="mcp-row">
                  <span class="mcp-health ${server.health}">${fontAwesomeIcon(
                    server.health === "ok"
                      ? "check"
                      : server.health === "error"
                        ? "xmark"
                        : server.health === "disabled"
                          ? "pause"
                          : server.health === "untrusted" ? "ban" : "circle-question",
                    { label: `MCP server health: ${server.health}` },
                  )}</span>
                  <div class="mcp-copy">
                    <span><strong>${server.name}</strong><span class="mcp-scope">${server.scope === "workspace" ? `workspace · ${server.workspace_name ?? ""}` : "app-wide"}</span></span>
                    <small>${server.command} ${(server.args ?? []).join(" ")}${server.detail ? ` · ${server.detail}` : ""}</small>
                  </div>
                  <label class="mcp-toggle" title=${`${server.enabled === false ? "Enable" : "Disable"} ${server.name}`}>
                    <input
                      type="checkbox"
                      role="switch"
                      aria-label=${`${server.enabled === false ? "Enable" : "Disable"} MCP server ${server.name}`}
                      .checked=${server.enabled !== false}
                      ?disabled=${this.#busy || this.#togglePending.has(this.#serverKey(server))}
                      @change=${(event: Event) => void this.#setEnabled(
                        server,
                        (event.currentTarget as HTMLInputElement).checked,
                      )}
                    />
                    <span class="mcp-toggle-track" aria-hidden="true"></span>
                  </label>
                  <div class="mcp-actions"><button type="button" @click=${() => void this.#showLogs(server.name)}>Logs</button><button type="button" @click=${() => this.#openMcpForm(server.scope === "workspace" ? "workspace" : "user", server)}>Edit</button><button type="button" @click=${() => void this.#remove(server)} ?disabled=${this.#busy}>Remove</button></div>
                </article>
              `)}
        </section>
        <div class="row"><button type="button" @click=${() => this.#openMcpForm("user")}>${fontAwesomeIcon("plus")} Add app-wide</button><button type="button" ?disabled=${workspaces.length === 0} @click=${() => this.#openMcpForm("workspace")}>${fontAwesomeIcon("plus")} Add to workspace</button><button type="button" @click=${() => { this.#importOpen = !this.#importOpen; this.#message = ""; this.#error = false; this.requestUpdate(); }}>${fontAwesomeIcon("file-import")} Import JSON</button></div>
        ${this.#importOpen ? html`<form class="card mcp-form mcp-import" @submit=${(event: SubmitEvent) => void this.#import(event)}>
          <h3>Import MCP config</h3>
          <p class="meta">Paste an existing <code>mcp.json</code>, Cursor/Claude <code>mcpServers</code> config, or VS Code <code>servers</code> config. Imported names replace matching servers in the selected scope. Only stdio servers are supported.</p>
          <div class="grid">
            <label><span>Import into</span><select name="scope" @change=${(event: Event) => { this.#formScope = (event.currentTarget as HTMLSelectElement).value === "workspace" ? "workspace" : "user"; this.requestUpdate(); }}><option value="user" ?selected=${this.#formScope === "user"}>App-wide</option><option value="workspace" ?selected=${this.#formScope === "workspace"}>Workspace</option></select></label>
            ${this.#formScope === "workspace" ? html`<label><span>Workspace</span><select name="workspace_id" required><option value="">Choose workspace</option>${workspaces.map((workspace) => html`<option value=${workspace.id}>${workspace.name}</option>`)}</select></label>` : html`<input name="workspace_id" type="hidden" value="" />`}
          </div>
          <label><span>Choose a JSON file</span><input type="file" accept=".json,application/json" @change=${(event: Event) => void this.#loadImportFile(event)} /></label>
          <label><span>Config JSON</span><textarea required spellcheck="false" placeholder='{"mcpServers":{"docs":{"command":"npx","args":["-y","docs-mcp"]}}}' .value=${this.#importJson} @input=${(event: Event) => { this.#importJson = (event.currentTarget as HTMLTextAreaElement).value; }}></textarea></label>
          <div class="row"><button class="primary" type="submit" ?disabled=${this.#busy || this.#importJson.trim() === ""}>Import servers</button><button type="button" @click=${() => { this.#importOpen = false; this.#importJson = ""; this.requestUpdate(); }}>Cancel</button></div>
        </form>` : nothing}
        ${this.#formOpen ? html`<form class="card mcp-form" @submit=${(event: SubmitEvent) => void this.#save(event)}>
          <h3>${this.#editingServer === undefined ? "New" : "Edit"} ${this.#formScope === "user" ? "app-wide server (~/.config/trouve/mcp.json — every workspace)" : "workspace server (.agents/.mcp.json)"}</h3>
          <input name="scope" type="hidden" .value=${this.#formScope} />
          ${this.#formScope === "workspace" ? (() => {
            const selectedWorkspaceId = this.#editingServer?.workspace_id ?? workspaces[0]?.id ?? "";
            return html`<label><span>Workspace</span><select name="workspace_id" ?disabled=${this.#editingServer !== undefined}><option value="" ?selected=${selectedWorkspaceId === ""}>Choose workspace</option>${workspaces.map((workspace) => html`<option value=${workspace.id} ?selected=${workspace.id === selectedWorkspaceId}>${workspace.name}</option>`)}</select>${this.#editingServer?.workspace_id ? html`<input name="workspace_id" type="hidden" .value=${this.#editingServer.workspace_id} />` : nothing}</label>`;
          })() : html`<input name="workspace_id" type="hidden" value="" />`}
          <div class="grid mcp-command-grid"><label><span class="visually-hidden">Name</span><input required name="name" autocomplete="off" placeholder="name (e.g. jira)" .value=${this.#editingServer?.name ?? ""} ?readonly=${this.#editingServer !== undefined} /></label><label><span class="visually-hidden">Command and arguments</span><input required name="command_line" autocomplete="off" spellcheck="false" placeholder="command and args (e.g. npx -y jira-mcp --stdio)" .value=${this.#editingServer === undefined ? "" : sessionMcpCommandLine(this.#editingServer)} /></label></div>
          <label><span class="visually-hidden">Environment</span><textarea name="env" autocomplete="off" spellcheck="false" placeholder="environment, one KEY=VALUE per line; ${"${VAR}"} expands at launch" .value=${this.#editingServer === undefined ? "" : sessionMcpEnvironmentLines(this.#editingServer).join("\n")}></textarea></label>
          <div class="row"><button class="primary" type="submit" ?disabled=${this.#busy}>${this.#editingServer === undefined ? "Add server" : "Save changes"}</button><button type="button" @click=${() => { this.#formOpen = false; this.#editingServer = undefined; this.requestUpdate(); }}>Cancel</button></div>
        </form>` : nothing}
        ${this.#logsName === "" ? nothing : html`<section class="card" aria-label=${`Logs for ${this.#logsName}`}><div class="row"><h3 class="grow">Logs — ${this.#logsName}</h3><button type="button" aria-label="Close logs" title="Close logs" @click=${() => { this.#logsName = ""; this.#logs = undefined; this.requestUpdate(); }}>${fontAwesomeIcon("xmark")}</button></div><pre>${this.#logs === undefined ? "Loading…" : this.#logs.lines.length === 0 ? "No log lines yet — logs appear after a health check or the first tool call." : this.#logs.lines.join("\n")}</pre></section>`}
      </div>
    `;
  }
}

export class TrouveIntegrationsSettings extends LitElement {
  static override styles = panelStyles;

  readonly #services = new ContextConsumer(this, { context: appServicesContext, subscribe: true });
  #integration: ProtocolGithubIntegration | undefined;
  #busy = false;
  #message = "";
  #error = false;
  #loginCode = "";
  #loginHost = "";
  #pollTimer: ReturnType<typeof setTimeout> | undefined;
  #pollRemaining = 0;
  #loginGeneration = 0;

  override connectedCallback(): void {
    super.connectedCallback();
    queueMicrotask(() => void this.#load());
  }

  override disconnectedCallback(): void {
    this.#loginGeneration += 1;
    if (this.#pollTimer !== undefined) clearTimeout(this.#pollTimer);
    this.#pollTimer = undefined;
    super.disconnectedCallback();
  }

  #providerId(host: string): string {
    return host === "github.com" ? "github" : `github:${host}`;
  }

  async #load(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      this.#integration = await protocol.githubIntegration();
      this.#error = false;
    } catch {
      this.#message = genericFailure("Loading integrations");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #startLogin(host: string): Promise<void> {
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    this.#busy = true;
    this.#message = `Starting sign-in for ${host}…`;
    this.#error = false;
    this.#loginHost = host;
    const loginGeneration = ++this.#loginGeneration;
    if (this.#pollTimer !== undefined) clearTimeout(this.#pollTimer);
    this.#pollTimer = undefined;
    this.requestUpdate();
    try {
      const started = await protocol.startProviderLogin(this.#providerId(host));
      if (!isSafeHttps(started.verification_url)) throw new TypeError("unsafe verification URL");
      this.#loginCode = started.user_code ?? "";
      this.#message = this.#loginCode === ""
        ? "Finish sign-in in the newly opened browser page."
        : `Enter code ${this.#loginCode} in the newly opened browser page.`;
      openExternal(this, started.verification_url);
      this.#pollRemaining = 120;
      this.#schedulePoll(host, loginGeneration);
    } catch {
      this.#message = genericFailure("Starting GitHub sign-in");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  #schedulePoll(host: string, loginGeneration: number): void {
    if (loginGeneration !== this.#loginGeneration) return;
    if (!this.isConnected || this.#pollRemaining <= 0) {
      this.#message = "Sign-in is still pending. Retry when you are ready.";
      this.requestUpdate();
      return;
    }
    this.#pollTimer = setTimeout(
      () => void this.#poll(host, loginGeneration),
      1_500,
    );
  }

  #cancelLogin(): void {
    this.#loginGeneration += 1;
    if (this.#pollTimer !== undefined) clearTimeout(this.#pollTimer);
    this.#pollTimer = undefined;
    this.#pollRemaining = 0;
    this.#loginCode = "";
    this.#loginHost = "";
    this.#message = "GitHub sign-in was dismissed.";
    this.#error = false;
    this.requestUpdate();
  }

  async #poll(host: string, loginGeneration: number): Promise<void> {
    this.#pollTimer = undefined;
    const protocol = this.#services.value?.protocol;
    if (
      protocol === undefined ||
      !this.isConnected ||
      loginGeneration !== this.#loginGeneration
    ) return;
    this.#pollRemaining -= 1;
    try {
      const status = await protocol.providerLoginStatus(this.#providerId(host));
      if (
        loginGeneration !== this.#loginGeneration ||
        this.#loginHost !== host ||
        !this.isConnected
      ) return;
      if (status.status === "success") {
        this.#message = `Signed in to ${host}.`;
        this.#loginCode = "";
        this.#loginHost = "";
        await this.#load();
        return;
      }
      if (status.status === "failed" || status.status === "none") {
        this.#message = "GitHub sign-in did not complete. Start it again to retry.";
        this.#error = true;
        this.requestUpdate();
        return;
      }
    } catch {
      // A transient status request does not invalidate the device flow.
    }
    this.#schedulePoll(host, loginGeneration);
  }

  async #completeCallback(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#loginHost === "") return;
    const form = event.currentTarget as HTMLFormElement;
    const callback = String(new FormData(form).get("callback") ?? "").trim();
    if (callback === "") return;
    this.#busy = true;
    this.requestUpdate();
    try {
      const status = await protocol.completeProviderLogin(this.#providerId(this.#loginHost), callback);
      if (status.status === "success") {
        form.reset();
        this.#message = `Signed in to ${this.#loginHost}.`;
        this.#loginHost = "";
        this.#loginCode = "";
        await this.#load();
      } else {
        this.#message = "The callback was accepted but sign-in is not complete yet.";
      }
    } catch {
      this.#message = genericFailure("Completing GitHub sign-in");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #disconnect(host: string): Promise<void> {
    if (!globalThis.confirm(`Disconnect ${host}? GitHub-backed workflows will stop until you sign in again.`)) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      await protocol.deleteProvider(this.#providerId(host));
      this.#message = `Disconnected ${host}.`;
      await this.#load();
    } catch {
      this.#message = genericFailure("Disconnecting GitHub");
      this.#error = true;
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #addHost(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined || this.#busy) return;
    const form = event.currentTarget as HTMLFormElement;
    const data = new FormData(form);
    const host = String(data.get("host") ?? "").trim().toLowerCase();
    const clientId = String(data.get("client_id") ?? "").trim();
    if (!/^[a-z0-9.-]+$/.test(host) || host.includes("..") || clientId === "") {
      this.#message = "Enter a hostname and the OAuth app client ID for that GitHub Enterprise instance.";
      this.#error = true;
      this.requestUpdate();
      return;
    }
    this.#busy = true;
    this.requestUpdate();
    try {
      this.#integration = await protocol.addGithubHost({ host, client_id: clientId });
      form.reset();
      this.#message = `Added ${host}. Sign in to finish setup.`;
      this.#error = false;
    } catch {
      this.#message = genericFailure("Adding GitHub Enterprise host");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  async #removeHost(host: string): Promise<void> {
    if (!globalThis.confirm(`Remove GitHub Enterprise host ${host}?`)) return;
    const protocol = this.#services.value?.protocol;
    if (protocol === undefined) return;
    this.#busy = true;
    this.requestUpdate();
    try {
      this.#integration = await protocol.removeGithubHost(host);
      this.#message = `Removed ${host}.`;
      this.#error = false;
    } catch {
      this.#message = genericFailure("Removing GitHub Enterprise host");
      this.#error = true;
    } finally {
      this.#busy = false;
      this.requestUpdate();
    }
  }

  override render() {
    const hosts = this.#integration?.hosts ?? [];
    return html`
      <div class="stack">
        <h2>Integrations</h2>
        <p class="meta">Powers the Pull Requests tab and PR creation. Sign in to each GitHub host separately with OAuth. GitHub access requests repository and organization-read permissions so team reviewers can be displayed. Existing OAuth connections must be re-signed in once to grant the additional permission. GitHub Enterprise hosts require a device-flow-enabled OAuth app client ID.</p>
        ${this.#message === "" ? nothing : html`<p class="status ${this.#error ? "error" : ""}" role="status" aria-live="polite">${this.#message}</p>`}
        ${hosts.length === 0 ? html`<p class="meta">No GitHub host information is available.</p>` : hosts.map((host) => html`
            <article class="card integration-host">
              <div class="row"><h3 class="grow">${host.host === "github.com" ? "GitHub" : host.host}</h3><span class="integration-status ${host.configured ? "connected" : ""}">${fontAwesomeIcon(host.configured ? "circle" : "circle-dot")} ${host.configured ? `connected (${githubConnectionSource(host.source)})` : "not configured"}</span>${host.removable ? html`<button type="button" @click=${() => void this.#removeHost(host.host)} ?disabled=${this.#busy}>Remove host</button>` : nothing}</div>
              <div class="row">
                ${host.oauth_available ? html`<button class=${host.configured ? "" : "primary"} type="button" @click=${() => void this.#startLogin(host.host)} ?disabled=${this.#busy}>${host.source === "oauth" ? "Re-sign in with GitHub" : "Sign in with GitHub"}</button>` : nothing}
                ${host.configured ? html`<button class="danger additive-action" type="button" @click=${() => void this.#disconnect(host.host)} ?disabled=${this.#busy}>Disconnect</button>` : nothing}
              </div>
              ${!host.oauth_available ? html`<p class="meta">To enable one-click sign-in, register an OAuth app on this instance (device flow enabled) and re-add the host with its client id.</p>` : nothing}
            </article>
          `)}
        ${this.#loginHost === "" ? nothing : html`
          <form class="card" @submit=${(event: SubmitEvent) => void this.#completeCallback(event)}>
            <h3>Complete ${this.#loginHost} sign-in</h3>
            ${this.#loginCode === "" ? nothing : html`<p>Device code: <strong><code>${this.#loginCode}</code></strong></p>`}
            <label><span>Callback URL or authentication code (only when requested)</span><input name="callback" autocomplete="off" /></label>
            <div class="row"><button type="submit" ?disabled=${this.#busy}>Submit callback</button><button type="button" @click=${() => this.#cancelLogin()}>Cancel</button></div>
          </form>`}
        <form class="card integration-add" @submit=${(event: SubmitEvent) => void this.#addHost(event)}>
          <h3>Add GitHub Enterprise host</h3>
          <p class="meta">Each self-hosted GitHub Enterprise Server instance uses its own OAuth app. Enable device flow on that app, then enter its public client id here.</p>
          <div class="integration-add-fields">
            <label><span>Hostname</span><input required name="host" inputmode="url" placeholder="github.example.com" /></label>
            <label><span>OAuth app client ID</span><input required name="client_id" autocomplete="off" /></label>
            <button class="primary" type="submit" ?disabled=${this.#busy}>Add</button>
          </div>
        </form>
      </div>
    `;
  }
}

customElements.define("trouve-git-worktree-settings", TrouveGitWorktreeSettings);
customElements.define("trouve-mcp-settings", TrouveMcpSettings);
customElements.define("trouve-integrations-settings", TrouveIntegrationsSettings);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-git-worktree-settings": TrouveGitWorktreeSettings;
    "trouve-mcp-settings": TrouveMcpSettings;
    "trouve-integrations-settings": TrouveIntegrationsSettings;
  }
}
