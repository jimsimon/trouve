import { html, LitElement, nothing, type PropertyValues } from "lit";
import { repeat } from "lit/directives/repeat.js";

import type { ChatPreferences } from "./chat-preferences.js";
import type { SafeStreamDiagnostic } from "@trouve-ai/protocol/event-stream";
import type {
  ProtocolCursorSnapshot,
  ProtocolIngressEvent,
  ProtocolModelInfo,
  ProtocolThreadToolDetails,
  ProtocolThreadViewSnapshot,
} from "@trouve-ai/protocol/client";
import { ThreadViewModel } from "./thread-view-model.js";
import {
  runningAgentActivity,
  type RunningAgentActivityInput,
} from "./agent-activity-model.js";
import { buildChatLayout } from "./chat-layout.js";
import { indexChatPresentation } from "./chat-presentation.js";
import {
  agentTurnLabels,
  TranscriptRenderer,
  turnPhaseLabel,
  type TranscriptSubagentItem,
} from "./transcript-renderer.js";
import { TurnCompletionAnnouncer } from "./turn-announcements.js";

/**
 * A started-on-demand event subscription. `CursorEventStream` satisfies it;
 * so does a static client that has nothing further to deliver.
 */
export interface TranscriptEventStream {
  start(): void;
  close(): void;
}

/**
 * The protocol surface a read-only transcript needs. `ProtocolClient`
 * satisfies it structurally, as does any client that proxies the same thread
 * endpoints from a remote trouve server.
 */
export interface TranscriptClient {
  threadView(
    threadId: string,
    before?: number,
    options?: { readonly signal?: AbortSignal },
  ): Promise<ProtocolCursorSnapshot<ProtocolThreadViewSnapshot>>;
  threadEvents(
    threadId: string,
    options: {
      readonly after: number;
      readonly onEvent: (event: ProtocolIngressEvent) => void;
      readonly onOpen?: () => void;
      readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
    },
  ): Promise<TranscriptEventStream>;
  threadToolDetails(threadId: string, callId: string): Promise<ProtocolThreadToolDetails>;
}

export type TranscriptViewState = "idle" | "loading" | "open" | "error";

/** Detail of the `trouve-transcript-state` event fired on every state change. */
export interface TranscriptStateDetail {
  readonly threadId: string;
  readonly state: TranscriptViewState;
}

export interface TranscriptOpenSubagentDetail {
  readonly sessionId: string;
  readonly threadId: string;
}

const TAIL_TOLERANCE_PX = 32;

/**
 * Read-only rendering of one thread transcript: seeds from the thread view
 * snapshot, follows the thread event stream, pages older history on demand,
 * and draws items with the same markup the desktop thread screen uses.
 */
