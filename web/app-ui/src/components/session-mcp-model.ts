import type { ProtocolMcpServerInfo } from "../services/protocol-client.js";

const shellWord = (value: string): string =>
  /^[A-Za-z0-9_./:@%+=,-]+$/u.test(value)
    ? value
    : `'${value.replaceAll("'", `'\\''`)}'`;

export const sessionMcpCommandLine = (server: ProtocolMcpServerInfo): string =>
  [server.command, ...(server.args ?? [])].map(shellWord).join(" ");

export interface ParsedMcpCommandLine {
  readonly command: string;
  readonly args: readonly string[];
}

/** Parse the same small POSIX-style command-line shape accepted by the Slint
 * settings form. This only tokenizes data for the protocol; it never invokes
 * a shell. */
export const parseMcpCommandLine = (value: string): ParsedMcpCommandLine | undefined => {
  const words: string[] = [];
  let word = "";
  let started = false;
  let quote: "single" | "double" | undefined;
  let escaped = false;

  const finish = (): void => {
    if (!started) return;
    words.push(word);
    word = "";
    started = false;
  };

  for (const character of value) {
    if (escaped) {
      word += character;
      started = true;
      escaped = false;
      continue;
    }
    if (quote === "single") {
      if (character === "'") quote = undefined;
      else word += character;
      started = true;
      continue;
    }
    if (quote === "double") {
      if (character === '"') quote = undefined;
      else if (character === "\\") escaped = true;
      else word += character;
      started = true;
      continue;
    }
    if (/\s/u.test(character)) {
      finish();
    } else if (character === "'") {
      quote = "single";
      started = true;
    } else if (character === '"') {
      quote = "double";
      started = true;
    } else if (character === "\\") {
      escaped = true;
      started = true;
    } else {
      word += character;
      started = true;
    }
  }
  if (quote !== undefined || escaped) return undefined;
  finish();
  const [command, ...args] = words;
  return command === undefined || command === "" ? undefined : { command, args };
};

export const sessionMcpEnvironmentLines = (
  server: ProtocolMcpServerInfo,
): readonly string[] => Object.entries(server.env ?? {})
  .sort(([left], [right]) => left.localeCompare(right))
  .map(([key, value]) => `${key}=${value}`);

export const sessionMcpHealthLabel = (health: string): string => {
  if (health === "ok") return "Ready";
  if (health === "disabled") return "Disabled by a higher-priority layer";
  if (health === "untrusted") return "Untrusted workspace definition";
  if (health === "error") return "Needs attention";
  return "Health not checked";
};
