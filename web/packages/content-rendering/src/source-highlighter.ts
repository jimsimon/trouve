import { classHighlighter, highlightTree } from "@lezer/highlight";

import type { HighlightToken } from "./content-worker-protocol.js";

const JAVASCRIPT_LANGUAGES = new Set([
  "javascript", "js", "jsx", "typescript", "ts", "tsx", "json",
]);

const FAMILY_BY_LANGUAGE: Readonly<Record<string, string>> = Object.freeze({
  bash: "shell", c: "c", "c++": "c", cc: "c", cpp: "c", cs: "c",
  csharp: "c", css: "css",
  dockerfile: "shell", go: "go", html: "markup", ini: "config", java: "java",
  kotlin: "java", makefile: "make", markdown: "markdown", md: "markdown",
  php: "php", python: "python", py: "python", rb: "ruby", ruby: "ruby",
  rust: "rust", rs: "rust",
  shell: "shell", sh: "shell", sql: "sql", swift: "swift", toml: "config",
  xml: "markup", yaml: "config", yml: "config", zsh: "shell",
});

const KEYWORD_SOURCE: Readonly<Record<string, string>> = Object.freeze({
  rust: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while abstract become box do final macro override priv typeof unsized virtual yield try",
  python: "and as assert async await break class continue def del elif else except False finally for from global if import in is lambda None nonlocal not or pass raise return True try while with yield match case",
  shell: "case do done elif else esac fi for function if in select then time until while coproc",
  c: "alignas alignof auto bool break case char class const constexpr continue default do double else enum explicit export extern false float for friend goto if inline int long namespace new nullptr operator private protected public register return short signed sizeof static struct switch template this throw true try typedef typename union unsigned using virtual void volatile wchar_t while",
  go: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var",
  java: "abstract as assert boolean break byte case catch char class const constructor continue data default do double else enum expect extends external false final finally float for fun goto if implements import in instanceof int interface internal is long native new null object open operator out override package private protected public return sealed short static strictfp super suspend switch synchronized this throw throws trait transient true try typealias typeof val var void volatile when while",
  ruby: "alias and begin break case class def defined do else elsif end ensure false for if in module next nil not or redo rescue retry return self super then true undef unless until when while yield",
  php: "abstract and array as break callable case catch class clone const continue declare default die do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile eval exit extends final finally fn for foreach function global goto if implements include include_once instanceof insteadof interface isset list match namespace new or print private protected public readonly require require_once return static switch throw trait true try unset use var while xor yield",
  swift: "actor any as associatedtype async await break case catch class continue convenience copy consuming default defer deinit didSet distributed do dynamic else enum extension fallthrough false fileprivate final for func get guard if import in indirect init inout internal is isolated let macro mutating nil nonisolated open operator optional override package precedencegroup private protocol public repeat required rethrows return self set some static struct subscript super switch throws true try typealias unowned var weak where while willSet",
  sql: "add all alter and any as asc authorization backup begin between by case check column constraint create cross database default delete desc distinct drop else end escape except exists foreign from full grant group having in index inner insert intersect into is join key left like limit not null on or order outer primary procedure references right row select set table top transaction trigger union unique update values view when where with",
  css: "and from important media not only or supports",
  config: "true false null none on off yes no",
  make: "define else endef endif export ifdef ifeq ifndef ifneq include override private sinclude undefine unexport vpath",
  markdown: "",
  markup: "",
});

const TYPE_WORDS = new Set([
  "bool", "boolean", "byte", "char", "decimal", "double", "f32", "f64",
  "float", "i8", "i16", "i32", "i64", "i128", "int", "isize", "long",
  "number", "object", "short", "str", "string", "u8", "u16", "u32", "u64",
  "u128", "uint", "ulong", "usize", "void",
]);

const keywordCache = new Map<string, ReadonlySet<string>>();

const keywordsFor = (family: string): ReadonlySet<string> => {
  const existing = keywordCache.get(family);
  if (existing !== undefined) return existing;
  const words = new Set((KEYWORD_SOURCE[family] ?? "").split(" ").filter(Boolean));
  keywordCache.set(family, words);
  return words;
};

const isIdentifierStart = (character: string): boolean => /[A-Za-z_$]/u.test(character);
const isIdentifierContinue = (character: string): boolean => /[\w$]/u.test(character);

const genericFamily = (language: string): string | undefined =>
  FAMILY_BY_LANGUAGE[language.trim().toLowerCase()];

export const supportsGenericHighlighting = (language: string): boolean =>
  genericFamily(language) !== undefined;

/** Bounded, dependency-free lexical highlighting for the common languages
 * that syntect covered in the native file viewer. It deliberately recognizes
 * only lexical constructs; malformed input remains selectable plain text. */
