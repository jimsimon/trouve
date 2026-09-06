import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing } from "lit";
import { repeat } from "lit/directives/repeat.js";

import { appServicesContext, appStoreContext } from "../contexts/app-contexts.js";
import { preferredSessionThreadId } from "../services/resume-preferences.js";
import {
  LOCAL_MODEL_WAITING_LABEL,
  SESSION_TITLE_WAITING_STATUS,
  titleGenerationTimeoutMs,
} from "../services/title-generation.js";
import type { AppStore, SessionListItem } from "../state/app-store.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  sessionAgePresentation,
  sessionStatusText,
} from "../state/session-inbox-model.js";
import { sessionIndicatorPresentation } from "../state/session-indicator-model.js";
import {
  visibleSessionPullRequestBadge,
  type SessionPullRequestBadge,
} from "./session-pull-request-badge.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import { copyChatText } from "./chat-presentation.js";
import {
  organizeWorkspaceSessions,
  pullRequestKind,
  workspaceSessionSectionCollapsed,
  type WorkspaceSessionGrouping,
  type WorkspaceSessionListFields,
  type WorkspaceSessionOrdering,
  type WorkspaceSessionSection,
} from "./workspace-session-list-model.js";

let nextArchivedListId = 0;

type OrganizedSessionListItem = SessionListItem & WorkspaceSessionListFields & {
  readonly pullRequestBadge: SessionPullRequestBadge | undefined;
};

export const enrichWorkspaceSessions = (
  store: Pick<AppStore, "sessionMetadata" | "sessionPullRequests">,
  sessions: readonly SessionListItem[],
  workspaceId: string,
): readonly OrganizedSessionListItem[] => sessions
  .filter((session) => workspaceId === "" || session.workspaceId === workspaceId)
  .map((session) => {
    const pullRequests = store.sessionPullRequests(session.id);
    return {
      ...session,
      createdAt: store.sessionMetadata(session.id)?.created_at ?? session.updatedAt,
      pullRequestKind: pullRequestKind(pullRequests),
      pullRequestBadge: visibleSessionPullRequestBadge(pullRequests),
    };
  });

/** A first real context consumer: gallery tests can provide an isolated store,
 * while application screens share the stable provider at the shell boundary. */
export class TrouveSessionList extends withSignalTracking(LitElement) {
  static override properties = {
    workspaceId: { type: String, attribute: "workspace-id" },
    showArchived: { type: Boolean, attribute: "show-archived" },
    grouping: { type: String },
    ordering: { type: String },
    showBranches: { type: Boolean, attribute: "show-branches" },
    showStatus: { type: Boolean, attribute: "show-status" },
    statusFilter: { type: Number, attribute: "status-filter" },
    pullRequestFilter: { type: Number, attribute: "pull-request-filter" },
  };

  workspaceId = "";
  showArchived = false;
  grouping: WorkspaceSessionGrouping = "repository";
  ordering: WorkspaceSessionOrdering = "updated";
  showBranches = true;
  showStatus = true;
  statusFilter = 0b1_1111;
  pullRequestFilter = 0b1_1111;
  #menuSessionId = "";
  #menuPosition = { x: 0, y: 0 };
  #editingSessionId = "";
  #deleteSessionId = "";
  #modalTitle = "";
  #busySessionId = "";
  #generatingSessionId = "";
  #generationAbort: AbortController | undefined;
  #requestError = "";
  readonly #expandedArchivedWorkspaceIds = new Set<string>();
  readonly #collapsedSessionSections = new Set<string>();
  readonly #instanceId = ++nextArchivedListId;
  readonly #archivedListId = `archived-sessions-${this.#instanceId}`;
  readonly #modalTitleId = `session-modal-title-${this.#instanceId}`;
  readonly #modalDescriptionId = `session-modal-description-${this.#instanceId}`;

  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    document.addEventListener("pointerdown", this.#dismissPopupFromPointer, true);
  }

