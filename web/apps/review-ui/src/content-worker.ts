/// <reference lib="webworker" />

// The review site's content worker: the shared markdown/diff/highlight
// handlers and nothing else, so transcript rendering leaves the main thread
// exactly as it does in the desktop app.
import {
  handleContentRenderingRequest,
  serveContentWorker,
} from "@trouve-ai/content-rendering/content-worker-handlers";
import {
  type ContentRenderingRequest,
  describeContentRenderingFailure,
} from "@trouve-ai/content-rendering/content-worker-protocol";

declare const self: DedicatedWorkerGlobalScope;

serveContentWorker<ContentRenderingRequest>(
  self,
  handleContentRenderingRequest,
  (reason) => describeContentRenderingFailure(reason) ?? "content processing failed",
);
