import { describe, expect, it } from "vitest";

import {
  LOCAL_TITLE_GENERATION_TIMEOUT_MS,
  TITLE_GENERATION_TIMEOUT_MS,
  titleGenerationTimeoutMs,
} from "./title-generation.js";

describe("title generation timing", () => {
  it("reserves the extended admission budget only for managed local models", () => {
    expect(titleGenerationTimeoutMs("local/qwen")).toBe(LOCAL_TITLE_GENERATION_TIMEOUT_MS);
    expect(titleGenerationTimeoutMs("openai/gpt-5")).toBe(TITLE_GENERATION_TIMEOUT_MS);
    expect(titleGenerationTimeoutMs(undefined)).toBe(TITLE_GENERATION_TIMEOUT_MS);
  });
});
