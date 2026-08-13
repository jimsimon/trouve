import type { ProtocolMcpServerInfo } from "../services/protocol-client.js";

export const MCP_CONFIG_CHANGED_EVENT = "trouve-mcp-config-changed";

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

export interface ImportedMcpServer {
  readonly name: string;
  readonly command: string;
  readonly args: readonly string[];
  readonly env: Readonly<Record<string, string>>;
  readonly enabled: boolean;
}

const jsonObject = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;

/** Parse the JSON shapes used by trouve, Cursor, Claude Desktop, and VS Code.
 * Only stdio servers can be imported because trouve's managed MCP settings do
 * not currently expose remote HTTP transports. The whole document is
 * validated before the caller writes any server. */
export const parseMcpConfigJson = (source: string): readonly ImportedMcpServer[] => {
  let parsed: unknown;
  try {
    parsed = JSON.parse(source);
  } catch (error) {
    const detail = error instanceof Error ? error.message : "invalid JSON";
    throw new Error(`The MCP config is not valid JSON: ${detail}`);
  }
  const root = jsonObject(parsed);
  if (root === undefined) throw new Error("The MCP config must be a JSON object.");
  const rawServers = root.mcpServers ?? root.servers;
  const servers = jsonObject(rawServers);
  if (servers === undefined) {
    throw new Error('The MCP config must contain an "mcpServers" or "servers" object.');
  }

  const imported: ImportedMcpServer[] = [];
  for (const [rawName, rawConfig] of Object.entries(servers)) {
    const name = rawName.trim();
    if (name === "") throw new Error("MCP server names cannot be empty.");
    const config = jsonObject(rawConfig);
    if (config === undefined) throw new Error(`MCP server “${name}” must be an object.`);
    if (config.url !== undefined || config.type === "http" || config.type === "sse") {
      throw new Error(`MCP server “${name}” uses a remote transport; only stdio servers can be imported.`);
    }
    if (typeof config.command !== "string" || config.command.trim() === "") {
      throw new Error(`MCP server “${name}” must have a non-empty string command.`);
    }
    const args = config.args ?? [];
    if (!Array.isArray(args) || !args.every((argument) => typeof argument === "string")) {
      throw new Error(`MCP server “${name}” args must be an array of strings.`);
    }
    const rawEnv = config.env ?? {};
    const env = jsonObject(rawEnv);
    if (env === undefined || !Object.values(env).every((value) => typeof value === "string")) {
      throw new Error(`MCP server “${name}” env must contain only string values.`);
    }
    imported.push({
      name,
      command: config.command,
      args: [...args] as string[],
      env: Object.fromEntries(Object.entries(env) as [string, string][]),
      enabled: config.disabled !== true,
    });
  }
  if (imported.length === 0) throw new Error("The MCP config does not contain any servers.");
  return imported;
};

/** Parse the small POSIX-style command-line shape accepted by the product
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
