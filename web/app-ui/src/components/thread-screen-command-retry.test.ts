import { describe, expect, it, vi } from "vitest";

import {
  commandRetryForSubmission,
  type CommandRetry,
} from "./thread-screen.js";

describe("commandRetryForSubmission", () => {
  const prior: CommandRetry = {
    threadId: "th_1",
    name: "new",
    arguments: "",
    idempotencyKey: "retry-key",
  };

  it("reuses the key only for an exact retry", () => {
    const createKey = vi.fn(() => "new-key");

    expect(commandRetryForSubmission(prior, "th_1", "new", "", true, createKey)).toBe(prior);
    expect(createKey).not.toHaveBeenCalled();
  });

  it("expires the key for unrelated submissions", () => {
    const createKey = vi.fn(() => "new-key");

    expect(commandRetryForSubmission(prior, "th_1", undefined, "", true, createKey)).toBeUndefined();
    expect(commandRetryForSubmission(prior, "th_1", "status", "", true, createKey)).toMatchObject({
      name: "status",
      idempotencyKey: "new-key",
    });
    expect(commandRetryForSubmission(prior, "th_2", "new", "", true, createKey)).toMatchObject({
      threadId: "th_2",
      idempotencyKey: "new-key",
    });
  });

  it("preserves the prior key when local validation rejects the request", () => {
    const createKey = vi.fn(() => "new-key");

    expect(commandRetryForSubmission(prior, "th_1", "redo", "", false, createKey)).toBe(prior);
    expect(createKey).not.toHaveBeenCalled();
  });
});
