import { describe, expect, it } from "vitest";

import type { TurnState } from "../state/thread-view-model.js";
import { checkpointBoundaryBeforeTurn } from "./turn-checkpoint-actions.js";

const usage = { input_tokens: 10, output_tokens: 2 };

describe("turn checkpoint boundaries", () => {
  it("selects the nearest completed checkpoint before the next prompt", () => {
    const states = new Map<number, TurnState>([
      [1, { kind: "completed", usage, checkpointId: "cp_1" }],
      [2, { kind: "failed", error: "stopped" }],
      [3, { kind: "completed", usage, checkpointId: "cp_3" }],
      [4, { kind: "running" }],
    ]);

    expect(checkpointBoundaryBeforeTurn(4, states)).toEqual({
      checkpointId: "cp_3",
      turn: 3,
    });
  });

  it("hides actions for legacy completed turns without checkpoint ids", () => {
    expect(checkpointBoundaryBeforeTurn(2, new Map([
      [1, { kind: "completed", usage } satisfies TurnState],
    ]))).toBeUndefined();
  });
});
