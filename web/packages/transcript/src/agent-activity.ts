import { html, LitElement, nothing, type PropertyValues } from "lit";

import {
  runningAgentActivity,
  type AgentActivityPresentation,
  type RunningAgentActivityInput,
} from "./agent-activity-model.js";

const ACTIVITY_CLOCK_INTERVAL_MS = 1_000;

export const liveAgentActivity = (
  input: RunningAgentActivityInput,
  nowMs = Date.now(),
): AgentActivityPresentation | undefined => runningAgentActivity({ ...input, nowMs });

export class TrouveAgentActivity extends LitElement {
  static override properties = {
    input: { attribute: false },
    presentation: { attribute: false },
    variant: { type: String },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  input: RunningAgentActivityInput | undefined;
  presentation: AgentActivityPresentation | undefined;
  variant: "row" | "transient" = "row";
  #tickTimer: ReturnType<typeof setInterval> | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    this.#syncClock();
  }

  override disconnectedCallback(): void {
    this.#stopClock();
    super.disconnectedCallback();
  }

  protected override updated(changed: PropertyValues<this>): void {
    if (changed.has("input")) this.#syncClock();
  }

  override render() {
    const activity = this.input === undefined
      ? this.presentation
      : liveAgentActivity(this.input) ?? this.presentation;
    if (activity === undefined) return nothing;
    const visual = this.variant === "transient"
      ? html`<div class="turn-transient-activity-copy" aria-hidden="true">
          <header class="turn-node-header"><strong>${activity.label}</strong></header>
        </div>`
      : html`<span class="agent-activity-copy" aria-hidden="true">
          <strong>${activity.label}</strong>
        </span>`;
    return html`
      ${visual}
      <span
        class="visually-hidden"
        role="status"
        aria-live="polite"
        aria-atomic="true"
      >${activity.announcementLabel}</span>
    `;
  }

  #syncClock(): void {
    if (!this.isConnected || this.input?.turnRunning !== true) {
      this.#stopClock();
      return;
    }
    this.#tickTimer ??= globalThis.setInterval(
      () => this.requestUpdate(),
      ACTIVITY_CLOCK_INTERVAL_MS,
    );
  }

  #stopClock(): void {
    if (this.#tickTimer !== undefined) globalThis.clearInterval(this.#tickTimer);
    this.#tickTimer = undefined;
  }
}

if (
  "customElements" in globalThis
  && customElements.get("trouve-agent-activity") === undefined
) {
  customElements.define("trouve-agent-activity", TrouveAgentActivity);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-agent-activity": TrouveAgentActivity;
  }
}
