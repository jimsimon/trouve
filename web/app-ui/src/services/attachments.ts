import type { ProtocolAttachmentUpload } from "./protocol-client.js";

export const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
export const MAX_PENDING_ATTACHMENT_BYTES = 20 * 1024 * 1024;
export const MAX_PENDING_ATTACHMENTS = 4;

export interface PendingAttachment {
  readonly upload: ProtocolAttachmentUpload;
  readonly size: number;
}

/** Coalesces repeated attachment actions until their first operation settles. */
export class PendingAttachmentOperations {
  readonly #pending = new Map<string, Promise<unknown>>();

  run<T>(key: string, operation: () => Promise<T>): Promise<T> | undefined {
    if (this.#pending.has(key)) return undefined;
    const pending = operation();
    this.#pending.set(key, pending);
    const finish = (): void => {
      if (this.#pending.get(key) === pending) this.#pending.delete(key);
    };
    void pending.then(finish, finish);
    return pending;
  }
}

const previewUrls = new WeakMap<PendingAttachment, string>();

const PREVIEWABLE_VIDEO_MIMES = new Set([
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
  "video/x-matroska",
  "video/x-msvideo",
]);

export const isVideoMime = (mime: string): boolean =>
  PREVIEWABLE_VIDEO_MIMES.has(mime.toLowerCase());

/** A CSP-compatible local preview for media that has already been encoded for
 * upload. Files and malformed MIME types deliberately have no URL. */
export const pendingAttachmentPreviewUrl = (
  attachment: PendingAttachment,
): string | undefined => {
  const mime = attachment.upload.mime.toLowerCase();
  const previewable = /^image\/[a-z0-9!#$&^_.+-]+$/iu.test(mime)
    || isVideoMime(mime);
  if (!previewable || attachment.upload.data === "") {
    return undefined;
  }
  const cached = previewUrls.get(attachment);
  if (cached !== undefined) return cached;
  const url = `data:${mime};base64,${attachment.upload.data}`;
  previewUrls.set(attachment, url);
  return url;
};

export class AttachmentEncodingError extends Error {
  constructor(readonly kind: "empty" | "too-large" | "read-failed") {
    super(kind);
    this.name = "AttachmentEncodingError";
  }
}

const safeMime = (mime: string): string =>
  /^[a-z0-9!#$&^_.+-]+\/[a-z0-9!#$&^_.+-]+$/i.test(mime)
    ? mime.toLowerCase()
    : "application/octet-stream";

/** Encode one user-selected browser File into the protocol's bounded upload.
 * Conversion is chunked to avoid spreading multi-megabyte arrays onto the
 * JavaScript call stack. */
export const encodeAttachment = async (
  file: File,
  fallbackName: string,
): Promise<PendingAttachment> => {
  if (file.size === 0) throw new AttachmentEncodingError("empty");
  if (file.size > MAX_ATTACHMENT_BYTES) {
    throw new AttachmentEncodingError("too-large");
  }
  let bytes: Uint8Array;
  try {
    bytes = new Uint8Array(await file.arrayBuffer());
  } catch {
    throw new AttachmentEncodingError("read-failed");
  }
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 8_192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8_192));
  }
  return {
    upload: {
      name: file.name.trim() === "" ? fallbackName : file.name,
      mime: safeMime(file.type),
      data: globalThis.btoa(binary),
    },
    size: file.size,
  };
};
