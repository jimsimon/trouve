import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("thread screen provisional setup integration", () => {
  const screen = readFileSync(
    new URL("./thread-screen.ts", import.meta.url),
    "utf8",
  );
  const setup = readFileSync(
    new URL("./new-thread-setup.ts", import.meta.url),
    "utf8",
  );

  it("opens a provisional tab without eagerly creating a thread", () => {
    expect(screen).toContain("openNewThreadSetup");
    expect(screen).toContain('class="provisional-thread-tab"');
    expect(screen).toContain("<trouve-new-thread-setup");
    expect(screen).not.toContain(
      "services.protocol.createThread({ session_id: this.sessionId })",
    );
  });

  it("uses the shared title generator before creating and seeding the thread", () => {
    expect(screen).toContain("services.protocol.generateSessionTitle(prompt");
    expect(screen).toContain("request = { ...request, title: generated.title.trim() }");
    expect(screen).toContain("services.protocol.createThread(request)");
    expect(screen).toContain("store.upsertThread(thread)");
    expect(screen).toContain(
      "services.protocol.sendMessage(thread.id, event.detail.initialMessage)",
    );
    expect(screen).toContain(
      "Thread was created, but its first message could not be sent.",
    );
  });

  it("keeps cancellation local and restores focus to the new-thread trigger", () => {
    expect(screen).toContain("#cancelNewThread");
    expect(screen).toContain("this.#newThreadSetupOpen = false");
    expect(screen).toContain(
      "this.querySelector<HTMLButtonElement>('[aria-label=\"New thread\"]')?.focus()",
    );
  });

  it("seeds setup controls from the already-loaded chat catalog", () => {
    expect(screen).toContain(".catalogModes=${this.#modes}");
    expect(screen).toContain(".catalogModels=${models}");
    expect(screen).toContain(".subscriptionHealth=${this.#subscriptionHealth}");
    expect(screen).not.toContain(
      "this.#threadSettingsPending || this.#models.length === 0 || connectivityBlocked",
    );
    expect(screen).not.toContain('class="composer-option subscription-option"');
  });

  it("keeps async new-thread defaults synchronized with native select options", () => {
    expect(setup).toContain(".selected=${mode.id === this.#draft.modeId}");
    expect(setup).toContain(".selected=${value === this.#draft.thinking}");
    expect(setup).toContain('.selected=${this.#draft.permissionMode === "ask"}');
    expect(setup).toContain(
      '.selected=${this.#draft.permissionMode === "allow_list"}',
    );
    expect(setup).toContain('.selected=${this.#draft.permissionMode === "yolo"}');
  });
});
