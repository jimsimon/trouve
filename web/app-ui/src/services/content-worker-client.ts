import type { CommandPaletteItem } from "../components/command-palette-model.js";
import {
  rankComposerCompletions,
  type ComposerCompletionCandidate,
  type RankedComposerCompletion,
} from "../components/composer-completion.js";
import type { ParsedDiffFile } from "../components/diff-parser.js";
import { filterFuzzyTextItems } from "./fuzzy-ranking.js";
import {
  type ContentWorkerRequest,
  ContentWorkerResponse,
  type HighlightToken,
  validateContentWorkerRequest,
} from "../workers/content-worker-protocol.js";

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

const isResponse = (value: unknown): value is ContentWorkerResponse => {
  if (typeof value !== "object" || value === null) return false;
  const candidate = value as { id?: unknown; ok?: unknown };
  return Number.isSafeInteger(candidate.id) && typeof candidate.ok === "boolean";
};

/** One lazy, bounded content worker for CPU-heavy presentation work. The
 * worker is intentionally not durable state: failures fall back to the same
 * pure functions, and an idle timer releases the renderer process resources. */
class ContentWorkerClient {
  #worker: Worker | undefined;
  #nextId = 1;
  #idleTimer: ReturnType<typeof setTimeout> | undefined;
  #idleTimeoutMs = DEFAULT_IDLE_TIMEOUT_MS;
  readonly #pending = new Map<number, PendingRequest>();

  request<T>(
    request: (id: number) => ContentWorkerRequest,
    fallback: () => Promise<T>,
  ): Promise<T> {
    const id = this.#nextId++;
    let message: ContentWorkerRequest;
    try {
      message = request(id);
      validateContentWorkerRequest(message);
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
    if (typeof Worker === "undefined") return undefined;
    try {
      const worker = new Worker(
        new URL("../workers/content-worker.ts", import.meta.url),
        { type: "module", name: "trouve-content" },
      );
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
    if (!isResponse(event.data)) {
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
    (id) => ({ id, type: "markdown", source }),
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
    (id) => ({ id, type: "diff", source }),
    async () => (await import("../components/diff-parser.js")).parseUnifiedDiff(source),
  );

export const rankComposerCompletionsOffThread = (
  candidates: readonly ComposerCompletionCandidate[],
  query: string,
  limit: number,
): Promise<readonly RankedComposerCompletion[]> =>
  contentWorker.request(
    (id) => ({ id, type: "composer-fuzzy", candidates, query, limit }),
    async () => rankComposerCompletions(candidates, query, limit),
  );

export const filterCommandPaletteItemsOffThread = (
  items: readonly CommandPaletteItem[],
  query: string,
): Promise<readonly CommandPaletteItem[]> =>
  contentWorker.request(
    (id) => ({ id, type: "palette-fuzzy", items, query }),
    async () => filterFuzzyTextItems(items, query),
  );

export const highlightSourceOffThread = (
  source: string,
  language: string,
): Promise<readonly HighlightToken[]> =>
  contentWorker.request(
    (id) => ({ id, type: "highlight", source, language }),
    async () => (await import("../workers/source-highlighter.js"))
      .highlightSource(source, language),
  );

export const activeContentWorkerCount = (): number => contentWorker.activeCount();
export const disposeContentWorker = (): void => {
  contentWorker.dispose();
  clearMarkdownCache();
};
export const setContentWorkerIdleTimeoutForTests = (timeoutMs: number): void => {
  contentWorker.setIdleTimeoutForTests(timeoutMs);
};
