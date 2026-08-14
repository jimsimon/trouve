import {
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "./attachments.js";

export interface ComposerDraft {
  readonly text: string;
  readonly cursor: number;
  readonly attachments: readonly PendingAttachment[];
}

export const EMPTY_COMPOSER_DRAFT: ComposerDraft = Object.freeze({
  text: "",
  cursor: 0,
  attachments: Object.freeze([]),
});

interface StoredComposerText {
  readonly text: string;
  readonly cursor: number;
}

interface StoredComposerTextRecord extends StoredComposerText {
  readonly updatedAt: number;
}

export interface ComposerDraftTextStorage {
  load(threadId: string): StoredComposerText | undefined;
  save(threadId: string, draft: StoredComposerText): void;
  clear(threadId: string): void;
}

export interface ComposerDraftAttachmentStorage {
  load(threadId: string): Promise<readonly PendingAttachment[]>;
  save(threadId: string, attachments: readonly PendingAttachment[]): Promise<void>;
  clear(threadId: string): Promise<void>;
}

const TEXT_STORAGE_KEY = "trouve.composer-drafts.v1";
const MAX_STORED_TEXT_DRAFTS = 100;
const ATTACHMENT_DATABASE = "trouve-composer-drafts";
const ATTACHMENT_STORE = "thread-attachments";

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const normalizedCursor = (value: unknown, text: string): number =>
  typeof value === "number" && Number.isFinite(value)
    ? Math.max(0, Math.min(text.length, Math.trunc(value)))
    : text.length;

const normalizeAttachment = (value: unknown): PendingAttachment | undefined => {
  if (!isRecord(value) || !isRecord(value["upload"])) return undefined;
  const upload = value["upload"];
  const name = upload["name"];
  const mime = upload["mime"];
  const data = upload["data"];
  const size = value["size"];
  if (
    typeof name !== "string"
    || typeof mime !== "string"
    || typeof data !== "string"
    || typeof size !== "number"
    || !Number.isSafeInteger(size)
    || size <= 0
    || size > MAX_ATTACHMENT_BYTES
    || data === ""
  ) return undefined;
  return Object.freeze({
    upload: Object.freeze({ name, mime, data }),
    size,
  });
};

const normalizeAttachments = (value: unknown): readonly PendingAttachment[] => {
  if (!Array.isArray(value)) return Object.freeze([]);
  const attachments: PendingAttachment[] = [];
  let bytes = 0;
  for (const candidate of value.slice(0, MAX_PENDING_ATTACHMENTS)) {
    const attachment = normalizeAttachment(candidate);
    if (attachment === undefined) continue;
    bytes += attachment.size;
    if (bytes > MAX_PENDING_ATTACHMENT_BYTES) break;
    attachments.push(attachment);
  }
  return Object.freeze(attachments);
};

export const normalizeComposerDraft = (value: unknown): ComposerDraft => {
  if (!isRecord(value)) return EMPTY_COMPOSER_DRAFT;
  const text = typeof value["text"] === "string" ? value["text"] : "";
  return Object.freeze({
    text,
    cursor: normalizedCursor(value["cursor"], text),
    attachments: normalizeAttachments(value["attachments"]),
  });
};

const storedTextRecords = (storage: Pick<Storage, "getItem">): Record<
  string,
  StoredComposerTextRecord
> => {
  try {
    const raw = storage.getItem(TEXT_STORAGE_KEY);
    if (raw === null) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return {};
    const records: Record<string, StoredComposerTextRecord> = {};
    for (const [threadId, value] of Object.entries(parsed)) {
      if (threadId === "" || !isRecord(value) || typeof value["text"] !== "string") {
        continue;
      }
      const text = value["text"];
      records[threadId] = {
        text,
        cursor: normalizedCursor(value["cursor"], text),
        updatedAt: typeof value["updatedAt"] === "number"
          && Number.isFinite(value["updatedAt"])
          ? value["updatedAt"]
          : 0,
      };
    }
    return records;
  } catch {
    return {};
  }
};

export const browserComposerDraftTextStorage = (
  storage: Pick<Storage, "getItem" | "setItem" | "removeItem">,
  now: () => number = Date.now,
): ComposerDraftTextStorage => {
  const write = (records: Record<string, StoredComposerTextRecord>): void => {
    try {
      const entries = Object.entries(records)
        .sort(([, left], [, right]) => right.updatedAt - left.updatedAt)
        .slice(0, MAX_STORED_TEXT_DRAFTS);
      if (entries.length === 0) storage.removeItem(TEXT_STORAGE_KEY);
      else storage.setItem(TEXT_STORAGE_KEY, JSON.stringify(Object.fromEntries(entries)));
    } catch {
      // The controller's in-memory draft remains available for this page lifetime.
    }
  };
  return {
    load: (threadId) => {
      const record = storedTextRecords(storage)[threadId];
      return record === undefined
        ? undefined
        : Object.freeze({ text: record.text, cursor: record.cursor });
    },
    save: (threadId, draft) => {
      const records = storedTextRecords(storage);
      records[threadId] = {
        text: draft.text,
        cursor: normalizedCursor(draft.cursor, draft.text),
        updatedAt: now(),
      };
      write(records);
    },
    clear: (threadId) => {
      const records = storedTextRecords(storage);
      if (delete records[threadId]) write(records);
    },
  };
};

const openAttachmentDatabase = (
  factory: IDBFactory,
): Promise<IDBDatabase | undefined> => new Promise((resolve) => {
  try {
    const request = factory.open(ATTACHMENT_DATABASE, 1);
    let settled = false;
    const finish = (database: IDBDatabase | undefined): void => {
      if (settled) {
        database?.close();
        return;
      }
      settled = true;
      resolve(database);
    };
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(ATTACHMENT_STORE)) {
        request.result.createObjectStore(ATTACHMENT_STORE);
      }
    };
    request.onsuccess = () => {
      request.result.onversionchange = () => request.result.close();
      finish(request.result);
    };
    request.onerror = () => finish(undefined);
    request.onblocked = () => finish(undefined);
  } catch {
    resolve(undefined);
  }
});

