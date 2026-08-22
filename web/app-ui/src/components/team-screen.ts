import { ContextConsumer } from "@lit/context";
import { css, html, LitElement, nothing, type PropertyValues } from "lit";
import { repeat } from "lit/directives/repeat.js";

import {
  appServicesContext,
  sessionContext,
  type AppServices,
} from "../contexts/app-contexts.js";
import type { CursorEventStream } from "../services/cursor-event-stream.js";
import type {
  ProtocolIngressEvent,
  ProtocolTeam,
  ProtocolTeamMember,
  ProtocolTeamStatus,
} from "../services/protocol-client.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";
import { latestTeamSnapshot, TeamRefreshCoordinator } from "./team-screen-model.js";

const TEAM_EVENT_PREFIX = "team.";
const TEAM_LOAD_RETRY_MS = 5_000;

const statusLabel = (status: ProtocolTeamStatus): string =>
  status[0]?.toUpperCase() + status.slice(1);

const memberUsage = (member: ProtocolTeamMember): string => {
  const input = member.usage?.input_tokens ?? 0;
  const cached = member.usage?.cached_input_tokens ?? 0;
  const output = member.usage?.output_tokens ?? 0;
  const total = input + cached + output;
  return total === 0 ? "No usage yet" : `${total.toLocaleString()} tokens`;
};

const messageTime = (value: string): string => {
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return value;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
};

export class TrouveTeamScreen extends LitElement {
  static override properties = {
    sessionId: { type: String, attribute: "session-id" },
  };

