import { describe, expect, it, vi } from "vitest";

import type { ThreadChatItem } from "../state/thread-view-model.js";
import {
  assistantCopyText,
  collapsedChatPreview,
  copyActionLabel,
  copyChatText,
  formatAttachmentBytes,
  formatTurnDuration,
  formatTurnMetadata,
  indexChatPresentation,
  isImageAttachment,
  isVideoAttachment,
  protocolAttachmentPath,
} from "./chat-presentation.js";

describe("chat presentation", () => {
  it("copies the visible response while preserving Markdown for the context action", () => {
    const markdown = [
      "## Result",
      "",
      "- **ready** with `code`",
      "",
      "| Name | State |",
      "| --- | --- |",
      "| app | done |",
      "",
      "```ts",
      "const ready = true;",
      "```",
    ].join("\n");
    expect(assistantCopyText(markdown)).toBe([
      "Result",
      "",
      "•  ready with code",
      "",
      "Name | State",
      "app | done",
      "",
      "const ready = true;",
    ].join("\n"));
  });

  it("indexes the latest turn, terminal state, and final assistant segment", () => {
    const items: ThreadChatItem[] = [
      {
        id: "turn:3",
        kind: "turn-status",
        turn: 3,
        state: { kind: "completed", usage: { input_tokens: 20, output_tokens: 8 } },
      },
      { id: "assistant:3:1", kind: "assistant", turn: 3, content: "one", complete: true },
      { id: "assistant:3:2", kind: "assistant", turn: 3, content: "two", complete: true },
      { id: "turn:4", kind: "turn-status", turn: 4, state: { kind: "running" } },
    ];

    const index = indexChatPresentation(items);
    expect(index.latestTurn).toBe(4);
    expect([...index.lastAssistantIds]).toEqual(["assistant:3:2"]);
    expect(index.turnsWithAssistant.has(3)).toBe(true);
    expect(index.turnsWithAssistant.has(4)).toBe(false);
    expect(index.turnStates.get(3)?.kind).toBe("completed");
  });

  it.each([
    [0, "0ms"],
    [999, "999ms"],
    [1_000, "1s"],
    [59_999, "59s"],
    [65_000, "1m 05s"],
    [3_723_000, "1h 02m"],
  ])("formats %d milliseconds as %s", (duration, expected) => {
    expect(formatTurnDuration(duration)).toBe(expected);
  });

  it("formats compact usage, cost, and duration metadata", () => {
    expect(formatTurnMetadata(
      { input_tokens: 1_234, output_tokens: 56, cost_usd: 0.123456 },
      65_000,
    )).toBe("1234 in / 56 out tokens · $0.1235 · 1m 05s");
    expect(formatTurnMetadata(
      { input_tokens: 10, output_tokens: 2, cost_usd: 0 },
      undefined,
    )).toBe("10 in / 2 out tokens");
  });

  it("keeps attachment URLs same-origin and encodes path-like IDs", () => {
    expect(protocolAttachmentPath({ id: "att_abc-123" }))
      .toBe("/v1/attachments/att_abc-123");
    expect(protocolAttachmentPath({ id: "../../outside?x=1" }))
      .toBe("/v1/attachments/..%2F..%2Foutside%3Fx%3D1");
    expect(protocolAttachmentPath({ id: "" })).toBeUndefined();
    expect(protocolAttachmentPath({ id: "bad\nvalue" })).toBeUndefined();
  });

  it("identifies previewable media attachments and renders bounded byte labels", () => {
    expect(isImageAttachment({ mime: "IMAGE/PNG" })).toBe(true);
    expect(isImageAttachment({ mime: "application/pdf" })).toBe(false);
    expect(isVideoAttachment({ mime: "VIDEO/MP4" })).toBe(true);
    expect(isVideoAttachment({ mime: "video/mpeg" })).toBe(false);
    expect(isVideoAttachment({ mime: "application/pdf" })).toBe(false);
    expect(formatAttachmentBytes(900)).toBe("900 B");
    expect(formatAttachmentBytes(1_025)).toBe("2 KB");
    expect(formatAttachmentBytes(2 * 1_024 * 1_024)).toBe("2.0 MB");
  });

  it("reports clipboard success and actionable failure states", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    await expect(copyChatText("hello", { writeText })).resolves.toBe("copied");
    expect(writeText).toHaveBeenCalledWith("hello");
    await expect(copyChatText("hello", undefined)).resolves.toBe("unavailable");
    await expect(copyChatText("", { writeText })).resolves.toBe("unavailable");
    await expect(copyChatText("hello", {
      writeText: vi.fn().mockRejectedValue(new Error("denied")),
    })).resolves.toBe("failed");
    expect(copyActionLabel(undefined)).toBe("Copy");
    expect(copyActionLabel("copied")).toBe("Copied");
    expect(copyActionLabel("failed")).toBe("Copy failed");
    expect(copyActionLabel("unavailable")).toBe("Clipboard unavailable");
  });

  it("bounds collapsed previews by native UTF-8 bytes", () => {
    expect(collapsedChatPreview("\n  first line  \nsecond")).toBe("first line");
    const preview = collapsedChatPreview("🙂".repeat(100));
    expect(new TextEncoder().encode(preview).byteLength).toBeLessThanOrEqual(122);
    expect(preview).not.toContain("�");
    expect(preview.endsWith("…")).toBe(true);
  });
});
