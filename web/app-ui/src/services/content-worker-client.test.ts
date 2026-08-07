import { afterEach, describe, expect, it, vi } from "vitest";

import {
  activeContentWorkerCount,
  cachedMarkdownOffThread,
  disposeContentWorker,
  filterCommandPaletteItemsOffThread,
  prepareUnifiedDiffOffThread,
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
    expect(activeContentWorkerCount()).toBe(0);
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
