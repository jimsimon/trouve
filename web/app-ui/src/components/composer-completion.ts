export const MAX_COMPOSER_COMPLETIONS = 8;
export const MAX_COMPOSER_COMPLETION_SOURCES = 5_000;

const MAX_COMPLETION_VALUE_LENGTH = 4_096;
const MAX_COMPLETION_DETAIL_LENGTH = 512;
const UNSAFE_COMPLETION_TEXT = /[\u0000-\u001f\u007f]/u;
const UTF8_ENCODER = new TextEncoder();

export interface ComposerCompletionCandidate {
  readonly value: string;
  readonly detail?: string;
}

export interface RankedComposerCompletion {
  readonly value: string;
  readonly detail: string;
  readonly sourceIndex: number;
}

export type ComposerCompletionToken =
  | {
      readonly kind: "command";
      readonly start: 0;
      readonly end: number;
      readonly query: string;
    }
  | {
      readonly kind: "file";
      readonly start: number;
      readonly end: number;
      readonly query: string;
    };

export interface AppliedComposerCompletion {
  readonly draft: string;
  /** DOM selection offset, measured in UTF-16 code units. */
  readonly cursor: number;
}

const isSplitSurrogatePair = (text: string, cursor: number): boolean => {
  if (cursor <= 0 || cursor >= text.length) return false;
  const previous = text.charCodeAt(cursor - 1);
  const next = text.charCodeAt(cursor);
  return previous >= 0xd800 && previous <= 0xdbff && next >= 0xdc00 && next <= 0xdfff;
};

/** Convert a textarea selection offset into the UTF-8 byte offsets used by
 * the protocol and Slint composer. Offsets inside a surrogate pair are
 * rejected instead of silently moving the caret. */
export const domUtf16OffsetToProtocolUtf8 = (
  text: string,
  offset: number,
): number | undefined => {
  if (
    !Number.isInteger(offset)
    || offset < 0
    || offset > text.length
    || isSplitSurrogatePair(text, offset)
  ) return undefined;
  return UTF8_ENCODER.encode(text.slice(0, offset)).length;
};

/** Convert a protocol UTF-8 byte offset back to a textarea UTF-16 offset.
 * Byte offsets in the middle of a multi-byte scalar are invalid. */
export const protocolUtf8OffsetToDomUtf16 = (
  text: string,
  offset: number,
): number | undefined => {
  if (!Number.isInteger(offset) || offset < 0) return undefined;
  if (offset === 0) return 0;

  let utf8Offset = 0;
  let utf16Offset = 0;
  for (const character of text) {
    utf8Offset += UTF8_ENCODER.encode(character).length;
    utf16Offset += character.length;
    if (utf8Offset === offset) return utf16Offset;
    if (utf8Offset > offset) return undefined;
  }
  return undefined;
};

/** Find the completion token currently being edited. Slash commands only
 * activate for a bare first token; file mentions can appear anywhere and are
 * resolved against the caret, matching the Slint composer's contract. */
export const composerCompletionToken = (
  draft: string,
  cursor: number,
): ComposerCompletionToken | undefined => {
  if (
    !Number.isInteger(cursor) ||
    cursor < 0 ||
    cursor > draft.length ||
    isSplitSurrogatePair(draft, cursor)
  ) return undefined;

  if (draft.startsWith("/") && !draft.slice(1).match(/\s/u)) {
    const end = domUtf16OffsetToProtocolUtf8(draft, draft.length);
    return end === undefined
      ? undefined
      : { kind: "command", start: 0, end, query: draft.slice(1) };
  }

  if (cursor === 0) return undefined;
  const start = draft.lastIndexOf("@", cursor - 1);
  if (start < 0) return undefined;
  const query = draft.slice(start + 1, cursor);
  if (/\s/u.test(query)) return undefined;
  const previous = start === 0 ? undefined : draft.slice(0, start).at(-1);
  if (previous !== undefined && !/\s/u.test(previous)) return undefined;
  const protocolStart = domUtf16OffsetToProtocolUtf8(draft, start);
  const protocolEnd = domUtf16OffsetToProtocolUtf8(draft, cursor);
  return protocolStart === undefined || protocolEnd === undefined
    ? undefined
    : { kind: "file", start: protocolStart, end: protocolEnd, query };
};

