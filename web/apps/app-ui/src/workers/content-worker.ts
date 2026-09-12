/// <reference lib="webworker" />

import {
  handleContentRenderingRequest,
  serveContentWorker,
} from "@trouve-ai/content-rendering/content-worker-handlers";
import { describeContentRenderingFailure } from "@trouve-ai/content-rendering/content-worker-protocol";

import { rankComposerCompletions } from "../components/composer-completion.js";
import { filterFuzzyTextItems } from "../services/fuzzy-ranking.js";
import {
  ContentWorkerRequest,
  type ContentWorkerResult,
  validateContentWorkerRequest,
} from "./content-worker-protocol.js";

declare const self: DedicatedWorkerGlobalScope;

const processRequest = async (
  request: ContentWorkerRequest,
): Promise<ContentWorkerResult> => {
  validateContentWorkerRequest(request);
  switch (request.type) {
    case "composer-fuzzy":
      return rankComposerCompletions(request.candidates, request.query, request.limit);
    case "palette-fuzzy":
      return filterFuzzyTextItems(request.items, request.query);
    default:
      return handleContentRenderingRequest(request);
  }
};

const failureMessage = (reason: unknown): string => {
  const rendering = describeContentRenderingFailure(reason);
  if (rendering !== undefined) return rendering;
  if (reason instanceof Error && reason.message === "too many fuzzy candidates") {
    return "too-many-candidates";
  }
  return "content processing failed";
};

serveContentWorker<ContentWorkerRequest>(self, processRequest, failureMessage);