export class TrouveTranscriptView extends LitElement {
  static override properties = {
    threadId: { type: String, attribute: "thread-id" },
    client: { attribute: false },
    models: { attribute: false },
    chatPreferences: { attribute: false },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  threadId = "";
  client: TranscriptClient | undefined;
  models: readonly ProtocolModelInfo[] = [];
  chatPreferences: ChatPreferences | undefined;

  #view = new ThreadViewModel();
  #state: TranscriptViewState = "idle";
  #error = "";
  #generation = 0;
  #openedFor: { readonly client: TranscriptClient | undefined; readonly threadId: string } | undefined;
  #stream: TranscriptEventStream | undefined;
  #historyLoading = false;
  #historyError = "";
  #historyObserver: IntersectionObserver | undefined;
  #observedSentinel: Element | undefined;
  #followTail = true;
  #pendingScrollRestore: { readonly height: number; readonly top: number } | undefined;
  readonly #transcript = this.#createTranscriptRenderer();
  readonly #announcer = new TurnCompletionAnnouncer();

  /** Connection state of the live transcript. */
  get state(): TranscriptViewState {
    return this.#state;
  }

  #setState(state: TranscriptViewState): void {
    if (this.#state === state) return;
    this.#state = state;
    this.dispatchEvent(new CustomEvent<TranscriptStateDetail>("trouve-transcript-state", {
      detail: { threadId: this.threadId, state },
      bubbles: true,
      composed: true,
    }));
  }

  #createTranscriptRenderer(): TranscriptRenderer {
    const view = this;
    return new TranscriptRenderer({
      element: this,
      get threadId() {
        return view.threadId;
      },
      requestUpdate: () => this.requestUpdate(),
      requestDisclosureUpdate: () => this.requestUpdate(),
      chatPreferences: () => this.chatPreferences,
      availableModels: () => this.models,
      interactionGeneration: () => this.#generation,
      isCurrentInteraction: (threadId, generation) => this.#isCurrent(threadId, generation),
      findTool: (callId) => this.#view.findTool(callId),
      loadToolDetails: async (threadId, callId, generation) => {
        const client = this.client;
        if (client === undefined) return;
        const details = await client.threadToolDetails(threadId, callId);
        if (!this.#isCurrent(threadId, generation)) return;
        if (!this.#view.replaceToolDetails(details)) {
          throw new Error("tool detail no longer belongs to this thread view");
        }
      },
      openSubagent: (item) => this.#openSubagent(item),
    });
  }

  override connectedCallback(): void {
    super.connectedCallback();
    // A reconnected element re-seeds and re-subscribes on its next update.
    this.#openedFor = undefined;
    this.requestUpdate();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    this.#generation += 1;
    this.#closeStream();
    this.#disconnectHistoryObserver();
    this.#transcript.invalidateCopyFeedback();
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    super.willUpdate(changed);
    const opened = this.#openedFor;
    if (
      opened !== undefined
      && opened.client === this.client
      && opened.threadId === this.threadId
    ) return;
    this.#openedFor = { client: this.client, threadId: this.threadId };
    void this.#open();
  }

  protected override updated(): void {
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport === null) return;
    const restore = this.#pendingScrollRestore;
    if (restore !== undefined) {
      this.#pendingScrollRestore = undefined;
      viewport.scrollTop = restore.top + (viewport.scrollHeight - restore.height);
    } else if (this.#followTail) {
      viewport.scrollTop = viewport.scrollHeight;
    }
    this.#syncHistoryObserver(viewport);
  }

