// Desktop wiring for the shared content worker. Importing this module installs
// the app's worker entry (which also serves the shared rendering requests) and
// exposes the app-specific fuzzy-ranking tasks over the same worker.
import {
  configureContentWorker,
  requestContentWork,
} from "@trouve-ai/content-rendering/content-worker-client";

import type { CommandPaletteItem } from "../components/command-palette-model.js";
import {
  rankComposerCompletions,
  type ComposerCompletionCandidate,
  type RankedComposerCompletion,
} from "../components/composer-completion.js";
import { filterFuzzyTextItems } from "./fuzzy-ranking.js";
import {
  type ContentWorkerRequest,
  validateContentWorkerRequest,
} from "../workers/content-worker-protocol.js";

configureContentWorker(
  () =>
    new Worker(
      new URL("../workers/content-worker.ts", import.meta.url),
      { type: "module", name: "trouve-content" },
    ),
);

export {
  activeContentWorkerCount,
  cachedMarkdownOffThread,
  disposeContentWorker,
  highlightSourceOffThread,
  prepareUnifiedDiffOffThread,
  renderMarkdownOffThread,
  setContentWorkerIdleTimeoutForTests,
} from "@trouve-ai/content-rendering/content-worker-client";

export const rankComposerCompletionsOffThread = (
  candidates: readonly ComposerCompletionCandidate[],
  query: string,
  limit: number,
): Promise<readonly RankedComposerCompletion[]> =>
  requestContentWork(
    (id): ContentWorkerRequest => ({ id, type: "composer-fuzzy", candidates, query, limit }),
    validateContentWorkerRequest,
    async () => rankComposerCompletions(candidates, query, limit),
  );

export const filterCommandPaletteItemsOffThread = (
  items: readonly CommandPaletteItem[],
  query: string,
): Promise<readonly CommandPaletteItem[]> =>
  requestContentWork(
    (id): ContentWorkerRequest => ({ id, type: "palette-fuzzy", items, query }),
    validateContentWorkerRequest,
    async () => filterFuzzyTextItems(items, query),
  );
