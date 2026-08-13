import { describe, expect, it, vi } from "vitest";

import type { PendingAttachment } from "./attachments.js";
import {
  browserComposerDraftTextStorage,
  ComposerDraftController,
  normalizeComposerDraft,
  type ComposerDraftAttachmentStorage,
  type ComposerDraftTextStorage,
} from "./composer-drafts.js";

const attachment = (name = "preview.png"): PendingAttachment => Object.freeze({
  upload: Object.freeze({
    name,
    mime: "image/png",
    data: "aGVsbG8=",
  }),
  size: 5,
});

class MemoryTextStorage implements ComposerDraftTextStorage {
  readonly values = new Map<string, { readonly text: string; readonly cursor: number }>();

  load(threadId: string) {
    return this.values.get(threadId);
  }

  save(threadId: string, draft: { readonly text: string; readonly cursor: number }): void {
    this.values.set(threadId, { ...draft });
  }

  clear(threadId: string): void {
    this.values.delete(threadId);
  }
}

class MemoryAttachmentStorage implements ComposerDraftAttachmentStorage {
  readonly values = new Map<string, readonly PendingAttachment[]>();

  async load(threadId: string): Promise<readonly PendingAttachment[]> {
    return this.values.get(threadId) ?? [];
  }

  async save(
    threadId: string,
    attachments: readonly PendingAttachment[],
  ): Promise<void> {
    this.values.set(threadId, [...attachments]);
  }

  async clear(threadId: string): Promise<void> {
    this.values.delete(threadId);
  }
}

describe("composer draft persistence", () => {
  it("normalizes cursor positions and rejects malformed attachment payloads", () => {
    expect(normalizeComposerDraft({
      text: "hello",
      cursor: 99,
      attachments: [attachment(), { upload: { data: "bad" }, size: -1 }],
    })).toEqual({
      text: "hello",
      cursor: 5,
      attachments: [attachment()],
    });
    expect(normalizeComposerDraft(null)).toEqual({
      text: "",
      cursor: 0,
      attachments: [],
    });
  });

  it("keeps drafts isolated per thread and restores attachments after reload", async () => {
    const textStorage = new MemoryTextStorage();
    const attachmentStorage = new MemoryAttachmentStorage();
    const controller = new ComposerDraftController({ textStorage, attachmentStorage });
    await controller.save("thread-a", {
      text: "first draft",
      cursor: 5,
      attachments: [attachment()],
    });
    await controller.save("thread-b", {
      text: "second draft",
      cursor: 12,
      attachments: [],
    });

    const reloaded = new ComposerDraftController({ textStorage, attachmentStorage });
    expect(reloaded.read("thread-a")).toMatchObject({
      text: "first draft",
      cursor: 5,
      attachments: [],
    });
    expect(await reloaded.hydrate("thread-a")).toEqual({
      text: "first draft",
      cursor: 5,
      attachments: [attachment()],
    });
    expect(reloaded.read("thread-b")).toMatchObject({
      text: "second draft",
      cursor: 12,
    });

    await reloaded.clear("thread-a");
    const afterSubmission = new ComposerDraftController({ textStorage, attachmentStorage });
    expect(afterSubmission.read("thread-a")).toMatchObject({ text: "", attachments: [] });
    expect(await afterSubmission.hydrate("thread-a")).toMatchObject({
      text: "",
      attachments: [],
    });
  });

  it("queues a return to persisted attachments behind an in-flight write", async () => {
    const persisted = attachment("persisted-a.png");
    const replacement = attachment("pending-b.png");
    let releaseReplacement!: () => void;
    const replacementGate = new Promise<void>((resolve) => {
      releaseReplacement = resolve;
    });
    const attachmentStorage = new MemoryAttachmentStorage();
    attachmentStorage.values.set("thread-a", [persisted]);
    const saves: string[] = [];
    attachmentStorage.save = async (threadId, attachments) => {
      const name = attachments[0]?.upload.name ?? "empty";
      saves.push(name);
      if (name === replacement.upload.name) await replacementGate;
      attachmentStorage.values.set(threadId, [...attachments]);
    };
    const controller = new ComposerDraftController({ attachmentStorage });
    await controller.hydrate("thread-a");

    const writeReplacement = controller.save("thread-a", {
      text: "B",
      cursor: 1,
      attachments: [replacement],
    });
    await vi.waitFor(() => expect(saves).toEqual([replacement.upload.name]));
    const restorePersisted = controller.save("thread-a", {
      text: "A",
      cursor: 1,
      attachments: [persisted],
    });
    releaseReplacement();
    await Promise.all([writeReplacement, restorePersisted]);

    expect(saves).toEqual([replacement.upload.name, persisted.upload.name]);
    expect(attachmentStorage.values.get("thread-a")).toEqual([persisted]);
  });

  it("permanently discards late stage, persist, and hydrate work for a deleted thread", async () => {
    const textStorage = new MemoryTextStorage();
    textStorage.values.set("thread-a", { text: "stored", cursor: 6 });
    const attachmentStorage = new MemoryAttachmentStorage();
    const storedAttachment = attachment("stored.png");
    let resolveLoad!: (attachments: readonly PendingAttachment[]) => void;
    const pendingLoad = new Promise<readonly PendingAttachment[]>((resolve) => {
      resolveLoad = resolve;
    });
    attachmentStorage.load = vi.fn(() => pendingLoad);
    const controller = new ComposerDraftController({ textStorage, attachmentStorage });
    const hydrate = controller.hydrate("thread-a");

    const discard = controller.discard("thread-a");
    expect(controller.stage("thread-a", {
      text: "late text",
      cursor: 9,
      attachments: [attachment("late.png")],
    })).toEqual({ text: "", cursor: 0, attachments: [] });
    const latePersist = controller.persist("thread-a");
    resolveLoad([storedAttachment]);
    await Promise.all([discard, hydrate, latePersist]);

    expect(controller.read("thread-a")).toEqual({ text: "", cursor: 0, attachments: [] });
    expect(await controller.hydrate("thread-a")).toEqual({
      text: "",
      cursor: 0,
      attachments: [],
    });
    expect(textStorage.values.has("thread-a")).toBe(false);
    expect(attachmentStorage.values.has("thread-a")).toBe(false);
  });

  it("uses bounded versioned browser text records and removes submitted drafts", () => {
    const values = new Map<string, string>();
    const storage = {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => void values.delete(key),
    };
    const first = browserComposerDraftTextStorage(storage, () => 10);
    first.save("thread-a", { text: "hello", cursor: 2 });
    first.save("thread-b", { text: "world", cursor: 99 });

    const reloaded = browserComposerDraftTextStorage(storage, () => 20);
    expect(reloaded.load("thread-a")).toEqual({ text: "hello", cursor: 2 });
    expect(reloaded.load("thread-b")).toEqual({ text: "world", cursor: 5 });
    reloaded.clear("thread-a");
    expect(reloaded.load("thread-a")).toBeUndefined();
    expect(reloaded.load("thread-b")).toEqual({ text: "world", cursor: 5 });
  });
});
