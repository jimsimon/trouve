import { describe, expect, it } from "vitest";

import { threadNavigationTitle } from "./thread-title.js";

const thread = {
  id: "th_1",
  mode: "code",
  model: "codex/gpt-5.6-sol",
};

describe("threadNavigationTitle", () => {
  it("uses the session name for its initial thread", () => {
    expect(threadNavigationTitle({
      thread: { ...thread, title: "Prompt-derived title" },
      sessionTitle: "Session name",
      initialThreadId: "th_1",
      modeDisplayName: "Code",
    })).toBe("Session name");
  });

  it("uses durable prompt-derived titles for later user threads", () => {
    expect(threadNavigationTitle({
      thread: { ...thread, id: "th_2", title: "Review the parser edge cases" },
      sessionTitle: "Session name",
      initialThreadId: "th_1",
    })).toBe("Review the parser edge cases");
  });

  it("prefixes subagents exactly once", () => {
    expect(threadNavigationTitle({
      thread: { ...thread, id: "th_child", title: "Inspect native hosting", spawned: true },
      sessionTitle: "Session name",
      initialThreadId: "th_1",
    })).toBe("Subagent: Inspect native hosting");
    expect(threadNavigationTitle({
      thread: { ...thread, id: "th_child", title: "Subagent: Inspect native hosting" },
      sessionTitle: "Session name",
      initialThreadId: "th_1",
    })).toBe("Subagent: Inspect native hosting");
  });

  it("falls back to mode and short model metadata for legacy threads", () => {
    expect(threadNavigationTitle({
      thread: { ...thread, id: "th_legacy" },
      sessionTitle: "Session name",
      initialThreadId: "th_1",
      modeDisplayName: "Code",
    })).toBe("Code · gpt-5.6-sol");
  });
});
