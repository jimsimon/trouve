import type {
  ComposerCompletionCandidate,
  RankedComposerCompletion,
} from "../components/composer-completion.js";
import type { ParsedDiffFile } from "../components/diff-parser.js";
import type { FuzzyTextItem } from "../services/fuzzy-ranking.js";

export interface HighlightToken {
  readonly from: number;
  readonly to: number;
  readonly classes: string;
}

export type ContentWorkerRequest =
  | { readonly id: number; readonly type: "markdown"; readonly source: string }
  | { readonly id: number; readonly type: "diff"; readonly source: string }
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
    }
  | {
      readonly id: number;
      readonly type: "highlight";
      readonly source: string;
      readonly language: string;
    };

export const CONTENT_WORKER_MAX_SOURCE_UNITS = 4 * 1024 * 1024;
export const CONTENT_WORKER_MAX_FUZZY_ITEMS = 10_000;

/** Apply the same resource bounds before posting and inside the worker. This
 * keeps worker rejection from turning into an unbounded main-thread fallback. */
export const validateContentWorkerRequest = (request: ContentWorkerRequest): void => {
  if (
    (request.type === "markdown" || request.type === "diff" || request.type === "highlight")
    && request.source.length > CONTENT_WORKER_MAX_SOURCE_UNITS
  ) throw new Error("content exceeds worker bounds");
  if (
    (request.type === "composer-fuzzy" && request.candidates.length > CONTENT_WORKER_MAX_FUZZY_ITEMS)
    || (request.type === "palette-fuzzy" && request.items.length > CONTENT_WORKER_MAX_FUZZY_ITEMS)
  ) throw new Error("too many fuzzy candidates");
};

export type ContentWorkerResult =
  | string
  | readonly ParsedDiffFile[]
  | readonly RankedComposerCompletion[]
  | readonly FuzzyTextItem[]
  | readonly HighlightToken[];

export type ContentWorkerResponse =
  | { readonly id: number; readonly ok: true; readonly value: ContentWorkerResult }
  | { readonly id: number; readonly ok: false; readonly error: string };
