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
  turnResponseItemId,
} from "./chat-presentation.js";

const completed = { kind: "completed", usage: { input_tokens: 1, output_tokens: 1 } } as const;
const running = { kind: "running" } as const;
const text = (id: string, content = "text"): ThreadChatItem =>
  ({ id, kind: "assistant", turn: 1, content, complete: true });
const progress = (id: string): ThreadChatItem =>
  ({ id, kind: "progress", turn: 1, content: "progress", complete: true });
const tool = (id: string): ThreadChatItem => ({
  id,
  kind: "tool",
  callId: id,
  tool: "shell",
  args: { command: "true" },
  status: "ok",
  result: undefined,
  output: { text: "", bytes: 0, omitted: false },
});

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

  describe("turnResponseItemId", () => {
    it("treats trailing assistant text as the response regardless of turn state", () => {
      const items = [tool("t1"), text("a1"), text("a2")];
      expect(turnResponseItemId(items, running)).toBe("a2");
      expect(turnResponseItemId(items, completed)).toBe("a2");
      expect(turnResponseItemId(items, undefined)).toBe("a2");
    });

    it("keeps text followed by a tool call as progress while the turn is running", () => {
      const items = [text("a1"), tool("t1")];
      expect(turnResponseItemId(items, running)).toBeUndefined();
      expect(turnResponseItemId(items, undefined)).toBeUndefined();
    });

    it("promotes the last text of a completed turn even when a tool call followed it", () => {
      expect(turnResponseItemId([text("a1"), tool("t1")], completed)).toBe("a1");
      expect(turnResponseItemId(
        [text("a1"), tool("t1"), text("a2"), tool("t2"), tool("t3")],
        completed,
      )).toBe("a2");
    });

    it("promotes harness progress when a completed turn ends on it", () => {
      expect(turnResponseItemId([progress("p1"), tool("t1")], completed)).toBe("p1");
      expect(turnResponseItemId([text("a1"), tool("t1"), progress("p1")], completed)).toBe("p1");
    });

    it("keeps streaming progress as progress until the turn completes", () => {
      expect(turnResponseItemId([progress("p1")], running)).toBeUndefined();
      expect(turnResponseItemId([progress("p1")], undefined)).toBeUndefined();
      expect(turnResponseItemId([progress("p1")], completed)).toBe("p1");
    });

    it("does not promote text in failed or cancelled turns", () => {
      const items = [text("a1"), tool("t1")];
      expect(turnResponseItemId(items, { kind: "failed", error: "boom" })).toBeUndefined();
      expect(turnResponseItemId(items, { kind: "cancelled" })).toBeUndefined();
    });

    it("does not promote text that a steer or question follows", () => {
      const steered: ThreadChatItem = {
        id: "s1", kind: "steered", turn: 1, content: "actually…", attachments: [],
      };
      const questions: ThreadChatItem = {
        id: "q1", kind: "questions", requestId: "r1", title: undefined, questions: [], answers: undefined,
      };
      expect(turnResponseItemId([text("a1"), steered, tool("t1")], completed)).toBeUndefined();
      expect(turnResponseItemId([text("a1"), questions], completed)).toBeUndefined();
    });

    it("returns nothing for turns without agent text", () => {
      expect(turnResponseItemId([tool("t1")], completed)).toBeUndefined();
      expect(turnResponseItemId([], completed)).toBeUndefined();
    });
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
