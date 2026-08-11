import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

const STORAGE_KEY = "trouve.resume.v1";
const MAX_ENTRIES = 1_000;
const MAX_ID_BYTES = 256;
const MAX_SCROLL_OFFSET = 1_000_000;

export interface ChatScrollBookmark {
  readonly itemId: string;
  readonly offset: number;
}

/** Attention at the transcript tail supersedes parked history when a thread
 * is reopened. This mirrors the native navigation policy: running work and a
 * persisted queue are both content the user must see immediately. */
export const chatBookmarkForNavigation = (
  bookmark: ChatScrollBookmark | undefined,
  turnRunning: boolean,
  hasQueue: boolean,
): ChatScrollBookmark | undefined =>
  turnRunning || hasQueue ? undefined : bookmark;

export interface ResumePreferences {
  readonly selectedSessionId: string;
  readonly sessionThreads: Readonly<Record<string, string>>;
  readonly threadScroll: Readonly<Record<string, ChatScrollBookmark>>;
  readonly closedThreadTabs: readonly string[];
  readonly pinnedThreadTabs: readonly string[];
}

/** Pick an already-open tab when navigation targets a session rather than a
 * specific thread. Explicit thread routes are handled separately and reopen
 * that tab; session-level navigation must never undo a prior close. */
export const preferredSessionThreadId = (
  preferences: ResumePreferences,
  sessionId: string,
  latestThreadId: string | undefined,
  availableThreadIds: readonly string[],
): string | undefined => {
  const closed = new Set(preferences.closedThreadTabs);
  const available = new Set(availableThreadIds);
  const known = (threadId: string | undefined): threadId is string =>
    threadId !== undefined
    && !closed.has(threadId)
    && (available.size === 0 || available.has(threadId));
  const remembered = preferences.sessionThreads[sessionId];
  if (known(remembered)) return remembered;
  if (known(latestThreadId)) return latestThreadId;
  for (let index = availableThreadIds.length - 1; index >= 0; index -= 1) {
    const threadId = availableThreadIds[index];
    if (threadId !== undefined && !closed.has(threadId)) return threadId;
  }
  return undefined;
};

const emptyRecord = <T>(): Readonly<Record<string, T>> => Object.freeze({});
const emptyList = <T>(): readonly T[] => Object.freeze([]);

export const DEFAULT_RESUME_PREFERENCES: ResumePreferences = Object.freeze({
  selectedSessionId: "",
  sessionThreads: emptyRecord<string>(),
  threadScroll: emptyRecord<ChatScrollBookmark>(),
  closedThreadTabs: emptyList<string>(),
  pinnedThreadTabs: emptyList<string>(),
});

export interface ResumePreferenceStorage {
  load(): ResumePreferences | undefined;
  save(preferences: ResumePreferences): void;
}

const idBytes = (value: string): number => new TextEncoder().encode(value).byteLength;

const validProtocolId = (value: unknown): value is string =>
  typeof value === "string" &&
  value.length > 0 &&
  idBytes(value) <= MAX_ID_BYTES &&
  /^[A-Za-z0-9._-]+$/u.test(value);

const validChatItemId = (value: unknown): value is string =>
  typeof value === "string" &&
  value.length > 0 &&
  idBytes(value) <= MAX_ID_BYTES &&
  !/[\u0000-\u001f\u007f]/u.test(value);

const objectRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;

const normalizeSessionThreads = (value: unknown): Readonly<Record<string, string>> => {
  const source = objectRecord(value);
  if (source === undefined) return emptyRecord<string>();
  const normalized: Record<string, string> = {};
  let count = 0;
  for (const [sessionId, threadId] of Object.entries(source)) {
    if (count >= MAX_ENTRIES) break;
    if (validProtocolId(sessionId) && validProtocolId(threadId)) {
      normalized[sessionId] = threadId;
      count += 1;
    }
  }
  return Object.freeze(normalized);
};

const normalizeThreadScroll = (
  value: unknown,
): Readonly<Record<string, ChatScrollBookmark>> => {
  const source = objectRecord(value);
  if (source === undefined) return emptyRecord<ChatScrollBookmark>();
  const normalized: Record<string, ChatScrollBookmark> = {};
  let count = 0;
  for (const [threadId, untrustedBookmark] of Object.entries(source)) {
    if (count >= MAX_ENTRIES) break;
    const bookmark = objectRecord(untrustedBookmark);
    if (
      !validProtocolId(threadId) ||
      bookmark === undefined ||
      !validChatItemId(bookmark["itemId"]) ||
      typeof bookmark["offset"] !== "number" ||
      !Number.isFinite(bookmark["offset"]) ||
      bookmark["offset"] < 0 ||
      bookmark["offset"] > MAX_SCROLL_OFFSET
    ) continue;
    normalized[threadId] = Object.freeze({
      itemId: bookmark["itemId"],
      offset: bookmark["offset"],
    });
    count += 1;
  }
  return Object.freeze(normalized);
};

