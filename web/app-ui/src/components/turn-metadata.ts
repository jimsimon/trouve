import { LitElement, nothing, type PropertyValues } from "lit";

import type { components as ProtocolComponents } from "../generated/protocol.js";
import { formatTurnDuration, formatTurnMetadata } from "./chat-presentation.js";

type Usage = ProtocolComponents["schemas"]["Usage"];

const TURN_CLOCK_INTERVAL_MS = 1_000;

export const liveTurnDurationMs = (
  startedAt: string,
  now = Date.now(),
): number | undefined => {
  const started = Date.parse(startedAt);
  if (!Number.isFinite(started) || !Number.isFinite(now)) return undefined;
  return Math.max(0, now - started);
};

export const turnMetadataText = (
  usage: Usage | undefined,
  durationMs: number | undefined,
): string => {
  if (usage !== undefined) return formatTurnMetadata(usage, durationMs);
  return durationMs === undefined ? "" : formatTurnDuration(durationMs);
};

export class TrouveTurnMetadata extends LitElement {
  static override properties = {
    usage: { attribute: false },
    running: { type: Boolean },
    startedAt: { type: String, attribute: "started-at" },
    durationMs: { attribute: false },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  usage: Usage | undefined;
  running = false;
  startedAt = "";
  durationMs: number | undefined;
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
    if (changed.has("running") || changed.has("startedAt")) this.#syncClock();
  }

  override render() {
    const durationMs = this.running
      ? liveTurnDurationMs(this.startedAt)
      : this.durationMs;
    const metadata = turnMetadataText(this.usage, durationMs);
    return metadata === "" ? nothing : metadata;
  }

  #syncClock(): void {
    if (
      !this.isConnected ||
      !this.running ||
      liveTurnDurationMs(this.startedAt) === undefined
    ) {
      this.#stopClock();
      return;
    }
    this.#tickTimer ??= globalThis.setInterval(
      () => this.requestUpdate(),
      TURN_CLOCK_INTERVAL_MS,
    );
  }

  #stopClock(): void {
    if (this.#tickTimer !== undefined) globalThis.clearInterval(this.#tickTimer);
    this.#tickTimer = undefined;
  }
}

if (
  "customElements" in globalThis &&
  customElements.get("trouve-turn-metadata") === undefined
) {
  customElements.define("trouve-turn-metadata", TrouveTurnMetadata);
}

declare global {
  interface HTMLElementTagNameMap {
    "trouve-turn-metadata": TrouveTurnMetadata;
  }
}
