import { describe, expect, it, vi } from "vitest";

import {
  browserChatPreferenceStorage,
  ChatPreferencesController,
  DEFAULT_CHAT_PREFERENCES,
  normalizeChatPreferences,
} from "./chat-preferences.js";

describe("chat preferences", () => {
  it("defaults to visible, top-level thinking output", () => {
    expect(DEFAULT_CHAT_PREFERENCES).toEqual({ collapseThinkingWithTools: false });
    expect(normalizeChatPreferences({})).toEqual({ collapseThinkingWithTools: false });
    expect(normalizeChatPreferences({
      collapseThinkingWithTools: "yes" as unknown as boolean,
    })).toEqual({ collapseThinkingWithTools: false });
  });

  it("normalizes and persists explicit changes", () => {
    const memory = new Map<string, string>();
    const storage = {
      getItem: vi.fn((key: string) => memory.get(key) ?? null),
      setItem: vi.fn((key: string, value: string) => memory.set(key, value)),
    };
    const adapter = browserChatPreferenceStorage(storage);
    const controller = new ChatPreferencesController(adapter);

    controller.update({ collapseThinkingWithTools: true });

    expect(controller.current.get()).toEqual({ collapseThinkingWithTools: true });
    expect(adapter.load()).toEqual({ collapseThinkingWithTools: true });
    expect(storage.setItem).toHaveBeenCalledOnce();
  });

  it("ignores corrupt browser state", () => {
    const storage = {
      getItem: vi.fn(() => "not-json"),
      setItem: vi.fn(),
    };
    expect(browserChatPreferenceStorage(storage).load()).toBeUndefined();
  });
});
