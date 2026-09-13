import {
  type ContentRenderingRequest,
  type ContentWorkerEnvelope,
  type HighlightToken,
  isContentWorkerResponse,
  validateContentRenderingRequest,
} from "./content-worker-protocol.js";
import type { ParsedDiffFile } from "./diff-parser.js";

/** Creates the application's content worker. Called lazily, only when the
 * runtime exposes `Worker`; the worker entry must serve the rendering requests
 * through `serveContentWorker` and may multiplex application tasks too. */
export type ContentWorkerFactory = () => Worker;

const DEFAULT_IDLE_TIMEOUT_MS = 30_000;
const MAX_MARKDOWN_CACHE_ENTRIES = 256;
const MAX_MARKDOWN_CACHE_UNITS = 4 * 1024 * 1024;

interface CachedMarkdown {
  readonly rendered: string;
  readonly units: number;
}

interface PendingRequest<T = unknown> {
  readonly resolve: (value: T) => void;
  readonly reject: (reason?: unknown) => void;
  readonly fallback: () => Promise<T>;
}

/** One lazy, bounded content worker for CPU-heavy presentation work. The
 * worker is intentionally not durable state: failures fall back to the same
 * pure functions, and an idle timer releases the renderer process resources. */
class ContentWorkerClient {
  #factory: ContentWorkerFactory | undefined;
  #worker: Worker | undefined;
  #nextId = 1;
  #idleTimer: ReturnType<typeof setTimeout> | undefined;
  #idleTimeoutMs = DEFAULT_IDLE_TIMEOUT_MS;
  readonly #pending = new Map<number, PendingRequest>();

  configure(factory: ContentWorkerFactory): void {
    this.#factory = factory;
  }

  request<Request extends ContentWorkerEnvelope, T>(
    request: (id: number) => Request,
    validate: (request: Request) => void,
    fallback: () => Promise<T>,
  ): Promise<T> {
    const id = this.#nextId++;
    let message: Request;
    try {
      message = request(id);
      validate(message);
    } catch (error) {
      return Promise.reject(error);
    }
    const worker = this.#ensureWorker();
    if (worker === undefined) return fallback();
    this.#clearIdleTimer();
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, { resolve, reject, fallback } as PendingRequest);
      try {
        worker.postMessage(message);
      } catch {
        this.#pending.delete(id);
        void fallback().then(resolve, reject);
        this.#scheduleIdleTermination();
      }
    });
  }

  activeCount(): number {
    return this.#worker === undefined ? 0 : 1;
  }

  setIdleTimeoutForTests(timeoutMs: number): void {
    this.#idleTimeoutMs = Math.max(0, timeoutMs);
  }

  dispose(): void {
    this.#clearIdleTimer();
    this.#worker?.terminate();
    this.#worker = undefined;
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) {
      void request.fallback().then(request.resolve, request.reject);
    }
  }

  #ensureWorker(): Worker | undefined {
    if (this.#worker !== undefined) return this.#worker;
    if (typeof Worker === "undefined" || this.#factory === undefined) return undefined;
    try {
      const worker = this.#factory();
      worker.addEventListener("message", this.#receive);
      worker.addEventListener("error", this.#failed);
      worker.addEventListener("messageerror", this.#failed);
      this.#worker = worker;
      return worker;
    } catch {
      return undefined;
    }
  }

  readonly #receive = (event: MessageEvent<unknown>): void => {
    if (!isContentWorkerResponse(event.data)) {
      this.#fallbackAll();
      return;
    }
    const pending = this.#pending.get(event.data.id);
    if (pending === undefined) return;
    this.#pending.delete(event.data.id);
    if (event.data.ok) {
      pending.resolve(event.data.value);
    } else {
      void pending.fallback().then(pending.resolve, pending.reject);
    }
    this.#scheduleIdleTermination();
  };

  readonly #failed = (): void => {
    this.#fallbackAll();
  };

  #fallbackAll(): void {
    this.#clearIdleTimer();
    this.#worker?.terminate();
    this.#worker = undefined;
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) {
      void request.fallback().then(request.resolve, request.reject);
    }
  }

  #scheduleIdleTermination(): void {
    if (this.#pending.size > 0 || this.#worker === undefined) return;
    this.#clearIdleTimer();
    this.#idleTimer = setTimeout(() => {
      this.#idleTimer = undefined;
      this.#worker?.terminate();
      this.#worker = undefined;
    }, this.#idleTimeoutMs);
  }

  #clearIdleTimer(): void {
    if (this.#idleTimer === undefined) return;
    clearTimeout(this.#idleTimer);
    this.#idleTimer = undefined;
  }
}

