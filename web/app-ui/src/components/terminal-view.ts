import xtermCss from "@xterm/xterm/css/xterm.css?inline";
import { ContextConsumer } from "@lit/context";
import { html, LitElement, unsafeCSS, type PropertyValues } from "lit";

import { terminalContext } from "../contexts/app-contexts.js";
import { parseOsc52ClipboardRequest } from "./terminal-clipboard.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import {
  normalizeTerminalTitle,
  terminalRequestedSize,
} from "./terminal-control-sequences.js";

// Match the server's retained PTY backlog so a slow dynamic import cannot
// discard output before xterm's parser is ready.
const REPLAY_LIMIT = 512 * 1024;

export interface TerminalInputDetail {
  readonly terminalId: string;
  readonly data: string;
}

export interface TerminalResizeDetail {
  readonly terminalId: string;
  readonly cols: number;
  readonly rows: number;
}

export interface TerminalClipboardRequestDetail {
  readonly terminalId: string;
  readonly text: string;
}

export interface TerminalTitleDetail {
  readonly terminalId: string;
  readonly title: string;
}

export interface TerminalNoticeDetail {
  readonly terminalId: string;
  readonly message: string;
}

export class TrouveTerminalView extends LitElement {
  static override properties = {
    terminalId: { type: String, attribute: "terminal-id" },
    active: { type: Boolean, reflect: true },
    label: { type: String },
  };

  static override styles = [
    unsafeCSS(xtermCss),
    unsafeCSS(`
      :host { position: relative; display: grid; grid-template-rows: minmax(0, 1fr) auto; min-width: 0; min-height: 0; height: 100%; background: var(--trouve-terminal-bg, var(--trouve-code-bg)); }
      #terminal { position: relative; min-width: 0; min-height: 8rem; padding: 5px 7px; overflow: hidden; }
      #terminal::after { content: ""; position: absolute; z-index: 20; inset: 0; border: 0 solid transparent; background: transparent; pointer-events: none; }
      :host([bell]) #terminal::after { border-width: 2px; border-color: color-mix(in srgb, var(--trouve-code-fg) 80%, transparent); background: color-mix(in srgb, var(--trouve-code-fg) 8%, transparent); }
      .history-badge { position: absolute; z-index: 25; inset-block-start: 8px; inset-inline-end: 12px; padding: 2px 7px; border: 1px solid var(--trouve-border-strong); border-radius: 999px; color: var(--trouve-text-dim); background: color-mix(in srgb, var(--trouve-panel-bg) 92%, transparent); font: 11px var(--trouve-font-mono); pointer-events: none; }
      .touch-modifiers { display: none; gap: 4px; padding: 5px max(6px, env(safe-area-inset-right)) max(5px, env(safe-area-inset-bottom)) max(6px, env(safe-area-inset-left)); border-top: 1px solid var(--trouve-rule); background: var(--trouve-panel-bg); overflow-x: auto; }
      .touch-modifiers button { min-width: 38px; min-height: 34px; border: 1px solid var(--trouve-border-strong); border-radius: var(--trouve-radius-sm); color: var(--trouve-text); background: var(--trouve-control-bg); font: 11px var(--trouve-font-mono); }
      .touch-modifiers button:focus-visible { outline: 2px solid var(--trouve-accent); outline-offset: 1px; }
      @media (pointer: coarse), (max-width: 760px) { .touch-modifiers { display: flex; } }
    `),
  ];

  terminalId = "";
  active = true;
  label = "Terminal";

