import { describe, expect, it } from "vitest";

import {
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
