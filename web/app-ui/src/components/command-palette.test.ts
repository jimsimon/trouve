import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  buildCommandPaletteItems,
  filterCommandPaletteItems,
  isCommandPaletteShortcut,
  nextCommandPaletteIndex,
  type CommandPaletteInput,
} from "./command-palette-model.js";

const input: CommandPaletteInput = {
  route: {
    kind: "session",
    workspaceId: "ws-search",
    sessionId: "se-active",
    threadId: "th-code",
    inspection: "diff",
  },
  workspaces: [
    { id: "ws-app", name: "Trouve app" },
    { id: "ws-search", name: "Trouve search" },
  ],
  sessions: [
    {
      id: "se-active",
      workspaceId: "ws-search",
      title: "Preserve frontend parity",
      branch: "trouve/web-parity",
      archived: false,
      latestThreadId: "th-code",
      state: "running",
    },
    {
      id: "se-review",
      workspaceId: "ws-app",
      title: "Review protocol changes",
      branch: "trouve/protocol-review",
      archived: false,
      latestThreadId: "th-review",
      state: "attention",
    },
  ],
  activeThreads: [
    {
      id: "th-plan",
      session_id: "se-active",
      mode: "plan",
      model: "openai/gpt-5.6",
    },
    {
      id: "th-code",
      session_id: "se-active",
      mode: "code",
      model: "anthropic/claude-sonnet",
      spawned: true,
    },
  ],
};

describe("command palette model", () => {
  it("keeps current-workspace actions first and exposes session/thread switching", () => {
    const items = buildCommandPaletteItems(input);
    expect(items[0]).toMatchObject({
      id: "action:new-session:ws-search",
      label: "New session",
      detail: "Trouve search",
      action: { kind: "new-session", workspaceId: "ws-search" },
    });
    expect(items.find((item) => item.id === "action:new-thread:se-active")?.action)
      .toEqual({
        kind: "new-thread",
        workspaceId: "ws-search",
        sessionId: "se-active",
      });
    expect(items.find((item) => item.id === "thread:th-code")).toMatchObject({
      group: "Threads",
      label: "⑂ code · claude-sonnet",
      detail: "Current · Thread 2",
      action: {
        kind: "navigate",
        mobilePane: "thread",
        route: { threadId: "th-code", inspection: "diff" },
      },
    });
    expect(items.find((item) => item.id === "session:se-review")).toMatchObject({
      state: "attention",
      detail: "Trouve app · trouve/protocol-review",
      action: {
        kind: "navigate",
        mobilePane: "thread",
        route: { sessionId: "se-review", threadId: "th-review" },
      },
    });
  });

  it("routes inspection commands to the mobile inspection pane", () => {
    const items = buildCommandPaletteItems(input);
    expect(items.find((item) => item.id === "view:inspection:terminal")?.action)
      .toEqual({
        kind: "navigate",
        mobilePane: "inspection",
        route: { ...input.route, inspection: "terminal" },
      });
  });

  it("omits session-scoped actions when no session is active", () => {
    const items = buildCommandPaletteItems({
      ...input,
      route: { kind: "settings" },
      activeThreads: [],
    });
    expect(items[0]?.id).toBe("action:new-session:ws-app");
    expect(items.some((item) => item.id.startsWith("action:new-thread:"))).toBe(false);
    expect(items.some((item) => item.group === "Threads")).toBe(false);
    expect(items.some((item) => item.id.startsWith("view:inspection:"))).toBe(false);
  });

  it("matches multi-token metadata and ranks strong label matches first", () => {
    const items = buildCommandPaletteItems(input);
    expect(filterCommandPaletteItems(items, "parity search").map((item) => item.id))
      .toEqual(["session:se-active"]);
    expect(filterCommandPaletteItems(items, "protocol review")[0]?.id)
      .toBe("session:se-review");
    expect(filterCommandPaletteItems(items, "cde claude").map((item) => item.id))
      .toContain("thread:th-code");
    expect(filterCommandPaletteItems(items, "no-such-command")).toEqual([]);
  });

  it("wraps vertical selection and supports direct first/last movement", () => {
    expect(nextCommandPaletteIndex("ArrowDown", 2, 3)).toBe(0);
    expect(nextCommandPaletteIndex("ArrowUp", 0, 3)).toBe(2);
    expect(nextCommandPaletteIndex("Home", 2, 3)).toBe(0);
    expect(nextCommandPaletteIndex("End", 0, 3)).toBe(2);
    expect(nextCommandPaletteIndex("ArrowDown", 0, 0)).toBeUndefined();
    expect(nextCommandPaletteIndex("PageDown", 0, 3)).toBeUndefined();
  });

  it("recognizes only the documented unmodified Ctrl/Cmd-K shortcut", () => {
    const base = {
      key: "k",
      ctrlKey: false,
      metaKey: false,
      altKey: false,
      shiftKey: false,
      isComposing: false,
    };
    expect(isCommandPaletteShortcut({ ...base, ctrlKey: true })).toBe(true);
    expect(isCommandPaletteShortcut({ ...base, key: "K", metaKey: true })).toBe(true);
    expect(isCommandPaletteShortcut({ ...base, ctrlKey: true, shiftKey: true })).toBe(false);
    expect(isCommandPaletteShortcut({ ...base, metaKey: true, altKey: true })).toBe(false);
    expect(isCommandPaletteShortcut({ ...base, ctrlKey: true, isComposing: true })).toBe(false);
    expect(isCommandPaletteShortcut({ ...base, ctrlKey: true, repeat: true })).toBe(false);
    expect(isCommandPaletteShortcut({ ...base, key: "p", ctrlKey: true })).toBe(false);
  });
});

describe("command palette component contract", () => {
  const read = (path: string): string =>
    readFileSync(new URL(path, import.meta.url), "utf8");
  const component = read("./command-palette.ts");
  const shell = read("../app/trouve-app.ts");
  const styles = read("../styles/app.css");

  it("consumes stable contexts through the owned signal adapter", () => {
    expect(component).toContain("new ContextConsumer");
    expect(component).toContain("context: appServicesContext");
    expect(component).toContain("context: appStoreContext");
    expect(component).toContain("withSignalTracking(LitElement)");
    expect(component).toContain("readSignal(services.router.route)");
    expect(component).toContain("readSignal(store.sessions)");
  });

  it("ships the keyboard, focus, and combobox/listbox interaction contract", () => {
    expect(component).toContain('globalThis.addEventListener("keydown"');
    expect(component).toContain("isCommandPaletteShortcut(event)");
    expect(component).toContain('querySelectorAll<HTMLDialogElement>("dialog[open]")');
    expect(component).toContain("#restoreFocus");
    expect(component).toContain("dialog.showModal()");
    expect(component).toContain('role="combobox"');
    expect(component).toContain('role="listbox"');
    expect(component).toContain("aria-activedescendant");
    expect(shell).toContain('aria-keyshortcuts="Control+K Meta+K"');
    expect(shell).toContain("<trouve-command-palette>");
    expect(styles).toContain("trouve-command-palette { display: contents; }");
    expect(styles).toContain(".command-palette-option[aria-selected=\"true\"]");
    expect(styles).toContain("var(--trouve-accent-bg)");
  });

  it("opens the provisional setup for new-thread actions", () => {
    expect(shell).toContain("#openThreadSetupFromCommandPalette");
    expect(shell).toContain("?.openNewThreadSetup()");
    expect(shell).not.toContain("#createThreadFromCommandPalette");
  });
});
