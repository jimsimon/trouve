import type { TurnState } from "./thread-view-model.js";

/**
 * Concise announcements for the transcript's dedicated live region.
 *
 * The transcript log itself is not live: streaming Markdown, tool output, and
 * progress text mutate constantly, and announcing every mutation (or, with
 * `aria-busy`, one large accumulated announcement at the end) makes the
 * conversation impossible to follow. Instead the running activity label has
 * its own status region (`trouve-agent-activity`) and this class publishes a
 * single message when a turn the reader watched finish completes. Failed and
 * cancelled turns are not repeated here; their terminal markers already carry
 * `role="alert"` / `role="status"`.
 *
 * A live region announces when its text changes, so the message is cleared
 * whenever a new turn starts. Loading a thread whose turns are already
 * finished announces nothing.
 */
export class TurnCompletionAnnouncer {
  #threadId: string | undefined;
  readonly #running = new Set<number>();
  #message = "";

  /** Observe the current turn states and return the text to publish. */
  observe(threadId: string, turnStates: ReadonlyMap<number, TurnState>): string {
    if (threadId !== this.#threadId) {
      this.#threadId = threadId;
      this.#running.clear();
      this.#message = "";
    }
    for (const [turn, state] of turnStates) {
      if (state.kind === "running" || state.kind === "waiting-for-capacity") {
        if (!this.#running.has(turn)) {
          this.#running.add(turn);
          this.#message = "";
        }
      } else if (this.#running.delete(turn)) {
        this.#message = state.kind === "completed" ? turnCompletedAnnouncement(turn) : "";
      }
    }
    return this.#message;
  }
}

export const turnCompletedAnnouncement = (turn: number): string => `Turn ${turn} complete`;
