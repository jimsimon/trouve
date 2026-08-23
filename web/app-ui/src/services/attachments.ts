import type { ProtocolAttachmentUpload } from "./protocol-client.js";

export const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
export const MAX_PENDING_ATTACHMENT_BYTES = 20 * 1024 * 1024;
export const MAX_PENDING_ATTACHMENTS = 4;

export interface PendingAttachment {
  readonly upload: ProtocolAttachmentUpload;
  readonly size: number;
}

/** Coalesces repeated attachment actions until their first operation settles. */
export class AttachmentOperationCapacityError extends Error {
  constructor() {
    super("too many attachment operations are pending");
    this.name = "AttachmentOperationCapacityError";
  }
}

export class PendingAttachmentOperations {
  readonly #pending = new Map<string, Promise<unknown>>();
  readonly #queue: Array<() => void> = [];
  #active = 0;

  constructor(
    readonly maxConcurrent = Number.POSITIVE_INFINITY,
    readonly maxPending = Number.POSITIVE_INFINITY,
  ) {}

  run<T>(key: string, operation: () => Promise<T>): Promise<T> | undefined {
    if (this.#pending.has(key)) return undefined;
    if (this.#pending.size >= this.maxPending) {
      return Promise.reject(new AttachmentOperationCapacityError());
    }
    let resolve!: (value: T | PromiseLike<T>) => void;
    let reject!: (reason?: unknown) => void;
    const pending = new Promise<T>((accept, refuse) => {
      resolve = accept;
      reject = refuse;
    });
    this.#pending.set(key, pending);
    this.#queue.push(() => {
      this.#active += 1;
      const finish = (): void => {
        if (this.#pending.get(key) === pending) this.#pending.delete(key);
        this.#active -= 1;
        this.#drain();
      };
      void Promise.resolve()
        .then(operation)
        .then(
          (value) => {
            finish();
            resolve(value);
          },
          (error: unknown) => {
            finish();
            reject(error);
          },
        );
    });
    this.#drain();
    return pending;
  }

  #drain(): void {
    while (this.#active < this.maxConcurrent) {
      const start = this.#queue.shift();
      if (start === undefined) return;
      start();
    }
  }
}

const previewUrls = new WeakMap<PendingAttachment, string>();

export const PREVIEWABLE_VIDEO_MIMES: ReadonlySet<string> = new Set([
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
  "video/x-matroska",
  "video/x-msvideo",
]);

export const isVideoMime = (mime: string): boolean =>
  PREVIEWABLE_VIDEO_MIMES.has(mime.toLowerCase());

/** Return the decoded byte length of canonical padded base64 without
 * allocating its binary representation. Alphabet validation remains with the
 * consumer that performs the single required decode. */
export const base64DecodedByteLength = (data: string): number | undefined => {
  if (data.length === 0 || data.length % 4 !== 0) return undefined;
  const padding = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
  const payloadLength = data.length - padding;
  if (payloadLength === 0 || data.slice(0, payloadLength).includes("=")) return undefined;
  const size = (data.length / 4) * 3 - padding;
  return size > 0 ? size : undefined;
};

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
