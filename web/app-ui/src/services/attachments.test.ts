import { describe, expect, it } from "vitest";

import {
  AttachmentEncodingError,
  encodeAttachment,
  MAX_ATTACHMENT_BYTES,
} from "./attachments.js";

describe("attachment encoding", () => {
  it("preserves bytes using standard padded base64 and a safe MIME", async () => {
    await expect(
      encodeAttachment(
        new File([new Uint8Array([0, 1, 2, 253, 254, 255])], "fixture.bin", {
          type: "application/x-fixture",
        }),
        "fallback.bin",
      ),
    ).resolves.toEqual({
      upload: {
        name: "fixture.bin",
        mime: "application/x-fixture",
        data: "AAEC/f7/",
      },
      size: 6,
    });
  });

  it("uses a fallback name/MIME and rejects empty or oversized files", async () => {
    await expect(
      encodeAttachment(new File([new Uint8Array([7])], "", { type: "invalid mime" }), "paste.bin"),
    ).resolves.toMatchObject({
      upload: { name: "paste.bin", mime: "application/octet-stream", data: "Bw==" },
    });
    await expect(encodeAttachment(new File([], "empty"), "empty")).rejects.toEqual(
      expect.objectContaining<Partial<AttachmentEncodingError>>({ kind: "empty" }),
    );
    await expect(
      encodeAttachment(
        new File([new Uint8Array(MAX_ATTACHMENT_BYTES + 1)], "large.bin"),
        "large.bin",
      ),
    ).rejects.toEqual(
      expect.objectContaining<Partial<AttachmentEncodingError>>({ kind: "too-large" }),
    );
  });
});
