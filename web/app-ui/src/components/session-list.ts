import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing } from "lit";
import { repeat } from "lit/directives/repeat.js";

import { appServicesContext, appStoreContext } from "../contexts/app-contexts.js";
import type { SessionListItem } from "../state/app-store.js";
import { readSignal, withSignalTracking } from "../state/reactivity.js";
import {
  groupWorkspaceSessions,
  sessionAgePresentation,
  sessionStatusText,
} from "../state/session-inbox-model.js";
import { sessionIndicatorPresentation } from "../state/session-indicator-model.js";
import {
  visibleSessionPullRequestBadge,
} from "./session-pull-request-badge.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

let nextArchivedListId = 0;

/** A first real context consumer: gallery tests can provide an isolated store,
 * while application screens share the stable provider at the shell boundary. */
export class TrouveSessionList extends withSignalTracking(LitElement) {
  static override properties = {
    workspaceId: { type: String, attribute: "workspace-id" },
    showArchived: { type: Boolean, attribute: "show-archived" },
  };

  workspaceId = "";
  showArchived = false;
  #menuSessionId = "";
  #editingSessionId = "";
  #deleteSessionId = "";
  #modalTitle = "";
  #busySessionId = "";
  #requestError = "";
  readonly #expandedArchivedWorkspaceIds = new Set<string>();
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
    const groups = groupWorkspaceSessions(sessions, {
      workspaceId: this.workspaceId,
      selectedSessionId,
      archivedExpanded: this.#expandedArchivedWorkspaceIds.has(this.workspaceId),
    });
    if (groups.active.length === 0 && groups.archived.length === 0) {
      return html`<p class="context-placeholder">No sessions</p>`;
    }
    return html`
      ${groups.active.length === 0
        ? html`<p class="context-placeholder session-list-empty">No active sessions</p>`
        : html`
            <ol class="session-list active-session-list" aria-label="Active sessions">
              ${repeat(
                groups.active,
                (session) => session.id,
                (session) => this.#renderSession(session, selectedSessionId, now),
              )}
            </ol>
          `}
      ${groups.archived.length === 0 || !this.showArchived
        ? nothing
        : html`
            <section class="archived-session-group" aria-label="Archived sessions">
              <button
                type="button"
                class="archived-session-toggle"
                aria-expanded=${groups.archivedExpanded}
                aria-controls=${this.#archivedListId}
                @click=${() => this.#toggleArchived(groups.archivedExpanded)}
              >
                ${fontAwesomeIcon(groups.archivedExpanded ? "caret-down" : "caret-right", {
                  className: "archived-session-chevron",
                })}
                <span>Archived (${groups.archived.length})</span>
              </button>
              <ol
                id=${this.#archivedListId}
                class="session-list archived-session-list"
                ?hidden=${!groups.archivedExpanded}
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
                <input id=${`rename-${this.#editingSessionId}`} name="title" .value=${this.#modalTitle} maxlength="200" placeholder="Session title" required />
                ${this.#requestError === "" ? nothing : html`<p class="dialog-error" role="alert">${this.#requestError}</p>`}
                <footer>
                  <button data-session-modal-action="cancel" type="button" @click=${this.#closeActions}>Cancel</button>
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

  #renderSession(
    session: SessionListItem,
    selectedSessionId: string | undefined,
    now: number,
  ) {
    const selected = session.id === selectedSessionId;
    const store = this.#store.value;
    const pullRequestBadge = visibleSessionPullRequestBadge(
      store?.sessionPullRequests(session.id) ?? [],
      session.state,
      selected,
    );
    const indicator = sessionIndicatorPresentation(session);
    const age = sessionAgePresentation(session.updatedAt, now);
    return html`
      <li class="session-entry">
        <div
          class="session-row-wrap ${selected ? "selected" : ""}"
          data-actions-open=${this.#menuSessionId === session.id}
        >
                <button
                  type="button"
                  class="session-row ${selected ? "selected" : ""}"
                  aria-current=${selected ? "page" : "false"}
                  @click=${() => this.#open(session)}
                >
                  ${pullRequestBadge !== undefined
                    ? html`<span
                        class="session-pr-badge ${pullRequestBadge.tone}"
                        title=${pullRequestBadge.tooltip}
                        aria-label=${pullRequestBadge.tooltip.replaceAll("\n", ". ")}
                      >${fontAwesomeIcon("code-pull-request")}</span>`
                    : html`<span
                        class="session-indicator ${indicator.kind}"
                        title=${indicator.tooltip === "" ? nothing : indicator.tooltip}
                        aria-hidden="true"
                      >${indicator.icon === undefined
                        ? nothing
                        : fontAwesomeIcon(indicator.icon)}</span>`}
                  <span class="session-copy">
                    <strong>${session.title}</strong>
                    <span class="session-status-text visually-hidden">Status: ${sessionStatusText(session)}</span>
                  </span>
                  ${age === undefined
                    ? nothing
                    : html`<time
                        class="session-age"
                        datetime=${session.updatedAt}
                        title=${age.label}
                        aria-label=${age.label}
                      >${age.compact}</time>`}
                </button>
                <button
                  class="session-menu-button"
                  type="button"
                  aria-label=${`Actions for ${session.title}`}
                  aria-expanded=${this.#menuSessionId === session.id}
                  @click=${() => this.#toggleMenu(session.id)}
                >${fontAwesomeIcon("ellipsis")}</button>
        </div>
        ${this.#menuSessionId === session.id && this.#editingSessionId === "" && this.#deleteSessionId === ""
          ? html`
              <div class="session-actions" aria-label=${`Actions for ${session.title}`}>
                <button type="button" @click=${() => this.#startRename(session)}>Rename</button>
                <button type="button" ?disabled=${this.#busySessionId === session.id} @click=${() => this.#setArchived(session.id, !session.archived)}>${session.archived ? "Unarchive" : "Archive"}</button>
                <button class="danger" type="button" @click=${() => this.#confirmDelete(session)}>Delete…</button>
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

  #open(session: {
    readonly id: string;
    readonly workspaceId: string;
    readonly latestThreadId: string | undefined;
  }): void {
    this.#store.value?.markSessionRead(session.id);
    this.#services.value?.router.navigate({
      kind: "session",
      workspaceId: session.workspaceId,
      sessionId: session.id,
      ...(session.latestThreadId === undefined
        ? {}
        : { threadId: session.latestThreadId }),
    });
    this.dispatchEvent(
      new CustomEvent("trouve-session-open", { bubbles: true, composed: true }),
    );
    this.#closeActions();
  }

  #toggleMenu(sessionId: string): void {
    this.#menuSessionId = this.#menuSessionId === sessionId ? "" : sessionId;
    this.#editingSessionId = "";
    this.#deleteSessionId = "";
    this.#requestError = "";
    this.requestUpdate();
  }

  readonly #dismissPopupFromPointer = (event: PointerEvent): void => {
    if (this.#menuSessionId === "") return;
    const target = event.target;
    if (target instanceof Element && target.closest(".session-actions, .session-menu-button") !== null) {
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

  async #setArchived(sessionId: string, archived: boolean): Promise<void> {
    await this.#updateSession(
      sessionId,
      { archived },
      archived ? "Session could not be archived." : "Session could not be restored.",
    );
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
      store.removeSession(sessionId);
      const route = readSignal(services.router.route);
      if (route.kind === "session" && route.sessionId === sessionId) {
        services.router.navigate({ kind: "inbox" }, true);
      }
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
