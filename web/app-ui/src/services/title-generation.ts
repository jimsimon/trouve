export const TITLE_GENERATION_TIMEOUT_MS = 48_000;
// The server permits a managed local request to wait five minutes for
// foreground work and another five minutes for a cold model load.
export const LOCAL_TITLE_GENERATION_TIMEOUT_MS = 11 * 60_000;
export const TITLE_GENERATION_SHIMMER_MS = 2_000;

export const titleGenerationTimeoutMs = (model: string | undefined): number =>
  model?.startsWith("local/") === true
    ? LOCAL_TITLE_GENERATION_TIMEOUT_MS
    : TITLE_GENERATION_TIMEOUT_MS;
