import { readFileSync } from "node:fs";

import { render } from "@lit-labs/ssr";
import { describe, expect, it } from "vitest";

import type { ProtocolPrInfo } from "../services/protocol-client.js";
import {
  buildCommandPaletteItems,
  filterCommandPaletteItems,
  isCommandPaletteShortcut,
  nextCommandPaletteIndex,
  type CommandPaletteInput,
} from "./command-palette-model.js";
import { renderCommandPaletteOption } from "./command-palette.js";

const pr = (
  number: number,
  overrides: Partial<ProtocolPrInfo> = {},
): ProtocolPrInfo => ({
  host: "github.com",
  repository: "trouve-ai/trouve",
  workspace_id: "ws-app",
  number,
  url: `https://github.com/trouve-ai/trouve/pull/${number}`,
  title: `Pull request ${number}`,
  state: "open",
  draft: false,
  base: "main",
  head: "trouve/protocol-review",
  checks: [],
  reviews: [],
  author: "octocat",
  ...overrides,
});

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
      active: true,
      attention: "none",
      outcome: "running",
      unread: false,
      latestThreadId: "th-code",
      navigationThreadId: "th-code",
      pullRequests: [],
      state: "running",
    },
    {
      id: "se-review",
      workspaceId: "ws-app",
      title: "Review protocol changes",
      branch: "trouve/protocol-review",
      archived: false,
      active: false,
      attention: "approval",
      outcome: "idle",
      unread: false,
      latestThreadId: "th-review",
      navigationThreadId: "th-review",
      pullRequests: [pr(3183)],
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
      label: "Subagent: code · claude-sonnet",
      icon: "code-branch",
      detail: "Thread 2",
      current: true,
      action: {
        kind: "navigate",
        mobilePane: "thread",
        route: { threadId: "th-code", inspection: "diff" },
      },
    });
    expect(items.find((item) => item.id === "session:se-review")).toMatchObject({
      state: "attention",
      current: false,
      sessionIndicator: {
        kind: "approval",
        icon: "triangle-exclamation",
        tooltip: "Approval pending",
      },
      detail: "Trouve app · trouve/protocol-review · PR #3183",
      action: {
        kind: "navigate",
        mobilePane: "thread",
        route: { sessionId: "se-review", threadId: "th-review" },
      },
    });
    expect(items.find((item) => item.id === "session:se-review")?.pullRequestBadge)
      .toMatchObject({ tone: "blocked", count: 1 });
    expect(items.find((item) => item.id === "session:se-active")).toMatchObject({
      detail: "Trouve search · trouve/web-parity",
      current: true,
    });
  });

  it("routes inspection commands to the mobile inspection pane", () => {
    const items = buildCommandPaletteItems(input);
    expect(items.find((item) => item.id === "view:inspection:plan")).toBeUndefined();
    expect(items.find((item) => item.id === "view:inspection:info")?.label)
      .toBe("Open Details");
    expect(items.find((item) => item.id === "view:inspection:info")?.action)
      .toEqual({
        kind: "navigate",
        mobilePane: "inspection",
        route: { ...input.route, inspection: "info" },
      });
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

  it("finds sessions by associated pull request number", () => {
    const items = buildCommandPaletteItems(input);
    expect(filterCommandPaletteItems(items, "3183").map((item) => item.id))
      .toEqual(["session:se-review"]);
    expect(filterCommandPaletteItems(items, "#3183").map((item) => item.id))
      .toEqual(["session:se-review"]);
    expect(filterCommandPaletteItems(items, "pr 3183").map((item) => item.id))
      .toEqual(["session:se-review"]);
  });

  it("projects the sidebar pull-request icon tone into eligible session results", () => {
    const base = input.sessions[1]!;
    const cases = [
      ["ready", pr(44, { merge_state_status: "clean" })],
      ["blocked", pr(43, { draft: true, merge_state_status: "clean" })],
      ["merged", pr(42, { state: "merged" })],
      ["closed", pr(41, { state: "closed" })],
    ] as const;
    const items = buildCommandPaletteItems({
      ...input,
      sessions: cases.map(([tone, pullRequest]) => ({
        ...base,
        id: `se-${tone}`,
        title: `${tone} pull request`,
        attention: "none",
        outcome: "idle",
        state: "idle",
        pullRequests: [pullRequest],
      })),
    });

    for (const [tone, pullRequest] of cases) {
      expect(items.find((item) => item.id === `session:se-${tone}`)).toMatchObject({
        pullRequestBadge: {
          tone,
          count: 1,
          tooltip: expect.stringContaining(`#${pullRequest.number}`),
        },
      });
    }
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
    expect(component).toContain("pullRequests: store.sessionPullRequests(session.id)");
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

  it("renders session results with the shared sidebar status and pull-request indicators", () => {
    expect(component).not.toContain('class="status-dot ${item.state}"');
    expect(styles).toContain(".session-indicator.busy::before");
    expect(styles).toContain(".session-pr-badge.ready { color: var(--trouve-ok); }");
    expect(styles).toContain(".session-pr-badge.blocked { color: var(--trouve-warn); }");
    expect(styles).toContain(".session-pr-badge.merged { color: var(--trouve-merged); }");
    expect(styles).toContain(".session-pr-badge.closed { color: var(--trouve-err); }");
    expect(styles).toContain(
      ".command-palette-icon .session-indicator { font-family: var(--trouve-font-sans); }",
    );
    expect(styles).toContain(".command-palette-trailing { display: flex;");
  });

  it("renders representative work statuses with Current before the PR badge", () => {
    const cases: readonly [
      Partial<CommandPaletteInput["sessions"][number]>,
      string,
      string,
    ][] = [
      [{ attention: "approval" }, "approval", "Approval pending"],
      [{ attention: "question" }, "question", "Question awaiting an answer"],
      [
        { attention: "none", outcome: "succeeded", unread: true, state: "done" },
        "unread",
        "Unviewed work",
      ],
    ];
    for (const [overrides, kind, workTooltip] of cases) {
      const item = buildCommandPaletteItems({
        ...input,
        route: {
          kind: "session",
          workspaceId: "ws-app",
          sessionId: "se-review",
          threadId: "th-review",
        },
        sessions: input.sessions.map((session) => session.id === "se-review"
          ? { ...session, ...overrides }
          : session),
      }).find(({ id }) => id === "session:se-review");
      expect(item).toBeDefined();
      const rendered = [...render(renderCommandPaletteOption(item!, 0, true, () => {}))]
        .join("");
      const trailingLabel = rendered.match(
        /class="command-palette-trailing"\s+aria-label="([^"]+)"/,
      )?.[1];

      expect(rendered.match(/class="command-palette-trailing"/g)).toHaveLength(1);
      expect(rendered).toContain(`class="session-indicator ${kind}"`);
      expect(rendered.indexOf(">Current<")).toBeLessThan(
        rendered.indexOf('class="session-pr-badge blocked"'),
      );
      expect(trailingLabel).toContain(workTooltip);
      expect(trailingLabel).toContain("Pull request\n#3183 · Unable to merge");
    }
  });

  it("opens the provisional setup for new-thread actions", () => {
    expect(shell).toContain("#openThreadSetupFromCommandPalette");
    expect(shell).toContain("?.openNewThreadSetup()");
    expect(shell).not.toContain("#createThreadFromCommandPalette");
  });
});