const normalizeThreadTabIds = (value: unknown): readonly string[] => {
  if (!Array.isArray(value)) return emptyList<string>();
  const normalized: string[] = [];
  const seen = new Set<string>();
  for (const threadId of value) {
    if (normalized.length >= MAX_ENTRIES) break;
    if (validProtocolId(threadId) && !seen.has(threadId)) {
      normalized.push(threadId);
      seen.add(threadId);
    }
  }
  return Object.freeze(normalized);
};

export const normalizeResumePreferences = (value: unknown): ResumePreferences => {
  const source = objectRecord(value);
  if (source === undefined) return DEFAULT_RESUME_PREFERENCES;
  const closedThreadTabs = normalizeThreadTabIds(source["closedThreadTabs"]);
  const closed = new Set(closedThreadTabs);
  return Object.freeze({
    selectedSessionId: validProtocolId(source["selectedSessionId"])
      ? source["selectedSessionId"]
      : "",
    sessionThreads: normalizeSessionThreads(source["sessionThreads"]),
    threadScroll: normalizeThreadScroll(source["threadScroll"]),
    closedThreadTabs,
    pinnedThreadTabs: Object.freeze(
      normalizeThreadTabIds(source["pinnedThreadTabs"]).filter((threadId) =>
        !closed.has(threadId)),
    ),
  });
};

const sameResumePreferences = (
  left: ResumePreferences,
  right: ResumePreferences,
): boolean => {
  if (left.selectedSessionId !== right.selectedSessionId) return false;
  const leftThreads = Object.entries(left.sessionThreads);
  const rightThreads = Object.entries(right.sessionThreads);
  if (
    leftThreads.length !== rightThreads.length ||
    leftThreads.some(([sessionId, threadId]) => right.sessionThreads[sessionId] !== threadId)
  ) return false;
  if (
    left.closedThreadTabs.length !== right.closedThreadTabs.length ||
    left.closedThreadTabs.some((threadId, index) => right.closedThreadTabs[index] !== threadId)
  ) return false;
  if (
    left.pinnedThreadTabs.length !== right.pinnedThreadTabs.length ||
    left.pinnedThreadTabs.some((threadId, index) => right.pinnedThreadTabs[index] !== threadId)
  ) return false;
  const leftScroll = Object.entries(left.threadScroll);
  const rightScroll = Object.entries(right.threadScroll);
  return leftScroll.length === rightScroll.length && leftScroll.every(
    ([threadId, bookmark]) => {
      const other = right.threadScroll[threadId];
      return other?.itemId === bookmark.itemId && other.offset === bookmark.offset;
    },
  );
};

const appendBounded = <T>(
  source: Readonly<Record<string, T>>,
  key: string,
  value: T | undefined,
): Readonly<Record<string, T>> => {
  const next: Record<string, T> = {};
  for (const [existingKey, existingValue] of Object.entries(source)) {
    if (existingKey !== key) next[existingKey] = existingValue;
  }
  if (value !== undefined) next[key] = value;
  const overflow = Object.keys(next).length - MAX_ENTRIES;
  if (overflow > 0) {
    for (const oldKey of Object.keys(next).slice(0, overflow)) delete next[oldKey];
  }
  return Object.freeze(next);
};

export const browserResumePreferenceStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): ResumePreferenceStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      return raw === null ? undefined : normalizeResumePreferences(JSON.parse(raw));
    } catch {
      return undefined;
    }
  },
  save: (preferences) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      // Resume state remains effective for this frontend lifetime.
    }
  },
});

export class ResumePreferencesController {
  readonly #storage: ResumePreferenceStorage | undefined;
  readonly #current = createSignal<ResumePreferences>(DEFAULT_RESUME_PREFERENCES);
  readonly current: ReadonlySignal<ResumePreferences> = this.#current;

  constructor(storage?: ResumePreferenceStorage) {
    this.#storage = storage;
    this.#current.set(storage?.load() ?? DEFAULT_RESUME_PREFERENCES);
  }

