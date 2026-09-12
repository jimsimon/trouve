import { describe, expect, it } from "vitest";

import type { TurnState } from "./thread-view-model.js";
import { TurnCompletionAnnouncer } from "./turn-announcements.js";

const usage: Extract<TurnState, { readonly kind: "completed" }>["usage"] = {
  input_tokens: 1,
  output_tokens: 1,
};
const states = (entries: ReadonlyArray<readonly [number, TurnState]>) => new Map(entries);

describe("TurnCompletionAnnouncer", () => {
  it("announces a watched turn once when it completes and clears when the next one starts", () => {
    const announcer = new TurnCompletionAnnouncer();
    expect(announcer.observe("th_1", states([[7, { kind: "running" }]]))).toBe("");
    expect(announcer.observe("th_1", states([[7, { kind: "running" }]]))).toBe("");
    expect(announcer.observe("th_1", states([[7, { kind: "completed", usage }]])))
      .toBe("Turn 7 complete");
    // Re-observing the same finished state keeps the message stable so the
    // live region does not re-announce on unrelated renders.
    expect(announcer.observe("th_1", states([[7, { kind: "completed", usage }]])))
      .toBe("Turn 7 complete");
    expect(announcer.observe("th_1", states([
      [7, { kind: "completed", usage }],
      [8, { kind: "waiting-for-capacity" }],
    ]))).toBe("");
    expect(announcer.observe("th_1", states([
      [7, { kind: "completed", usage }],
      [8, { kind: "completed", usage }],
    ]))).toBe("Turn 8 complete");
  });

  it("stays silent for turns that were already finished when the thread loaded", () => {
    const announcer = new TurnCompletionAnnouncer();
    expect(announcer.observe("th_1", states([
      [1, { kind: "completed", usage }],
      [2, { kind: "failed", error: "boom" }],
    ]))).toBe("");
  });

  it("leaves failed and cancelled turns to their own alert markers", () => {
    const announcer = new TurnCompletionAnnouncer();
    announcer.observe("th_1", states([[3, { kind: "running" }]]));
    expect(announcer.observe("th_1", states([[3, { kind: "failed", error: "boom" }]]))).toBe("");
    announcer.observe("th_1", states([[4, { kind: "running" }]]));
    expect(announcer.observe("th_1", states([[4, { kind: "cancelled" }]]))).toBe("");
  });

  it("forgets watched turns when the thread changes", () => {
    const announcer = new TurnCompletionAnnouncer();
    announcer.observe("th_1", states([[3, { kind: "running" }]]));
    expect(announcer.observe("th_2", states([[3, { kind: "completed", usage }]]))).toBe("");
    announcer.observe("th_2", states([[9, { kind: "running" }]]));
    expect(announcer.observe("th_2", states([[9, { kind: "completed", usage }]])))
      .toBe("Turn 9 complete");
  });
});
