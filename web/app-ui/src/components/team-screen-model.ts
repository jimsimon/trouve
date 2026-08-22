import type { ProtocolTeam } from "../services/protocol-client.js";

/** Keep a completed refresh from rolling the UI behind a newer snapshot. */
export const latestTeamSnapshot = (
  current: ProtocolTeam | undefined,
  incoming: ProtocolTeam,
): ProtocolTeam =>
  current !== undefined
    && (current.snapshot_cursor ?? 0) > (incoming.snapshot_cursor ?? 0)
    ? current
    : incoming;

/** Serialize snapshot refreshes while coalescing any burst that arrives
 * during one request into a single follow-up request. Resetting starts a new
 * lifecycle epoch so a detached element cannot block or clear its successor. */
export class TeamRefreshCoordinator {
  #epoch = 0;
  #pending = false;
  #dirty = false;

  reset(): void {
    this.#epoch += 1;
    this.#pending = false;
    this.#dirty = false;
  }

  request(refresh: () => Promise<void>): void {
    if (this.#pending) {
      this.#dirty = true;
      return;
    }
    this.#pending = true;
    void this.#drain(this.#epoch, refresh);
  }

  async #drain(epoch: number, refresh: () => Promise<void>): Promise<void> {
    do {
      this.#dirty = false;
      await refresh();
    } while (epoch === this.#epoch && this.#dirty);
    if (epoch === this.#epoch) this.#pending = false;
  }
}
