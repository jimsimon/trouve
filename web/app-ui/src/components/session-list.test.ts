import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

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

  it("does not expose session state through the colored dot alone", () => {
    expect(component).toContain('class="status-dot ${session.state}" aria-hidden="true"');
    expect(component).toContain('class="session-status-text visually-hidden"');
    expect(component).toContain("Status: ${sessionStatusText(session)}");
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

  it("keeps actions in the Slint-shaped popup and rename/delete in a modal", () => {
    expect(component).toContain('class="session-actions"');
    expect(component).toContain('class="session-modal"');
    expect(component).toContain('dialog.showModal()');
    expect(component).toContain('>Rename session</h2>');
    expect(component).toContain('>Delete session “${this.#modalTitle}”?</h2>');
    expect(component).toContain("This removes the session's worktree, branch history in trouve, and its event log. The git branch itself is kept.");
    expect(styles).toContain('.session-actions { position: absolute;');
    expect(styles).toContain('width: 150px;');
    expect(styles).toContain('.session-modal { width: min(380px, calc(100vw - 32px));');
  });
});
