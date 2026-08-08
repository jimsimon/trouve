import type { TurnState } from "../state/thread-view-model.js";

export interface TurnCheckpointBoundary {
  readonly checkpointId: string;
  readonly turn: number;
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
