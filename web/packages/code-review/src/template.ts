import { html as litHtml, type TemplateResult } from "lit";

/**
 * Lit's `html` tag with JSX whitespace rules applied to the static markup.
 *
 * The dashboard was ported from JSX, whose compiler drops the indentation
 * between sibling elements and around expressions and folds the remaining
 * line breaks in text into single spaces. Lit keeps every character of a
 * template, so an indented template would add whitespace text nodes the
 * Preact tree never had; between inline elements those render as visible
 * spaces and they shift text shaping elsewhere. Cleaning the static strings
 * once per call site keeps the templates readable while the DOM stays
 * identical to the JSX output.
 *
 * Only text content is touched: attribute lists inside tags are copied as-is,
 * and expression values are never altered. Explicit `${" "}` values survive,
 * exactly like JSX's `{" "}`.
 */
export function html(strings: TemplateStringsArray, ...values: unknown[]): TemplateResult {
  let cleaned = cleanedTemplates.get(strings);
  if (cleaned === undefined) {
    cleaned = withJsxWhitespace(strings);
    cleanedTemplates.set(strings, cleaned);
  }
  return litHtml(cleaned, ...values);
}

// Lit caches prepared templates by the identity of the strings array, so the
// cleaned array must be stable per call site as well.
const cleanedTemplates = new WeakMap<TemplateStringsArray, TemplateStringsArray>();

function withJsxWhitespace(strings: TemplateStringsArray): TemplateStringsArray {
  const cleaned: string[] = [];
  let inTag = false;
  let quote: '"' | "'" | null = null;
  for (const source of strings) {
    let result = "";
    // Text runs are flushed through `cleanJsxText` when a tag starts or the
    // static string ends (the latter is where an expression follows).
    let textStart = inTag ? -1 : 0;
    for (let index = 0; index < source.length; index++) {
      const char = source[index] as string;
      if (inTag) {
        result += char;
        if (quote !== null) {
          if (char === quote) quote = null;
        } else if (char === '"' || char === "'") {
          quote = char;
        } else if (char === ">") {
          inTag = false;
          textStart = index + 1;
        }
      } else if (char === "<") {
        result += cleanJsxText(source.slice(textStart, index)) + char;
        inTag = true;
        textStart = -1;
      }
    }
    if (!inTag) result += cleanJsxText(source.slice(textStart));
    cleaned.push(result);
  }
  return Object.freeze(Object.assign(cleaned, { raw: Object.freeze([...cleaned]) }));
}

/**
 * Babel's `cleanJSXElementLiteralChild`: split into lines, trim the
 * indentation of every line but the first and the trailing spaces of every
 * line but the last, drop blank lines, and join the rest with single spaces.
 */
export function cleanJsxText(text: string): string {
  const lines = text.split(/\r\n|\n|\r/u);
  let lastNonEmptyLine = 0;
  for (const [index, line] of lines.entries()) {
    if (/[^ \t]/u.test(line)) lastNonEmptyLine = index;
  }
  let result = "";
  for (const [index, line] of lines.entries()) {
    let trimmed = line.replaceAll("\t", " ");
    if (index !== 0) trimmed = trimmed.replace(/^ +/u, "");
    if (index !== lines.length - 1) trimmed = trimmed.replace(/ +$/u, "");
    if (trimmed) {
      if (index !== lastNonEmptyLine) trimmed += " ";
      result += trimmed;
    }
  }
  return result;
}
