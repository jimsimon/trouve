/** Largest source prefix ending at a complete blank line outside a fenced
 * code block. Markdown parsing can restart there, so streamed deltas normally
 * reprocess only the unstable tail instead of the full response. */
export const stableMarkdownPrefixLength = (source: string): number => {
  let offset = 0;
  let stable = 0;
  let fence: {
    readonly marker: "`" | "~";
    readonly length: number;
    readonly markdownExample: boolean;
    nestedOpen: boolean;
  } | undefined;
  const lines = [...source.matchAll(/.*(?:\n|$)/gu)]
    .map((match) => match[0])
    .filter((raw) => raw !== "");
  for (let index = 0; index < lines.length; index += 1) {
    const raw = lines[index] ?? "";
    offset += raw.length;
    const line = raw.endsWith("\n") ? raw.slice(0, -1) : raw;
    if (fence !== undefined) {
      const closing = /^ {0,3}(`+|~+)\s*$/u.exec(line);
      if (
        closing !== null &&
        closing[1]?.[0] === fence.marker &&
        closing[1].length >= fence.length
      ) {
        if (fence.nestedOpen) {
          const laterCloser = lines.slice(index + 1).some((candidateRaw) => {
            const candidate = candidateRaw.endsWith("\n")
              ? candidateRaw.slice(0, -1)
              : candidateRaw;
            const match = /^ {0,3}(`+|~+)\s*$/u.exec(candidate);
            return match?.[1]?.[0] === fence?.marker
              && (match?.[1]?.length ?? 0) >= (fence?.length ?? Number.MAX_SAFE_INTEGER);
          });
          if (laterCloser) fence.nestedOpen = false;
        } else {
          fence = undefined;
        }
      } else if (fence.markdownExample) {
        const nested = /^ {0,3}(`{3,}|~{3,})([^\r\n]+)\r?$/u.exec(line);
        if (
          nested?.[1]?.[0] === fence.marker
          && nested[1].length === fence.length
          && nested[2]?.trim() !== ""
          && !(nested[1][0] === "`" && nested[2]?.includes("`"))
        ) fence.nestedOpen = true;
      }
      continue;
    }
    const opening = /^ {0,3}(`{3,}|~{3,})([^\r\n]*)\r?$/u.exec(line);
    if (
      opening?.[1] !== undefined
      && !(opening[1][0] === "`" && opening[2]?.includes("`"))
    ) {
      const language = opening[2]?.trim().split(/\s+/u)[0]?.toLowerCase() ?? "";
      fence = {
        marker: opening[1][0] as "`" | "~",
        length: opening[1].length,
        markdownExample: language === "markdown" || language === "md",
        nestedOpen: false,
      };
    } else if (line.trim() === "") {
      stable = offset;
    }
  }
  return stable;
};
