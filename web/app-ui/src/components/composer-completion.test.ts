import { describe, expect, it } from "vitest";

import {
  applyComposerCompletion,
  composerCompletionToken,
  domUtf16OffsetToProtocolUtf8,
  isComposerCompletionTokenCurrent,
  protocolUtf8OffsetToDomUtf16,
  rankComposerCompletions,
} from "./composer-completion.js";

describe("composer completion", () => {
  it("recognizes bare slash commands and the file mention under the caret", () => {
    expect(composerCompletionToken("/rev", 4)).toEqual({
      kind: "command",
      start: 0,
      end: 4,
      query: "rev",
    });
    expect(composerCompletionToken("fix @src/ma", 11)).toEqual({
      kind: "file",
      start: 4,
      end: 11,
      query: "src/ma",
    });
    expect(composerCompletionToken("fix @src/main.ts please", 8)).toEqual({
      kind: "file",
      start: 4,
      end: 8,
      query: "src",
    });
  });

  it("does not activate in arguments, emails, completed mentions, or invalid cursors", () => {
    expect(composerCompletionToken("/review staged", 14)).toBeUndefined();
    expect(composerCompletionToken("mail me@example.com", 12)).toBeUndefined();
    expect(composerCompletionToken("@src/main.ts done", 17)).toBeUndefined();
    expect(composerCompletionToken("@😀", 2)).toBeUndefined();
    expect(composerCompletionToken("@x", 0)).toBeUndefined();
    expect(composerCompletionToken("@x", 99)).toBeUndefined();
  });

  it("shares an exact DOM UTF-16 to protocol UTF-8 offset conversion", () => {
    const draft = "fix 😀 @源/main.rs";
    expect(domUtf16OffsetToProtocolUtf8(draft, 6)).toBe(8);
    expect(protocolUtf8OffsetToDomUtf16(draft, 8)).toBe(6);
    expect(domUtf16OffsetToProtocolUtf8(draft, 5)).toBeUndefined();
    expect(protocolUtf8OffsetToDomUtf16(draft, 5)).toBeUndefined();
    expect(domUtf16OffsetToProtocolUtf8(draft, 99)).toBeUndefined();
    expect(protocolUtf8OffsetToDomUtf16(draft, 99)).toBeUndefined();
  });

  it("returns protocol byte ranges while applying replacements at DOM offsets", () => {
    const draft = "fix 😀 @源/ma later";
    const cursor = draft.indexOf(" later");
    const token = composerCompletionToken(draft, cursor);
    expect(token).toEqual({
      kind: "file",
      start: 9,
      end: 16,
      query: "源/ma",
    });
    expect(applyComposerCompletion(draft, token!, "源/main.rs")).toEqual({
      draft: "fix 😀 @源/main.rs  later",
      cursor: 18,
    });
  });

  it("ranks exact, prefix, basename, substring, and subsequence matches stably", () => {
    const candidates = [
      { value: "docs/review-guide.md" },
      { value: "src/review.ts" },
      { value: "review" },
      { value: "src/request-view.ts" },
      { value: "review-later" },
    ];
    expect(rankComposerCompletions(candidates, "review").map(({ value }) => value)).toEqual([
      "review",
      "review-later",
      "src/review.ts",
      "docs/review-guide.md",
      "src/request-view.ts",
    ]);
    expect(rankComposerCompletions(candidates, "rv").map(({ value }) => value)).toContain(
      "src/request-view.ts",
    );
    expect(rankComposerCompletions(candidates, "").map(({ value }) => value)).toEqual(
      candidates.map(({ value }) => value),
    );
  });

  it("bounds results and excludes empty, oversized, and control-character labels", () => {
    const candidates = [
      { value: "" },
      { value: "safe.ts", detail: "  Safe\n  file  " },
      { value: "spoof\nrow.ts" },
      { value: "x".repeat(4_097) },
      ...Array.from({ length: 20 }, (_, index) => ({ value: `src/file-${index}.ts` })),
    ];
    const matches = rankComposerCompletions(candidates, "", 99);
    expect(matches).toHaveLength(8);
    expect(matches[0]).toMatchObject({ value: "safe.ts", detail: "Safe file" });
  });

  it("splices command and file selections without discarding surrounding draft text", () => {
    const command = composerCompletionToken("/rev", 4);
    const file = composerCompletionToken("fix @src/ma later", 11);
    expect(command).toBeDefined();
    expect(file).toBeDefined();
    expect(applyComposerCompletion("/rev", command!, "/review")).toEqual({
      draft: "/review ",
      cursor: 8,
    });
    expect(applyComposerCompletion("fix @src/ma later", file!, "src/main.ts")).toEqual({
      draft: "fix @src/main.ts  later",
      cursor: 17,
    });
  });

  it("rejects stale or non-boundary protocol replacement ranges", () => {
    const stale = composerCompletionToken("fix @src", 8);
    expect(stale).toBeDefined();
    expect(applyComposerCompletion("fix @other", stale!, "src/main.ts")).toBeUndefined();
    expect(applyComposerCompletion("😀 @src", {
      kind: "file",
      start: 1,
      end: 9,
      query: "src",
    }, "src/main.ts")).toBeUndefined();
    expect(isComposerCompletionTokenCurrent("fix @src later", 8, stale!)).toBe(true);
    expect(isComposerCompletionTokenCurrent("fix @src later", 4, stale!)).toBe(false);
    expect(isComposerCompletionTokenCurrent("fix @other", 10, stale!)).toBe(false);
  });
});
