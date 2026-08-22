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

    expect(commandRetryForSubmission(prior, "th_1", "new", "", createKey)).toBe(prior);
    expect(createKey).not.toHaveBeenCalled();
  });

  it("expires the key for unrelated submissions", () => {
    const createKey = vi.fn(() => "new-key");

    expect(commandRetryForSubmission(prior, "th_1", undefined, "", createKey)).toBeUndefined();
    expect(commandRetryForSubmission(prior, "th_1", "status", "", createKey)).toMatchObject({
      name: "status",
      idempotencyKey: "new-key",
    });
    expect(commandRetryForSubmission(prior, "th_2", "new", "", createKey)).toMatchObject({
      threadId: "th_2",
      idempotencyKey: "new-key",
    });
  });
});
