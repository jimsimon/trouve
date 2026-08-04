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
  String(await markdownProcessor.process(source));
