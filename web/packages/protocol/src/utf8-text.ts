const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

export const utf8Length = (text: string): number => encoder.encode(text).byteLength;

/** Longest complete-scalar prefix fitting within a UTF-8 byte budget. */
export const utf8Prefix = (text: string, maximumBytes: number): string => {
  const limit = Math.max(0, Math.floor(maximumBytes));
  const encoded = encoder.encode(text);
  if (encoded.byteLength <= limit) return text;
  let end = Math.min(limit, encoded.byteLength);
  while (end > 0 && (encoded[end]! & 0xc0) === 0x80) end -= 1;
  return decoder.decode(encoded.subarray(0, end));
};
