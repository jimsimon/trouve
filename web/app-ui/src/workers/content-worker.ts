/// <reference lib="webworker" />

import { rankComposerCompletions } from "../components/composer-completion.js";
import { parseUnifiedDiff } from "../components/diff-parser.js";
import { filterFuzzyTextItems } from "../services/fuzzy-ranking.js";
import { renderMarkdownDirect } from "../services/markdown-renderer.js";
import {
  ContentWorkerRequest,
  type ContentWorkerResponse,
  type ContentWorkerResult,
  validateContentWorkerRequest,
} from "./content-worker-protocol.js";
import { highlightSource } from "./source-highlighter.js";

declare const self: DedicatedWorkerGlobalScope;

const processRequest = async (
  request: ContentWorkerRequest,
): Promise<ContentWorkerResult> => {
  validateContentWorkerRequest(request);
  switch (request.type) {
    case "markdown":
      return renderMarkdownDirect(request.source);
    case "diff":
      return parseUnifiedDiff(request.source);
    case "composer-fuzzy":
      return rankComposerCompletions(request.candidates, request.query, request.limit);
    case "palette-fuzzy":
      return filterFuzzyTextItems(request.items, request.query);
    case "highlight":
      return highlightSource(request.source, request.language);
  }
};

const failureMessage = (reason: unknown): string => {
  if (reason instanceof Error) {
    if (reason.message === "content exceeds worker bounds") return "content-too-large";
    if (reason.message === "too many fuzzy candidates") return "too-many-candidates";
  }
  return "content processing failed";
};

self.addEventListener("message", (event: MessageEvent<ContentWorkerRequest>) => {
  const request = event.data;
  void processRequest(request).then(
    (value) => {
      const response: ContentWorkerResponse = { id: request.id, ok: true, value };
      self.postMessage(response);
    },
    (reason: unknown) => {
      const response: ContentWorkerResponse = {
        id: request.id,
        ok: false,
        error: failureMessage(reason),
      };
      self.postMessage(response);
    },
  );
});