const contentWorker = new ContentWorkerClient();
const markdownCache = new Map<string, CachedMarkdown>();
const pendingMarkdown = new Map<string, Promise<string>>();
let markdownCacheUnits = 0;
let markdownCacheGeneration = 0;

const cachedMarkdown = (source: string): string | undefined => {
  const cached = markdownCache.get(source);
  if (cached === undefined) return undefined;
  markdownCache.delete(source);
  markdownCache.set(source, cached);
  return cached.rendered;
};

const retainMarkdown = (source: string, rendered: string): void => {
  const units = source.length + rendered.length;
  if (units > MAX_MARKDOWN_CACHE_UNITS) return;
  const previous = markdownCache.get(source);
  if (previous !== undefined) markdownCacheUnits -= previous.units;
  markdownCache.delete(source);
  markdownCache.set(source, { rendered, units });
  markdownCacheUnits += units;
  while (
    markdownCache.size > MAX_MARKDOWN_CACHE_ENTRIES
    || markdownCacheUnits > MAX_MARKDOWN_CACHE_UNITS
  ) {
    const oldest = markdownCache.entries().next().value as
      | [string, CachedMarkdown]
      | undefined;
    if (oldest === undefined) break;
    markdownCache.delete(oldest[0]);
    markdownCacheUnits -= oldest[1].units;
  }
};

const clearMarkdownCache = (): void => {
  markdownCacheGeneration += 1;
  markdownCache.clear();
  pendingMarkdown.clear();
  markdownCacheUnits = 0;
};

export const cachedMarkdownOffThread = (source: string): string | undefined =>
  cachedMarkdown(source);

export const renderMarkdownOffThread = (source: string): Promise<string> => {
  const cached = cachedMarkdown(source);
  if (cached !== undefined) return Promise.resolve(cached);
  const pending = pendingMarkdown.get(source);
  if (pending !== undefined) return pending;
  const generation = markdownCacheGeneration;
  const request = contentWorker.request(
    (id): ContentRenderingRequest => ({ id, type: "markdown", source }),
    validateContentRenderingRequest,
    async () => (await import("./markdown-renderer.js")).renderMarkdownDirect(source),
  );
  const requested = request.then((rendered) => {
    if (pendingMarkdown.get(source) === requested) pendingMarkdown.delete(source);
    if (generation === markdownCacheGeneration) retainMarkdown(source, rendered);
    return rendered;
  }, (error: unknown) => {
    if (pendingMarkdown.get(source) === requested) pendingMarkdown.delete(source);
    throw error;
  });
  pendingMarkdown.set(source, requested);
  return requested;
};

export const prepareUnifiedDiffOffThread = (
  source: string,
): Promise<readonly ParsedDiffFile[]> =>
  contentWorker.request(
    (id): ContentRenderingRequest => ({ id, type: "diff", source }),
    validateContentRenderingRequest,
    async () => (await import("./diff-parser.js")).parseUnifiedDiff(source),
  );

export const highlightSourceOffThread = (
  source: string,
  language: string,
): Promise<readonly HighlightToken[]> =>
  contentWorker.request(
    (id): ContentRenderingRequest => ({ id, type: "highlight", source, language }),
    validateContentRenderingRequest,
    async () => (await import("./source-highlighter.js")).highlightSource(source, language),
  );

/** Install the application's worker factory. Until configured, work runs on
 * the main thread through the same bounded pure implementations. */
export const configureContentWorker = (factory: ContentWorkerFactory): void => {
  contentWorker.configure(factory);
};

/** Multiplex an application-defined task over the shared content worker. */
export const requestContentWork = <Request extends ContentWorkerEnvelope, T>(
  request: (id: number) => Request,
  validate: (request: Request) => void,
  fallback: () => Promise<T>,
): Promise<T> => contentWorker.request(request, validate, fallback);

export const activeContentWorkerCount = (): number => contentWorker.activeCount();
export const disposeContentWorker = (): void => {
  contentWorker.dispose();
  clearMarkdownCache();
};
export const setContentWorkerIdleTimeoutForTests = (timeoutMs: number): void => {
  contentWorker.setIdleTimeoutForTests(timeoutMs);
};
