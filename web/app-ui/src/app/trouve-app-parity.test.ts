import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./trouve-app.ts", import.meta.url), "utf8");

describe("root shell parity wiring", () => {
  it("keeps TODO state in the session overview instead of a duplicate pane", () => {
    expect(source).toContain('void import("../components/session-info-panel.js")');
    expect(source).toContain("<trouve-session-info-panel");
    expect(source).not.toContain("todo-plan-panel");
    expect(source).not.toContain('inspection === "plan"');
  });

  it("contains chat file actions to session metadata before revealing them", () => {
    expect(source).toContain("@trouve-open-file=${this.#openFile}");
    expect(source).toContain("this.#store.sessionMetadata(route.sessionId)");
    expect(source).toContain("sessionRelativeFilePath(");
    expect(source).toContain("await workspace.openFile(pending.path, pending.from, pending.to)");
  });

  it("keeps workspace ordering keyboard, pointer, and touch accessible", () => {
    expect(source).toContain("#workspaceOrderKeyDown");
    expect(source).toContain("@dragstart=${(event: DragEvent)");
    expect(source).toContain("@drop=${(event: DragEvent)");
    expect(source).toContain("Use Up and Down arrow keys or drag.");
    expect(source).toContain("this.#workspaceOrder.move(workspaceId, offset)");
  });

  it("refreshes durable pull-request projections in the background", () => {
    expect(source).toContain("const GITHUB_REFRESH_INTERVAL_MS = 30_000");
    expect(source).toContain("await this.#protocolClient.refreshGithubPrs()");
    expect(source).toContain("this.#scheduleGithubRefresh()");
    expect(source).toContain('globalThis.document?.visibilityState === "hidden"');
  });

  it("reconciles activity while desktop sleep is inhibited", () => {
    expect(source).toContain("const SLEEP_ACTIVITY_RECONCILE_INTERVAL_MS = 15_000");
    expect(source).toContain("this.#protocolIngress.reconcileSessionActivity()");
    expect(source).toContain("this.#scheduleSleepActivityReconciliation(shouldPreventSleep)");
  });

  it("leaves a route when its session is deleted by another client", () => {
    expect(source).toContain('route.kind === "session"');
    expect(source).toContain("!sessions.some((session) => session.id === route.sessionId)");
    expect(source).toContain('this.#router.navigate({ kind: "inbox" }, true)');
    expect(source).toContain("this.#tombstoneSession(event.session_id)");
    expect(source).toContain("this.#threadIngress.invalidateSession(sessionId)");
    expect(source).toContain("this.#composerDrafts.discard(threadId)");
  });

  it("renders new-session agent controls without waiting for catalog refreshes", () => {
    expect(source).toContain('name="mode"');
    expect(source).toContain('name="thinking"');
    expect(source).toContain('name="permission_mode"');
    expect(source).not.toContain(
      '?disabled=${this.#newSessionPending || this.#newSessionOptionsPending}',
    );
    expect(source).not.toContain(
      '.disabled=${this.#newSessionPending || this.#newSessionOptionsPending}',
    );
    expect(source).toContain(
      'this.#newSessionSubscriptionHealth = readSignal(this.#subscriptionHealth.current)',
    );
  });

  it("keeps async new-session defaults synchronized with native select options", () => {
    expect(source).toContain(".selected=${mode.id === this.#newSessionModeId}");
    expect(source).toContain(".selected=${value === this.#newSessionThinking}");
    expect(source).toContain(
      '.selected=${this.#newSessionPermissionMode === "ask"}',
    );
    expect(source).toContain(
      '.selected=${this.#newSessionPermissionMode === "allow_list"}',
    );
    expect(source).toContain(
      '.selected=${this.#newSessionPermissionMode === "yolo"}',
    );
    expect(source).toContain("this.#modelCatalog.staticModels()");
    expect(source).toContain("readSignal(this.#modelCatalog.staticCurrent)");
    expect(source).toContain('this.#modelCatalog.liveModels("if-stale")');
    expect(source).toContain("readSignal(this.#modelCatalog.liveLoaded)");
    expect(source).toContain("this.#modelCatalog.subscribeLive");
    expect(source).toContain("this.#unsubscribeFromNewSessionLiveModels()");
    expect(source).toContain(
      "this.#newSessionOptionsGeneration += 1;\n    this.#unsubscribeFromNewSessionLiveModels();\n    this.#protocolIngress.stop();",
    );
    expect(source).toContain(
      'if (this.#newSessionOpen && this.#newSessionWorkspaceId !== "")',
    );
    expect(source).toContain(
      "void this.#loadNewSessionOptions(this.#newSessionWorkspaceId, true)",
    );
    const submissionSnapshot = source.indexOf("const submissionOptions = {");
    expect(submissionSnapshot).toBeGreaterThan(0);
    expect(source.indexOf(
      "await this.#generateSessionTitle(prompt)",
      submissionSnapshot,
    )).toBeGreaterThan(submissionSnapshot);
    expect(source).toContain(
      "this.#reconcileNewSessionDefaults(this.#newSessionModels)",
    );
    expect(source).not.toContain("this.#reconcileNewSessionModelCatalog()");
    expect(source).toContain("this.#resetNewSessionOptionsForWorkspace(workspaceId)");
  });
});
