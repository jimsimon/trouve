const MAX_OSC52_ENCODED_LENGTH = 256 * 1024;
const MAX_OSC52_TEXT_BYTES = 128 * 1024;

export type Osc52ClipboardRequest =
  | { readonly kind: "copy"; readonly text: string }
  | { readonly kind: "read" }
  | { readonly kind: "invalid" };

/** Classify every OSC 52 request so reads and malformed writes can be
 * explicitly blocked instead of falling through to an engine-dependent
 * default handler. */
export const parseOsc52ClipboardRequest = (
  data: string,
): Osc52ClipboardRequest => {
  if (data.length === 0 || data.length > MAX_OSC52_ENCODED_LENGTH) {
    return { kind: "invalid" };
  }
  const separator = data.indexOf(";");
  if (separator < 0) return { kind: "invalid" };
  const selection = data.slice(0, separator);
  const payload = data.slice(separator + 1);
  if (!/^[cps0-7]*$/u.test(selection) || payload === "") {
    return { kind: "invalid" };
  }
  if (payload === "?") return { kind: "read" };
  if (!/^[A-Za-z0-9+/]*={0,2}$/u.test(payload)) return { kind: "invalid" };

  let binary: string;
  try {
    binary = globalThis.atob(payload);
  } catch {
    return { kind: "invalid" };
  }
  if (binary.length === 0 || binary.length > MAX_OSC52_TEXT_BYTES) {
    return { kind: "invalid" };
  }
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  try {
    return {
      kind: "copy",
      text: new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    };
  } catch {
    return { kind: "invalid" };
  }
};

/** Decode an OSC 52 clipboard request without ever granting it. The returned
 * text must still pass through a user-visible confirmation before the browser
 * clipboard is touched. */
export const decodeOsc52ClipboardRequest = (data: string): string | undefined => {
  const request = parseOsc52ClipboardRequest(data);
  return request.kind === "copy" ? request.text : undefined;
};
