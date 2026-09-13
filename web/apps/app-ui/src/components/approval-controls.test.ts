import { describe, expect, it } from "vitest";

import {
  approvalDecisionForShortcut,
  ApprovalSubmissionTracker,
} from "./approval-controls.js";

describe("approval controls", () => {
  it.each([
    ["y", "approve"],
    ["Y", "approve"],
    ["a", "always_approve"],
    ["A", "always_approve"],
    ["n", "deny"],
    ["N", "deny"],
  ] as const)("maps %s to %s", (key, decision) => {
    expect(approvalDecisionForShortcut({ key })).toBe(decision);
  });

  it("does not capture modified, repeated, composing, or editable input", () => {
    expect(approvalDecisionForShortcut({ key: "y", ctrlKey: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "a", altKey: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "n", metaKey: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "y", repeat: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "a", isComposing: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "n", editable: true })).toBeUndefined();
    expect(approvalDecisionForShortcut({ key: "Enter" })).toBeUndefined();
  });

  it("guards one submission per call while allowing independent approvals", () => {
    const tracker = new ApprovalSubmissionTracker();
    expect(tracker.begin("call-1")).toBe(true);
    expect(tracker.begin("call-1")).toBe(false);
    expect(tracker.begin("call-2")).toBe(true);
    expect(tracker.has("call-1")).toBe(true);

    tracker.finish("call-1");
    expect(tracker.has("call-1")).toBe(false);
    expect(tracker.begin("call-1")).toBe(true);
  });
});
