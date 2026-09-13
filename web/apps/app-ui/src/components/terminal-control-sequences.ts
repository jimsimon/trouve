const MAX_TERMINAL_TITLE_BYTES = 512;
const TERMINAL_CONTROL_CHARACTER = /\p{Cc}/u;
const terminalTitleEncoder = new TextEncoder();
const terminalTitleDecoder = new TextDecoder();

/** Match the native VT callback boundary: take at most 512 UTF-8 bytes,
 * decode lossily, and remove control characters before exposing a title. */
export const normalizeTerminalTitle = (title: string): string => {
  const bytes = terminalTitleEncoder.encode(title);
  const bounded = bytes.subarray(0, MAX_TERMINAL_TITLE_BYTES);
  return [...terminalTitleDecoder.decode(bounded)]
    .filter((character) => !TERMINAL_CONTROL_CHARACTER.test(character))
    .join("");
};

export interface TerminalRequestedSize {
  readonly cols: number;
  readonly rows: number;
}

/** Parse CSI 8 ; rows ; cols t without treating any other xterm window
 * operation as a shell-requested PTY size. */
export const terminalRequestedSize = (
  params: readonly (number | readonly number[])[],
): TerminalRequestedSize | undefined => {
  const scalar = (
    value: number | readonly number[] | undefined,
  ): number | undefined => typeof value === "number" ? value : undefined;
  if (scalar(params[0]) !== 8) return undefined;
  const rows = scalar(params[1]);
  const cols = scalar(params[2]);
  if (
    rows === undefined ||
    cols === undefined ||
    !Number.isSafeInteger(rows) ||
    !Number.isSafeInteger(cols) ||
    rows <= 0 ||
    cols <= 0
  ) {
    return undefined;
  }
  return { cols, rows };
};
