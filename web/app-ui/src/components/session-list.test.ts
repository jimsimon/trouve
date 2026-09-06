import { readFileSync } from "node:fs";

import { describe, expect, it, vi } from "vitest";

import type { AppStore, SessionListItem } from "../state/app-store.js";
import {
  sessionIndicatorPresentation,
  type SessionIndicatorFields,
} from "../state/session-indicator-model.js";
import { enrichWorkspaceSessions } from "./session-list.js";

const session = (id: string, workspaceId: string): SessionListItem => ({
  id,
  workspaceId,
  title: id,
  branch: "main",
  archived: false,
  active: false,
  attention: "none",
  outcome: "idle",
  latestThreadId: undefined,
  updatedAt: "2026-08-22T00:00:00Z",
  state: "idle",
  unread: false,
});

describe("session list component contract", () => {
  const read = (path: string): string =>
    readFileSync(new URL(path, import.meta.url), "utf8");
  const component = read("./session-list.ts");
  const shell = read("../app/trouve-app.ts");
  const styles = read("../styles/app.css");

  it("filters by workspace before deriving metadata and pull-request state", () => {
    const sessionMetadata = vi.fn(() => undefined);
    const sessionPullRequests = vi.fn(() => []);
    const store = {
      sessionMetadata,
      sessionPullRequests,
    } as unknown as Pick<AppStore, "sessionMetadata" | "sessionPullRequests">;

    const enriched = enrichWorkspaceSessions(
      store,
      [session("se_first", "ws_first"), session("se_second", "ws_second")],
      "ws_first",
    );

    expect(enriched.map(({ id }) => id)).toEqual(["se_first"]);
    expect(sessionMetadata).toHaveBeenCalledOnce();
    expect(sessionMetadata).toHaveBeenCalledWith("se_first");
    expect(sessionPullRequests).toHaveBeenCalledOnce();
    expect(sessionPullRequests).toHaveBeenCalledWith("se_first");
  });

  it("renders archived sessions as a separate accessible disclosure", () => {
    expect(component).toContain("organizeWorkspaceSessions(organizedSessions");
    expect(component).toContain("repeat(");
    expect(component).toContain("(session) => session.id");
    expect(component).toContain('? "Active sessions" : `${section.label} sessions`');
    expect(component).toContain('class="archived-session-toggle"');
    expect(component).toContain("aria-expanded=${archivedExpanded}");
    expect(component).toContain("aria-controls=${this.#archivedListId}");
    expect(component).toContain("?hidden=${!archivedExpanded}");
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

  it("keeps compact rows while optionally rendering branch names", () => {
    expect(component).toContain('<span class="session-copy">');
    expect(component).toContain("sessionTitleGenerationPresentation(session.id)");
    expect(component).toContain('class="naming-title-shimmer session-title-shimmer"');
    expect(component).toContain("Naming session…");
    expect(component).toContain('class="naming-title-pending"');
    expect(component).toContain("Waiting for the local model.");
    expect(styles).toContain(".naming-title-pending");
    expect(component).toContain("sessionAgePresentation(session.updatedAt, now)");
    expect(component).toContain('class="session-age"');
    expect(component).toContain("this.showBranches");
    expect(component).toContain('class="session-branch"');
    expect(component).toContain("${session.branch}");
    expect(styles).toMatch(/\.session-row-wrap \{[^}]*height:\s*34px/s);
    expect(styles).toMatch(/\.session-row \{[^}]*height:\s*34px/s);
    expect(styles).toContain(".session-row-wrap.with-branch, .session-row.with-branch { height: 46px; }");
    expect(styles).toMatch(
      /\.session-copy strong \{[^}]*overflow:\s*hidden[^}]*text-overflow:\s*ellipsis[^}]*white-space:\s*nowrap/s,
    );
  });

  it("keeps ages visible and opens row actions as a context menu", () => {
    expect(component).toContain("data-actions-open=${this.#menuSessionId === session.id}");
    expect(component).toContain("@contextmenu=${(event: MouseEvent) => this.#openContextMenu(event, session.id)}");
    expect(component).toContain('event.key !== "ContextMenu"');
    expect(component).toContain("event.shiftKey && event.key === \"F10\"");
    expect(component).not.toContain("session-menu-button");
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
    expect(component).toContain("services.tombstoneSession(sessionId)");
    expect(shell).toContain("this.#threadIngress.invalidateSession(sessionId)");
    expect(shell).toContain("this.#composerDrafts.discard(threadId)");
    expect(shell).toContain("?? inboxRecoverySession(sessions)");
    expect(shell).toContain('route.kind === "inbox" && recoverySession !== undefined');
  });

  it("keeps actions in the contextual popup and rename/delete in a modal", () => {
    expect(component).toContain('class="session-actions"');
    expect(component).toContain('class="session-modal"');
    expect(component).toContain('dialog.showModal()');
    expect(component).toContain('>Rename session</h2>');
    expect(component).toContain('generateSessionTitleSuggestion(sessionId, {');
    expect(component).toContain('signal: abort.signal');
    expect(component).toContain('? "Generating…" : "Generate"');
    expect(component).toContain("You can still enter one manually.");
    expect(component).toContain('>Delete session “${this.#modalTitle}”?</h2>');
    expect(component).toContain("This removes the session's worktree, branch history in trouve, and its event log. The git branch itself is kept.");
    expect(styles).toMatch(
      /\.session-actions \{[^}]*position: fixed;[^}]*width: 150px;/u,
    );
    expect(styles).toContain('.session-modal { width: min(380px, calc(100vw - 32px));');
  });
});
