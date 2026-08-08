import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  sessionIndicatorPresentation,
  type SessionIndicatorFields,
} from "../state/session-indicator-model.js";

describe("session list component contract", () => {
  const read = (path: string): string =>
    readFileSync(new URL(path, import.meta.url), "utf8");
  const component = read("./session-list.ts");
  const shell = read("../app/trouve-app.ts");
  const styles = read("../styles/app.css");

  it("renders archived sessions as a separate accessible disclosure", () => {
    expect(component).toContain("groupWorkspaceSessions(sessions");
    expect(component).toContain("repeat(");
    expect(component).toContain("(session) => session.id");
    expect(component).toContain('aria-label="Active sessions"');
    expect(component).toContain('class="archived-session-toggle"');
    expect(component).toContain("aria-expanded=${groups.archivedExpanded}");
    expect(component).toContain("aria-controls=${this.#archivedListId}");
    expect(component).toContain("?hidden=${!groups.archivedExpanded}");
    expect(component).toContain("Archived (${groups.archived.length})");
    expect(styles).toContain(".archived-session-list .session-copy strong");
    expect(styles).toContain("var(--trouve-text-mid)");
  });

  it("does not expose session state through its visual indicator alone", () => {
    expect(component).toContain('class="session-indicator ${indicator.kind}"');
    expect(component).not.toContain('class="status-dot ${session.state}"');
    expect(component).toContain('class="session-status-text visually-hidden"');
    expect(component).toContain("Status: ${sessionStatusText(session)}");
  });

  it("keeps navigation sessions to one line without rendering branch names", () => {
    expect(component).toContain('<span class="session-copy">');
    expect(component).toContain("<strong>${session.title}</strong>");
    expect(component).toContain("sessionAgePresentation(session.updatedAt, now)");
    expect(component).toContain('class="session-age"');
    expect(component).not.toContain("session.branch");
    expect(component).not.toContain("<small>");
    expect(styles).toMatch(/\.session-row-wrap \{[^}]*height:\s*34px/s);
    expect(styles).toMatch(/\.session-row \{[^}]*height:\s*34px/s);
    expect(styles).toMatch(
      /\.session-copy strong \{[^}]*overflow:\s*hidden[^}]*text-overflow:\s*ellipsis[^}]*white-space:\s*nowrap/s,
    );
  });

  it("keeps ages visible while revealing row actions only on interaction", () => {
    expect(component).toContain("data-actions-open=${this.#menuSessionId === session.id}");
    expect(styles).toMatch(
      /@media \(hover: hover\) and \(pointer: fine\) \{[^}]*\.session-menu-button \{[^}]*opacity:\s*0[^}]*pointer-events:\s*none/s,
    );
    expect(styles).toContain(".session-row-wrap:hover .session-menu-button");
    expect(styles).toContain(".session-row-wrap:focus-within .session-menu-button");
    expect(styles).toContain(".session-row-wrap:hover .session-age");
    expect(styles).toMatch(
      /\.session-copy strong \{[^}]*color:\s*var\(--trouve-text-mid\)/s,
    );
    expect(styles).toContain(".session-row-wrap.selected .session-copy strong");
  });

  it("uses the attention, error, unread, busy, and idle presentations", () => {
    expect(component).toContain("sessionIndicatorPresentation(session)");
    const idle: SessionIndicatorFields = {
      active: false,
      attention: "none",
      outcome: "idle",
      unread: false,
    };
    expect([
      sessionIndicatorPresentation({ ...idle, attention: "approval" }),
      sessionIndicatorPresentation({ ...idle, attention: "question" }),
      sessionIndicatorPresentation({ ...idle, attention: "both" }),
      sessionIndicatorPresentation({ ...idle, outcome: "failed", unread: true }),
      sessionIndicatorPresentation({ ...idle, outcome: "succeeded", unread: true }),
      sessionIndicatorPresentation({ ...idle, active: true }),
      sessionIndicatorPresentation(idle),
    ].map(({ kind, icon }) => ({ kind, icon }))).toEqual([
      { kind: "approval", icon: "triangle-exclamation" },
      { kind: "question", icon: "circle-question" },
      { kind: "both", icon: "triangle-exclamation" },
      { kind: "error", icon: "xmark" },
      { kind: "unread", icon: "circle" },
      { kind: "busy", icon: undefined },
      { kind: "none", icon: undefined },
    ]);
    expect(styles).toMatch(
      /\.session-indicator\.busy::before \{[^}]*width:\s*10px[^}]*height:\s*10px[^}]*background:\s*var\(--trouve-accent\)[^}]*animation:\s*trouve-session-busy-pulse 1\.6s linear infinite/s,
    );
    expect(styles).toContain(
      "[data-reduce-motion] .session-indicator.busy::before { animation: none; opacity: 1; }",
    );
  });

  it("preserves row actions and returns a deleted selection to shell recovery", () => {
    expect(component).toContain(">Rename</button>");
    expect(component).toContain('${session.archived ? "Unarchive" : "Archive"}');
    expect(component).toContain("await services.protocol.deleteSession(sessionId)");
    expect(component).toContain('route.kind === "session" && route.sessionId === sessionId');
    expect(component).toContain('services.router.navigate({ kind: "inbox" }, true)');
    expect(shell).toContain("?? inboxRecoverySession(sessions)");
    expect(shell).toContain('route.kind === "inbox" && recoverySession !== undefined');
  });

  it("keeps actions in the compact popup and rename/delete in a modal", () => {
    expect(component).toContain('class="session-actions"');
    expect(component).toContain('class="session-modal"');
    expect(component).toContain('dialog.showModal()');
    expect(component).toContain('>Rename session</h2>');
    expect(component).toContain('>Delete session “${this.#modalTitle}”?</h2>');
    expect(component).toContain("This removes the session's worktree, branch history in trouve, and its event log. The git branch itself is kept.");
    expect(styles).toMatch(
      /\.session-actions \{[^}]*position: absolute;[^}]*width: 150px;/u,
    );
    expect(styles).toContain('.session-modal { width: min(380px, calc(100vw - 32px));');
  });
});
