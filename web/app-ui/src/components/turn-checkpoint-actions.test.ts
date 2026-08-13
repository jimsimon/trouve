import { describe, expect, it } from "vitest";

import type { TurnState } from "../state/thread-view-model.js";
import {
  CheckpointActionScope,
  checkpointBoundaryAfterTurn,
  checkpointBoundaryBeforeTurn,
} from "./turn-checkpoint-actions.js";

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

  it("exposes the latest completed checkpoint without needing a later turn", () => {
    const states = new Map<number, TurnState>([
      [7, { kind: "completed", usage, checkpointId: "cp_latest" }],
    ]);
    expect(checkpointBoundaryAfterTurn(7, states)).toEqual({
      checkpointId: "cp_latest",
      turn: 7,
    });
    expect(checkpointBoundaryAfterTurn(8, states)).toBeUndefined();
  });
});

describe("checkpoint action scope", () => {
  it("does not let an old session completion clear a new session action", () => {
    const scope = new CheckpointActionScope();
    const oldAction = scope.begin("restore:old");
    expect(oldAction).toBeDefined();

    scope.reset();
    const newAction = scope.begin("fork:new");
    expect(newAction).toBeDefined();
    expect(scope.action).toBe("fork:new");

    expect(scope.finish(oldAction!)).toBe(false);
    expect(scope.action).toBe("fork:new");
    expect(scope.finish(newAction!)).toBe(true);
    expect(scope.action).toBe("");
  });
});
