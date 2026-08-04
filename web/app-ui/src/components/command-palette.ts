import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing } from "lit";

import { appServicesContext, appStoreContext } from "../contexts/app-contexts.js";
import { filterCommandPaletteItemsOffThread } from "../services/content-worker-client.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  buildCommandPaletteItems,
  filterCommandPaletteItems,
  isCommandPaletteShortcut,
  nextCommandPaletteIndex,
  type CommandPaletteAction,
  type CommandPaletteGroup,
  type CommandPaletteItem,
} from "./command-palette-model.js";

export const COMMAND_PALETTE_ACTION_EVENT = "trouve-command-palette-action";

export interface CommandPaletteActionDetail {
  readonly action: CommandPaletteAction;
}

const GROUP_ORDER = ["Actions", "Threads", "Sessions", "Views"] as const satisfies
  readonly CommandPaletteGroup[];
const WORKER_FUZZY_THRESHOLD = 200;

const stateLabel = (state: NonNullable<CommandPaletteItem["state"]>): string =>
  ({
    running: "Running",
    attention: "Needs attention",
    idle: "Idle",
    done: "Completed",
    failed: "Failed",
  })[state];

export class TrouveCommandPalette extends withSignalTracking(LitElement) {
  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });

  #open = false;
  #query = "";
  #selectedIndex = 0;
  #restoreTarget: HTMLElement | null = null;
  #workerMatchKey = "";
  #workerRequestedKey = "";
  #workerMatches: readonly CommandPaletteItem[] = [];
  #workerGeneration = 0;
  #workerPending = false;

  override connectedCallback(): void {
    super.connectedCallback();
    globalThis.addEventListener("keydown", this.#globalKeyDown, true);
  }

  override disconnectedCallback(): void {
    globalThis.removeEventListener("keydown", this.#globalKeyDown, true);
    this.#restoreFocus();
    super.disconnectedCallback();
  }

  openPalette(): void {
    if (this.#open) return;
    const active = globalThis.document?.activeElement;
    this.#restoreTarget = active instanceof HTMLElement ? active : null;
    this.#query = "";
    this.#selectedIndex = 0;
    this.#open = true;
    this.requestUpdate();
  }

  closePalette(): void {
    if (!this.#open) return;
    this.#open = false;
    this.#query = "";
    this.#selectedIndex = 0;
    const dialog = this.querySelector<HTMLDialogElement>("#command-palette-dialog");
    if (dialog?.open === true) dialog.close();
    else this.#restoreFocus();
    this.requestUpdate();
  }

  protected override updated(): void {
    const dialog = this.querySelector<HTMLDialogElement>("#command-palette-dialog");
    if (this.#open && dialog !== null && !dialog.open) {
      try {
        dialog.showModal();
        dialog.querySelector<HTMLInputElement>("#command-palette-input")?.focus();
      } catch {
        this.#open = false;
        this.#restoreFocus();
      }
    } else if (!this.#open && dialog?.open === true) {
      dialog.close();
    }
    if (this.#open) {
      this.querySelector<HTMLElement>(
        `[data-command-index="${this.#selectedIndex}"]`,
      )?.scrollIntoView({ block: "nearest" });
    }
  }

  readonly #globalKeyDown = (event: KeyboardEvent): void => {
    if (!isCommandPaletteShortcut(event)) return;
    if (
      !this.#open &&
      [...(globalThis.document?.querySelectorAll<HTMLDialogElement>("dialog[open]") ?? [])]
        .some((dialog) => !this.contains(dialog))
    ) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (this.#open) this.closePalette();
    else this.openPalette();
  };

  readonly #dialogCancelled = (event: Event): void => {
    event.preventDefault();
    this.closePalette();
  };

  readonly #dialogClosed = (): void => {
    if (this.#open) {
      this.#open = false;
      this.#query = "";
      this.#selectedIndex = 0;
      this.requestUpdate();
    }
    this.#restoreFocus();
  };

  readonly #backdropClicked = (event: MouseEvent): void => {
    const dialog = event.currentTarget as HTMLDialogElement;
    if (event.target !== dialog) return;
    const bounds = dialog.getBoundingClientRect();
    if (
      event.clientX < bounds.left ||
      event.clientX > bounds.right ||
      event.clientY < bounds.top ||
      event.clientY > bounds.bottom
    ) {
      this.closePalette();
    }
  };

  #restoreFocus(): void {
    const target = this.#restoreTarget;
    this.#restoreTarget = null;
    if (target === null) return;
    queueMicrotask(() => {
      if (target.isConnected) target.focus();
    });
  }

  #items(): readonly CommandPaletteItem[] {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) return [];
    const route = readSignal(services.router.route);
    return buildCommandPaletteItems({
      route,
      workspaces: readSignal(store.workspaces),
      sessions: readSignal(store.sessions),
      activeThreads:
        route.kind === "session" ? store.threadsForSession(route.sessionId) : [],
    });
  }

  #matchingItems(): readonly CommandPaletteItem[] {
    const items = this.#items();
    if (items.length < WORKER_FUZZY_THRESHOLD || this.#query.trim() === "") {
      this.#workerGeneration += 1;
      this.#workerPending = false;
      this.#workerRequestedKey = "";
      return filterCommandPaletteItems(items, this.#query);
    }
    const key = `${this.#query}\u0000${items.map((item) => item.id).join("\u0000")}`;
    if (key !== this.#workerRequestedKey) {
      this.#workerRequestedKey = key;
      this.#workerPending = true;
      const generation = ++this.#workerGeneration;
      void filterCommandPaletteItemsOffThread(items, this.#query).then(
        (matches) => {
          if (generation !== this.#workerGeneration || !this.isConnected) return;
          this.#workerMatchKey = key;
          this.#workerMatches = matches;
          this.#workerPending = false;
          this.#selectedIndex = 0;
          this.requestUpdate();
        },
        () => {
          if (generation !== this.#workerGeneration || !this.isConnected) return;
          this.#workerRequestedKey = "";
          this.#workerPending = false;
          this.requestUpdate();
        },
      );
    }
    return this.#workerMatchKey === key ? this.#workerMatches : [];
  }

  readonly #queryChanged = (event: Event): void => {
    this.#query = (event.currentTarget as HTMLInputElement).value;
    this.#selectedIndex = 0;
    this.requestUpdate();
  };

  readonly #inputKeyDown = (event: KeyboardEvent): void => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      this.closePalette();
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      const matches = this.#matchingItems();
      const index = Math.min(this.#selectedIndex, Math.max(0, matches.length - 1));
      const item = matches[index];
      if (item !== undefined) this.#activate(item);
      return;
    }
    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const next = nextCommandPaletteIndex(
      event.key,
      this.#selectedIndex,
      this.#matchingItems().length,
    );
    if (next === undefined) return;
    event.preventDefault();
    this.#selectedIndex = next;
    this.requestUpdate();
  };

  #activate(item: CommandPaletteItem): void {
    // Dismissal restores the invoking control. Activation lets the destination
    // establish focus instead (most notably the new-session dialog's prompt).
    this.#restoreTarget = null;
    this.closePalette();
    this.dispatchEvent(
      new CustomEvent<CommandPaletteActionDetail>(COMMAND_PALETTE_ACTION_EVENT, {
        bubbles: true,
        composed: true,
        detail: { action: item.action },
      }),
    );
  }

  #renderGroup(
    group: CommandPaletteGroup,
    matches: readonly CommandPaletteItem[],
  ) {
    const indexed = matches
      .map((item, index) => ({ item, index }))
      .filter(({ item }) => item.group === group);
    if (indexed.length === 0) return nothing;
    return html`
      <section class="command-palette-group" aria-labelledby=${`command-group-${group}`}>
        <h2 id=${`command-group-${group}`}>${group}</h2>
        ${indexed.map(({ item, index }) => html`
          <button
            id=${`command-palette-option-${index}`}
            class="command-palette-option"
            type="button"
            role="option"
            tabindex="-1"
            aria-selected=${index === this.#selectedIndex ? "true" : "false"}
            aria-label=${item.state === undefined
              ? `${item.label}, ${item.detail}`
              : `${item.label}, ${stateLabel(item.state)}, ${item.detail}`}
            data-command-index=${index}
            @click=${() => this.#activate(item)}
          >
            <span class="command-palette-icon" aria-hidden="true">
              ${item.state === undefined
                ? item.icon
                : html`<span class="status-dot ${item.state}"></span>`}
            </span>
            <span class="command-palette-copy">
              <strong>${item.label}</strong>
              <small>${item.detail}</small>
            </span>
            ${item.state === undefined
              ? nothing
              : html`<span class="command-palette-state ${item.state}">${stateLabel(item.state)}</span>`}
          </button>
        `)}
      </section>
    `;
  }

  override render() {
    const matches = this.#matchingItems();
    const selectedIndex = Math.min(
      this.#selectedIndex,
      Math.max(0, matches.length - 1),
    );
    if (selectedIndex !== this.#selectedIndex) this.#selectedIndex = selectedIndex;
    const activeDescendant = matches.length === 0
      ? nothing
      : `command-palette-option-${selectedIndex}`;
    return html`
      <dialog
        id="command-palette-dialog"
        class="command-palette-dialog"
        aria-labelledby="command-palette-title"
        @cancel=${this.#dialogCancelled}
        @close=${this.#dialogClosed}
        @click=${this.#backdropClicked}
      >
        <header class="command-palette-search">
          <span aria-hidden="true">⌕</span>
          <label class="visually-hidden" for="command-palette-input" id="command-palette-title">Search commands, sessions, and threads</label>
          <input
            id="command-palette-input"
            type="search"
            role="combobox"
            autocomplete="off"
            spellcheck="false"
            placeholder="Search commands, sessions, and threads"
            aria-expanded="true"
            aria-controls="command-palette-results"
            aria-autocomplete="list"
            aria-activedescendant=${activeDescendant}
            .value=${this.#query}
            @input=${this.#queryChanged}
            @keydown=${this.#inputKeyDown}
          />
          <kbd>Esc</kbd>
        </header>
        <div
          id="command-palette-results"
          class="command-palette-results"
          role="listbox"
          aria-label="Matching commands"
        >
          ${this.#workerPending
            ? html`<div class="command-palette-empty" role="status">
                <strong>Searching…</strong>
                <span>Ranking commands, sessions, and threads.</span>
              </div>`
            : matches.length === 0
            ? html`<div class="command-palette-empty" role="status">
                <strong>No matching commands</strong>
                <span>Try a session title, branch, thread mode, or action.</span>
              </div>`
            : GROUP_ORDER.map((group) => this.#renderGroup(group, matches))}
        </div>
        <footer class="command-palette-footer">
          <span><kbd>↑</kbd><kbd>↓</kbd> navigate</span>
          <span><kbd>↵</kbd> open</span>
          <span class="command-palette-count" role="status">${matches.length} ${matches.length === 1 ? "result" : "results"}</span>
        </footer>
      </dialog>
    `;
  }
}

customElements.define("trouve-command-palette", TrouveCommandPalette);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-command-palette": TrouveCommandPalette;
  }
}
