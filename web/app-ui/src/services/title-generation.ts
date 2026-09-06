import type { AppStore } from "../state/app-store.js";

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

export const titleGenerationTimeoutMs = (): number =>
  // The naming-settings event can race the request that changed it. Use an
  // envelope large enough for either server budget so a stale cloud model
  // snapshot cannot abort a local request while it is legitimately queued.
  LOCAL_TITLE_GENERATION_TIMEOUT_MS;
