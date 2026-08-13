/** Largest source prefix ending at a complete blank line outside a fenced
 * code block. Markdown parsing can restart there, so streamed deltas normally
 * reprocess only the unstable tail instead of the full response. */
export const stableMarkdownPrefixLength = (source: string): number => {
  let offset = 0;
  let stable = 0;
  let fence: { readonly marker: "`" | "~"; readonly length: number } | undefined;
  for (const match of source.matchAll(/.*(?:\n|$)/gu)) {
    const raw = match[0];
    if (raw === "") continue;
    offset += raw.length;
    const line = raw.endsWith("\n") ? raw.slice(0, -1) : raw;
    if (fence !== undefined) {
      const closing = /^ {0,3}(`+|~+)\s*$/u.exec(line);
      if (
        closing !== null &&
        closing[1]?.[0] === fence.marker &&
        closing[1].length >= fence.length
      ) {
        fence = undefined;
      }
      continue;
    }
    const opening = /^ {0,3}(`{3,}|~{3,})/u.exec(line);
    if (opening?.[1] !== undefined) {
      fence = {
        marker: opening[1][0] as "`" | "~",
        length: opening[1].length,
      };
    } else if (line.trim() === "") {
      stable = offset;
    }
  }
  return stable;
};