  #terminal: import("@xterm/xterm").Terminal | undefined;
  #fit: import("@xterm/addon-fit").FitAddon | undefined;
  #search: import("@xterm/addon-search").SearchAddon | undefined;
  #resizeObserver: ResizeObserver | undefined;
  #appearanceObserver: MutationObserver | undefined;
  #disposables: import("@xterm/xterm").IDisposable[] = [];
  #replay = "";
  #generation = 0;
  #lastSize = "";
  #focusRequested = false;
  #bellTimer: ReturnType<typeof setTimeout> | undefined;
  #historyLines = 0;
  readonly #terminalScope = new ContextConsumer(this, {
    context: terminalContext,
    subscribe: true,
  });

  get #effectiveTerminalId(): string {
    return this.terminalId || this.#terminalScope.value?.terminalId || "";
  }

  protected override firstUpdated(): void {
    // Every tab keeps a live xterm parser, including background tabs. This
    // mirrors the native GridState-per-tab model and makes switching instant.
    void this.#mount();
  }

  protected override updated(changed: PropertyValues<this>): void {
    if (
      changed.has("terminalId") &&
      changed.get("terminalId") !== undefined
    ) {
      // A keyed parent normally replaces the element. This defensive path
      // prevents output from one PTY leaking into another if a consumer reuses
      // the custom element directly.
      this.#replay = "";
      this.#disposeRenderer();
      void this.#mount();
      return;
    }
    if (changed.has("active")) {
      if (this.active) {
        void this.#mount().then(() => this.#fitAndNotify());
      } else {
        this.#terminal?.blur();
      }
    } else if (changed.has("label") && this.#terminal !== undefined) {
      this.#terminal.element?.setAttribute("aria-label", this.label);
    }
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    this.#disposeRenderer();
    super.disconnectedCallback();
  }

  write(data: string): void {
    if (this.#terminal !== undefined) {
      this.#terminal.write(data, () => this.#refreshHistoryBadge());
      return;
    }
    this.#replay = (this.#replay + data).slice(-REPLAY_LIMIT);
  }

  clear(): void {
    this.#replay = "";
    this.#terminal?.clear();
    this.#refreshHistoryBadge();
  }

  override focus(): void {
    if (this.#terminal === undefined) {
      this.#focusRequested = true;
      return;
    }
    this.#focusRequested = false;
    this.#terminal.focus();
  }

  findNext(query: string): boolean {
    return query !== "" && (this.#search?.findNext(query) ?? false);
  }

  findPrevious(query: string): boolean {
    return query !== "" && (this.#search?.findPrevious(query) ?? false);
  }

  clearSearch(): void {
    this.#search?.clearDecorations();
  }

  selectedText(): string {
    return this.#terminal?.getSelection() ?? "";
  }

  paste(text: string): void {
    if (text === "") return;
    this.#terminal?.paste(text);
    this.#terminal?.focus();
  }

  #send(data: string): void {
    this.dispatchEvent(
      new CustomEvent<TerminalInputDetail>("trouve-terminal-input", {
        detail: { terminalId: this.#effectiveTerminalId, data },
        bubbles: true,
        composed: true,
      }),
    );
    this.#terminal?.focus();
  }

  #disposeRenderer(): void {
    this.#generation += 1;
    this.#resizeObserver?.disconnect();
    this.#resizeObserver = undefined;
    this.#appearanceObserver?.disconnect();
    this.#appearanceObserver = undefined;
    if (this.#bellTimer !== undefined) clearTimeout(this.#bellTimer);
    this.#bellTimer = undefined;
    this.removeAttribute("bell");
    for (const disposable of this.#disposables.splice(0)) disposable.dispose();
    this.#terminal?.dispose();
    this.#terminal = undefined;
    this.#fit = undefined;
    this.#search = undefined;
    this.#lastSize = "";
    this.#setHistoryLines(0);
  }

  async #mount(): Promise<void> {
    if (!this.isConnected || this.#terminal !== undefined) return;
    const generation = ++this.#generation;
    const [xterm, fit, search, unicode, links] = await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
      import("@xterm/addon-search"),
      import("@xterm/addon-unicode11"),
      import("@xterm/addon-web-links"),
    ]);
    if (generation !== this.#generation || !this.isConnected) return;
    const parent = this.renderRoot.querySelector<HTMLElement>("#terminal");
    if (parent === null) return;
    const style = getComputedStyle(this);
    const token = (name: string, fallback: string): string =>
      style.getPropertyValue(name).trim() || fallback;
    const requestedFontSize = Number.parseFloat(token("--trouve-font-size", "13"));
    const terminal = new xterm.Terminal({
      // The Unicode 11 addon registers through xterm's proposed Unicode API.
      // Keep this enabled for as long as that addon is loaded; otherwise the
      // constructor succeeds but mounting aborts before xterm creates its
      // keyboard textarea, cursor, or renderer.
      allowProposedApi: true,
      cols: 100,
      convertEol: false,
      cursorBlink:
        !(globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false),
      fontFamily: token("--trouve-font-mono", "monospace"),
      fontSize: Number.isFinite(requestedFontSize) ? requestedFontSize : 13,
      screenReaderMode: true,
      scrollback: 5_000,
      rows: 28,
      theme: {
        background: token("--trouve-code-bg", "#111318"),
        foreground: token("--trouve-code-fg", "#d7d9df"),
        cursor: token("--trouve-text-hi", "#ffffff"),
        selectionBackground: token("--trouve-selection-bg", "#315a88"),
        black: token("--trouve-term-black", "#111318"),
        red: token("--trouve-err", "#e39ea6"),
        green: token("--trouve-ok", "#88c999"),
        yellow: token("--trouve-warn", "#dbc57b"),
        blue: token("--trouve-accent", "#75a7df"),
        magenta: token("--trouve-term-magenta", "#c397d8"),
        cyan: token("--trouve-term-cyan", "#7ac8c8"),
        white: token("--trouve-text", "#d7d9df"),
      },
    });
    const fitAddon = new fit.FitAddon();
    const searchAddon = new search.SearchAddon();
    terminal.loadAddon(fitAddon);
    terminal.loadAddon(searchAddon);
    terminal.loadAddon(new unicode.Unicode11Addon());
    terminal.unicode.activeVersion = "11";
    terminal.loadAddon(
      new links.WebLinksAddon((event, uri) => {
        event.preventDefault();
        try {
          if (new URL(uri).protocol !== "https:") return;
        } catch {
          return;
        }
        this.dispatchEvent(
          new CustomEvent<{ href: string }>("trouve-open-external", {
            detail: { href: uri },
            bubbles: true,
            composed: true,
          }),
        );
      }),
    );
    this.#disposables.push(
      terminal.parser.registerOscHandler(52, (data) => {
        const request = parseOsc52ClipboardRequest(data);
        if (request.kind === "copy") {
          this.dispatchEvent(
            new CustomEvent<TerminalClipboardRequestDetail>(
              "trouve-terminal-clipboard-request",
              {
                detail: {
                  terminalId: this.#effectiveTerminalId,
                  text: request.text,
                },
                bubbles: true,
                composed: true,
              },
            ),
          );
        } else {
          this.#notice(
            request.kind === "read"
              ? "blocked terminal clipboard read"
              : "blocked invalid clipboard request",
          );
        }
        return true;
      }),
      terminal.parser.registerCsiHandler({ final: "t" }, (params) => {
        if (params[0] !== 8) return false;
        const requested = terminalRequestedSize(params);
        if (requested !== undefined) {
          this.#notice(
            `shell requested ${requested.cols}×${requested.rows}; panel size is fixed`,
          );
        }
        return true;
      }),
    );
    terminal.open(parent);
    terminal.element?.setAttribute("aria-label", this.label);
    this.#disposables.push(
      terminal.onData((data) => this.#send(data)),
      terminal.onBell(() => this.#ringBell()),
      terminal.onScroll(() => this.#refreshHistoryBadge()),
      terminal.onTitleChange((title) => {
        const normalized = normalizeTerminalTitle(title);
        if (normalized === "") return;
        this.dispatchEvent(
          new CustomEvent<TerminalTitleDetail>("trouve-terminal-title", {
            detail: {
              terminalId: this.#effectiveTerminalId,
              title: normalized,
            },
            bubbles: true,
            composed: true,
          }),
        );
      }),
    );
    this.#terminal = terminal;
    this.#fit = fitAddon;
    this.#search = searchAddon;
    if (this.#replay !== "") {
      terminal.write(this.#replay, () => this.#refreshHistoryBadge());
      this.#replay = "";
    }
    this.#observeAppearance();
    this.#resizeObserver = new ResizeObserver(() => this.#fitAndNotify());
    this.#resizeObserver.observe(parent);
    this.#fitAndNotify();
    if (this.#focusRequested) {
      this.#focusRequested = false;
      terminal.focus();
    }
  }

  #notice(message: string): void {
    this.dispatchEvent(
      new CustomEvent<TerminalNoticeDetail>("trouve-terminal-notice", {
        detail: { terminalId: this.#effectiveTerminalId, message },
        bubbles: true,
        composed: true,
      }),
    );
  }

  #ringBell(): void {
    // A bell from a background shell updates its parser but does not flash the
    // visible terminal, matching the native active-tab behavior.
    if (!this.active) return;
    if (this.#bellTimer !== undefined) clearTimeout(this.#bellTimer);
    this.setAttribute("bell", "");
    this.#bellTimer = setTimeout(() => {
      this.#bellTimer = undefined;
      this.removeAttribute("bell");
    }, 120);
  }

  #refreshHistoryBadge(): void {
    const buffer = this.#terminal?.buffer.active;
    this.#setHistoryLines(
      buffer === undefined ? 0 : Math.max(0, buffer.baseY - buffer.viewportY),
    );
  }

  #setHistoryLines(lines: number): void {
    if (lines === this.#historyLines) return;
    this.#historyLines = lines;
    this.requestUpdate();
  }

  #observeAppearance(): void {
    if (globalThis.MutationObserver === undefined) return;
    const observer = new MutationObserver(() => this.#refreshAppearance());
    const app = this.closest("trouve-app");
    if (app !== null) {
      observer.observe(app, {
        attributes: true,
        attributeFilter: ["style", "data-reduce-motion"],
      });
    }
    const themeBoundary = this.closest("[data-theme]");
    if (themeBoundary !== null && themeBoundary !== app) {
      observer.observe(themeBoundary, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
    }
    this.#appearanceObserver = observer;
  }

  #refreshAppearance(): void {
    const terminal = this.#terminal;
    if (terminal === undefined) return;
    const style = getComputedStyle(this);
    const token = (name: string, fallback: string): string =>
      style.getPropertyValue(name).trim() || fallback;
    const requestedFontSize = Number.parseFloat(token("--trouve-font-size", "13"));
    terminal.options = {
      fontFamily: token("--trouve-font-mono", "monospace"),
      fontSize: Number.isFinite(requestedFontSize) ? requestedFontSize : 13,
      theme: {
        background: token("--trouve-code-bg", "#111318"),
        foreground: token("--trouve-code-fg", "#d7d9df"),
        cursor: token("--trouve-text-hi", "#ffffff"),
        selectionBackground: token("--trouve-selection-bg", "#315a88"),
        black: token("--trouve-term-black", "#111318"),
        red: token("--trouve-err", "#e39ea6"),
        green: token("--trouve-ok", "#88c999"),
        yellow: token("--trouve-warn", "#dbc57b"),
        blue: token("--trouve-accent", "#75a7df"),
        magenta: token("--trouve-term-magenta", "#c397d8"),
        cyan: token("--trouve-term-cyan", "#7ac8c8"),
        white: token("--trouve-text", "#d7d9df"),
      },
    };
    this.#fitAndNotify();
  }

  #fitAndNotify(): void {
    const terminal = this.#terminal;
    const fit = this.#fit;
    if (terminal === undefined || fit === undefined) return;
    try {
      fit.fit();
    } catch {
      return;
    }
    const size = `${terminal.cols}x${terminal.rows}`;
    if (size === this.#lastSize || terminal.cols <= 0 || terminal.rows <= 0) return;
    this.#lastSize = size;
    this.dispatchEvent(
      new CustomEvent<TerminalResizeDetail>("trouve-terminal-resize", {
        detail: {
          terminalId: this.#effectiveTerminalId,
          cols: terminal.cols,
          rows: terminal.rows,
        },
        bubbles: true,
        composed: true,
      }),
    );
  }

  override render() {
    return html`
      <div id="terminal" role="application" aria-label=${this.label}></div>
      <span class="history-badge" ?hidden=${this.#historyLines === 0}>
        history · ${this.#historyLines}
      </span>
      <div class="touch-modifiers" aria-label="Terminal modifier keys">
        <button type="button" @click=${() => this.#send("\u001b")}>Esc</button>
        <button type="button" @click=${() => this.#send("\t")}>Tab</button>
        <button type="button" @click=${() => this.#send("\u0003")}>Ctrl-C</button>
        <button type="button" @click=${() => this.#send("\u0004")}>Ctrl-D</button>
        <button type="button" aria-label="Arrow up" @click=${() => this.#send("\u001b[A")}>${fontAwesomeIcon("arrow-up")}</button>
        <button type="button" aria-label="Arrow down" @click=${() => this.#send("\u001b[B")}>${fontAwesomeIcon("arrow-down")}</button>
        <button type="button" aria-label="Arrow left" @click=${() => this.#send("\u001b[D")}>${fontAwesomeIcon("arrow-left")}</button>
        <button type="button" aria-label="Arrow right" @click=${() => this.#send("\u001b[C")}>${fontAwesomeIcon("arrow-right")}</button>
      </div>
    `;
  }
}

customElements.define("trouve-terminal-view", TrouveTerminalView);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-terminal-view": TrouveTerminalView;
  }
}
