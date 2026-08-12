import type { TurnState } from "../state/thread-view-model.js";

export interface TurnCheckpointBoundary {
  readonly checkpointId: string;
  readonly turn: number;
}

export interface CheckpointActionToken {
  readonly generation: number;
  readonly action: string;
}

/** View-scoped ownership for asynchronous checkpoint actions. Resetting the
 * scope invalidates every completion captured by the previous session or
 * thread, so an old `finally` cannot clear a newer view's busy state. */
export class CheckpointActionScope {
  #generation = 0;
  #action = "";

  get action(): string {
    return this.#action;
  }

  begin(action: string): CheckpointActionToken | undefined {
    if (action === "" || this.#action !== "") return undefined;
    this.#generation += 1;
    this.#action = action;
    return Object.freeze({ generation: this.#generation, action });
  }

  reset(): void {
    this.#generation += 1;
    this.#action = "";
  }

  isCurrent(token: CheckpointActionToken): boolean {
    return token.generation === this.#generation && token.action === this.#action;
  }

  finish(token: CheckpointActionToken): boolean {
    if (!this.isCurrent(token)) return false;
    this.#action = "";
    return true;
  }
}

/** Find the nearest completed turn before a prompt. Cancelled/failed turns
 * and retained snapshots from older servers do not expose an actionable
 * checkpoint and are skipped. */
export const checkpointBoundaryBeforeTurn = (
  turn: number,
  turnStates: ReadonlyMap<number, TurnState>,
): TurnCheckpointBoundary | undefined => {
  let boundary: TurnCheckpointBoundary | undefined;
  for (const [candidateTurn, state] of turnStates) {
    if (
      candidateTurn >= turn
      || state.kind !== "completed"
      || state.checkpointId === undefined
      || (boundary !== undefined && candidateTurn <= boundary.turn)
    ) continue;
    boundary = { checkpointId: state.checkpointId, turn: candidateTurn };
  }
  return boundary;
};

/** Return the checkpoint owned by one completed turn. This is used for the
 * trailing boundary: unlike inter-turn rules, there is no later turn whose
 * number can be used to discover the final checkpoint. */
export const checkpointBoundaryAfterTurn = (
  turn: number,
  turnStates: ReadonlyMap<number, TurnState>,
): TurnCheckpointBoundary | undefined => {
  const state = turnStates.get(turn);
  return state?.kind === "completed" && state.checkpointId !== undefined
    ? { checkpointId: state.checkpointId, turn }
    : undefined;
};