  static override styles = css`
    :host {
      display: block;
      min-width: 0;
      min-height: 0;
      height: 100%;
      overflow: hidden;
      color: var(--trouve-text);
      background: var(--trouve-win-bg);
      font: var(--trouve-font-size)/1.45 var(--trouve-font-sans);
    }
    *, *::before, *::after { box-sizing: border-box; }
    button, textarea { color: inherit; font: inherit; }
    button:not(:disabled) { cursor: pointer; }
    button:disabled, textarea:disabled { cursor: default; opacity: .58; }
    button:focus-visible, textarea:focus-visible {
      outline: 2px solid var(--trouve-accent);
      outline-offset: 2px;
    }
    h1, h2, h3, p { margin: 0; }
    .team-screen {
      height: 100%;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      overflow: hidden;
    }
    .team-header {
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: start;
      gap: 16px;
      border-bottom: 1px solid var(--trouve-rule);
      padding: 18px 20px;
      background: var(--trouve-surface);
    }
    .team-heading { min-width: 0; display: grid; gap: 6px; }
    .team-heading h1 {
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 17px;
      line-height: 1.25;
      text-overflow: ellipsis;
    }
    .team-meta, .member-meta {
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 6px;
      color: var(--trouve-text-dim);
      font-size: 11px;
    }
    .status, .member-state {
      width: fit-content;
      border-radius: 999px;
      padding: 2px 7px;
      background: var(--trouve-pill-bg);
      text-transform: capitalize;
    }
    .status.active, .member-state.running { color: var(--trouve-ok); }
    .status.paused, .member-state.queued { color: var(--trouve-warn); }
    .status.cancelled, .member-state.failed { color: var(--trouve-err); }
    .team-actions { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
    button {
      min-height: 30px;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius-sm);
      padding: 4px 10px;
      background: var(--trouve-control-bg);
    }
    button:hover:not(:disabled) { background: var(--trouve-hover-bg); }
    button.danger { color: var(--trouve-err); }
    .team-body {
      min-height: 0;
      display: grid;
      grid-template-columns: minmax(180px, 240px) minmax(0, 1fr);
      overflow: hidden;
    }
    .roster {
      min-height: 0;
      overflow: auto;
      border-right: 1px solid var(--trouve-rule);
      padding: 14px 12px;
      background: var(--trouve-surface);
    }
    .roster h2 {
      margin: 0 4px 10px;
      color: var(--trouve-text-dim);
      font-size: 10px;
      letter-spacing: .06em;
      text-transform: uppercase;
    }
    .member-list { display: grid; gap: 7px; margin: 0; padding: 0; list-style: none; }
    .member {
      display: grid;
      gap: 4px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      padding: 9px;
      background: var(--trouve-inset-bg);
    }
    .member.orchestrator { border-color: var(--trouve-accent); }
    .member-heading { min-width: 0; display: flex; align-items: baseline; gap: 5px; }
    .member-heading strong {
      overflow: hidden;
      color: var(--trouve-text-hi);
      font-size: 12px;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .member-heading code { color: var(--trouve-accent); font: 10px var(--trouve-font-mono); }
    .member-role {
      color: var(--trouve-text-mid);
      font-size: 10px;
      line-height: 1.35;
      overflow-wrap: anywhere;
    }
    .member-model {
      overflow: hidden;
      color: var(--trouve-text-dim);
      font: 9px var(--trouve-font-mono);
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .timeline-column {
      min-width: 0;
      min-height: 0;
      display: grid;
      grid-template-rows: minmax(0, 1fr) auto;
      overflow: hidden;
    }
    .timeline {
      min-height: 0;
      overflow: auto;
      padding: 20px clamp(14px, 4vw, 48px);
    }
    .message-list {
      width: min(860px, 100%);
      display: grid;
      gap: 12px;
      margin: 0 auto;
      padding: 0;
      list-style: none;
    }
    .timeline-note {
      width: min(860px, 100%);
      margin: 0 auto 12px;
      color: var(--trouve-text-dim);
      font-size: 10px;
      text-align: center;
    }
    .message {
      display: grid;
      gap: 6px;
      border: 1px solid var(--trouve-card-border);
      border-radius: var(--trouve-radius);
      padding: 11px 13px;
      background: var(--trouve-surface);
    }
    .message.human {
      border-color: color-mix(in srgb, var(--trouve-accent) 50%, var(--trouve-card-border));
    }
    .message.system { background: var(--trouve-inset-bg); }
    .message header { display: flex; flex-wrap: wrap; align-items: baseline; gap: 7px; }
    .message header strong { color: var(--trouve-text-hi); font-size: 12px; }
    .message header span { color: var(--trouve-text-dim); font-size: 9px; }
    .message p {
      color: var(--trouve-text);
      line-height: 1.55;
      overflow-wrap: anywhere;
      white-space: pre-wrap;
    }
    .mentions { display: flex; flex-wrap: wrap; gap: 4px; }
    .mention {
      border-radius: 999px;
      padding: 1px 6px;
      color: var(--trouve-accent);
      background: var(--trouve-pill-bg);
      font: 9px var(--trouve-font-mono);
    }
    .empty, .loading {
      height: 100%;
      display: grid;
      place-items: center;
      padding: 24px;
      color: var(--trouve-text-dim);
      text-align: center;
    }
    .composer {
      border-top: 1px solid var(--trouve-rule);
      padding: 12px clamp(14px, 4vw, 48px) 16px;
      background: var(--trouve-surface);
    }
    .composer form {
      width: min(860px, 100%);
      display: grid;
      grid-template-columns: minmax(0, 1fr) auto;
      align-items: end;
      gap: 8px;
      margin: 0 auto;
    }
    .composer textarea {
      width: 100%;
      min-height: 44px;
      max-height: 180px;
      resize: vertical;
      border: 1px solid var(--trouve-border-strong);
      border-radius: var(--trouve-radius);
      padding: 10px 11px;
      background: var(--trouve-input-bg);
      line-height: 1.4;
    }
    .composer button {
      min-width: 72px;
      min-height: 44px;
      border-color: var(--trouve-primary-border);
      color: var(--trouve-on-accent);
      background: var(--trouve-primary-bg);
    }
    .notice {
      width: min(860px, 100%);
      margin: 0 auto 7px;
      color: var(--trouve-err);
      font-size: 11px;
    }
    @media (max-width: 760px) {
      .team-header { grid-template-columns: 1fr; padding: 14px; }
      .team-actions { justify-content: flex-start; }
      .team-body { grid-template-columns: 1fr; grid-template-rows: auto minmax(0, 1fr); }
      .roster {
        max-height: 190px;
        border-right: 0;
        border-bottom: 1px solid var(--trouve-rule);
      }
      .member-list { grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); }
      button { min-height: 44px; }
    }
  `;

