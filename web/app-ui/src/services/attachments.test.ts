import { describe, expect, it } from "vitest";

import {
  pendingAttachmentPreviewUrl,
  type PendingAttachment,
} from "./attachments.js";

const attachment = (mime: string, data = "iVBORw0KGgo="): PendingAttachment => ({
  upload: { name: "preview.png", mime, data },
  size: 8,
});

describe("pending attachment previews", () => {
  it("builds a local data URL for safely typed images", () => {
    const pending = attachment("image/PNG");
    expect(pendingAttachmentPreviewUrl(pending)).toBe(
      "data:image/png;base64,iVBORw0KGgo=",
    );
    expect(pendingAttachmentPreviewUrl(pending)).toBe(
      pendingAttachmentPreviewUrl(pending),
    );
  });

  it("does not create previews for files or malformed image MIME types", () => {
    expect(pendingAttachmentPreviewUrl(attachment("text/plain"))).toBeUndefined();
    expect(pendingAttachmentPreviewUrl(attachment("image/png;evil"))).toBeUndefined();
    expect(pendingAttachmentPreviewUrl(attachment("image/png", ""))).toBeUndefined();
  });
});
