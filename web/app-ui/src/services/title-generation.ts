import type { AppStore } from "../state/app-store.js";

export const TITLE_GENERATION_TIMEOUT_MS = 48_000;
// The server permits a managed local request to wait five minutes for
// foreground work and another five minutes for a cold model load.
export const LOCAL_TITLE_GENERATION_TIMEOUT_MS = 11 * 60_000;
export const LOCAL_MODEL_WAITING_LABEL = "Waiting for the local model.";

type TitleGenerationStore = Pick<
  AppStore,
  "beginTitleGeneration" | "markTitleGenerationWaiting"
>;

export const beginTitleGeneration = (
  store: TitleGenerationStore,
  id: string,
  provisionalTitle: string,
  model: string | undefined,
): ReturnType<typeof globalThis.setTimeout> | undefined => {
  store.beginTitleGeneration(id, provisionalTitle);
  return model?.startsWith("local/") === true
    ? globalThis.setTimeout(
        () => store.markTitleGenerationWaiting(id, provisionalTitle),
        2_000,
      )
    : undefined;
};

export const titleGenerationTimeoutMs = (model: string | undefined): number =>
  model?.startsWith("local/") === true
    ? LOCAL_TITLE_GENERATION_TIMEOUT_MS
    : TITLE_GENERATION_TIMEOUT_MS;
