import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

describe("terminal control integration", () => {
  const panel = readFileSync(new URL("./terminal-panel.ts", import.meta.url), "utf8");
  const view = readFileSync(new URL("./terminal-view.ts", import.meta.url), "utf8");

  it("keeps search, selection copy, and paste in the active xterm view", () => {
    expect(panel).toContain("findNext(this.#searchQuery)");
    expect(panel).toContain("findPrevious(this.#searchQuery)");
    expect(panel).toContain("navigator.clipboard.writeText(selection)");
    expect(panel).toContain("navigator.clipboard.readText()");
    expect(view).toContain("this.#terminal?.getSelection()");
    expect(view).toContain("this.#terminal?.paste(text)");
  });

  it("restarts only the active tab and preserves its position", () => {
    expect(panel).toContain("services.protocol.killTerminal(oldId)");
    expect(panel).toContain("services.protocol.createTerminal(");
    expect(panel).toContain("terminals[index] = terminal");
    expect(panel).toContain("retainedTitle");
  });

  it("opens and focuses a shell on the first visit when the session has no PTY", () => {
    expect(panel).toContain("if (terminals.length === 0)");
    expect(panel).toContain("services.protocol.openTerminal(");
    expect(panel).toContain("this.#view(this.#activeId)?.focus()");
    expect(view).toContain("#focusRequested = true");
    expect(view).toContain("if (this.#focusRequested)");
  });

  it("replays retained output for terminals that have already exited", () => {
    expect(panel).toContain("if (this.#streams.has(terminal.id)) return;");
    expect(panel).not.toContain("if (terminal.exited || this.#streams.has(terminal.id)) return;");
  });

  it("distinguishes expired backlog from malformed terminal output", () => {
    expect(panel).toContain('diagnostic.kind === "non-contiguous-offset"');
    expect(panel).toContain("Some earlier terminal output is no longer available.");
    expect(panel).toContain("Some terminal output could not be decoded.");
  });

  it("enables the xterm API required by the Unicode 11 addon", () => {
    expect(view).toContain("allowProposedApi: true");
    expect(view).toContain("new unicode.Unicode11Addon()");
    expect(view).not.toContain("allowProposedApi: false");
  });

  it("uses the shared roving horizontal-tab keyboard model", () => {
    expect(panel).toContain("nextHorizontalTabIndex(event.key, index");
    expect(panel).toContain("rovingTabIndex(");
  });

  it("retains keyed background parsers and native terminal control feedback", () => {
    expect(panel).toContain("repeat(");
    expect(panel).toContain("(terminal) => terminal.id");
    expect(view).not.toContain("else this.#disposeRenderer()");
    expect(view).toContain("terminal.onBell(");
    expect(view).toContain("terminal.onTitleChange(");
    expect(view).toContain("terminal.onScroll(");
    expect(view).toContain("buffer.baseY - buffer.viewportY");
    expect(view).toContain("history · ${this.#historyLines}");
    expect(view).toContain('registerCsiHandler({ final: "t" }');
    expect(view).toContain("blocked terminal clipboard read");
  });
});
