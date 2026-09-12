import {
  type ContentRenderingRequest,
  type ContentRenderingResult,
  isContentRenderingRequest,
  validateContentRenderingRequest,
} from "@trouve-ai/content-rendering/content-worker-protocol";

import type {
  ComposerCompletionCandidate,
  RankedComposerCompletion,
} from "../components/composer-completion.js";
import type { FuzzyTextItem } from "../services/fuzzy-ranking.js";

/** The desktop app multiplexes its fuzzy-ranking work over the shared
 * content-rendering worker. */
export type ContentWorkerRequest =
  | ContentRenderingRequest
  | {
      readonly id: number;
      readonly type: "composer-fuzzy";
      readonly candidates: readonly ComposerCompletionCandidate[];
      readonly query: string;
      readonly limit: number;
    }
  | {
      readonly id: number;
      readonly type: "palette-fuzzy";
      readonly items: readonly FuzzyTextItem[];
      readonly query: string;
    };

export const CONTENT_WORKER_MAX_FUZZY_ITEMS = 10_000;

/** Apply the same resource bounds before posting and inside the worker. This
 * keeps worker rejection from turning into an unbounded main-thread fallback. */
export const validateContentWorkerRequest = (request: ContentWorkerRequest): void => {
  if (isContentRenderingRequest(request)) {
    validateContentRenderingRequest(request);
    return;
  }
  if (
    (request.type === "composer-fuzzy" && request.candidates.length > CONTENT_WORKER_MAX_FUZZY_ITEMS)
    || (request.type === "palette-fuzzy" && request.items.length > CONTENT_WORKER_MAX_FUZZY_ITEMS)
  ) throw new Error("too many fuzzy candidates");
};

export type ContentWorkerResult =
  | ContentRenderingResult
  | readonly RankedComposerCompletion[]
  | readonly FuzzyTextItem[];
