import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import rehypeStringify from "rehype-stringify";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import { unified } from "unified";

import {
  isApplicationRouteTarget,
  parseChatFileTarget,
} from "../components/chat-file-link.js";
import { highlightSource } from "../workers/source-highlighter.js";

interface HastNode {
  type?: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
}

interface MarkdownFence {
  readonly indent: string;
  readonly marker: "`" | "~";
  readonly length: number;
  readonly info: string;
}

const markdownFence = (line: string): MarkdownFence | undefined => {
  const match = /^( {0,3})(`{3,}|~{3,})([^\r\n]*)\r?$/u.exec(line);
  const run = match?.[2];
  const info = match?.[3] ?? "";
  if (run === undefined || (run[0] === "`" && info.includes("`"))) return undefined;
  return {
    indent: match?.[1] ?? "",
    marker: run[0] as "`" | "~",
    length: run.length,
    info,
  };
};

const markdownFenceLanguage = (info: string): string =>
  info.trim().split(/\s+/u)[0]?.toLowerCase() ?? "";

const isMarkdownExampleFence = (fence: MarkdownFence): boolean =>
  ["markdown", "md"].includes(markdownFenceLanguage(fence.info));

const isClosingFence = (fence: MarkdownFence, outer: MarkdownFence): boolean =>
  fence.marker === outer.marker
  && fence.length >= outer.length
  && fence.info.trim() === "";

/**
 * Models occasionally put a same-length fenced block inside a
 * ```markdown example and then emit a second closer for the outer block.
 * CommonMark closes at the first fence, but the later closer makes the
 * intended nesting unambiguous. Lengthen only that outer pair before remark
 * parses it; a single closer keeps ordinary CommonMark meaning.
 */
export const recoverNestedMarkdownFences = (source: string): string => {
  const lines = source.match(/[^\n]*(?:\n|$)/gu)?.filter(Boolean) ?? [];
  const endings = lines.map((raw) =>
    raw.endsWith("\r\n") ? "\r\n" : raw.endsWith("\n") ? "\n" : ""
  );
  const bodies = lines.map((raw, index) =>
    raw.slice(0, raw.length - (endings[index]?.length ?? 0))
  );
  let changed = false;

  for (let openerIndex = 0; openerIndex < bodies.length; openerIndex += 1) {
    const outer = markdownFence(bodies[openerIndex] ?? "");
    if (outer === undefined || !isMarkdownExampleFence(outer)) continue;

    let nestedOpen = false;
    let recoveredInner = false;
    let outerCloserIndex: number | undefined;
    for (let index = openerIndex + 1; index < bodies.length; index += 1) {
      const candidate = markdownFence(bodies[index] ?? "");
      if (candidate === undefined || candidate.marker !== outer.marker) continue;
      if (candidate.length === outer.length && candidate.info.trim() !== "") {
        nestedOpen = true;
        continue;
      }
      if (!isClosingFence(candidate, outer)) continue;
      if (nestedOpen) {
        const laterCloser = bodies.slice(index + 1).some((line) => {
          const later = markdownFence(line);
          return later !== undefined && isClosingFence(later, outer);
        });
        if (!laterCloser) break;
        nestedOpen = false;
        recoveredInner = true;
        continue;
      }
      if (recoveredInner) outerCloserIndex = index;
      break;
    }
    if (outerCloserIndex === undefined) continue;

    let replacementLength = outer.length + 1;
    for (const body of bodies.slice(openerIndex + 1, outerCloserIndex)) {
      const candidate = markdownFence(body);
      if (candidate?.marker === outer.marker) {
        replacementLength = Math.max(replacementLength, candidate.length + 1);
      }
    }
    const replacement = outer.marker.repeat(replacementLength);
    bodies[openerIndex] = `${outer.indent}${replacement}${outer.info}`;
    const closer = markdownFence(bodies[outerCloserIndex] ?? "");
    bodies[outerCloserIndex] = `${closer?.indent ?? ""}${replacement}`;
    openerIndex = outerCloserIndex;
    changed = true;
  }
  if (!changed) return source;
  return bodies.map((body, index) => `${body}${endings[index] ?? ""}`).join("");
};

export const safeMarkdownHref = (
  href: string,
): "internal" | "external" | undefined => {
  if (/[\u0000-\u001f\u007f]/u.test(href)) return undefined;
  if (href.startsWith("#")) return "internal";
  if (href.startsWith("/")) {
    if (
      href.startsWith("//")
      || href.startsWith("/\\")
      || /^\/(?:%2f|%5c)/iu.test(href)
    ) return undefined;
    try {
      const url = new URL(href, "https://trouve.invalid");
      return url.origin === "https://trouve.invalid" ? "internal" : undefined;
    } catch {
      return undefined;
    }
  }
  try {
    const url = new URL(href);
    return url.protocol === "https:"
      && url.host !== ""
      && url.username === ""
      && url.password === ""
      ? "external"
      : undefined;
  } catch {
    return undefined;
  }
};

