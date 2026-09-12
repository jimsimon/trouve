import { describe, expect, it, vi } from "vitest";

import {
  browserResumePreferenceStorage,
  chatBookmarkForNavigation,
  DEFAULT_RESUME_PREFERENCES,
  normalizeResumePreferences,
  preferredSessionThreadId,
  ResumePreferencesController,
} from "./resume-preferences.js";

describe("resume preferences", () => {
  it("normalizes untrusted persisted state and keeps chat anchors immutable", () => {
    const normalized = normalizeResumePreferences({
      selectedSessionId: "se-1",
      sessionThreads: {
        "se-1": "th-1",
        "bad id": "th-secret",
        "se-2": "bad/thread",
      },
      threadScroll: {
        "th-1": { itemId: "assistant:42", offset: 18.5 },
        "th-2": { itemId: "bad\nitem", offset: 2 },
        "th-3": { itemId: "assistant:43", offset: Number.POSITIVE_INFINITY },
      },
      closedThreadTabs: ["th-2", "bad/thread", "th-2", "th-3"],
      pinnedThreadTabs: ["th-1", "bad/thread", "th-1", "th-2"],
    });

    expect(normalized).toEqual({
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-1" },
      threadScroll: { "th-1": { itemId: "assistant:42", offset: 18.5 } },
      closedThreadTabs: ["th-2", "th-3"],
      pinnedThreadTabs: ["th-1"],
    });
    expect(Object.isFrozen(normalized)).toBe(true);
    expect(Object.isFrozen(normalized.threadScroll["th-1"])).toBe(true);
    expect(normalizeResumePreferences(null)).toBe(DEFAULT_RESUME_PREFERENCES);
  });

  it("round-trips browser storage and ignores corrupt JSON", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => values.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => values.set(key, value)),
    };
    const adapter = browserResumePreferenceStorage(storage);
    adapter.save({
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-1" },
      threadScroll: { "th-1": { itemId: "user:1", offset: 4 } },
      closedThreadTabs: ["th-2"],
      pinnedThreadTabs: ["th-1"],
    });
    expect(adapter.load()).toEqual({
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-1" },
      threadScroll: { "th-1": { itemId: "user:1", offset: 4 } },
      closedThreadTabs: ["th-2"],
      pinnedThreadTabs: ["th-1"],
    });
    values.set("trouve.resume.v1", "{");
    expect(adapter.load()).toBeUndefined();
  });

  it("tracks the selected thread and removes tail bookmarks without redundant saves", () => {
    const storage = { load: () => undefined, save: vi.fn() };
    const controller = new ResumePreferencesController(storage);
    const selected = controller.select("se-1", "th-1");
    expect(selected.sessionThreads).toEqual({ "se-1": "th-1" });
    expect(controller.select("se-1", "th-1")).toBe(selected);
    expect(storage.save).toHaveBeenCalledTimes(1);

    controller.setThreadScroll("th-1", { itemId: "assistant:1", offset: 12 }, false);
    expect(storage.save).toHaveBeenCalledTimes(1);
    controller.persist();
    expect(storage.save).toHaveBeenCalledTimes(2);
    expect(controller.setThreadScroll("th-1", undefined).threadScroll).toEqual({});

    const closed = controller.setThreadTabClosed("th-2", true);
    expect(closed.closedThreadTabs).toEqual(["th-2"]);
    expect(controller.setThreadTabClosed("th-2", true)).toBe(closed);
    expect(controller.setThreadTabClosed("th-2", false).closedThreadTabs).toEqual([]);

    const pinned = controller.setThreadTabPinned("th-1", true);
    expect(pinned.pinnedThreadTabs).toEqual(["th-1"]);
    expect(controller.setThreadTabPinned("th-1", true)).toBe(pinned);
    const closedPinned = controller.setThreadTabClosed("th-1", true);
    expect(closedPinned.pinnedThreadTabs).toEqual([]);
    expect(controller.setThreadTabPinned("th-1", true)).toBe(closedPinned);
  });

  it("opens running and queued threads at the tail instead of parked history", () => {
    const bookmark = { itemId: "assistant:42", offset: 18.5 } as const;
    expect(chatBookmarkForNavigation(bookmark, false, false)).toBe(bookmark);
    expect(chatBookmarkForNavigation(bookmark, true, false)).toBeUndefined();
    expect(chatBookmarkForNavigation(bookmark, false, true)).toBeUndefined();
  });

  it("keeps session-level navigation on an open thread tab", () => {
    const preferences = normalizeResumePreferences({
      selectedSessionId: "se-1",
      sessionThreads: { "se-1": "th-closed" },
      closedThreadTabs: ["th-closed", "th-latest"],
    });
    expect(preferredSessionThreadId(
      preferences,
      "se-1",
      "th-latest",
      ["th-open", "th-closed", "th-latest"],
    )).toBe("th-open");
    expect(preferredSessionThreadId(
      preferences,
      "se-1",
      "th-latest",
      ["th-closed", "th-latest"],
    )).toBeUndefined();
  });

  it("keeps only the most recently touched thousand entries", () => {
    const controller = new ResumePreferencesController();
    for (let index = 0; index < 1_005; index += 1) {
      controller.select(`se-${index}`, `th-${index}`, false);
    }
    const current = controller.current.get();
    expect(Object.keys(current.sessionThreads)).toHaveLength(1_000);
    expect(current.sessionThreads["se-0"]).toBeUndefined();
    expect(current.sessionThreads["se-1004"]).toBe("th-1004");

    for (let index = 0; index < 1_005; index += 1) {
      controller.setThreadTabClosed(`th-closed-${index}`, true, false);
    }
    const closed = controller.current.get().closedThreadTabs;
    expect(closed).toHaveLength(1_000);
    expect(closed[0]).toBe("th-closed-5");
    expect(closed.at(-1)).toBe("th-closed-1004");
  });
});
