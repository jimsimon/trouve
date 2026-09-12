export interface FuzzyTextItem {
  readonly label: string;
  readonly detail: string;
  readonly keywords: string;
}

const normalized = (value: string): string =>
  value
    .normalize("NFKD")
    .replace(/\p{Mark}/gu, "")
    .toLowerCase()
    .trim();

const subsequenceScore = (needle: string, haystack: string): number | undefined => {
  let cursor = 0;
  let first = -1;
  let gaps = 0;
  for (const character of needle) {
    const found = haystack.indexOf(character, cursor);
    if (found < 0) return undefined;
    if (first < 0) first = found;
    gaps += found - cursor;
    cursor = found + 1;
  }
  return first + gaps;
};

interface NormalizedFuzzyTextItem {
  readonly label: string;
  readonly detail: string;
  readonly keywords: string;
}

const tokenScore = (
  token: string,
  item: NormalizedFuzzyTextItem,
): number | undefined => {
  const { label, detail, keywords } = item;
  if (label === token) return 0;
  if (label.startsWith(token)) return 5 + label.length - token.length;
  const labelWord = label.split(/\s+/u).findIndex((word) => word.startsWith(token));
  if (labelWord >= 0) return 20 + labelWord;
  const labelIndex = label.indexOf(token);
  if (labelIndex >= 0) return 35 + labelIndex;
  const detailIndex = detail.indexOf(token);
  if (detailIndex >= 0) return 60 + detailIndex;
  const keywordIndex = keywords.indexOf(token);
  if (keywordIndex >= 0) return 80 + keywordIndex;
  const fuzzy = subsequenceScore(token, label);
  return fuzzy === undefined ? undefined : 110 + fuzzy;
};

/** Worker-safe, token-aware fuzzy filtering with deterministic source-order
 * tie breaking. Keeping the algorithm in a DOM-free module makes the direct
 * and worker paths projection-equivalent. */
export const filterFuzzyTextItems = <T extends FuzzyTextItem>(
  items: readonly T[],
  query: string,
): readonly T[] => {
  const tokens = normalized(query).split(/\s+/u).filter(Boolean);
  if (tokens.length === 0) return items;
  return items
    .map((item, index) => {
      const normalizedItem = {
        label: normalized(item.label),
        detail: normalized(item.detail),
        keywords: normalized(item.keywords),
      };
      let score = 0;
      for (const token of tokens) {
        const next = tokenScore(token, normalizedItem);
        if (next === undefined) return undefined;
        score += next;
      }
      return { item, index, score };
    })
    .filter(
      (entry): entry is { item: T; index: number; score: number } =>
        entry !== undefined,
    )
    .sort((left, right) => left.score - right.score || left.index - right.index)
    .map(({ item }) => item);
};
