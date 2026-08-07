import type { components as ProtocolComponents } from "../generated/protocol.js";
import type {
  ThreadChatItem,
  TurnState,
} from "../state/thread-view-model.js";
import { utf8Length, utf8Prefix } from "../services/utf8-text.js";

type Attachment = ProtocolComponents["schemas"]["Attachment"];
type Usage = ProtocolComponents["schemas"]["Usage"];

export type ChatCopyResult = "copied" | "failed" | "unavailable";

export interface ClipboardTextWriter {
  writeText(text: string): Promise<void> | void;
}

export interface ChatPresentationIndex {
  readonly latestTurn: number | undefined;
  readonly lastAssistantIds: ReadonlySet<string>;
  readonly turnStates: ReadonlyMap<number, TurnState>;
  readonly turnsWithAssistant: ReadonlySet<number>;
}

/** First meaningful line for a collapsed card, bounded exactly like the
 * native renderer so a model-generated single-line response cannot inflate
 * every virtual row's DOM and accessibility name. */
export const collapsedChatPreview = (content: string): string => {
  const line = content.split(/\r?\n/u).find((candidate) => candidate.trim() !== "")?.trim() ?? "";
  return utf8Length(line) <= 120 ? line : `${utf8Prefix(line, 119)}…`;
};

/** Approximate the visible text of a styled Markdown response for its quick
 * copy action. The response context menu deliberately retains the original
 * source for its separate "Copy as markdown" command. */
export const assistantCopyText = (markdown: string): string => {
  let fenced = false;
  const output: string[] = [];
  for (const sourceLine of markdown.split("\n")) {
    if (/^\s*(```+|~~~+)/u.test(sourceLine)) {
      fenced = !fenced;
      continue;
    }
    if (fenced) {
      output.push(sourceLine);
      continue;
    }
    if (/^\s*\|?\s*:?-{3,}:?\s*(?:\|\s*:?-{3,}:?\s*)+\|?\s*$/u.test(sourceLine)) {
      continue;
    }
    let line = sourceLine
      .replace(/^\s{0,3}#{1,6}\s+/u, "")
      .replace(/^(\s*)[-+*]\s+/u, "$1•  ")
      .replace(/^\s*>\s?/u, "")
      .replace(/^\s*\|(.*)\|\s*$/u, "$1")
      .replaceAll("**", "")
      .replaceAll("`", "");
    if (sourceLine.includes("|")) {
      line = line.split("|").map((cell) => cell.trim()).join(" | ");
    }
    output.push(line);
  }
  return output.join("\n");
};

/** Build the small amount of turn-level state needed while rendering a window. */
export const indexChatPresentation = (
  items: readonly ThreadChatItem[],
): ChatPresentationIndex => {
  let latestTurn: number | undefined;
  const lastAssistantIdsByTurn = new Map<number, string>();
  const turnStates = new Map<number, TurnState>();
  const turnsWithAssistant = new Set<number>();

  for (const item of items) {
    if ("turn" in item) {
      latestTurn = latestTurn === undefined
        ? item.turn
        : Math.max(latestTurn, item.turn);
    }
    if (item.kind === "assistant") {
      lastAssistantIdsByTurn.set(item.turn, item.id);
      turnsWithAssistant.add(item.turn);
    } else if (item.kind === "turn-status") {
      turnStates.set(item.turn, item.state);
    }
  }

  return {
    latestTurn,
    lastAssistantIds: new Set(lastAssistantIdsByTurn.values()),
    turnStates,
    turnsWithAssistant,
  };
};

/** Match the compact duration labels used by existing chat cards. */
export const formatTurnDuration = (durationMs: number): string => {
  const milliseconds = Math.max(0, Math.floor(durationMs));
  if (milliseconds < 1_000) return `${milliseconds}ms`;

  const seconds = Math.floor(milliseconds / 1_000);
  if (seconds < 60) return `${seconds}s`;

  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) {
    return `${minutes}m ${remainingSeconds.toString().padStart(2, "0")}s`;
  }

  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `${hours}h ${remainingMinutes.toString().padStart(2, "0")}m`;
};

export const formatTurnMetadata = (
  usage: Usage,
  durationMs: number | undefined,
): string => {
  const parts = [
    `${usage.input_tokens} in / ${usage.output_tokens} out tokens`,
  ];
  if (
    usage.cost_usd !== undefined
    && usage.cost_usd !== null
    && Number.isFinite(usage.cost_usd)
    && usage.cost_usd > 0
  ) {
    parts.push(`$${usage.cost_usd.toFixed(4)}`);
  }
  if (durationMs !== undefined && Number.isFinite(durationMs)) {
    parts.push(formatTurnDuration(durationMs));
  }
  return parts.join(" · ");
};

/**
 * Return an encoded, same-origin protocol path. Attachment IDs never become
 * arbitrary URLs, even if a malformed replay contains path separators.
 */
export const protocolAttachmentPath = (
  attachment: Pick<Attachment, "id">,
): string | undefined => {
  const { id } = attachment;
  if (
    id === ""
    || id.length > 512
    || /[\u0000-\u001f\u007f]/u.test(id)
  ) return undefined;
  try {
    return `/v1/attachments/${encodeURIComponent(id)}`;
  } catch {
    return undefined;
  }
};

export const isImageAttachment = (
  attachment: Pick<Attachment, "mime">,
): boolean => attachment.mime.toLowerCase().startsWith("image/");

export const formatAttachmentBytes = (bytes: number): string => {
  const safeBytes = Number.isFinite(bytes) ? Math.max(0, Math.floor(bytes)) : 0;
  if (safeBytes < 1_024) return `${safeBytes} B`;
  if (safeBytes < 1_024 * 1_024) return `${Math.ceil(safeBytes / 1_024)} KB`;
  return `${(safeBytes / (1_024 * 1_024)).toFixed(1)} MB`;
};

export const copyChatText = async (
  text: string,
  clipboard: ClipboardTextWriter | undefined,
): Promise<ChatCopyResult> => {
  if (clipboard === undefined || text === "") return "unavailable";
  try {
    await clipboard.writeText(text);
    return "copied";
  } catch {
    return "failed";
  }
};

export const copyActionLabel = (
  result: ChatCopyResult | undefined,
): string => {
  switch (result) {
    case "copied":
      return "Copied";
    case "failed":
      return "Copy failed";
    case "unavailable":
      return "Clipboard unavailable";
    default:
      return "Copy";
  }
};
