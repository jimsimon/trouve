import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";

import {
  appServicesContext,
  sessionContext,
} from "../contexts/app-contexts.js";
import type { ProtocolMcpServerInfo } from "../services/protocol-client.js";
import {
  sessionMcpCommandLine,
  sessionMcpEnvironmentLines,
} from "./session-mcp-model.js";

export class TrouveSessionMcpPanel extends LitElement {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  sessionId = "";
  #servers: readonly ProtocolMcpServerInfo[] = [];
  #loading = false;
  #error = "";
  #generation = 0;
  #observedSessionId = "";

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });

  protected override updated(changed: PropertyValues<this>): void {
    const sessionId = this.#effectiveSessionId;
    if (!changed.has("sessionId") && sessionId === this.#observedSessionId) return;
    this.#observedSessionId = sessionId;
    this.#generation += 1;
    this.#servers = [];
    this.#loading = false;
    this.#error = "";
    void this.#load();
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    super.disconnectedCallback();
  }

  override render() {
    return html`
      <section class="session-mcp-surface" aria-labelledby="session-mcp-title" aria-busy=${this.#loading ? "true" : "false"}>
        <header class="inspection-summary session-mcp-header">
          <span>
            <strong id="session-mcp-title">Effective MCP servers for this session</strong>
          </span>
          <button type="button" ?disabled=${this.#loading} @click=${() => void this.#load()}>
            ${this.#loading ? "Refreshing…" : "↻ Refresh"}
          </button>
        </header>
        <p class="session-mcp-description">App-wide, workspace, and branch configs merged the way a turn in this session sees them. Each entry shows the layer whose definition won.</p>
        ${this.#error === ""
          ? nothing
          : html`<div class="session-mcp-notice" role="alert">
              <span>${this.#error}</span>
              <button type="button" @click=${() => void this.#load()}>Retry</button>
            </div>`}
        ${this.#loading && this.#servers.length === 0
          ? html`<div class="screen-empty" role="status"><span>Loading effective MCP configuration…</span></div>`
          : this.#servers.length === 0 && this.#error === ""
            ? html`<div class="screen-empty"><span>No MCP servers apply to this session.</span></div>`
            : html`<ul class="session-mcp-list">
                ${this.#servers.map((server) => this.#renderServer(server))}
              </ul>`}
      </section>
    `;
  }

  #renderServer(server: ProtocolMcpServerInfo) {
    const environment = sessionMcpEnvironmentLines(server);
    return html`
      <li class="session-mcp-card health-${server.health}">
        <header>
          <strong>${server.name}</strong>
          <span class="session-mcp-scope">${server.scope}</span>
        </header>
        <code>${sessionMcpCommandLine(server)}</code>
        ${environment.length === 0
          ? nothing
          : html`<pre aria-label=${`${server.name} environment`}>${environment.join("\n")}</pre>`}
        ${server.detail === "" ? nothing : html`<p>${server.detail}</p>`}
      </li>
    `;
  }

  async #load(): Promise<void> {
    const protocol = this.#services.value?.protocol;
    const sessionId = this.#effectiveSessionId;
    if (protocol === undefined || sessionId === "" || this.#loading) return;
    const generation = ++this.#generation;
    this.#loading = true;
    this.#error = "";
    this.requestUpdate();
    try {
      const servers = await protocol.sessionMcpServers(sessionId);
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return;
      this.#servers = servers;
    } catch {
      if (generation === this.#generation && sessionId === this.#effectiveSessionId) {
        this.#error = "The effective MCP configuration could not be loaded.";
      }
    } finally {
      if (generation === this.#generation && sessionId === this.#effectiveSessionId) {
        this.#loading = false;
        this.requestUpdate();
      }
    }
  }
}

customElements.define("trouve-session-mcp-panel", TrouveSessionMcpPanel);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-session-mcp-panel": TrouveSessionMcpPanel;
  }
}
