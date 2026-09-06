import { describe, expect, it } from "vitest";

import {
  LOCAL_MODEL_WAITING_LABEL,
  LOCAL_TITLE_GENERATION_TIMEOUT_MS,
  SESSION_TITLE_WAITING_STATUS,
  THREAD_TITLE_WAITING_STATUS,
  TITLE_GENERATION_TIMEOUT_MS,
  titleGenerationTimeoutMs,
} from "./title-generation.js";

describe("title generation timing", () => {
  it("reserves the extended admission budget only for managed local models", () => {
    expect(titleGenerationTimeoutMs("local/qwen")).toBe(LOCAL_TITLE_GENERATION_TIMEOUT_MS);
    expect(titleGenerationTimeoutMs("openai/gpt-5")).toBe(TITLE_GENERATION_TIMEOUT_MS);
    expect(titleGenerationTimeoutMs(undefined)).toBe(TITLE_GENERATION_TIMEOUT_MS);
  });

  it("identifies the pending title target in assistive status text", () => {
    expect(SESSION_TITLE_WAITING_STATUS).toBe(
      `Session name pending. ${LOCAL_MODEL_WAITING_LABEL}`,
    );
    expect(THREAD_TITLE_WAITING_STATUS).toBe(
      `Thread name pending. ${LOCAL_MODEL_WAITING_LABEL}`,
    );
  });
});
