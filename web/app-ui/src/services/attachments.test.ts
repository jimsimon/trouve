import { describe, expect, it } from "vitest";

import {
  AttachmentOperationCapacityError,
  base64DecodedByteLength,
  isVideoMime,
  PendingAttachmentOperations,
  pendingAttachmentPreviewUrl,
  type PendingAttachment,
} from "./attachments.js";

const attachment = (mime: string, data = "iVBORw0KGgo="): PendingAttachment => ({
  upload: { name: "preview.png", mime, data },
  size: 8,
});

describe("pending attachment previews", () => {
  it("derives padded base64 sizes without decoding the payload", () => {
    expect(base64DecodedByteLength("dmlkZW8=")).toBe(5);
    expect(base64DecodedByteLength("YQ==")).toBe(1);
    expect(base64DecodedByteLength("")).toBeUndefined();
    expect(base64DecodedByteLength("YQ=")).toBeUndefined();
  });

  it("coalesces duplicate attachment operations until the first settles", async () => {
    const operations = new PendingAttachmentOperations();
    let resolve!: () => void;
    const first = operations.run("video-source", () => new Promise<void>((done) => {
      resolve = done;
    }));

    expect(first).toBeDefined();
    expect(operations.run("video-source", async () => {})).toBeUndefined();
    const other = operations.run("other-source", async () => {});
    expect(other).toBeDefined();
    await other;
    resolve();
    await first;
    await Promise.resolve();
    expect(operations.run("video-source", async () => {})).toBeDefined();
  });

  it("bounds distinct attachment operations before their downloads start", async () => {
    const operations = new PendingAttachmentOperations(1, 8);
    const resolvers: Array<() => void> = [];
    let active = 0;
    let peakActive = 0;
    const pending = Array.from({ length: 8 }, (_, index) =>
      operations.run(`video-${index}`, () => new Promise<void>((resolve) => {
        active += 1;
        peakActive = Math.max(peakActive, active);
        resolvers.push(() => {
          active -= 1;
          resolve();
        });
      })),
    );

    const rejected = operations.run("video-8", async () => {});
    await expect(rejected).rejects.toBeInstanceOf(AttachmentOperationCapacityError);
    for (const operation of pending) {
      await Promise.resolve();
      resolvers.shift()?.();
      await operation;
    }
    expect(peakActive).toBe(1);
  });

  it("builds a local data URL for safely typed images", () => {
    const pending = attachment("image/PNG");
    expect(pendingAttachmentPreviewUrl(pending)).toBe(
      "data:image/png;base64,iVBORw0KGgo=",
    );
    expect(pendingAttachmentPreviewUrl(pending)).toBe(
      pendingAttachmentPreviewUrl(pending),
    );
  });

  it("builds previews for video formats handed to an external player", () => {
    const pending = attachment("video/MP4");
    expect(isVideoMime(pending.upload.mime)).toBe(true);
    expect(pendingAttachmentPreviewUrl(pending)).toBe(
      "data:video/mp4;base64,iVBORw0KGgo=",
    );
    expect(isVideoMime("video/svg+xml")).toBe(false);
  });

  it("does not create previews for files or malformed media MIME types", () => {
    expect(pendingAttachmentPreviewUrl(attachment("text/plain"))).toBeUndefined();
    expect(pendingAttachmentPreviewUrl(attachment("image/png;evil"))).toBeUndefined();
    expect(pendingAttachmentPreviewUrl(attachment("video/svg+xml"))).toBeUndefined();
    expect(pendingAttachmentPreviewUrl(attachment("image/png", ""))).toBeUndefined();
  });
});