const transactionCompletion = (transaction: IDBTransaction): Promise<void> =>
  new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error ?? new Error("draft storage failed"));
    transaction.onabort = () => reject(transaction.error ?? new Error("draft storage aborted"));
  });

export const browserComposerDraftAttachmentStorage = (
  factory: IDBFactory,
): ComposerDraftAttachmentStorage => {
  const database = openAttachmentDatabase(factory);
  return {
    load: async (threadId) => {
      const opened = await database;
      if (opened === undefined) return Object.freeze([]);
      return await new Promise<readonly PendingAttachment[]>((resolve) => {
        try {
          const transaction = opened.transaction(ATTACHMENT_STORE, "readonly");
          const request = transaction.objectStore(ATTACHMENT_STORE).get(threadId);
          request.onsuccess = () => resolve(normalizeAttachments(request.result));
          request.onerror = () => resolve(Object.freeze([]));
        } catch {
          resolve(Object.freeze([]));
        }
      });
    },
    save: async (threadId, attachments) => {
      const opened = await database;
      if (opened === undefined) return;
      const transaction = opened.transaction(ATTACHMENT_STORE, "readwrite");
      transaction.objectStore(ATTACHMENT_STORE).put([...attachments], threadId);
      await transactionCompletion(transaction);
    },
    clear: async (threadId) => {
      const opened = await database;
      if (opened === undefined) return;
      const transaction = opened.transaction(ATTACHMENT_STORE, "readwrite");
      transaction.objectStore(ATTACHMENT_STORE).delete(threadId);
      await transactionCompletion(transaction);
    },
  };
};

const sameAttachments = (
  left: readonly PendingAttachment[] | undefined,
  right: readonly PendingAttachment[],
): boolean => left !== undefined
  && left.length === right.length
  && left.every((attachment, index) => {
    const candidate = right[index];
    return candidate !== undefined
      && attachment.size === candidate.size
      && attachment.upload.name === candidate.upload.name
      && attachment.upload.mime === candidate.upload.mime
      && attachment.upload.data === candidate.upload.data;
  });

/** Thread-scoped draft state. `stage` is memory-only and cheap enough for
 * every input event; `persist` is debounced by the composer and serializes
 * large attachment writes so a late transaction cannot resurrect old data. */
export class ComposerDraftController {
  readonly #textStorage: ComposerDraftTextStorage | undefined;
  readonly #attachmentStorage: ComposerDraftAttachmentStorage | undefined;
  readonly #drafts = new Map<string, ComposerDraft>();
  readonly #versions = new Map<string, number>();
  readonly #discarded = new Set<string>();
  readonly #attachmentWrites = new Map<string, Promise<void>>();
  readonly #queuedAttachments = new Map<string, readonly PendingAttachment[]>();
  readonly #persistedAttachments = new Map<string, readonly PendingAttachment[]>();

  constructor(options: {
    readonly textStorage?: ComposerDraftTextStorage;
    readonly attachmentStorage?: ComposerDraftAttachmentStorage;
  } = {}) {
    this.#textStorage = options.textStorage;
    this.#attachmentStorage = options.attachmentStorage;
  }

