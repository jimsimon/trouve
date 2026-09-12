import { describe, expect, it } from "vitest";

import { subagentThreadIsReadOnly } from "./subagent-access.js";

const modes = [
  { id: "code", read_only: false },
  { id: "plan", read_only: true },
];

describe("subagent access", () => {
  it("keeps exploration and audit modes read-only", () => {
    expect(subagentThreadIsReadOnly({ spawned: true, mode: "plan" }, modes)).toBe(true);
  });

  it("allows follow-up interaction in non-read-only modes", () => {
    expect(subagentThreadIsReadOnly({ spawned: true, mode: "code" }, modes)).toBe(false);
    expect(subagentThreadIsReadOnly(
      { spawned: true, mode: "code" },
      [{ id: "code" }],
    )).toBe(false);
  });

  it("fails closed for an unresolved spawned mode without affecting regular threads", () => {
    expect(subagentThreadIsReadOnly({ spawned: true, mode: "workspace-audit" }, modes)).toBe(true);
    expect(subagentThreadIsReadOnly({ spawned: false, mode: "workspace-audit" }, [])).toBe(false);
  });
});
