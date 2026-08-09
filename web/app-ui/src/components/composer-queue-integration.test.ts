import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("thread composer and queue integration", () => {
  const screen = readFileSync(new URL("./thread-screen.ts", import.meta.url), "utf8");

  it("shows explicit completion loading, empty, unavailable, and automatic retry states", () => {
    expect(screen).toContain("Loading workspace files…");
    expect(screen).toContain("Slash commands are unavailable for this thread.");
    expect(screen).toContain("No matching workspace files.");
    expect(screen).toContain("File suggestions are unavailable.");
    expect(screen).toContain("#scheduleMentionPathsRetry(sessionId)");
    expect(screen).toContain("trouve will retry automatically");
    expect(screen).toContain("generation !== this.#pathsGeneration");
    expect(screen).toContain("isComposerCompletionTokenCurrent");
    expect(screen).toContain("sourceStillContainsValue");
  });

  it("keeps input autogrow and submission safe across IME composition", () => {
    expect(screen).toContain("composerTextareaLayout(");
    expect(screen).toContain("textarea.value.length > 0");
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
    expect(screen).toContain("dispatchQueuedPrompt(promptId)");
    expect(screen).toContain("Send now and stop current turn");
    expect(screen).toContain("#focusQueueControlNow");
    expect(screen).toContain("Your edit is still available.");
    expect(screen).toContain("The prompts remain queued.");
  });

  it("reorders queued prompts from the focusable row without move buttons", () => {
    expect(screen).toContain("#queueRowKeyDown");
    expect(screen).toContain("#commitQueueKeyboardReorder");
    expect(screen).toContain('aria-describedby=${keyboardFocusable ? "queue-reorder-instructions"');
    expect(screen).toContain('aria-keyshortcuts=${keyboardFocusable');
    expect(screen).toContain("Press Space or Enter to pick up this queued prompt");
    expect(screen).toContain('data-keyboard-reordering=${keyboardActive ? "true"');
    expect(screen).not.toContain('data-queue-action="earlier"');
    expect(screen).not.toContain('data-queue-action="later"');
  });
});
