import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export interface ChatPreferences {
  /** Include thinking output in the collapsible runs formed around tool calls. */
  readonly collapseThinkingWithTools: boolean;
  /** Include context-compaction boundaries in collapsible tool-activity runs. */
  readonly collapseCompactionWithTools: boolean;
}

export const DEFAULT_CHAT_PREFERENCES: ChatPreferences = Object.freeze({
  collapseThinkingWithTools: false,
  collapseCompactionWithTools: false,
});

const STORAGE_KEY = "trouve.chat.v1";

export interface ChatPreferenceStorage {
  load(): ChatPreferences | undefined;
  save(preferences: ChatPreferences): void;
}

export const normalizeChatPreferences = (
  value: Partial<ChatPreferences>,
  fallback: ChatPreferences = DEFAULT_CHAT_PREFERENCES,
): ChatPreferences => Object.freeze({
  collapseThinkingWithTools:
    typeof value.collapseThinkingWithTools === "boolean"
      ? value.collapseThinkingWithTools
      : fallback.collapseThinkingWithTools,
  collapseCompactionWithTools:
    typeof value.collapseCompactionWithTools === "boolean"
      ? value.collapseCompactionWithTools
      : fallback.collapseCompactionWithTools,
});

export const browserChatPreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): ChatPreferenceStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw === null) return undefined;
      const parsed: unknown = JSON.parse(raw);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return undefined;
      }
      return normalizeChatPreferences(parsed as Partial<ChatPreferences>);
    } catch {
      return undefined;
    }
  },
  save: (preferences) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      // Preference remains effective for this frontend lifetime.
    }
  },
});

export class ChatPreferencesController {
  readonly #storage: ChatPreferenceStorage | undefined;
  readonly current = createSignal<ChatPreferences>(DEFAULT_CHAT_PREFERENCES);

  constructor(storage?: ChatPreferenceStorage) {
    this.#storage = storage;
    this.current.set(storage?.load() ?? DEFAULT_CHAT_PREFERENCES);
  }

  replace(value: Partial<ChatPreferences>, persist = true): ChatPreferences {
    const next = normalizeChatPreferences(value, this.current.get());
    this.current.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  update(patch: Partial<ChatPreferences>): ChatPreferences {
    return this.replace({ ...this.current.get(), ...patch });
  }
}

export const createBrowserChatPreferencesController = (
  persistLocally = true,
) => {
  if (!persistLocally) return new ChatPreferencesController();
  try {
    return new ChatPreferencesController(
      browserChatPreferenceStorage(globalThis.localStorage),
    );
  } catch {
    return new ChatPreferencesController();
  }
};

export type ChatPreferencesSignal = ReadonlySignal<ChatPreferences>;
