import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./trouve-app.ts", import.meta.url), "utf8");

describe("root shell parity wiring", () => {
  it("uses the dedicated todo projection in the inspection shell", () => {
    expect(source).toContain('import "../components/todo-plan-panel.js"');
    expect(source).toContain("<trouve-todo-plan-panel");
    expect(source).not.toContain(".todos=${activeView.todos}");
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
});