/** Do not let model-authored Markdown trigger arbitrary network requests or
 * unsafe navigation. Attachments and local-file actions use typed components,
 * not Markdown images or custom URL schemes. */
const safeLinks = () => (tree: HastNode): void => {
  const visit = (node: HastNode): void => {
    const children = node.children;
    if (children !== undefined) {
      for (let index = 0; index < children.length; index += 1) {
        const child = children[index];
        if (child?.type === "element" && child.tagName === "img") {
          const alt = child.properties?.["alt"];
          children[index] = {
            type: "text",
            value: typeof alt === "string" && alt !== "" ? `[image: ${alt}]` : "[image]",
          };
        } else if (child !== undefined) {
          visit(child);
        }
      }
    }

    if (node.type !== "element" || node.tagName !== "a") return;
    const href = node.properties?.["href"];
    if (typeof href !== "string") return;
    const file = parseChatFileTarget(href);
    if (file !== undefined && !isApplicationRouteTarget(href)) {
      node.properties!["href"] = "#";
      node.properties!["dataTrouveFileTarget"] = href;
      return;
    }
    const kind = safeMarkdownHref(href);
    if (kind === undefined) {
      delete node.properties?.["href"];
      return;
    }
    if (kind === "external" && node.properties !== undefined) {
      node.properties["rel"] = ["noopener", "noreferrer"];
    }
  };
  visit(tree);
};

const textContent = (node: HastNode): string =>
  node.type === "text"
    ? (node.value ?? "")
    : (node.children ?? []).map(textContent).join("");

const tokenClasses = (classes: string): readonly string[] =>
  classes.split(/\s+/u).filter((name) => /^tok-[A-Za-z]+$/u.test(name));

/** Match the native Markdown renderer's fenced-code highlighting without
 * trusting model-authored markup. Token spans are generated before the
 * sanitizer and only Trouve's bounded `tok-*` classes survive it. */
const highlightCodeFences = () => async (tree: HastNode): Promise<void> => {
  const pending: Promise<void>[] = [];
  const visit = (node: HastNode): void => {
    if (node.type === "element" && node.tagName === "pre") {
      const code = node.children?.find(
        (child) => child.type === "element" && child.tagName === "code",
      );
      const classes = code?.properties?.["className"];
      const languageClass = Array.isArray(classes)
        ? classes.find(
          (value): value is string =>
            typeof value === "string" && value.startsWith("language-"),
        )
        : undefined;
      const language = languageClass?.slice("language-".length) ?? "";
      if (code !== undefined && language !== "") {
        pending.push((async () => {
          const source = textContent(code);
          const tokens = await highlightSource(source, language);
          if (tokens.length === 0) return;
          const children: HastNode[] = [];
          let offset = 0;
          for (const token of tokens) {
            if (token.from < offset || token.to <= token.from || token.to > source.length) continue;
            if (token.from > offset) {
              children.push({ type: "text", value: source.slice(offset, token.from) });
            }
            const className = tokenClasses(token.classes);
            children.push(className.length === 0
              ? { type: "text", value: source.slice(token.from, token.to) }
              : {
                type: "element",
                tagName: "span",
                properties: { className: [...className] },
                children: [{ type: "text", value: source.slice(token.from, token.to) }],
              });
            offset = token.to;
          }
          if (offset < source.length) children.push({ type: "text", value: source.slice(offset) });
          code.children = children;
        })());
        return;
      }
    }
    for (const child of node.children ?? []) visit(child);
  };
  visit(tree);
  await Promise.all(pending);
};

const markdownProcessor = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkRehype)
  .use(safeLinks)
  .use(highlightCodeFences)
  .use(rehypeSanitize, {
    ...defaultSchema,
    attributes: {
      ...defaultSchema.attributes,
      a: [
        ...(defaultSchema.attributes?.["a"] ?? []),
        ["rel", "noopener", "noreferrer"],
        "dataTrouveFileTarget",
      ],
      span: [
        ...(defaultSchema.attributes?.["span"] ?? []),
        ["className", /^tok-[A-Za-z]+$/u],
      ],
    },
  })
  .use(rehypeStringify);

export const renderMarkdownDirect = async (source: string): Promise<string> =>
  String(await markdownProcessor.process(recoverNestedMarkdownFences(source)));
