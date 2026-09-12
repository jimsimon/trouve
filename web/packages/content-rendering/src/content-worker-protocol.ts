import type { ParsedDiffFile } from "./diff-parser.js";

export interface HighlightToken {
  readonly from: number;
  readonly to: number;
  readonly classes: string;
}

/** Every message posted to the content worker carries a request id and a
 * discriminator. Applications extend the rendering variants below with their
 * own CPU-heavy tasks and multiplex them over the same worker. */
export interface ContentWorkerEnvelope {
  readonly id: number;
  readonly type: string;
}

export type ContentRenderingRequest =
  | { readonly id: number; readonly type: "markdown"; readonly source: string }
  | { readonly id: number; readonly type: "diff"; readonly source: string }
  | {
      readonly id: number;
      readonly type: "highlight";
      readonly source: string;
      readonly language: string;
    };

export const CONTENT_WORKER_MAX_SOURCE_UNITS = 4 * 1024 * 1024;

export const isContentRenderingRequest = (
  request: ContentWorkerEnvelope,
): request is ContentRenderingRequest =>
  request.type === "markdown" || request.type === "diff" || request.type === "highlight";

/** Apply the same resource bounds before posting and inside the worker. This
 * keeps worker rejection from turning into an unbounded main-thread fallback. */
export const validateContentRenderingRequest = (request: ContentRenderingRequest): void => {
  if (request.source.length > CONTENT_WORKER_MAX_SOURCE_UNITS) {
    throw new Error("content exceeds worker bounds");
  }
};

export type ContentRenderingResult =
  | string
  | readonly ParsedDiffFile[]
  | readonly HighlightToken[];

export type ContentWorkerResponse<Result = unknown> =
  | { readonly id: number; readonly ok: true; readonly value: Result }
  | { readonly id: number; readonly ok: false; readonly error: string };

export const isContentWorkerResponse = (value: unknown): value is ContentWorkerResponse => {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as { id?: unknown; ok?: unknown };
  return Number.isSafeInteger(candidate.id) && typeof candidate.ok === "boolean";
};

/** Stable failure code for rendering-bound violations; undefined for reasons
 * the rendering layer does not own. */
export const describeContentRenderingFailure = (reason: unknown): string | undefined =>
  reason instanceof Error && reason.message === "content exceeds worker bounds"
    ? "content-too-large"
    : undefined;