  async #open(): Promise<void> {
    this.#generation += 1;
    const generation = this.#generation;
    this.#closeStream();
    this.#disconnectHistoryObserver();
    this.#transcript.reset();
    this.#view = new ThreadViewModel();
    this.#historyLoading = false;
    this.#historyError = "";
    this.#followTail = true;
    this.#pendingScrollRestore = undefined;
    const client = this.client;
    const threadId = this.threadId;
    if (client === undefined || threadId === "") {
      this.#setState("idle");
      this.#error = "";
      this.requestUpdate();
      return;
    }
    this.#setState("loading");
    this.#error = "";
    this.requestUpdate();
    try {
      const snapshot = await client.threadView(threadId);
      if (!this.#isCurrent(threadId, generation)) return;
      this.#view.replaceSnapshot(snapshot.cursor, snapshot.value);
      this.requestUpdate();
      const stream = await client.threadEvents(threadId, {
        after: this.#view.cursor,
        onEvent: (event) => {
          if (!this.#isCurrent(threadId, generation) || event.kind === "unknown") return;
          if (this.#view.apply(event.envelope)) this.requestUpdate();
        },
        onOpen: () => {
          if (!this.#isCurrent(threadId, generation)) return;
          this.#setState("open");
          this.requestUpdate();
        },
      });
      if (!this.#isCurrent(threadId, generation)) {
        stream.close();
        return;
      }
      this.#stream = stream;
      stream.start();
    } catch {
      if (!this.#isCurrent(threadId, generation)) return;
      this.#error = "The transcript could not be loaded.";
      this.#setState("error");
      this.requestUpdate();
    }
  }

  #isCurrent(threadId: string, generation: number): boolean {
    return this.isConnected && this.threadId === threadId && this.#generation === generation;
  }

  #closeStream(): void {
    this.#stream?.close();
    this.#stream = undefined;
  }

  async #loadOlderHistory(): Promise<void> {
    const client = this.client;
    const threadId = this.threadId;
    const generation = this.#generation;
    if (client === undefined || threadId === "" || this.#historyLoading) return;
    if (!this.#view.hasOlder || this.#view.itemOffset === 0) return;
    this.#historyLoading = true;
    this.#historyError = "";
    this.#disconnectHistoryObserver();
    this.requestUpdate();
    try {
      const page = await client.threadView(threadId, this.#view.itemOffset);
      if (!this.#isCurrent(threadId, generation)) return;
      const viewport = this.querySelector<HTMLElement>(".chat-stream");
      if (viewport !== null && !this.#followTail) {
        this.#pendingScrollRestore = {
          height: viewport.scrollHeight,
          top: viewport.scrollTop,
        };
      }
      if (!this.#view.prependSnapshot(page.value)) {
        this.#pendingScrollRestore = undefined;
        throw new Error("non-contiguous thread history page");
      }
    } catch {
      if (this.#isCurrent(threadId, generation)) {
        this.#historyError = "Earlier messages could not be loaded.";
      }
    } finally {
      if (this.#isCurrent(threadId, generation)) {
        this.#historyLoading = false;
        this.requestUpdate();
      }
    }
  }

  #syncHistoryObserver(viewport: HTMLElement): void {
    const sentinel = viewport.querySelector(".chat-history-sentinel");
    if (sentinel === null || this.#historyLoading || this.#historyError !== "") {
      this.#disconnectHistoryObserver();
      return;
    }
    if (this.#observedSentinel === sentinel) return;
    this.#disconnectHistoryObserver();
    if (typeof IntersectionObserver === "undefined") return;
    this.#historyObserver = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) void this.#loadOlderHistory();
      },
      { root: viewport, rootMargin: "200% 0px 0px 0px" },
    );
    this.#historyObserver.observe(sentinel);
    this.#observedSentinel = sentinel;
  }

  #disconnectHistoryObserver(): void {
    this.#historyObserver?.disconnect();
    this.#historyObserver = undefined;
    this.#observedSentinel = undefined;
  }

  #openSubagent(item: TranscriptSubagentItem): void {
    this.dispatchEvent(new CustomEvent<TranscriptOpenSubagentDetail>("trouve-open-subagent", {
      detail: { sessionId: item.sessionId, threadId: item.threadId },
      bubbles: true,
      composed: true,
    }));
  }