  replace(value: unknown, persist = true): ResumePreferences {
    const next = normalizeResumePreferences(value);
    const current = this.#current.get();
    if (sameResumePreferences(current, next)) return current;
    return this.#commit(next, persist);
  }

  #commit(next: ResumePreferences, persist: boolean): ResumePreferences {
    this.#current.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  select(sessionId: string, threadId?: string, persist = true): ResumePreferences {
    const current = this.#current.get();
    if (!validProtocolId(sessionId)) return current;
    if (threadId !== undefined && !validProtocolId(threadId)) return current;
    if (
      current.selectedSessionId === sessionId &&
      (threadId === undefined || current.sessionThreads[sessionId] === threadId)
    ) return current;
    const next = Object.freeze({
      selectedSessionId: sessionId,
      sessionThreads: threadId === undefined
        ? current.sessionThreads
        : appendBounded(current.sessionThreads, sessionId, threadId),
      threadScroll: current.threadScroll,
      closedThreadTabs: current.closedThreadTabs,
      pinnedThreadTabs: current.pinnedThreadTabs,
    });
    return this.#commit(next, persist);
  }

  setThreadTabClosed(
    threadId: string,
    closed: boolean,
    persist = true,
  ): ResumePreferences {
    const current = this.#current.get();
    if (!validProtocolId(threadId)) return current;
    const existingIndex = current.closedThreadTabs.indexOf(threadId);
    if ((closed && existingIndex >= 0) || (!closed && existingIndex < 0)) return current;
    const closedThreadTabs = closed
      ? Object.freeze([
          ...current.closedThreadTabs.slice(-(MAX_ENTRIES - 1)),
          threadId,
        ])
      : Object.freeze(current.closedThreadTabs.filter((candidate) => candidate !== threadId));
    return this.#commit(Object.freeze({
      ...current,
      closedThreadTabs,
      pinnedThreadTabs: closed
        ? Object.freeze(current.pinnedThreadTabs.filter((candidate) => candidate !== threadId))
        : current.pinnedThreadTabs,
    }), persist);
  }

  setThreadTabPinned(
    threadId: string,
    pinned: boolean,
    persist = true,
  ): ResumePreferences {
    const current = this.#current.get();
    if (!validProtocolId(threadId) || current.closedThreadTabs.includes(threadId)) {
      return current;
    }
    const existingIndex = current.pinnedThreadTabs.indexOf(threadId);
    if ((pinned && existingIndex >= 0) || (!pinned && existingIndex < 0)) return current;
    const pinnedThreadTabs = pinned
      ? Object.freeze([
          ...current.pinnedThreadTabs.slice(-(MAX_ENTRIES - 1)),
          threadId,
        ])
      : Object.freeze(current.pinnedThreadTabs.filter((candidate) => candidate !== threadId));
    return this.#commit(Object.freeze({
      ...current,
      pinnedThreadTabs,
    }), persist);
  }

  setThreadScroll(
    threadId: string,
    bookmark: ChatScrollBookmark | undefined,
    persist = true,
  ): ResumePreferences {
    const current = this.#current.get();
    if (!validProtocolId(threadId)) return current;
    if (
      bookmark !== undefined &&
      (!validChatItemId(bookmark.itemId) ||
        !Number.isFinite(bookmark.offset) ||
        bookmark.offset < 0 ||
        bookmark.offset > MAX_SCROLL_OFFSET)
    ) return current;
    const existing = current.threadScroll[threadId];
    if (
      (bookmark === undefined && existing === undefined) ||
      (bookmark !== undefined &&
        existing?.itemId === bookmark.itemId &&
        existing.offset === bookmark.offset)
    ) return current;
    const normalizedBookmark = bookmark === undefined
      ? undefined
      : Object.freeze({ ...bookmark });
    return this.#commit(Object.freeze({
      ...current,
      threadScroll: appendBounded(
        current.threadScroll,
        threadId,
        normalizedBookmark,
      ),
    }), persist);
  }

  persist(): void {
    this.#storage?.save(this.#current.get());
  }
}

export const createBrowserResumePreferencesController = (
  persistLocally = true,
): ResumePreferencesController => {
  if (!persistLocally) return new ResumePreferencesController();
  try {
    return new ResumePreferencesController(
      browserResumePreferenceStorage(globalThis.localStorage),
    );
  } catch {
    return new ResumePreferencesController();
  }
};
