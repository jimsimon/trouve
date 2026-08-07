import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("thread composer and queue integration", () => {
  const screen = readFileSync(new URL("./thread-screen.ts", import.meta.url), "utf8");

  it("shows explicit completion loading, empty, unavailable, and retry states", () => {
    expect(screen).toContain("Loading workspace files…");
    expect(screen).toContain("Slash commands are unavailable for this thread.");
    expect(screen).toContain("No matching workspace files.");
    expect(screen).toContain("File suggestions are unavailable.");
    expect(screen).toContain("#retryMentionPaths");
    expect(screen).toContain("generation !== this.#pathsGeneration");
    expect(screen).toContain("isComposerCompletionTokenCurrent");
    expect(screen).toContain("sourceStillContainsValue");
  });

  it("keeps input autogrow and submission safe across IME composition", () => {
    expect(screen).toContain("composerTextareaLayout(textarea.scrollHeight)");
    expect(screen).toContain("@compositionstart=${this.#composerCompositionStarted}");
    expect(screen).toContain("@compositionend=${this.#composerCompositionEnded}");
    expect(screen).toContain("isComposerCompositionKey({");
    expect(screen).toContain(".value=${live(this.#composerDraft)}");
  });

  it("renders model-derived thinking, context, and fast controls", () => {
    expect(screen).toContain("modelOptionControls(selectedModel, thread?.model_options)");
    expect(screen).toContain('aria-label="Thinking level"');
    expect(screen).toContain('aria-label="Context size"');
    expect(screen).toContain('"fast",');
    expect(screen).toContain("...(thread.model_options ?? {})");
  });

  it("keeps queue mutations disabled, recoverable, and explicit on failure", () => {
    expect(screen).toContain("queueControlState({");
    expect(screen).toContain("Editing queued prompt");
    expect(screen).toContain('title="Update queued prompt"');
    expect(screen).toContain("#queueEditRetainedAttachments");
    expect(screen).toContain("retained_attachment_ids:");
    expect(screen).toContain("#restoreComposerAfterQueueEdit");
    expect(screen).toContain('data-queue-action="send-now"');
    expect(screen).toContain('data-queue-action="dispatch"');
    expect(screen).toContain("#sendQueuedNow");
    expect(screen).toContain("#focusQueueControlNow");
    expect(screen).toContain("Your edit is still available.");
    expect(screen).toContain("The prompts remain queued.");
    expect(screen).toContain("The prompt was moved first, but the queue could not be started.");
  });
});
