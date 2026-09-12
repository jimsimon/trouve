import { afterEach, describe, expect, it, vi } from "vitest";

import {
  activeContentWorkerCount,
  cachedMarkdownOffThread,
  disposeContentWorker,
  filterCommandPaletteItemsOffThread,
  prepareUnifiedDiffOffThread,
  rankComposerCompletionsOffThread,
  renderMarkdownOffThread,
  setContentWorkerIdleTimeoutForTests,
} from "./content-worker-client.js";

afterEach(() => {
  disposeContentWorker();
  setContentWorkerIdleTimeoutForTests(30_000);
  vi.unstubAllGlobals();
});

describe("lazy content worker", () => {
  it("uses the same bounded pure implementations when workers are unavailable", async () => {
    vi.stubGlobal("Worker", undefined);
    await expect(renderMarkdownOffThread("**safe** <script>bad()</script>"))
      .resolves.toContain("<strong>safe</strong>");
    await expect(renderMarkdownOffThread("<script>bad()</script>"))
      .resolves.not.toContain("<script>");
    await expect(prepareUnifiedDiffOffThread(
      "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old\n+new\n",
    )).resolves.toMatchObject([{ path: "a.ts" }]);
    await expect(filterCommandPaletteItemsOffThread([
      {
        id: "settings",
        group: "Views",
        label: "Settings",
        detail: "Application settings",
        keywords: "preferences",
        icon: "gear",
        action: {
          kind: "navigate",
          route: { kind: "settings" },
          mobilePane: "thread",
        },
      },
    ], "pref")).resolves.toHaveLength(1);
    await expect(rankComposerCompletionsOffThread([
      { value: "bounded.ts", detail: `${" detail ".repeat(10_000)}tail` },
    ], "", 8)).resolves.toMatchObject([
      { value: "bounded.ts", detail: expect.any(String) },
    ]);
    const [completion] = await rankComposerCompletionsOffThread([
      { value: "bounded.ts", detail: `${" detail ".repeat(10_000)}tail` },
    ], "", 8);
    expect(completion?.detail.length).toBeLessThanOrEqual(512);
    expect(activeContentWorkerCount()).toBe(0);
  });

  it("rejects oversized work before a main-thread fallback can bypass worker bounds", async () => {
    vi.stubGlobal("Worker", undefined);
    await expect(renderMarkdownOffThread("x".repeat(4 * 1024 * 1024 + 1)))
      .rejects.toThrow("content exceeds worker bounds");
    const item = {
      id: "settings",
      group: "Views",
      label: "Settings",
      detail: "Application settings",
      keywords: "preferences",
      icon: "gear" as const,
      action: {
        kind: "navigate" as const,
        route: { kind: "settings" as const },
        mobilePane: "thread" as const,
      },
    };
    await expect(filterCommandPaletteItemsOffThread(Array(10_001).fill(item), "pref"))
      .rejects.toThrow("too many fuzzy candidates");
  });

  it("starts on demand and terminates after the configured idle period", async () => {
    let terminated = 0;
    let posted = 0;
    class FakeWorker {
      readonly #listeners = new Map<string, ((event: MessageEvent<unknown>) => void)[]>();

      addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
        const callback = typeof listener === "function"
          ? listener as (event: MessageEvent<unknown>) => void
          : (event: MessageEvent<unknown>) => listener.handleEvent(event);
        this.#listeners.set(type, [...(this.#listeners.get(type) ?? []), callback]);
      }

      postMessage(request: { readonly id: number }): void {
        posted += 1;
        queueMicrotask(() => {
          const event = { data: { id: request.id, ok: true, value: "<p>worker</p>" } } as MessageEvent;
          for (const listener of this.#listeners.get("message") ?? []) listener(event);
        });
      }

      terminate(): void {
        terminated += 1;
      }
    }
    vi.stubGlobal("Worker", FakeWorker);
    setContentWorkerIdleTimeoutForTests(0);

    const first = renderMarkdownOffThread("worker");
    const duplicate = renderMarkdownOffThread("worker");
    await expect(Promise.all([first, duplicate])).resolves.toEqual([
      "<p>worker</p>",
      "<p>worker</p>",
    ]);
    expect(cachedMarkdownOffThread("worker")).toBe("<p>worker</p>");
    await expect(renderMarkdownOffThread("worker")).resolves.toBe("<p>worker</p>");
    expect(posted).toBe(1);
    expect(activeContentWorkerCount()).toBe(1);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(activeContentWorkerCount()).toBe(0);
    expect(terminated).toBe(1);
  });
});