  override disconnectedCallback(): void {
    document.removeEventListener("pointerdown", this.#dismissPopupFromPointer, true);
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const dialog = this.querySelector<HTMLDialogElement>(".session-modal");
    const modalOpen = this.#editingSessionId !== "" || this.#deleteSessionId !== "";
    if (modalOpen && dialog !== null && !dialog.open) {
      try {
        dialog.showModal();
      } catch {
        dialog.show();
      }
      if (this.#editingSessionId !== "") {
        dialog.querySelector<HTMLInputElement>('input[name="title"]')?.select();
      } else {
        dialog.querySelector<HTMLButtonElement>('[data-session-modal-action="cancel"]')?.focus();
      }
    } else if (!modalOpen && dialog?.open === true) {
      dialog.close();
    }
  }

  override render() {
    const store = this.#store.value;
    if (store === undefined) {
      return html`<p class="context-placeholder" role="status">No session context</p>`;
    }
    const sessions = readSignal(store.sessions);
    const route = this.#services.value?.router.route;
    const currentRoute = route === undefined ? undefined : readSignal(route);
    const selectedSessionId =
      currentRoute?.kind === "session" ? currentRoute.sessionId : undefined;
    const now = Date.now();
    const organizedSessions = enrichWorkspaceSessions(store, sessions, this.workspaceId);
    const groups = organizeWorkspaceSessions(organizedSessions, {
      workspaceId: this.workspaceId,
      grouping: this.grouping,
      ordering: this.ordering,
      statusFilter: this.statusFilter,
      pullRequestFilter: this.pullRequestFilter,
      now,
    });
    const selectedArchived = groups.archived.some(({ id }) => id === selectedSessionId);
    const archivedExpanded = groups.archived.length > 0 && (
      this.#expandedArchivedWorkspaceIds.has(this.workspaceId) || selectedArchived
    );
    if (groups.sections.length === 0 && groups.archived.length === 0) {
      return html`<p class="context-placeholder">No matching sessions</p>`;
    }
    return html`
      ${groups.sections.length === 0
        ? html`<p class="context-placeholder session-list-empty">No active sessions</p>`
        : groups.sections.map((section) =>
            this.#renderSection(section, selectedSessionId, now))}
      ${groups.archived.length === 0 || !this.showArchived
        ? nothing
        : html`
            <section class="archived-session-group" aria-label="Archived sessions">
              <button
                type="button"
                class="archived-session-toggle"
                aria-expanded=${archivedExpanded}
                aria-controls=${this.#archivedListId}
                @click=${() => this.#toggleArchived(archivedExpanded)}
              >
                ${fontAwesomeIcon(archivedExpanded ? "caret-down" : "caret-right", {
                  className: "archived-session-chevron",
                })}
                <span>Archived (${groups.archived.length})</span>
              </button>
              <ol
                id=${this.#archivedListId}
                class="session-list archived-session-list"
                ?hidden=${!archivedExpanded}
              >
                ${repeat(
                  groups.archived,
                  (session) => session.id,
                  (session) => this.#renderSession(session, selectedSessionId, now),
                )}
              </ol>
            </section>
          `}
      ${this.#requestError === ""
        ? nothing
        : html`<p class="session-action-error" role="alert">${this.#requestError}</p>`}
      <dialog
        class="session-modal"
        aria-labelledby=${this.#modalTitleId}
        aria-describedby=${this.#deleteSessionId === "" ? nothing : this.#modalDescriptionId}
        @cancel=${(event: Event) => { event.preventDefault(); this.#closeActions(); }}
      >
        ${this.#editingSessionId !== ""
          ? html`
              <form class="session-modal-layout" @submit=${(event: SubmitEvent) => this.#rename(event, this.#editingSessionId)}>
                <h2 id=${this.#modalTitleId}>Rename session</h2>
                <label class="visually-hidden" for=${`rename-${this.#editingSessionId}`}>Session title</label>
                <input id=${`rename-${this.#editingSessionId}`} name="title" .value=${this.#modalTitle} @input=${(event: InputEvent) => { this.#modalTitle = (event.currentTarget as HTMLInputElement).value; }} maxlength="200" placeholder="Session title" required />
                ${this.#requestError === "" ? nothing : html`<p class="dialog-error" role="alert">${this.#requestError}</p>`}
                <footer>
                  <button data-session-modal-action="cancel" type="button" @click=${this.#closeActions}>Cancel</button>
                  <button type="button" ?disabled=${this.#busySessionId === this.#editingSessionId || this.#generatingSessionId === this.#editingSessionId} @click=${() => void this.#generateRename(this.#editingSessionId)}>${this.#generatingSessionId === this.#editingSessionId ? "Generating…" : "Generate"}</button>
                  <button class="primary" type="submit" ?disabled=${this.#busySessionId === this.#editingSessionId}>Rename</button>
                </footer>
              </form>
            `
          : this.#deleteSessionId !== ""
            ? html`
                <div class="session-modal-layout">
                  <h2 id=${this.#modalTitleId}>Delete session “${this.#modalTitle}”?</h2>
                  <p id=${this.#modalDescriptionId}>This removes the session's worktree, branch history in trouve, and its event log. The git branch itself is kept.</p>
                  ${this.#requestError === "" ? nothing : html`<p class="dialog-error" role="alert">${this.#requestError}</p>`}
                  <footer>
                    <button data-session-modal-action="cancel" type="button" @click=${this.#closeActions}>Cancel</button>
                    <button class="primary" type="button" ?disabled=${this.#busySessionId === this.#deleteSessionId} @click=${() => void this.#delete(this.#deleteSessionId)}>Delete</button>
                  </footer>
                </div>
              `
            : nothing}
      </dialog>
    `;
  }

  #renderSection(
    section: WorkspaceSessionSection<OrganizedSessionListItem>,
    selectedSessionId: string | undefined,
    now: number,
  ) {
    const sectionKey = `${this.workspaceId}:${this.grouping}:${section.key}`;
    const collapsed = workspaceSessionSectionCollapsed(
      section,
      this.#collapsedSessionSections.has(sectionKey),
      selectedSessionId,
    );
    const listId = `session-section-${this.#instanceId}-${section.key}`;
    return html`
      ${section.label === ""
        ? nothing
        : html`<button
            type="button"
            class="session-section-toggle"
            aria-expanded=${collapsed ? "false" : "true"}
            aria-controls=${listId}
            @click=${() => this.#toggleSection(sectionKey)}
          >
            ${fontAwesomeIcon(collapsed ? "caret-right" : "caret-down")}
            <span>${section.label} (${section.sessions.length})</span>
          </button>`}
      <ol
        id=${listId}
        class="session-list active-session-list"
        aria-label=${section.label === "" ? "Active sessions" : `${section.label} sessions`}
        ?hidden=${collapsed}
      >
        ${repeat(
          section.sessions,
          (session) => session.id,
          (session) => this.#renderSession(session, selectedSessionId, now),
        )}
      </ol>
    `;
  }

  #renderSession(
    session: OrganizedSessionListItem,
    selectedSessionId: string | undefined,
    now: number,
  ) {
    const selected = session.id === selectedSessionId;
    const pullRequestBadge = session.pullRequestBadge;
    const indicator = sessionIndicatorPresentation(session);
    const age = sessionAgePresentation(session.updatedAt, now);
    const titleWaiting = this.#store.value?.titleGenerationWaiting(session.id);
    const titleShimmer = titleWaiting === false;
    return html`
      <li class="session-entry">
        <div
          class="session-row-wrap ${selected ? "selected" : ""} ${
            this.showBranches ? "with-branch" : ""
          }"
          data-actions-open=${this.#menuSessionId === session.id}
          @contextmenu=${(event: MouseEvent) => this.#openContextMenu(event, session.id)}
        >
                <button
                  type="button"
                  class="session-row ${selected ? "selected" : ""} ${
                    this.showBranches ? "with-branch" : ""
                  } ${this.showStatus ? "" : "without-status"}"
                  aria-current=${selected ? "page" : "false"}
                  data-session-row-id=${session.id}
                  aria-haspopup="menu"
                  aria-expanded=${this.#menuSessionId === session.id ? "true" : "false"}
                  @keydown=${(event: KeyboardEvent) => this.#sessionRowKeydown(event, session.id)}
                  @click=${() => this.#open(session)}
                >
                  ${!this.showStatus
                    ? html`<span class="session-indicator session-indicator-hidden" aria-hidden="true"></span>`
                    : html`<span
                        class="session-indicator ${indicator.kind}"
                        title=${indicator.tooltip === "" ? nothing : indicator.tooltip}
                        aria-hidden="true"
                      >${indicator.icon === undefined
                        ? nothing
                        : fontAwesomeIcon(indicator.icon)}</span>`}
                  <span class="session-copy">
                    <strong class=${titleWaiting ? "title-waiting" : nothing} title=${titleWaiting ? LOCAL_MODEL_WAITING_LABEL : nothing}>${titleShimmer
                      ? html`<span class="naming-title-shimmer session-title-shimmer" aria-hidden="true"></span><span class="visually-hidden">Naming session…</span>`
                      : session.title}</strong>
                    ${titleWaiting
                      ? html`<span class="visually-hidden" role="status">${SESSION_TITLE_WAITING_STATUS}</span>`
                      : nothing}
                    ${this.showBranches
                      ? html`<small class="session-branch" title=${session.branch}>${session.branch}</small>`
                      : nothing}
                    <span class="session-status-text visually-hidden">Status: ${sessionStatusText(session)}</span>
                  </span>
                  ${!this.showStatus || pullRequestBadge === undefined
                    ? nothing
                    : html`<span
                        class="session-pr-badge ${pullRequestBadge.tone}"
                        title=${pullRequestBadge.tooltip}
                        aria-label=${pullRequestBadge.tooltip.replaceAll("\n", ". ")}
                      >${fontAwesomeIcon("code-pull-request")}</span>`}
                  ${age === undefined
                    ? nothing
                    : html`<time
                        class="session-age"
                        datetime=${session.updatedAt}
                        title=${age.label}
                        aria-label=${age.label}
                      >${age.compact}</time>`}
                </button>
        </div>
        ${this.#menuSessionId === session.id && this.#editingSessionId === "" && this.#deleteSessionId === ""
          ? html`
              <div class="session-actions" role="menu" aria-label=${`Actions for ${session.title}`} style=${`left:${this.#menuPosition.x}px;top:${this.#menuPosition.y}px`} @contextmenu=${(event: Event) => event.preventDefault()} @keydown=${this.#contextMenuKeydown}>
                <button type="button" role="menuitem" tabindex="-1" @click=${() => this.#startRename(session)}>Rename</button>
                <button type="button" role="menuitem" tabindex="-1" @click=${() => void this.#copySessionId(session.id)}>Copy Session Id</button>
                <button type="button" role="menuitem" tabindex="-1" ?disabled=${this.#busySessionId === session.id} @click=${() => this.#setArchived(session.id, !session.archived)}>${session.archived ? "Unarchive" : "Archive"}</button>
                <button class="danger" type="button" role="menuitem" tabindex="-1" @click=${() => this.#confirmDelete(session)}>Delete…</button>
              </div>
            `
          : nothing}
      </li>
    `;
  }

  #toggleArchived(expanded: boolean): void {
    if (expanded) {
      this.#expandedArchivedWorkspaceIds.delete(this.workspaceId);
    } else {
      this.#expandedArchivedWorkspaceIds.add(this.workspaceId);
    }
    this.requestUpdate();
  }

  #toggleSection(sectionKey: string): void {
    if (!this.#collapsedSessionSections.delete(sectionKey)) {
      this.#collapsedSessionSections.add(sectionKey);
    }
    this.requestUpdate();
  }

  #open(session: {
    readonly id: string;
    readonly workspaceId: string;
    readonly latestThreadId: string | undefined;
  }): void {
    const store = this.#store.value;
    const services = this.#services.value;
    store?.markSessionRead(session.id);
    if (services === undefined) return;
    const threadId = preferredSessionThreadId(
      readSignal(services.resumePreferences),
      session.id,
      session.latestThreadId,
      store?.threadsForSession(session.id).map((thread) => thread.id) ?? [],
    );
    services.router.navigate({
      kind: "session",
      workspaceId: session.workspaceId,
      sessionId: session.id,
      ...(threadId === undefined ? {} : { threadId }),
    });
    this.dispatchEvent(
      new CustomEvent("trouve-session-open", { bubbles: true, composed: true }),
    );
    this.#closeActions();
  }

  #openContextMenu(event: MouseEvent | KeyboardEvent, sessionId: string): void {
    event.preventDefault();
    const target = event.currentTarget as HTMLElement;
    const bounds = target.getBoundingClientRect();
    const pointerEvent = event instanceof MouseEvent ? event : undefined;
    const pointerX = pointerEvent !== undefined && pointerEvent.clientX > 0
      ? pointerEvent.clientX
      : bounds.left + 12;
    const pointerY = pointerEvent !== undefined && pointerEvent.clientY > 0
      ? pointerEvent.clientY
      : bounds.bottom;
    this.#menuPosition = {
      x: Math.max(4, Math.min(pointerX, globalThis.innerWidth - 158)),
      y: Math.max(4, Math.min(pointerY, globalThis.innerHeight - 148)),
    };
    this.#menuSessionId = sessionId;
    this.#editingSessionId = "";
    this.#deleteSessionId = "";
    this.#requestError = "";
    this.requestUpdate();
    void this.updateComplete.then(() => {
      this.querySelector<HTMLButtonElement>(".session-actions [role='menuitem']")?.focus();
    });
  }

  #sessionRowKeydown(event: KeyboardEvent, sessionId: string): void {
    if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    this.#openContextMenu(event, sessionId);
  }

  #contextMenuKeydown(event: KeyboardEvent): void {
    const items = [...(event.currentTarget as HTMLElement)
      .querySelectorAll<HTMLButtonElement>("[role='menuitem']:not(:disabled)")];
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    let next: number | undefined;
    if (event.key === "ArrowDown") next = (current + 1) % items.length;
    else if (event.key === "ArrowUp") next = (current - 1 + items.length) % items.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = items.length - 1;
    else if (event.key === "Escape") {
      event.preventDefault();
      const sessionId = this.#menuSessionId;
      this.#menuSessionId = "";
      this.requestUpdate();
      void this.updateComplete.then(() => {
        this.querySelector<HTMLButtonElement>(`[data-session-row-id="${CSS.escape(sessionId)}"]`)
          ?.focus();
      });
      return;
    }
    if (next === undefined || items.length === 0) return;
    event.preventDefault();
    items[next]?.focus();
  }

  readonly #dismissPopupFromPointer = (event: PointerEvent): void => {
    if (this.#menuSessionId === "") return;
    const target = event.target;
    if (target instanceof Element && target.closest(".session-actions") !== null) {
      return;
    }
    this.#menuSessionId = "";
    this.requestUpdate();
  };

  #startRename(session: Pick<SessionListItem, "id" | "title">): void {
    this.#editingSessionId = session.id;
    this.#menuSessionId = "";
    this.#modalTitle = session.title;
    this.#requestError = "";
    this.requestUpdate();
  }

  readonly #closeActions = (): void => {
    this.#generationAbort?.abort();
    this.#generationAbort = undefined;
    this.#generatingSessionId = "";
    this.#menuSessionId = "";
    this.#editingSessionId = "";
    this.#deleteSessionId = "";
    this.#modalTitle = "";
    this.requestUpdate();
  };

  async #rename(event: SubmitEvent, sessionId: string): Promise<void> {
    event.preventDefault();
    const form = event.currentTarget as HTMLFormElement;
    const title = String(new FormData(form).get("title") ?? "").trim();
    if (title === "") return;
    await this.#updateSession(sessionId, { title }, "Session could not be renamed.");
  }

  async #generateRename(sessionId: string): Promise<void> {
    const services = this.#services.value;
    if (services === undefined || this.#generatingSessionId !== "") return;
    const startingTitle = this.#modalTitle;
    const abort = new AbortController();
    this.#generationAbort = abort;
    const timeout = globalThis.setTimeout(
      () => abort.abort(),
      titleGenerationTimeoutMs(),
    );
    this.#generatingSessionId = sessionId;
    this.#requestError = "";
    this.requestUpdate();
    try {
      const suggestion = await services.protocol.generateSessionTitleSuggestion(sessionId, {
        signal: abort.signal,
      });
      if (this.#editingSessionId !== sessionId || this.#modalTitle !== startingTitle) return;
      this.#modalTitle = suggestion.title.trim();
    } catch {
      if (this.#editingSessionId === sessionId) {
        this.#requestError = "A title could not be generated. You can still enter one manually.";
      }
    } finally {
      globalThis.clearTimeout(timeout);
      if (this.#generationAbort === abort) {
        this.#generationAbort = undefined;
        this.#generatingSessionId = "";
        this.requestUpdate();
      }
    }
  }

  async #setArchived(sessionId: string, archived: boolean): Promise<void> {
    await this.#updateSession(
      sessionId,
      { archived },
      archived ? "Session could not be archived." : "Session could not be restored.",
    );
  }

  async #copySessionId(sessionId: string): Promise<void> {
    this.#closeActions();
    const result = await copyChatText(sessionId, globalThis.navigator?.clipboard);
    if (result !== "copied") {
      this.#requestError = "Session id could not be copied.";
      this.requestUpdate();
    }
  }

  async #updateSession(
    sessionId: string,
    update: { readonly title?: string; readonly archived?: boolean },
    errorMessage: string,
  ): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) return;
    this.#busySessionId = sessionId;
    this.#requestError = "";
    this.requestUpdate();
    try {
      store.upsertSessionMetadata(await services.protocol.updateSession(sessionId, update));
      this.#closeActions();
    } catch {
      this.#requestError = errorMessage;
    } finally {
      this.#busySessionId = "";
      this.requestUpdate();
    }
  }

  #confirmDelete(session: Pick<SessionListItem, "id" | "title">): void {
    this.#deleteSessionId = session.id;
    this.#menuSessionId = "";
    this.#modalTitle = session.title;
    this.requestUpdate();
  }

  async #delete(sessionId: string): Promise<void> {
    const services = this.#services.value;
    const store = this.#store.value;
    if (services === undefined || store === undefined) return;
    this.#busySessionId = sessionId;
    this.#requestError = "";
    this.requestUpdate();
    try {
      await services.protocol.deleteSession(sessionId);
      services.tombstoneSession(sessionId);
      this.#closeActions();
    } catch {
      this.#requestError = "Session could not be deleted.";
    } finally {
      this.#busySessionId = "";
      this.requestUpdate();
    }
  }
}

customElements.define("trouve-session-list", TrouveSessionList);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-session-list": TrouveSessionList;
  }
}
