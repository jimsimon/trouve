import { describe, expect, it } from "vitest";

import {
  LOCAL_MODEL_WAITING_LABEL,
  LOCAL_TITLE_GENERATION_TIMEOUT_MS,
  SESSION_TITLE_WAITING_STATUS,
  THREAD_TITLE_WAITING_STATUS,
  titleGenerationTimeoutMs,
} from "./title-generation.js";

describe("title generation timing", () => {
  it("allows the server to apply either model timeout despite stale settings", () => {
    expect(titleGenerationTimeoutMs()).toBe(LOCAL_TITLE_GENERATION_TIMEOUT_MS);
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
