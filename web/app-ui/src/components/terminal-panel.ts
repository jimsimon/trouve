import { ContextConsumer, ContextProvider } from "@lit/context";
import { html, LitElement, nothing, type PropertyValues } from "lit";
import { repeat } from "lit/directives/repeat.js";

import {
  appServicesContext,
  sessionContext,
  terminalContext,
} from "../contexts/app-contexts.js";
import type { ProtocolTerminalInfo } from "../services/protocol-client.js";
import { TerminalOutputStream } from "../services/terminal-output-stream.js";
import type {
  TerminalClipboardRequestDetail,
  TerminalInputDetail,
  TerminalNoticeDetail,
  TerminalResizeDetail,
  TerminalTitleDetail,
  TrouveTerminalView,
} from "./terminal-view.js";
import { nextHorizontalTabIndex, rovingTabIndex } from "./tab-navigation.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import "./terminal-view.js";

const INITIAL_COLS = 100;
const INITIAL_ROWS = 28;

export class TrouveTerminalPanel extends LitElement {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  sessionId = "";
  #generation = 0;
  #loading = false;
  #error = "";
  #terminals: readonly ProtocolTerminalInfo[] = [];
  #activeId = "";
  #busy = "";
  #searchQuery = "";
  #controlStatus = "";
  #observedSessionId = "";
  #exited = new Set<string>();
  readonly #titles = new Map<string, string>();
  readonly #notices = new Map<string, string>();
  readonly #searchQueries = new Map<string, string>();
  readonly #clipboardRequests = new Map<string, string>();
  readonly #streams = new Map<string, TerminalOutputStream>();
  readonly #pendingOutput = new Map<string, string>();
  readonly #resizeTimers = new Map<string, ReturnType<typeof setTimeout>>();

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });
  readonly #terminalProvider = new ContextProvider(this, {
    context: terminalContext,
    initialValue: { terminalId: "" },
  });

  protected override updated(changed: PropertyValues<this>): void {
    const sessionId = this.#effectiveSessionId;
    if (sessionId !== this.#observedSessionId) {
      this.#observedSessionId = sessionId;
      this.#titles.clear();
      this.#notices.clear();
      this.#searchQueries.clear();
      this.#clipboardRequests.clear();
      this.#searchQuery = "";
      this.#controlStatus = "";
      void this.#load();
    }
    this.#flushPendingOutput();
  }

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    this.#disposeStreams();
    for (const timer of this.#resizeTimers.values()) clearTimeout(timer);
    this.#resizeTimers.clear();
    super.disconnectedCallback();
  }

  override render() {
    if (this.#loading) {
      return html`<div class="screen-empty" role="status"><span>Loading terminals…</span></div>`;
    }
    if (this.#error !== "" && this.#terminals.length === 0) {
      return html`<div class="screen-empty" role="alert"><strong>Unable to load terminals</strong><span>${this.#error}</span><button type="button" @click=${() => this.#load()}>Retry</button></div>`;
    }
    const clipboardRequest = this.#clipboardRequests.get(this.#activeId);
    const activeNotice = this.#notices.get(this.#activeId) ?? "";
    return html`
      <section
        class="terminal-panel-content"
        @trouve-terminal-input=${this.#terminalInput}
        @trouve-terminal-resize=${this.#terminalResize}
        @trouve-terminal-clipboard-request=${this.#terminalClipboardRequested}
        @trouve-terminal-title=${this.#terminalTitleChanged}
        @trouve-terminal-notice=${this.#terminalNoticeChanged}
      >
        <div class="terminal-toolbar">
          <strong>Shell in the session worktree</strong>
          <label class="visually-hidden" for="terminal-search">Find in terminal</label>
          <input
            id="terminal-search"
            type="search"
            placeholder="Find in terminal…"
            ?disabled=${this.#activeId === ""}
            .value=${this.#searchQuery}
            @input=${(event: Event) => {
              this.#searchQuery = (event.currentTarget as HTMLInputElement).value;
              this.#searchQueries.set(this.#activeId, this.#searchQuery);
              if (this.#searchQuery === "") {
                this.#view(this.#activeId)?.clearSearch();
                this.#controlStatus = "";
              }
            }}
            @keydown=${(event: KeyboardEvent) => {
              if (event.key !== "Enter") return;
              event.preventDefault();
              this.#search(!event.shiftKey);
            }}
          />
          <button type="button" aria-label="Previous terminal search match" ?disabled=${this.#activeId === "" || this.#searchQuery === ""} @click=${() => this.#search(false)}>${fontAwesomeIcon("arrow-up")}</button>
          <button type="button" aria-label="Next terminal search match" ?disabled=${this.#activeId === "" || this.#searchQuery === ""} @click=${() => this.#search(true)}>${fontAwesomeIcon("arrow-down")}</button>
          <button class="terminal-additive-action" type="button" ?disabled=${this.#activeId === "" || this.#busy !== ""} @click=${() => void this.#copySelection()}>Copy</button>
          <button class="terminal-additive-action" type="button" ?disabled=${this.#activeId === "" || this.#busy !== "" || this.#exited.has(this.#activeId)} @click=${() => void this.#pasteClipboard()}>Paste</button>
          ${clipboardRequest === undefined
            ? nothing
            : html`<span
                class="terminal-clipboard-actions"
                role="alertdialog"
                aria-label=${`The terminal requested permission to copy ${clipboardRequest.length} character${clipboardRequest.length === 1 ? "" : "s"} to your clipboard.`}
              >
                <button type="button" @click=${() => void this.#resolveClipboardRequest(true)}>Allow copy</button>
                <button type="button" @click=${() => void this.#resolveClipboardRequest(false)}>Deny</button>
              </span>`}
          <button type="button" ?disabled=${this.#activeId === "" || this.#busy !== ""} @click=${() => void this.#restartActive()}>${this.#busy === "restart" ? "Restarting…" : html`${fontAwesomeIcon("arrows-rotate")} Restart`}</button>
          <span role="status" aria-live="polite">${this.#controlStatus}</span>
        </div>
        <div class="terminal-tabs" role="tablist" aria-label="Session terminals">
          ${this.#terminals.map(
            (terminal, index) => html`
              <span class="terminal-tab-item ${terminal.id === this.#activeId ? "selected" : ""}">
                <button
                  type="button"
                  role="tab"
                  aria-selected=${terminal.id === this.#activeId}
                  tabindex=${rovingTabIndex(
                    index,
                    this.#terminals.findIndex((candidate) => candidate.id === this.#activeId),
                    this.#terminals.length,
                  )}
                  @click=${() => this.#select(terminal.id)}
                  @keydown=${(event: KeyboardEvent) => this.#tabKeydown(event, index)}
                >${terminal.exited || this.#exited.has(terminal.id)
                  ? fontAwesomeIcon("circle")
                  : nothing}${this.#terminalTitle(terminal.id, index)}</button>
                <button
                  class="terminal-close"
                  type="button"
                  aria-label=${`Close ${this.#terminalTitle(terminal.id, index)}`}
                  @click=${() => this.#closeTerminal(terminal.id)}
                >${fontAwesomeIcon("xmark")}</button>
              </span>
            `,
          )}
          <button type="button" aria-label="New terminal" ?disabled=${this.#busy !== ""} @click=${() => this.#open(true)}>${fontAwesomeIcon("plus")}</button>
        </div>
        ${this.#error !== ""
          ? html`<p class="terminal-notice error" role="alert">${this.#error}</p>`
          : activeNotice !== ""
            ? html`<p class="terminal-notice" role="status">${activeNotice}</p>`
            : nothing}
        <div class="terminal-stack">
          ${this.#terminals.length === 0
            ? html`<div class="screen-empty terminal-empty"><span>Create a terminal tab to start a shell in this session.</span></div>`
            : repeat(
                this.#terminals,
                (terminal) => terminal.id,
                (terminal, index) => html`
                  <trouve-terminal-view
                    class=${terminal.id === this.#activeId ? "active" : "inactive"}
                    terminal-id=${terminal.id}
                    label=${this.#terminalTitle(terminal.id, index)}
                    .active=${terminal.id === this.#activeId}
                  ></trouve-terminal-view>
                `,
              )}
        </div>
      </section>
    `;
  }

  async #load(): Promise<void> {
    const services = this.#services.value;
    const generation = ++this.#generation;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "") return;
    this.#disposeStreams();
    this.#loading = true;
    this.#error = "";
    this.requestUpdate();
    try {
      let terminals = await services.protocol.terminals(sessionId);
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return;
      if (terminals.length === 0) {
        terminals = [await services.protocol.openTerminal(
          sessionId,
          INITIAL_COLS,
          INITIAL_ROWS,
        )];
      }
      if (generation !== this.#generation || sessionId !== this.#effectiveSessionId) return;
      this.#terminals = terminals;
      terminals.forEach((terminal, index) => {
        if (!this.#titles.has(terminal.id)) {
          this.#titles.set(terminal.id, `Terminal ${index + 1}`);
        }
        if (!this.#searchQueries.has(terminal.id)) {
          this.#searchQueries.set(terminal.id, "");
        }
      });
      this.#exited = new Set(
        terminals.filter((terminal) => terminal.exited).map((terminal) => terminal.id),
      );
      const activeStillExists = terminals.some((terminal) => terminal.id === this.#activeId);
      this.#activeId = activeStillExists ? this.#activeId : (terminals[0]?.id ?? "");
      this.#searchQuery = this.#searchQueries.get(this.#activeId) ?? "";
      this.#controlStatus = this.#notices.get(this.#activeId) ?? "";
      this.#terminalProvider.setValue({ terminalId: this.#activeId });
      for (const terminal of terminals) this.#follow(terminal);
    } catch {
      if (generation === this.#generation) this.#error = "The terminal request failed.";
    } finally {
      if (generation === this.#generation) {
        this.#loading = false;
        this.requestUpdate();
        if (this.#activeId !== "") {
          void this.updateComplete.then(() => this.#view(this.#activeId)?.focus());
        }
      }
    }
  }

  async #open(createNew: boolean): Promise<void> {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (services === undefined || sessionId === "") return;
    this.#error = "";
    try {
      const terminal = createNew
        ? await services.protocol.createTerminal(sessionId, INITIAL_COLS, INITIAL_ROWS)
        : await services.protocol.openTerminal(sessionId, INITIAL_COLS, INITIAL_ROWS);
      if (!this.isConnected) return;
      this.#terminals = [
        ...this.#terminals.filter((candidate) => candidate.id !== terminal.id),
        terminal,
      ];
      this.#titles.set(terminal.id, `Terminal ${this.#terminals.length}`);
      this.#searchQueries.set(terminal.id, "");
      if (terminal.exited) this.#exited.add(terminal.id);
      else this.#exited.delete(terminal.id);
      this.#select(terminal.id);
      this.#follow(terminal);
      this.requestUpdate();
    } catch {
      this.#error = "The terminal could not be opened.";
      this.requestUpdate();
    }
  }

  #follow(terminal: ProtocolTerminalInfo): void {
    if (terminal.exited || this.#streams.has(terminal.id)) return;
    const services = this.#services.value;
    if (services === undefined) return;
    const stream = new TerminalOutputStream({
      path: services.protocol.terminalOutputUrl(terminal.id),
      onData: (data) => this.#write(terminal.id, data),
      onExit: () => {
        this.#exited.add(terminal.id);
        this.requestUpdate();
      },
      onDiagnostic: () => {
        this.#error = "Some terminal output could not be decoded.";
        this.requestUpdate();
      },
    });
    this.#streams.set(terminal.id, stream);
    stream.start();
  }

  #select(terminalId: string, focusTerminal = true): void {
    this.#activeId = terminalId;
    this.#searchQuery = this.#searchQueries.get(terminalId) ?? "";
    this.#controlStatus = this.#notices.get(terminalId) ?? "";
    this.#terminalProvider.setValue({ terminalId });
    this.requestUpdate();
    if (focusTerminal) {
      void this.updateComplete.then(() => this.#view(terminalId)?.focus());
    }
  }

  #tabKeydown(event: KeyboardEvent, index: number): void {
    const target = nextHorizontalTabIndex(event.key, index, this.#terminals.length);
    if (target === undefined) return;
    event.preventDefault();
    const terminal = this.#terminals[target];
    if (terminal === undefined) return;
    this.#select(terminal.id, false);
    void this.updateComplete.then(() => {
      this.querySelectorAll<HTMLButtonElement>('.terminal-tabs [role="tab"]')[target]?.focus();
    });
  }

  #search(next: boolean): void {
    const view = this.#view(this.#activeId);
    const found = next
      ? view?.findNext(this.#searchQuery)
      : view?.findPrevious(this.#searchQuery);
    this.#controlStatus = found ? "Terminal match found." : "No terminal match found.";
    this.requestUpdate();
  }

  async #copySelection(): Promise<void> {
    const selection = this.#view(this.#activeId)?.selectedText() ?? "";
    if (selection === "") {
      this.#controlStatus = "Select terminal text to copy.";
      this.requestUpdate();
      return;
    }
    try {
      await globalThis.navigator.clipboard.writeText(selection);
      this.#controlStatus = "Terminal selection copied.";
    } catch {
      this.#controlStatus = "Terminal selection could not be copied.";
    }
    this.requestUpdate();
  }

  async #pasteClipboard(): Promise<void> {
    try {
      const text = await globalThis.navigator.clipboard.readText();
      if (!this.isConnected) return;
      if (text === "") {
        this.#controlStatus = "The clipboard has no text to paste.";
      } else {
        this.#view(this.#activeId)?.paste(text);
        this.#controlStatus = "Clipboard text pasted.";
      }
    } catch {
      this.#controlStatus = "Clipboard text could not be read.";
    }
    this.requestUpdate();
  }

  async #restartActive(): Promise<void> {
    const services = this.#services.value;
    const oldId = this.#activeId;
    const index = this.#terminals.findIndex((terminal) => terminal.id === oldId);
    if (services === undefined || index < 0 || this.#busy !== "") return;
    this.#busy = "restart";
    this.#error = "";
    this.requestUpdate();
    try {
      await services.protocol.killTerminal(oldId).catch(() => undefined);
      const terminal = await services.protocol.createTerminal(
        this.#effectiveSessionId,
        INITIAL_COLS,
        INITIAL_ROWS,
      );
      if (!this.isConnected) return;
      this.#streams.get(oldId)?.close();
      this.#streams.delete(oldId);
      this.#pendingOutput.delete(oldId);
      this.#exited.delete(oldId);
      const terminals = [...this.#terminals];
      const retainedTitle = this.#titles.get(oldId);
      const retainedSearch = this.#searchQueries.get(oldId) ?? "";
      terminals[index] = terminal;
      this.#terminals = terminals;
      this.#titles.delete(oldId);
      this.#notices.delete(oldId);
      this.#searchQueries.delete(oldId);
      this.#clipboardRequests.delete(oldId);
      this.#titles.set(terminal.id, retainedTitle ?? `Terminal ${index + 1}`);
      this.#searchQueries.set(terminal.id, retainedSearch);
      this.#activeId = terminal.id;
      this.#terminalProvider.setValue({ terminalId: terminal.id });
      this.#follow(terminal);
      this.#controlStatus = "Terminal restarted.";
    } catch {
      this.#error = "The terminal could not be restarted.";
    } finally {
      this.#busy = "";
      this.requestUpdate();
      void this.updateComplete.then(() => this.#view(this.#activeId)?.focus());
    }
  }

  async #closeTerminal(terminalId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined) return;
    const removedIndex = this.#terminals.findIndex(
      (terminal) => terminal.id === terminalId,
    );
    if (removedIndex < 0) return;
    await services.protocol.killTerminal(terminalId).catch(() => undefined);
    this.#streams.get(terminalId)?.close();
    this.#streams.delete(terminalId);
    this.#pendingOutput.delete(terminalId);
    const resizeTimer = this.#resizeTimers.get(terminalId);
    if (resizeTimer !== undefined) clearTimeout(resizeTimer);
    this.#resizeTimers.delete(terminalId);
    this.#exited.delete(terminalId);
    this.#titles.delete(terminalId);
    this.#notices.delete(terminalId);
    this.#searchQueries.delete(terminalId);
    this.#clipboardRequests.delete(terminalId);
    const activeId = this.#activeId;
    this.#terminals = this.#terminals.filter(
      (terminal) => terminal.id !== terminalId,
    );
    this.#activeId = activeId !== terminalId && this.#terminals.some(
      (terminal) => terminal.id === activeId,
    )
      ? activeId
      : (this.#terminals[Math.min(removedIndex, this.#terminals.length - 1)]?.id ?? "");
    this.#searchQuery = this.#searchQueries.get(this.#activeId) ?? "";
    this.#controlStatus = this.#notices.get(this.#activeId) ?? "";
    this.#terminalProvider.setValue({ terminalId: this.#activeId });
    this.requestUpdate();
  }

  readonly #terminalInput = (event: CustomEvent<TerminalInputDetail>): void => {
    const services = this.#services.value;
    if (services === undefined || this.#exited.has(event.detail.terminalId)) return;
    void services.protocol.terminalInput(event.detail.terminalId, event.detail.data).catch(() => {
      this.#error = "Terminal input could not be sent.";
      this.requestUpdate();
    });
  };

  readonly #terminalResize = (event: CustomEvent<TerminalResizeDetail>): void => {
    const services = this.#services.value;
    if (services === undefined || this.#exited.has(event.detail.terminalId)) return;
    const previous = this.#resizeTimers.get(event.detail.terminalId);
    if (previous !== undefined) clearTimeout(previous);
    this.#resizeTimers.set(
      event.detail.terminalId,
      setTimeout(() => {
        this.#resizeTimers.delete(event.detail.terminalId);
        void services.protocol
          .terminalResize(
            event.detail.terminalId,
            event.detail.cols,
            event.detail.rows,
          )
          .catch(() => undefined);
      }, 80),
    );
  };

  readonly #terminalClipboardRequested = (
    event: CustomEvent<TerminalClipboardRequestDetail>,
  ): void => {
    this.#clipboardRequests.set(event.detail.terminalId, event.detail.text);
    if (event.detail.terminalId === this.#activeId) {
      this.#controlStatus = "Terminal clipboard request awaiting confirmation.";
    }
    this.requestUpdate();
  };

  readonly #terminalTitleChanged = (
    event: CustomEvent<TerminalTitleDetail>,
  ): void => {
    if (!this.#terminals.some((terminal) => terminal.id === event.detail.terminalId)) {
      return;
    }
    this.#titles.set(event.detail.terminalId, event.detail.title);
    this.requestUpdate();
  };

  readonly #terminalNoticeChanged = (
    event: CustomEvent<TerminalNoticeDetail>,
  ): void => {
    if (!this.#terminals.some((terminal) => terminal.id === event.detail.terminalId)) {
      return;
    }
    this.#notices.set(event.detail.terminalId, event.detail.message);
    if (event.detail.terminalId === this.#activeId) {
      this.#controlStatus = event.detail.message;
    }
    this.requestUpdate();
  };

  async #resolveClipboardRequest(allow: boolean): Promise<void> {
    const terminalId = this.#activeId;
    const request = this.#clipboardRequests.get(terminalId);
    if (request === undefined) return;
    this.#clipboardRequests.delete(terminalId);
    if (!allow) {
      this.#controlStatus = "Terminal clipboard request denied.";
      this.requestUpdate();
      return;
    }
    try {
      await globalThis.navigator.clipboard.writeText(request);
      this.#controlStatus = "Terminal clipboard request allowed.";
    } catch {
      this.#controlStatus = "Terminal clipboard request could not be completed.";
    }
    this.requestUpdate();
  }

  #write(terminalId: string, data: string): void {
    const view = this.#view(terminalId);
    if (view !== undefined) {
      view.write(data);
      return;
    }
    const pending = `${this.#pendingOutput.get(terminalId) ?? ""}${data}`.slice(
      -512 * 1024,
    );
    this.#pendingOutput.set(terminalId, pending);
  }

  #flushPendingOutput(): void {
    for (const [terminalId, data] of this.#pendingOutput) {
      const view = this.#view(terminalId);
      if (view === undefined) continue;
      view.write(data);
      this.#pendingOutput.delete(terminalId);
    }
  }

  #view(terminalId: string): TrouveTerminalView | undefined {
    return [...this.querySelectorAll("trouve-terminal-view")].find(
      (view) => view.terminalId === terminalId,
    );
  }

  #terminalTitle(terminalId: string, index: number): string {
    return this.#titles.get(terminalId) ?? `Terminal ${index + 1}`;
  }

  #disposeStreams(): void {
    for (const stream of this.#streams.values()) stream.close();
    this.#streams.clear();
    this.#pendingOutput.clear();
  }
}

customElements.define("trouve-terminal-panel", TrouveTerminalPanel);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-terminal-panel": TrouveTerminalPanel;
  }
}
