import { describe, expect, test } from "vitest";

import { nativeTextHistoryCommand } from "./native-text-history.js";

describe("nativeTextHistoryCommand", () => {
  test("supports the common undo and redo bindings", () => {
    expect(nativeTextHistoryCommand({ key: "z", ctrlKey: true })).toBe("undo");
    expect(nativeTextHistoryCommand({ key: "Z", metaKey: true })).toBe("undo");
    expect(nativeTextHistoryCommand({ key: "z", ctrlKey: true, shiftKey: true }))
      .toBe("redo");
    expect(nativeTextHistoryCommand({ key: "y", ctrlKey: true })).toBe("redo");
  });

  test("leaves unrelated and composing shortcuts to their control", () => {
    expect(nativeTextHistoryCommand({ key: "z" })).toBeUndefined();
    expect(nativeTextHistoryCommand({ key: "z", ctrlKey: true, altKey: true }))
      .toBeUndefined();
    expect(nativeTextHistoryCommand({ key: "z", ctrlKey: true, isComposing: true }))
      .toBeUndefined();
    expect(nativeTextHistoryCommand({ key: "y", ctrlKey: true, shiftKey: true }))
      .toBeUndefined();
  });
});