export const isComposerCompletionTokenCurrent = (
  draft: string,
  cursor: number,
  expected: ComposerCompletionToken,
): boolean => {
  const current = composerCompletionToken(draft, cursor);
  return current !== undefined
    && current.kind === expected.kind
    && current.start === expected.start
    && current.end === expected.end
    && current.query === expected.query;
};

const boundaryBonus = (value: string, position: number): number =>
  position === 0 || /[\s/_.-]/u.test(value[position - 1] ?? "") ? 45 : 0;

const subsequenceScore = (value: string, query: string): number | undefined => {
  let position = -1;
  let score = 0;
  let consecutive = 0;
  for (const character of query) {
    const next = value.indexOf(character, position + 1);
    if (next < 0) return undefined;
    consecutive = next === position + 1 ? consecutive + 1 : 0;
    score += 100 + consecutive * 25 + boundaryBonus(value, next) - Math.min(next, 50);
    position = next;
  }
  return score;
};

const completionScore = (rawValue: string, rawQuery: string): number | undefined => {
  const value = rawValue.toLowerCase();
  const query = rawQuery.trim().toLowerCase();
  if (query === "") return 0;
  if (value === query) return 1_000_000;
  if (value.startsWith(query)) return 900_000 - value.length;

  const basename = value.slice(value.lastIndexOf("/") + 1);
  if (basename.startsWith(query)) return 850_000 - basename.length;

  const substring = value.indexOf(query);
  if (substring >= 0) return 700_000 - substring * 100 - value.length;
  return subsequenceScore(value, query);
};

/** Rank a bounded set of server-provided paths or commands. Invalid control
 * text is excluded so a malicious or unusual filename cannot spoof popup rows. */
export const rankComposerCompletions = (
  candidates: readonly ComposerCompletionCandidate[],
  query: string,
  limit = MAX_COMPOSER_COMPLETIONS,
): readonly RankedComposerCompletion[] => {
  const boundedLimit = Math.max(0, Math.min(MAX_COMPOSER_COMPLETIONS, Math.floor(limit)));
  if (boundedLimit === 0) return [];
  return candidates
    .slice(0, MAX_COMPOSER_COMPLETION_SOURCES)
    .map((candidate, sourceIndex) => {
      const value = candidate.value;
      if (
        value.length === 0 ||
        value.length > MAX_COMPLETION_VALUE_LENGTH ||
        UNSAFE_COMPLETION_TEXT.test(value)
      ) return undefined;
      const score = completionScore(value, query);
      if (score === undefined) return undefined;
      return {
        value,
        detail: (candidate.detail ?? "").replace(/\s+/gu, " ").trim().slice(
          0,
          MAX_COMPLETION_DETAIL_LENGTH,
        ),
        sourceIndex,
        score,
      };
    })
    .filter((candidate): candidate is NonNullable<typeof candidate> => candidate !== undefined)
    .sort((left, right) => right.score - left.score || left.sourceIndex - right.sourceIndex)
    .slice(0, boundedLimit)
    .map(({ value, detail, sourceIndex }) => ({ value, detail, sourceIndex }));
};

export const applyComposerCompletion = (
  draft: string,
  token: ComposerCompletionToken,
  value: string,
): AppliedComposerCompletion | undefined => {
  const start = protocolUtf8OffsetToDomUtf16(draft, token.start);
  const end = protocolUtf8OffsetToDomUtf16(draft, token.end);
  if (start === undefined || end === undefined || start > end) return undefined;

  const prefix = token.kind === "command" ? "/" : "@";
  // A completion may outlive the input event that produced it. Reject stale
  // replacement ranges so a late click or async source result cannot splice
  // unrelated draft text.
  if (draft.slice(start, end) !== `${prefix}${token.query}`) return undefined;
  const normalized = token.kind === "command" ? value.replace(/^\/+/, "") : value;
  const insertion = `${prefix}${normalized} `;
  const nextDraft = `${draft.slice(0, start)}${insertion}${draft.slice(end)}`;
  return { draft: nextDraft, cursor: start + insertion.length };
};
