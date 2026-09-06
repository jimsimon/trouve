import type { AppStore } from "../state/app-store.js";

export const TITLE_GENERATION_TIMEOUT_MS = 48_000;
// The server permits a managed local request to wait five minutes for
// foreground work and another five minutes for a cold model load.
export const LOCAL_TITLE_GENERATION_TIMEOUT_MS = 11 * 60_000;
export const LOCAL_MODEL_WAITING_LABEL = "Waiting for the local model.";
export const SESSION_TITLE_WAITING_STATUS =
  `Session name pending. ${LOCAL_MODEL_WAITING_LABEL}`;
export const THREAD_TITLE_WAITING_STATUS =
  `Thread name pending. ${LOCAL_MODEL_WAITING_LABEL}`;

type TitleGenerationStore = Pick<
  AppStore,
  "beginTitleGeneration" | "markTitleGenerationWaiting"
>;

export const beginTitleGeneration = (
  store: TitleGenerationStore,
  id: string,
  provisionalTitle: string,
  model: string | undefined,
): ReturnType<typeof globalThis.setTimeout> | 0 => {
  store.beginTitleGeneration(id, provisionalTitle);
  return model?.startsWith("local/")
    ? globalThis.setTimeout(
        () => store.markTitleGenerationWaiting(id, provisionalTitle),
        2_000,
      )
    : 0;
};

export const titleGenerationTimeoutMs = (model: string | undefined): number =>
  model?.startsWith("local/") ? 11 * 60_000 : 48_000;
