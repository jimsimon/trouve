/** Per-tool retained output budget. Tool output is durable and may be
 * arbitrarily large, so chat projections keep only the most recent UTF-8
 * bytes instead of letting every historical tool card grow without bound. */
export const MAX_TOOL_OUTPUT_BYTES = 64 * 1024;

export const TOOL_OUTPUT_OMITTED_MESSAGE = "… earlier tool output omitted …\n";

export interface ToolOutputBuffer {
  /** A valid-Unicode suffix of the tool's output. */
  readonly text: string;
  /** UTF-8 byte length of `text`. */
  readonly bytes: number;
  /** True once any earlier bytes have been discarded. */
  readonly omitted: boolean;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const encodedBuffers = new WeakMap<ToolOutputBuffer, Uint8Array>();

const outputBuffer = (
  text: string,
  encoded: Uint8Array,
  omitted: boolean,
): ToolOutputBuffer => {
  const buffer = Object.freeze({ text, bytes: encoded.byteLength, omitted });
  encodedBuffers.set(buffer, encoded);
  return buffer;
};

export const emptyToolOutput = (): ToolOutputBuffer =>
  outputBuffer("", new Uint8Array(), false);

/** Return the longest valid-Unicode suffix whose UTF-8 encoding fits. */
const utf8Tail = (
  text: string,
  encoded: Uint8Array,
  maxBytes: number,
): { readonly text: string; readonly bytes: number } => {
  if (encoded.byteLength <= maxBytes) {
    return { text, bytes: encoded.byteLength };
  }
  if (maxBytes <= 0) return { text: "", bytes: 0 };

  let start = encoded.byteLength - maxBytes;
  // A UTF-8 continuation byte cannot begin a decoded suffix. Skipping at
  // most three bytes preserves the next complete scalar value.
  while (start < encoded.byteLength && (encoded[start]! & 0xc0) === 0x80) {
    start += 1;
  }
  const tail = encoded.subarray(start);
  return { text: decoder.decode(tail), bytes: tail.byteLength };
};

/**
 * Append one durable `tool.output` chunk while retaining a bounded tail.
 *
 * Contract:
 * - `text` is always a suffix of all output seen so far;
 * - `bytes` never exceeds `maxBytes` and counts UTF-8 bytes, not UTF-16 code
 *   units;
 * - `omitted` is sticky and becomes true exactly when nonempty earlier output
 *   is discarded; and
 * - empty chunks are a no-op and return the original buffer.
 */
export const appendBoundedToolOutput = (
  current: ToolOutputBuffer,
  chunk: string,
  maxBytes = MAX_TOOL_OUTPUT_BYTES,
): ToolOutputBuffer => {
  if (chunk === "") return current;
  const limit = Math.max(0, Math.floor(maxBytes));
  const encodedChunk = encoder.encode(chunk);

  if (encodedChunk.byteLength >= limit) {
    const tail = utf8Tail(chunk, encodedChunk, limit);
    const tailBytes = encodedChunk.subarray(encodedChunk.byteLength - tail.bytes);
    return outputBuffer(
      tail.text,
      tailBytes,
      current.omitted ||
        current.bytes > 0 ||
        tail.bytes < encodedChunk.byteLength,
    );
  }

  const retainedBudget = limit - encodedChunk.byteLength;
  let retained = { text: current.text, bytes: current.bytes };
  const currentEncoded = encodedBuffers.get(current) ?? encoder.encode(current.text);
  let retainedEncoded = currentEncoded;
  let omitted = current.omitted;
  if (current.bytes > retainedBudget) {
    retained = utf8Tail(
      current.text,
      currentEncoded,
      retainedBudget,
    );
    retainedEncoded = currentEncoded.subarray(currentEncoded.byteLength - retained.bytes);
    omitted = true;
  }
  const combined = new Uint8Array(retainedEncoded.byteLength + encodedChunk.byteLength);
  combined.set(retainedEncoded);
  combined.set(encodedChunk, retainedEncoded.byteLength);
  return outputBuffer(`${retained.text}${chunk}`, combined, omitted);
};