  read(threadId: string): ComposerDraft {
    if (threadId === "" || this.#discarded.has(threadId)) return EMPTY_COMPOSER_DRAFT;
    const cached = this.#drafts.get(threadId);
    if (cached !== undefined) return cached;
    const stored = this.#textStorage?.load(threadId);
    const draft = normalizeComposerDraft({
      text: stored?.text ?? "",
      cursor: stored?.cursor ?? 0,
      attachments: [],
    });
    this.#drafts.set(threadId, draft);
    return draft;
  }

  stage(threadId: string, value: ComposerDraft): ComposerDraft {
    if (threadId === "" || this.#discarded.has(threadId)) return EMPTY_COMPOSER_DRAFT;
    const draft = normalizeComposerDraft(value);
    this.#drafts.set(threadId, draft);
    this.#versions.set(threadId, (this.#versions.get(threadId) ?? 0) + 1);
    return draft;
  }

  async hydrate(threadId: string): Promise<ComposerDraft> {
    const current = this.read(threadId);
    if (
      threadId === ""
      || this.#discarded.has(threadId)
      || this.#attachmentStorage === undefined
    ) return current;
    const version = this.#versions.get(threadId) ?? 0;
    try {
      await this.#attachmentWrites.get(threadId);
      const attachments = await this.#attachmentStorage.load(threadId);
      if (
        this.#discarded.has(threadId)
        || (this.#versions.get(threadId) ?? 0) !== version
      ) return this.read(threadId);
      const hydrated = normalizeComposerDraft({ ...current, attachments });
      this.#drafts.set(threadId, hydrated);
      this.#persistedAttachments.set(threadId, hydrated.attachments);
      return hydrated;
    } catch {
      return this.read(threadId);
    }
  }

  persist(threadId: string): Promise<void> {
    if (threadId === "") return Promise.resolve();
    if (this.#discarded.has(threadId)) {
      try {
        this.#textStorage?.clear(threadId);
      } catch {
        // The permanent in-memory tombstone still blocks late staging.
      }
      return this.#queueAttachmentPersistence(threadId, Object.freeze([]), true);
    }
    const draft = this.read(threadId);
    try {
      if (draft.text === "") this.#textStorage?.clear(threadId);
      else this.#textStorage?.save(threadId, { text: draft.text, cursor: draft.cursor });
    } catch {
      // The staged draft remains available for this page lifetime.
    }
    return this.#queueAttachmentPersistence(threadId, draft.attachments, false);
  }

  save(threadId: string, value: ComposerDraft): Promise<void> {
    this.stage(threadId, value);
    return this.persist(threadId);
  }

  clear(threadId: string): Promise<void> {
    if (threadId === "") return Promise.resolve();
    if (this.#discarded.has(threadId)) return this.persist(threadId);
    this.#drafts.set(threadId, EMPTY_COMPOSER_DRAFT);
    this.#versions.set(threadId, (this.#versions.get(threadId) ?? 0) + 1);
    try {
      this.#textStorage?.clear(threadId);
    } catch {
      // The in-memory clear still prevents this page from restoring the draft.
    }
    return this.#queueAttachmentPersistence(threadId, Object.freeze([]), true);
  }

  /** Permanently discard a deleted thread's draft for this controller
   * lifetime. Unlike submission `clear`, this tombstone rejects every late
   * stage, persist, and hydrate completion that was already queued by the
   * composer when its session disappeared. */
  discard(threadId: string): Promise<void> {
    if (threadId === "") return Promise.resolve();
    this.#discarded.add(threadId);
    this.#drafts.set(threadId, EMPTY_COMPOSER_DRAFT);
    this.#versions.set(threadId, (this.#versions.get(threadId) ?? 0) + 1);
    try {
      this.#textStorage?.clear(threadId);
    } catch {
      // The permanent in-memory tombstone still blocks late staging.
    }
    return this.#queueAttachmentPersistence(threadId, Object.freeze([]), true);
  }

  #queueAttachmentPersistence(
    threadId: string,
    attachments: readonly PendingAttachment[],
    force: boolean,
  ): Promise<void> {
    if (this.#attachmentStorage === undefined) return Promise.resolve();
    if (
      !force
      && (
        sameAttachments(this.#queuedAttachments.get(threadId), attachments)
        || (
          !this.#attachmentWrites.has(threadId)
          && sameAttachments(this.#persistedAttachments.get(threadId), attachments)
        )
      )
    ) return this.#attachmentWrites.get(threadId) ?? Promise.resolve();

    this.#queuedAttachments.set(threadId, attachments);
    const previous = this.#attachmentWrites.get(threadId) ?? Promise.resolve();
    const operation = previous.catch(() => {}).then(async () => {
      if (attachments.length === 0) await this.#attachmentStorage!.clear(threadId);
      else await this.#attachmentStorage!.save(threadId, attachments);
      this.#persistedAttachments.set(threadId, attachments);
    }).catch(() => {
      // Quota and private-mode failures leave the staged in-memory draft intact.
    });
    this.#attachmentWrites.set(threadId, operation);
    void operation.finally(() => {
      if (this.#attachmentWrites.get(threadId) === operation) {
        this.#attachmentWrites.delete(threadId);
      }
      if (this.#queuedAttachments.get(threadId) === attachments) {
        this.#queuedAttachments.delete(threadId);
      }
    });
    return operation;
  }
}

export const createBrowserComposerDraftController = (): ComposerDraftController => {
  let textStorage: ComposerDraftTextStorage | undefined;
  let attachmentStorage: ComposerDraftAttachmentStorage | undefined;
  try {
    textStorage = browserComposerDraftTextStorage(globalThis.localStorage);
  } catch {
    // Restricted browser contexts retain drafts in memory for this page.
  }
  try {
    if (globalThis.indexedDB !== undefined) {
      attachmentStorage = browserComposerDraftAttachmentStorage(globalThis.indexedDB);
    }
  } catch {
    // A restricted browser may not expose IndexedDB.
  }
  return new ComposerDraftController({
    ...(textStorage === undefined ? {} : { textStorage }),
    ...(attachmentStorage === undefined ? {} : { attachmentStorage }),
  });
};
