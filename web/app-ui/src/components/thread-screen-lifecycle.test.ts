import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("./thread-screen.ts", import.meta.url), "utf8");

const section = (start: string, end: string): string => {
  const startAt = source.indexOf(start);
  const endAt = source.indexOf(end, startAt + start.length);
  expect(startAt, `missing section start: ${start}`).toBeGreaterThanOrEqual(0);
  expect(endAt, `missing section end: ${end}`).toBeGreaterThan(startAt);
  return source.slice(startAt, endAt);
};

const expectGuardBetween = (
  body: string,
  awaited: string,
  guard: string,
  mutation: string,
): void => {
  const awaitedAt = body.indexOf(awaited);
  const guardAt = body.indexOf(guard, awaitedAt + awaited.length);
  const mutationAt = body.indexOf(mutation, guardAt + guard.length);
  expect(awaitedAt, `missing await: ${awaited}`).toBeGreaterThanOrEqual(0);
  expect(guardAt, `missing post-await guard: ${guard}`).toBeGreaterThan(awaitedAt);
  expect(mutationAt, `missing guarded mutation: ${mutation}`).toBeGreaterThan(guardAt);
};

describe("thread screen asynchronous lifecycle guards", () => {
  const disconnected = section(
    "override disconnectedCallback(): void {",
    "\n  #selectThreadWithKeyboard(",
  );

  it("invalidates history before a late page can recreate a deleted view", () => {
    const history = section(
      "async #loadOlderHistory(loadAll: boolean, signal?: AbortSignal)",
      "\n  readonly #toggleAccessibleHistory",
    );
    expect(disconnected).toContain("this.#historyGeneration += 1;");
    expect(disconnected).toContain("this.#historyLoading = false;");
    expectGuardBetween(
      history,
      "const page = await services.protocol.threadView",
      "if (!this.#isCurrentHistoryRequest",
      "store.prependThreadViewSnapshot",
    );
  });

  it("cancels find-owned history requests across query and view lifecycles", () => {
    const find = section(
      "#cancelChatFindHistoryLoading(): void {",
      "\n  #selectThreadWithKeyboard(",
    );
    const history = section(
      "async #loadOlderHistory(loadAll: boolean, signal?: AbortSignal)",
      "\n  readonly #toggleAccessibleHistory",
    );
    expect(find).toContain("this.#chatFindHistoryAbort?.abort();");
    expect(find).toContain('this.#chatFindQuery.trim() === ""');
    expect(find).toContain("new AbortController()");
    expect(find).toContain("this.#loadOlderHistory(false, abort.signal)");
    expect(history).toContain("!signal?.aborted");
    expect(find).toContain("const restorationPending = restoredActiveUnitId !== undefined");
    expect(find).toContain("&& view.hasOlder");
    expect(disconnected).toContain("this.#cancelChatFindHistoryLoading();");
  });

  it("guards an accepted message before reconciling optimistic store state", () => {
    const composer = section(
      "async #submitComposer(form: HTMLFormElement, steering: boolean)",
      "\n  readonly #filesSelected",
    );
    expectGuardBetween(
      composer,
      "const accepted = await services.protocol.sendMessage",
      "if (!this.#isCurrentTurnRequest",
      "const pendingOptimistic = this.#optimisticPrompt;",
    );
  });

  it("guards the queue-edit refresh after its nested list request", () => {
    const saveQueued = section(
      "async #saveQueued(form: HTMLFormElement)",
      "\n  async #deleteQueued(",
    );
    expectGuardBetween(
      saveQueued,
      "const queue = await services.protocol.listQueue(threadId);",
      "if (!this.#isCurrentThreadInteraction",
      "store.replaceThreadQueue(threadId, queue);",
    );
  });

  it("guards reorder recovery after its nested list request", () => {
    const dropQueued = section(
      "async #dropQueued(",
      "\n  readonly #endQueueDrag",
    );
    expectGuardBetween(
      dropQueued,
      "const queue = await services.protocol.listQueue(threadId);",
      "if (!this.#isCurrentThreadInteraction",
      "store.replaceThreadQueue(threadId, queue);",
    );
  });

  it("invalidates checkpoint tokens and guards both late checkpoint completions", () => {
    const restore = section(
      "async #restoreTurnCheckpoint(",
      "\n  async #forkTurnCheckpoint(",
    );
    const fork = section(
      "async #forkTurnCheckpoint(",
      "\n  #renderTurnCard(",
    );
    expect(disconnected).toContain("this.#checkpointActions.reset();");
    expectGuardBetween(
      restore,
      "await services.protocol.restoreCheckpoint",
      "if (!this.#isCurrentCheckpointAction",
      "globalThis.dispatchEvent",
    );
    expectGuardBetween(
      fork,
      "const fork = await services.protocol.forkCheckpoint",
      "if (!this.#isCurrentCheckpointAction",
      "store.upsertSessionMetadata",
    );
  });

  it("owns new-thread title, creation, navigation, and initial-message completions", () => {
    const submit = section(
      "readonly #submitNewThread = async (",
      "\n  readonly #cancelNewThread",
    );
    const ownership = section(
      "#isCurrentNewThreadRequest(token: NewThreadRequestToken)",
      "\n  async #updateThreadModelOption(",
    );
    expect(disconnected).toContain("this.#newThreadRequest = undefined;");
    expectGuardBetween(
      submit,
      "const generated = await services.protocol.generateSessionTitle",
      "if (!this.#isCurrentNewThreadRequest",
      "if (generated.title.trim()",
    );
    expectGuardBetween(
      submit,
      "const thread = await services.protocol.createThread(request);",
      "if (!this.#isCurrentNewThreadRequest",
      "store.upsertThread(thread);",
    );
    expectGuardBetween(
      submit,
      "if (event.detail.initialMessage !== undefined)",
      "if (!this.#isCurrentNewThreadRequest",
      "await services.protocol.sendMessage",
    );
    expectGuardBetween(
      submit,
      "await services.protocol.sendMessage(thread.id, event.detail.initialMessage);",
      "if (!this.#isCurrentNewThreadRequest",
      "} catch {",
    );
    expect(ownership).toContain("currentThreadIds.includes(route.threadId ?? \"\")");
  });

  it("requires a connected, live session for every shared thread scope", () => {
    const currentScope = section(
      "#isCurrentThreadScope(sessionId: string, threadId: string)",
      "\n  #isCurrentHistoryRequest(",
    );
    expect(currentScope).toContain("this.isConnected");
    expect(currentScope).toContain("this.sessionId === sessionId");
    expect(currentScope).toContain("this.threadId === threadId");
    expect(currentScope).toContain("isSessionTombstoned(sessionId) !== true");
    expect(currentScope).toContain('route?.kind === "session"');
    expect(currentScope).toContain('(route.threadId ?? "") === threadId');
  });
});
