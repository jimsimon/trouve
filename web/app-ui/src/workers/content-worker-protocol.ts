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

export type ContentWorkerResult =
  | string
  | readonly ParsedDiffFile[]
  | readonly RankedComposerCompletion[]
  | readonly FuzzyTextItem[]
  | readonly HighlightToken[];

export type ContentWorkerResponse =
  | { readonly id: number; readonly ok: true; readonly value: ContentWorkerResult }
  | { readonly id: number; readonly ok: false; readonly error: string };
