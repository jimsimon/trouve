import { describe, expect, it } from "vitest";

import {
  LOCAL_MODEL_WAITING_LABEL,
  LOCAL_TITLE_GENERATION_TIMEOUT_MS,
  SESSION_TITLE_WAITING_STATUS,
  THREAD_TITLE_WAITING_STATUS,
  TITLE_GENERATION_TIMED_OUT_MESSAGE,
  titleGenerationFailureMessage,
  titleGenerationTimeoutMs,
} from "./title-generation.js";

describe("title generation failure messages", () => {
  it("relays the server's reason for a failed naming request", () => {
    expect(
      titleGenerationFailureMessage(
        new Error("codex/gpt: Selected model is at capacity (a retry also failed)"),
      ),
    ).toBe(
      "Automatic naming failed: codex/gpt: Selected model is at capacity (a retry also failed)",
    );
  });

  it("explains client-side timeouts and unknown failures", () => {
    const abort = new Error("aborted");
    abort.name = "AbortError";
    expect(titleGenerationFailureMessage(abort)).toBe(TITLE_GENERATION_TIMED_OUT_MESSAGE);
    expect(titleGenerationFailureMessage(new Error("  "))).toBe("Automatic naming failed.");
    expect(titleGenerationFailureMessage("boom")).toBe("Automatic naming failed.");
  });
});

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