  sessionId = "";

  readonly #services = new ContextConsumer(this, {
    context: appServicesContext,
    subscribe: true,
  });
  readonly #sessionScope = new ContextConsumer(this, {
    context: sessionContext,
    subscribe: true,
  });

  #observedServices: AppServices | undefined;
  #observedSessionId = "";
  #generation = 0;
  #stream: CursorEventStream<ProtocolIngressEvent> | undefined;
  #team: ProtocolTeam | undefined;
  #loading = false;
  #pending = false;
  readonly #refreshes = new TeamRefreshCoordinator();
  #draft = "";
  #draftIdempotencyKey = "";
  #error = "";
  #loadRetryTimer: ReturnType<typeof setTimeout> | undefined;

  get #effectiveSessionId(): string {
    return this.sessionId || this.#sessionScope.value?.sessionId || "";
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.requestUpdate();
  }

  override disconnectedCallback(): void {
    this.#generation += 1;
    this.#stream?.close();
    this.#stream = undefined;
    this.#observedServices = undefined;
    this.#clearLoadRetry();
    super.disconnectedCallback();
  }

  protected override updated(_changed: PropertyValues<this>): void {
    const services = this.#services.value;
    const sessionId = this.#effectiveSessionId;
    if (
      services === this.#observedServices
      && sessionId === this.#observedSessionId
    ) return;
    this.#observedServices = services;
    this.#observedSessionId = sessionId;
    void this.#open(services, sessionId);
  }

  override render() {
    const team = this.#team;
    if (this.#loading && team === undefined) {
      return html`<div class="loading" role="status">Loading team…</div>`;
    }
    if (team === undefined) {
      return html`<div class="empty" role="alert">
        <div><strong>Team unavailable</strong><p>${this.#error || "This team could not be loaded."}</p></div>
      </div>`;
    }
    const terminal = team.status === "completed" || team.status === "cancelled";
    return html`
      <section class="team-screen" aria-label="Team session">
        <header class="team-header">
          <div class="team-heading">
            <h1>${team.goal}</h1>
            <div class="team-meta">
              <span class=${`status ${team.status}`}>${statusLabel(team.status)}</span>
              <span>${team.members.length} teammates</span>
              <span>${team.turns_used.toLocaleString()} / ${team.max_turns.toLocaleString()} automatic turns</span>
            </div>
          </div>
          <div class="team-actions" aria-label="Team controls">
            ${team.status === "active"
              ? html`<button type="button" ?disabled=${this.#pending} @click=${() => void this.#changeStatus("pause")}>Pause</button>`
              : team.status === "paused"
                ? html`<button type="button" ?disabled=${this.#pending} @click=${() => void this.#changeStatus("resume")}>Resume</button>`
                : nothing}
            <button type="button" ?disabled=${this.#pending || terminal} @click=${() => void this.#changeStatus("complete")}>Complete</button>
            <button class="danger" type="button" ?disabled=${this.#pending || terminal} @click=${() => void this.#changeStatus("cancel")}>Cancel</button>
          </div>
        </header>
        <div class="team-body">
          <aside class="roster" aria-label="Team roster">
            <h2>Roster</h2>
            <ul class="member-list">
              ${repeat(
                team.members,
                (member) => member.id,
                (member) => html`
                  <li class=${`member ${member.id === team.orchestrator_member_id ? "orchestrator" : ""}`}>
                    <div class="member-heading">
                      <strong>${member.display_name}</strong>
                      <code>@${member.handle}</code>
                    </div>
                    <p class="member-role">${member.role}</p>
                    <div class="member-meta">
                      <span class=${`member-state ${member.state}`}>${member.state}</span>
                      <span>${memberUsage(member)}</span>
                    </div>
                    <span class="member-model" title=${member.model}>${member.mode} · ${member.model}</span>
                  </li>
                `,
              )}
            </ul>
          </aside>
          <div class="timeline-column">
            <div class="timeline" aria-label="Shared team timeline" aria-live="polite">
              ${team.messages_truncated === true
                ? html`<p class="timeline-note" role="status">Showing the most recent team messages.</p>`
                : nothing}
              ${team.messages.length === 0
                ? html`<div class="empty">The shared timeline is waiting for its first update.</div>`
                : html`<ol class="message-list">
                    ${repeat(
                      team.messages,
                      (message) => message.id,
                      (message) => html`
                        <li class=${`message ${message.author_kind}`}>
                          <header>
                            <strong>${message.author_handle === "" ? "System" : `@${message.author_handle}`}</strong>
                            <span>${messageTime(message.created_at)}</span>
                          </header>
                          <p>${message.content}</p>
                          ${(message.mentions?.length ?? 0) === 0
                            ? nothing
                            : html`<div class="mentions" aria-label="Mentions">
                                ${message.mentions?.map((mention) =>
                                  html`<span class="mention">@${mention.handle}</span>`
                                )}
                              </div>`}
                        </li>
                      `,
                    )}
                  </ol>`}
            </div>
            <div class="composer">
              ${this.#error === "" ? nothing : html`<p class="notice" role="alert">${this.#error}</p>`}
              <form @submit=${this.#sendMessage}>
                <textarea
                  name="message"
                  aria-label="Team message"
                  rows="2"
                  maxlength="100000"
                  autocomplete="off"
                  placeholder="Message the team. Use @handle to direct a teammate."
                  .value=${this.#draft}
                  ?disabled=${this.#pending || terminal}
                  @input=${(event: Event) => {
                    this.#draft = (event.currentTarget as HTMLTextAreaElement).value;
                    this.#draftIdempotencyKey = "";
                    this.requestUpdate();
                  }}
                ></textarea>
                <button type="submit" ?disabled=${this.#pending || terminal || this.#draft.trim() === ""}>
                  ${this.#pending ? "Sending…" : html`${fontAwesomeIcon("message")} Send`}
                </button>
              </form>
            </div>
          </div>
        </div>
      </section>
    `;
  }

  async #open(services: AppServices | undefined, sessionId: string): Promise<void> {
    this.#clearLoadRetry();
    const generation = ++this.#generation;
    this.#refreshes.reset();
    this.#stream?.close();
    this.#stream = undefined;
    this.#team = undefined;
    this.#draftIdempotencyKey = "";
    this.#error = "";
    this.#loading = services !== undefined && sessionId !== "";
    this.requestUpdate();
    if (services === undefined || sessionId === "") return;
    try {
      const team = await services.protocol.team(sessionId);
      if (!this.#isCurrent(generation, services, sessionId)) return;
      this.#team = latestTeamSnapshot(this.#team, team);
      this.#loading = false;
      this.requestUpdate();
      const stream = await services.protocol.sessionEvents(sessionId, {
        after: team.snapshot_cursor ?? 0,
        onEvent: (event) => this.#receiveTeamEvent(event, generation),
        onDiagnostic: () => this.#scheduleRefresh(generation),
      });
      if (!this.#isCurrent(generation, services, sessionId)) {
        stream.close();
        return;
      }
      this.#stream = stream;
      stream.start();
    } catch {
      if (!this.#isCurrent(generation, services, sessionId)) return;
      this.#loading = false;
      this.#error = "This team could not be loaded. Retrying automatically.";
      this.#scheduleLoadRetry();
      this.requestUpdate();
    }
  }

  #receiveTeamEvent(event: ProtocolIngressEvent, generation: number): void {
    const type = event.kind === "known" ? event.envelope.type : event.type;
    if (type.startsWith(TEAM_EVENT_PREFIX)) this.#scheduleRefresh(generation);
  }

  #scheduleRefresh(generation: number): void {
    if (generation !== this.#generation) return;
    this.#refreshes.request(() => this.#refresh(generation));
  }

  async #refresh(generation = this.#generation): Promise<void> {
    const services = this.#observedServices;
    const sessionId = this.#observedSessionId;
    if (services === undefined || sessionId === "" || generation !== this.#generation) return;
    try {
      const team = await services.protocol.team(sessionId);
      if (!this.#isCurrent(generation, services, sessionId)) return;
      this.#team = latestTeamSnapshot(this.#team, team);
      this.#error = "";
      this.requestUpdate();
    } catch {
      if (!this.#isCurrent(generation, services, sessionId)) return;
      this.#error = "The latest team update could not be loaded.";
      this.requestUpdate();
    }
  }

  readonly #sendMessage = async (event: SubmitEvent): Promise<void> => {
    event.preventDefault();
    const services = this.#observedServices;
    const sessionId = this.#observedSessionId;
    const content = this.#draft.trim();
    if (services === undefined || sessionId === "" || content === "" || this.#pending) return;
    const generation = this.#generation;
    this.#pending = true;
    this.#draftIdempotencyKey ||= globalThis.crypto.randomUUID();
    const idempotencyKey = this.#draftIdempotencyKey;
    this.#error = "";
    this.requestUpdate();
    try {
      const message = await services.protocol.postTeamMessage(
        sessionId,
        content,
        idempotencyKey,
      );
      if (!this.#isCurrent(generation, services, sessionId)) return;
      const team = this.#team;
      if (team !== undefined && !team.messages.some((candidate) => candidate.id === message.id)) {
        this.#team = { ...team, messages: [...team.messages, message] };
      }
      this.#draft = "";
      this.#draftIdempotencyKey = "";
    } catch {
      if (this.#isCurrent(generation, services, sessionId)) {
        this.#error = "The message could not be sent.";
      }
    } finally {
      if (this.#isCurrent(generation, services, sessionId)) {
        this.#pending = false;
        this.requestUpdate();
      }
    }
  };

  async #changeStatus(
    action: "pause" | "resume" | "complete" | "cancel",
  ): Promise<void> {
    const services = this.#observedServices;
    const sessionId = this.#observedSessionId;
    if (services === undefined || sessionId === "" || this.#pending) return;
    const generation = this.#generation;
    this.#pending = true;
    this.#error = "";
    this.requestUpdate();
    try {
      const team = await services.protocol.setTeamStatus(sessionId, action);
      if (this.#isCurrent(generation, services, sessionId)) {
        this.#team = latestTeamSnapshot(this.#team, team);
      }
    } catch {
      if (this.#isCurrent(generation, services, sessionId)) {
        this.#error = `The team could not be ${action === "pause" ? "paused" : action === "resume" ? "resumed" : action === "complete" ? "completed" : "cancelled"}.`;
      }
    } finally {
      if (this.#isCurrent(generation, services, sessionId)) {
        this.#pending = false;
        this.requestUpdate();
      }
    }
  }

  #scheduleLoadRetry(): void {
    if (!this.isConnected || this.#loadRetryTimer !== undefined) return;
    this.#loadRetryTimer = globalThis.setTimeout(() => {
      this.#loadRetryTimer = undefined;
      if (typeof document !== "undefined" && document.visibilityState === "hidden") {
        this.#scheduleLoadRetry();
        return;
      }
      void this.#open(this.#observedServices, this.#observedSessionId);
    }, TEAM_LOAD_RETRY_MS);
  }

  #clearLoadRetry(): void {
    if (this.#loadRetryTimer === undefined) return;
    globalThis.clearTimeout(this.#loadRetryTimer);
    this.#loadRetryTimer = undefined;
  }

  #isCurrent(generation: number, services: AppServices, sessionId: string): boolean {
    return this.isConnected
      && generation === this.#generation
      && services === this.#observedServices
      && sessionId === this.#observedSessionId;
  }
}

customElements.define("trouve-team-screen", TrouveTeamScreen);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-team-screen": TrouveTeamScreen;
  }
}
