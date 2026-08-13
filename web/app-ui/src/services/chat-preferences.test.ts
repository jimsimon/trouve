import { describe, expect, it, vi } from "vitest";

import {
  browserChatPreferenceStorage,
  ChatPreferencesController,
  DEFAULT_CHAT_PREFERENCES,
  effectiveChatCollapsePreferences,
  normalizeChatPreferences,
} from "./chat-preferences.js";

describe("chat preferences", () => {
  it("defaults to grouped tools with visible, top-level thinking output", () => {
    expect(DEFAULT_CHAT_PREFERENCES).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
    expect(normalizeChatPreferences({})).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
    expect(normalizeChatPreferences({
      collapseSequentialToolCalls: "yes" as unknown as boolean,
      collapseThinkingWithTools: "yes" as unknown as boolean,
      collapseCompactionWithTools: "yes" as unknown as boolean,
      collapseTodoUpdatesWithTools: "yes" as unknown as boolean,
    })).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
  });

  it("ignores subordinate collapse preferences when sequential grouping is off", () => {
    expect(effectiveChatCollapsePreferences({
      collapseSequentialToolCalls: false,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    })).toEqual({
      collapseSequentialToolCalls: false,
      collapseThinkingWithTools: false,
      collapseCompactionWithTools: false,
      collapseTodoUpdatesWithTools: false,
    });
  });

  it("normalizes and persists explicit changes", () => {
    const memory = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => memory.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => memory.set(key, value)),
    };
    const adapter = browserChatPreferenceStorage(storage);
    const controller = new ChatPreferencesController(adapter);

    controller.update({
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });

    expect(controller.current.get()).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
    expect(adapter.load()).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
    expect(storage.setItem).toHaveBeenCalledOnce();
  });

  it("restores an explicit change in a new frontend lifetime", () => {
    const memory = new Map<string, string>();
    const storage = {
      getItem: (key: string) => memory.get(key) ?? null,
      setItem: (key: string, value: string) => memory.set(key, value),
    };

    new ChatPreferencesController(browserChatPreferenceStorage(storage)).update({
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
    const reloaded = new ChatPreferencesController(
      browserChatPreferenceStorage(storage),
    );

    expect(reloaded.current.get()).toEqual({
      collapseSequentialToolCalls: true,
      collapseThinkingWithTools: true,
      collapseCompactionWithTools: true,
      collapseTodoUpdatesWithTools: true,
    });
  });

  it("ignores corrupt browser state", () => {
    const storage = {
      getItem: vi.fn(() => "not-json"),
      setItem: vi.fn(),
    };
    expect(browserChatPreferenceStorage(storage).load()).toBeUndefined();
  });
});