  readonly #scrolled = (event: Event): void => {
    const viewport = event.currentTarget;
    if (!(viewport instanceof HTMLElement)) return;
    const gap = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight;
    const following = gap <= TAIL_TOLERANCE_PX;
    if (following !== this.#followTail) {
      this.#followTail = following;
      this.requestUpdate();
    }
  };

  readonly #jumpToLatest = (): void => {
    this.#followTail = true;
    const viewport = this.querySelector<HTMLElement>(".chat-stream");
    if (viewport !== null) viewport.scrollTop = viewport.scrollHeight;
    this.requestUpdate();
  };

  readonly #retryHistory = (): void => {
    this.#historyError = "";
    void this.#loadOlderHistory();
  };

  override render() {
    if (this.#state === "error") {
      return html`<div class="chat-scroll-shell">
        <div class="chat-stream" role="log" aria-label="Conversation">
          <p class="transcript-status" role="alert">${this.#error}</p>
        </div>
      </div>`;
    }
    const view = this.#view;
    const items = view.items;
    const presentation = indexChatPresentation(items);
    const layout = buildChatLayout(items);
    const turnLabels = agentTurnLabels(view.turnModels, view.turnThinkingLevels);
    let activeTurn: number | undefined;
    for (const [turn, state] of presentation.turnStates) {
      if (
        (state.kind === "waiting-for-capacity" || state.kind === "running")
        && (activeTurn === undefined || turn > activeTurn)
      ) {
        activeTurn = turn;
      }
    }
    const activityInput: RunningAgentActivityInput = {
      items,
      turnRunning: view.turnRunning,
      thinking: view.thinking,
      compacting: view.compacting,
      turnModels: view.turnModels,
      turnStartedAt: view.turnStartedAt,
      nowMs: Date.now(),
    };
    const activityLabel = turnPhaseLabel(view.turnPhase);
    const liveActivityInput = activityLabel === undefined ? activityInput : undefined;
    const activityPresentation = activityLabel === undefined
      ? runningAgentActivity(activityInput)
      : { label: activityLabel, announcementLabel: activityLabel };
    let nestedActivityUnitId: string | undefined;
    if (activityPresentation !== undefined && activeTurn !== undefined) {
      for (let index = layout.units.length - 1; index >= 0; index -= 1) {
        const unit = layout.units[index];
        if (
          unit?.kind === "turn"
          && unit.turn === activeTurn
          && this.#transcript.isTurnCardOpen(unit)
        ) {
          nestedActivityUnitId = unit.id;
          break;
        }
      }
    }
    const hasRunningCompaction = items.some(
      (item) => item.kind === "compaction" && item.state.kind === "running",
    );
    const loading = this.#state === "loading" && !view.snapshotLoaded;
    const completionAnnouncement = this.#announcer.observe(
      this.threadId,
      presentation.turnStates,
    );
    return html`
      <div class="chat-scroll-shell">
        <div
          class="chat-stream"
          data-thread-id=${this.threadId}
          role="log"
          aria-label="Conversation"
          aria-live="off"
          aria-busy=${view.turnRunning || view.compacting || loading}
          @scroll=${this.#scrolled}
        >
          ${loading
            ? html`<p class="transcript-status" role="status">Loading transcript…</p>`
            : nothing}
          <div class="chat-virtual-canvas transcript-flow">
            ${view.hasOlder
              ? html`<span class="chat-history-sentinel" aria-hidden="true"></span>`
              : nothing}
            ${layout.units.length === 0
              ? nothing
              : html`<div class="chat-edge-spacer start" aria-hidden="true"></div>`}
            ${repeat(layout.units, (unit) => unit.id, (unit, unitIndex) => html`<div>${
              this.#transcript.renderUnit(
                unit,
                undefined,
                turnLabels,
                view.turnModels,
                view.turnDurationMs,
                presentation,
                unit.id === nestedActivityUnitId ? activityPresentation : undefined,
                unit.id === nestedActivityUnitId ? liveActivityInput : undefined,
                view.turnRunning,
                unitIndex === layout.units.length - 1,
              )
            }</div>`)}
            ${view.compacting && !hasRunningCompaction
              ? this.#transcript.renderCompactionMarker({ kind: "running" })
              : nothing}
            ${activityPresentation !== undefined && nestedActivityUnitId === undefined
              ? this.#transcript.renderActivityRow(activityPresentation, liveActivityInput)
              : nothing}
          </div>
          ${!this.#followTail && layout.units.length > 0
            ? html`<button class="follow-tail" type="button" @click=${this.#jumpToLatest}>Jump to latest</button>`
            : nothing}
        </div>
        <span
          class="visually-hidden transcript-announcement"
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >${completionAnnouncement}</span>
        ${this.#historyLoading || this.#historyError !== ""
          ? html`<div class="chat-history-status" role="status">
              ${this.#historyError === ""
                ? "Loading earlier messages…"
                : html`${this.#historyError}
                  <button type="button" @click=${this.#retryHistory}>Retry</button>`}
            </div>`
          : nothing}
      </div>
    `;
  }
}

customElements.define("trouve-transcript-view", TrouveTranscriptView);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-transcript-view": TrouveTranscriptView;
  }
  interface HTMLElementEventMap {
    "trouve-transcript-state": CustomEvent<TranscriptStateDetail>;
  }
}
