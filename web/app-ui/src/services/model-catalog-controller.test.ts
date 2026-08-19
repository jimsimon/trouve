import { describe, expect, it, vi } from "vitest";

import type { ProtocolModelInfo } from "./protocol-client.js";
import { ModelCatalogController } from "./model-catalog-controller.js";
import { readSignal } from "../state/reactivity.js";

const model = (id: string): ProtocolModelInfo => ({
  id,
  display_name: id,
  context_window: 0,
  supports_tools: true,
  options_schema: {},
});

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept;
    reject = decline;
  });
  return { promise, resolve, reject };
};

describe("ModelCatalogController", () => {
  it("returns the static snapshot before adopting live Cursor discovery", async () => {
    const live = deferred<readonly ProtocolModelInfo[]>();
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => {
        staticCalls += 1;
        return [model("cursor/default")];
      },
      refreshModels: () => {
        liveCalls += 1;
        return live.promise;
      },
    });

    await expect(controller.refresh()).resolves.toEqual([model("cursor/default")]);
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "cursor/default",
    ]);
    await expect(controller.staticModels()).resolves.toEqual([model("cursor/default")]);
    expect(readSignal(controller.liveLoaded)).toBe(false);
    expect(readSignal(controller.refreshing)).toBe(true);

    live.resolve([model("cursor/default"), model("cursor/gpt-5.6")]);
    await live.promise;
    await Promise.resolve();

    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "cursor/default",
      "cursor/gpt-5.6",
    ]);
    expect(readSignal(controller.liveLoaded)).toBe(true);
    expect(readSignal(controller.staticCurrent).map(({ id }) => id)).toEqual([
      "cursor/default",
    ]);
    await expect(controller.staticModels()).resolves.toEqual([model("cursor/default")]);
    expect(readSignal(controller.refreshing)).toBe(false);
    await expect(controller.refresh()).resolves.toHaveLength(2);
    expect(staticCalls).toBe(1);
    expect(liveCalls).toBe(1);
  });

  it("distinguishes an authoritative empty live catalog from static fallback", async () => {
    let staticCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => {
        staticCalls += 1;
        return [model("cursor/static")];
      },
      refreshModels: async () => [],
    });

    await expect(controller.refresh()).resolves.toEqual([model("cursor/static")]);
    await expect(controller.liveModels()).resolves.toEqual([]);
    expect(readSignal(controller.liveLoaded)).toBe(true);
    expect(readSignal(controller.current)).toEqual([]);
    await expect(controller.refresh()).resolves.toEqual([]);
    expect(staticCalls).toBe(1);
  });

  it("coalesces concurrent static and live discovery", async () => {
    const staticResult = deferred<readonly ProtocolModelInfo[]>();
    const liveResult = deferred<readonly ProtocolModelInfo[]>();
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: () => {
        staticCalls += 1;
        return staticResult.promise;
      },
      refreshModels: () => {
        liveCalls += 1;
        return liveResult.promise;
      },
    });

    const first = controller.refresh();
    const second = controller.refresh();
    staticResult.resolve([model("cursor/default")]);
    await Promise.all([first, second]);
    expect(staticCalls).toBe(1);
    expect(liveCalls).toBe(1);

    liveResult.resolve([model("cursor/default")]);
    await liveResult.promise;
    await Promise.resolve();
  });

  it("forces fresh static and live snapshots after connectivity recovers", async () => {
    const forcedStatic = deferred<readonly ProtocolModelInfo[]>();
    const forcedLive = deferred<readonly ProtocolModelInfo[]>();
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: () => {
        staticCalls += 1;
        return staticCalls === 1
          ? Promise.resolve([model("local/offline")])
          : forcedStatic.promise;
      },
      refreshModels: () => {
        liveCalls += 1;
        return liveCalls === 1
          ? Promise.resolve([model("local/offline")])
          : forcedLive.promise;
      },
    });

    await controller.refresh();
    await vi.waitFor(() => expect(liveCalls).toBe(1));
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "local/offline",
    ]);

    const force = controller.refresh("force");
    await vi.waitFor(() => expect(staticCalls).toBe(2));
    const joinedStatic = controller.staticModels();
    let staticResolved = false;
    void joinedStatic.then(() => {
      staticResolved = true;
    });
    await Promise.resolve();
    expect(staticResolved).toBe(false);
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "local/offline",
    ]);

    forcedStatic.resolve([model("codex/gpt-5.6-sol")]);
    await expect(Promise.all([force, joinedStatic])).resolves.toEqual([
      [model("codex/gpt-5.6-sol")],
      [model("codex/gpt-5.6-sol")],
    ]);
    await vi.waitFor(() => expect(liveCalls).toBe(2));
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "local/offline",
    ]);

    forcedLive.resolve([model("codex/gpt-5.6-sol"), model("cursor/gpt-5.6")]);
    await vi.waitFor(() => expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "codex/gpt-5.6-sol",
      "cursor/gpt-5.6",
    ]));

    expect(staticCalls).toBe(2);
    expect(readSignal(controller.staticCurrent).map(({ id }) => id)).toEqual([
      "codex/gpt-5.6-sol",
    ]);
  });

  it("uses a cached static snapshot when a forced reload fails", async () => {
    const forcedStatic = deferred<readonly ProtocolModelInfo[]>();
    let staticCalls = 0;
    const controller = new ModelCatalogController({
      models: () => {
        staticCalls += 1;
        return staticCalls === 1
          ? Promise.resolve([model("cursor/cached")])
          : forcedStatic.promise;
      },
      refreshModels: async () => [model("cursor/live")],
    });

    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live"),
    ]));

    const force = controller.refresh("force");
    await vi.waitFor(() => expect(staticCalls).toBe(2));
    const joinedStatic = controller.staticModels();
    const forceRejected = expect(force).rejects.toThrow("reload failed");
    const joinedResolved = expect(joinedStatic).resolves.toEqual([
      model("cursor/cached"),
    ]);

    forcedStatic.reject(new Error("reload failed"));
    await Promise.all([forceRejected, joinedResolved]);
    expect(readSignal(controller.staticCurrent)).toEqual([
      model("cursor/cached"),
    ]);
    expect(readSignal(controller.current)).toEqual([
      model("cursor/live"),
    ]);
  });

  it("publishes every successful live refresh until the listener unsubscribes", async () => {
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: async () => [model(`cursor/live-${++liveCalls}`)],
    });
    const published: string[][] = [];
    const unsubscribe = controller.subscribeLive((models) => {
      published.push(models.map(({ id }) => id));
    });

    await controller.liveModels("force");
    await controller.liveModels("force");
    expect(published).toEqual([
      ["cursor/live-1"],
      ["cursor/live-2"],
    ]);

    unsubscribe();
    await controller.liveModels("force");
    expect(published).toEqual([
      ["cursor/live-1"],
      ["cursor/live-2"],
    ]);
  });
});
