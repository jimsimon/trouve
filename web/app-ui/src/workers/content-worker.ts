/// <reference lib="webworker" />

import { rankComposerCompletions } from "../components/composer-completion.js";
import { parseUnifiedDiff } from "../components/diff-parser.js";
import { filterFuzzyTextItems } from "../services/fuzzy-ranking.js";
import { renderMarkdownDirect } from "../services/markdown-renderer.js";
import type {
  ContentWorkerRequest,
  ContentWorkerResponse,
  ContentWorkerResult,
} from "./content-worker-protocol.js";
import { highlightSource } from "./source-highlighter.js";

declare const self: DedicatedWorkerGlobalScope;

const MAX_SOURCE_UNITS = 4 * 1024 * 1024;
const MAX_FUZZY_ITEMS = 10_000;

const boundedSource = (source: string): string => {
  if (source.length > MAX_SOURCE_UNITS) throw new Error("content exceeds worker bounds");
  return source;
};

const processRequest = async (
  request: ContentWorkerRequest,
): Promise<ContentWorkerResult> => {
  switch (request.type) {
    case "markdown":
      return renderMarkdownDirect(boundedSource(request.source));
    case "diff":
      return parseUnifiedDiff(boundedSource(request.source));
    case "composer-fuzzy":
      if (request.candidates.length > MAX_FUZZY_ITEMS) throw new Error("too many fuzzy candidates");
      return rankComposerCompletions(request.candidates, request.query, request.limit);
    case "palette-fuzzy":
      if (request.items.length > MAX_FUZZY_ITEMS) throw new Error("too many fuzzy candidates");
      return filterFuzzyTextItems(request.items, request.query);
    case "highlight":
      return highlightSource(boundedSource(request.source), request.language);
  }
};

self.addEventListener("message", (event: MessageEvent<ContentWorkerRequest>) => {
  const request = event.data;
  void processRequest(request).then(
    (value) => {
      const response: ContentWorkerResponse = { id: request.id, ok: true, value };
      self.postMessage(response);
    },
    () => {
      const response: ContentWorkerResponse = {
        id: request.id,
        ok: false,
        error: "content processing failed",
      };
      self.postMessage(response);
    },
  );
});