export const highlightSourceGeneric = (
  source: string,
  language: string,
): readonly HighlightToken[] => {
  const family = genericFamily(language);
  if (family === undefined || source === "") return [];
  const keywords = keywordsFor(family);
  const lineComments = family === "python" || family === "shell" || family === "ruby"
    || family === "config" || family === "make"
    ? ["#"]
    : family === "sql" ? ["--"] : ["//"];
  const blockComments = family === "markup"
    ? [["<!--", "-->"]] as const
    : family === "c" || family === "go" || family === "java" || family === "rust"
      || family === "swift" || family === "php" || family === "css"
      ? [["/*", "*/"]] as const
      : [];
  const tokens: HighlightToken[] = [];
  const push = (from: number, to: number, classes: string): void => {
    if (to > from) tokens.push({ from, to, classes });
  };
  let index = 0;
  let inMarkupTag = false;
  let markupExpectTagName = false;
  while (index < source.length) {
    const lineMarker = lineComments.find((marker) => source.startsWith(marker, index));
    if (lineMarker !== undefined) {
      const end = source.indexOf("\n", index + lineMarker.length);
      const to = end < 0 ? source.length : end;
      push(index, to, "tok-comment");
      index = to;
      continue;
    }
    const block = blockComments.find(([open]) => source.startsWith(open, index));
    if (block !== undefined) {
      const end = source.indexOf(block[1], index + block[0].length);
      const to = end < 0 ? source.length : end + block[1].length;
      push(index, to, "tok-comment");
      index = to;
      continue;
    }

    const character = source[index] ?? "";
    if (character === '"' || character === "'" || character === "`") {
      const triple = (family === "python" || family === "ruby") &&
        source.startsWith(character.repeat(3), index);
      const delimiter = triple ? character.repeat(3) : character;
      let end = index + delimiter.length;
      while (end < source.length) {
        if (source.startsWith(delimiter, end)) {
          end += delimiter.length;
          break;
        }
        if (!triple && source[end] === "\\") end += 1;
        end += 1;
      }
      push(index, Math.min(end, source.length), "tok-string");
      index = Math.min(end, source.length);
      continue;
    }

    if (/\d/u.test(character) && !isIdentifierContinue(source[index - 1] ?? "")) {
      let end = index + 1;
      while (end < source.length && /[\w.+-]/u.test(source[end] ?? "")) end += 1;
      push(index, end, "tok-number");
      index = end;
      continue;
    }

    if (isIdentifierStart(character)) {
      let end = index + 1;
      while (end < source.length && isIdentifierContinue(source[end] ?? "")) end += 1;
      const word = source.slice(index, end);
      const normalized = family === "sql" ? word.toLowerCase() : word;
      let lookahead = end;
      while (/\s/u.test(source[lookahead] ?? "")) lookahead += 1;
      const configurationKey = (family === "config" || family === "css") &&
        (source[lookahead] === ":" || source[lookahead] === "=");
      const markupName = family === "markup" && inMarkupTag;
      if (keywords.has(normalized)) push(index, end, "tok-keyword");
      else if (TYPE_WORDS.has(normalized) || /^[A-Z][A-Za-z0-9_]*$/u.test(word)) {
        push(index, end, "tok-typeName");
      } else if (markupName && markupExpectTagName) {
        push(index, end, "tok-typeName");
      } else if (configurationKey || markupName) {
        push(index, end, "tok-propertyName");
      }
      if (markupName) markupExpectTagName = false;
      index = end;
      continue;
    }
    if (family === "markup" && character === "<") {
      inMarkupTag = true;
      markupExpectTagName = true;
    } else if (family === "markup" && character === ">") {
      inMarkupTag = false;
      markupExpectTagName = false;
    }
    index += 1;
  }
  return tokens;
};

const javascriptParser = async (language: string) => {
  const normalized = language.trim().toLowerCase();
  if (!JAVASCRIPT_LANGUAGES.has(normalized)) return undefined;
  const { parser } = await import("@lezer/javascript");
  if (normalized === "tsx") return parser.configure({ dialect: "ts jsx" });
  if (normalized === "typescript" || normalized === "ts") {
    return parser.configure({ dialect: "ts" });
  }
  if (normalized === "jsx") return parser.configure({ dialect: "jsx" });
  return parser;
};

/** Parse and classify source without constructing an EditorView. This is used
 * for the large-file selectable fallback, where putting the complete parse on
 * the UI thread would defeat the fallback's purpose. */
export const highlightSource = async (
  source: string,
  language: string,
): Promise<readonly HighlightToken[]> => {
  const parser = await javascriptParser(language);
  if (parser === undefined) return highlightSourceGeneric(source, language);
  if (source === "") return [];
  const tokens: HighlightToken[] = [];
  highlightTree(parser.parse(source), classHighlighter, (from, to, classes) => {
    tokens.push({ from, to, classes });
  });
  return tokens;
};
