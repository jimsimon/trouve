// Worker-side half of the content worker. Applications import this from their
// own worker entry so extra tasks can share the single bounded worker.
import {
  type ContentRenderingRequest,
  type ContentRenderingResult,
  type ContentWorkerEnvelope,
  type ContentWorkerResponse,
  validateContentRenderingRequest,
} from "./content-worker-protocol.js";
import { parseUnifiedDiff } from "./diff-parser.js";
import { renderMarkdownDirect } from "./markdown-renderer.js";
import { highlightSource } from "./source-highlighter.js";

export const handleContentRenderingRequest = async (
  request: ContentRenderingRequest,
): Promise<ContentRenderingResult> => {
  validateContentRenderingRequest(request);
  switch (request.type) {
    case "markdown":
      return renderMarkdownDirect(request.source);
    case "diff":
      return parseUnifiedDiff(request.source);
    case "highlight":
      return highlightSource(request.source, request.language);
  }
};

/** The subset of DedicatedWorkerGlobalScope the message loop needs, typed
 * structurally so this module compiles under both DOM and WebWorker libs. */
export interface ContentWorkerScope {
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<unknown>) => void,
  ): void;
  postMessage(message: unknown): void;
}

export const serveContentWorker = <Request extends ContentWorkerEnvelope>(
  scope: ContentWorkerScope,
  process: (request: Request) => Promise<unknown>,
  describeFailure: (reason: unknown) => string,
): void => {
  scope.addEventListener("message", (event) => {
    const request = event.data as Request;
    void process(request).then(
      (value) => {
        const response: ContentWorkerResponse = { id: request.id, ok: true, value };
        scope.postMessage(response);
      },
      (reason: unknown) => {
        const response: ContentWorkerResponse = {
          id: request.id,
          ok: false,
          error: describeFailure(reason),
        };
        scope.postMessage(response);
      },
    );
  });
};
