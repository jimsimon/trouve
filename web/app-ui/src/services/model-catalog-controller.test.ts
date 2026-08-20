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

  it("retries a missing static catalog after an authoritative empty live result", async () => {
    let staticCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => {
        staticCalls += 1;
        if (staticCalls === 1) throw new Error("static unavailable");
        return [model("cursor/static")];
      },
      refreshModels: async () => [],
    });

    await expect(controller.liveModels()).resolves.toEqual([]);
    await expect(controller.refresh()).resolves.toEqual([]);
    await vi.waitFor(() => expect(staticCalls).toBe(2));
    await expect(controller.staticModels()).resolves.toEqual([
      model("cursor/static"),
    ]);
    expect(staticCalls).toBe(2);
  });

  it("keeps usable live models while retrying a missing static catalog", async () => {
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => {
        staticCalls += 1;
        throw new Error("static unavailable");
      },
      refreshModels: async () => [model(`cursor/live-${++liveCalls}`)],
    }, { liveTtlMs: 0 });

    await expect(controller.liveModels()).resolves.toEqual([model("cursor/live-1")]);
    await expect(controller.refresh()).resolves.toEqual([model("cursor/live-1")]);
    await vi.waitFor(() => expect(staticCalls).toBe(2));
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live-2"),
    ]));
  });

  it("discovers live models when the independent static catalog fails", async () => {
    const staticError = new Error("static unavailable");
    let liveSucceeds = true;
    const controller = new ModelCatalogController({
      models: async () => {
        throw staticError;
      },
      refreshModels: async () => {
        if (!liveSucceeds) throw new Error("live unavailable");
        return [model("cursor/live")];
      },
    });

    await expect(controller.liveModels()).resolves.toEqual([model("cursor/live")]);
    expect(readSignal(controller.current)).toEqual([model("cursor/live")]);

    liveSucceeds = false;
    await expect(controller.liveModels("force")).rejects.toBe(staticError);
  });

  it("does not wait for a stalled static catalog before live discovery", async () => {
    const staticResult = deferred<readonly ProtocolModelInfo[]>();
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: () => staticResult.promise,
      refreshModels: async () => {
        liveCalls += 1;
        return [model("cursor/live")];
      },
    });

    await expect(controller.liveModels()).resolves.toEqual([model("cursor/live")]);
    expect(liveCalls).toBe(1);
    expect(readSignal(controller.current)).toEqual([model("cursor/live")]);
  });

  it("does not wait for a stalled static catalog after live discovery fails", async () => {
    const staticResult = deferred<readonly ProtocolModelInfo[]>();
    const liveError = new Error("live unavailable");
    const controller = new ModelCatalogController({
      models: () => staticResult.promise,
      refreshModels: async () => {
        throw liveError;
      },
    });
    let rejection: unknown;

    void controller.liveModels().catch((error: unknown) => {
      rejection = error;
    });

    await vi.waitFor(() => expect(rejection).toBe(liveError));
  });

  it("reports the live failure while a static retry is pending", async () => {
    const staticRetry = deferred<readonly ProtocolModelInfo[]>();
    const initialStaticError = new Error("initial static unavailable");
    const liveError = new Error("live unavailable");
    let staticCalls = 0;
    const controller = new ModelCatalogController({
      models: () => {
        staticCalls += 1;
        return staticCalls === 1
          ? Promise.reject(initialStaticError)
          : staticRetry.promise;
      },
      refreshModels: async () => {
        throw liveError;
      },
    });

    await expect(controller.staticModels()).rejects.toBe(initialStaticError);
    const liveResult = controller.liveModels("force");

    await vi.waitFor(() => expect(staticCalls).toBe(2));
    await expect(liveResult).rejects.toBe(liveError);
    staticRetry.resolve([model("cursor/static")]);
    await expect(controller.staticModels()).resolves.toEqual([
      model("cursor/static"),
    ]);
  });

  it("installs pending live work before synchronous refresh re-entry", async () => {
    const liveResult = deferred<readonly ProtocolModelInfo[]>();
    let liveCalls = 0;
    let reentrant: Promise<readonly ProtocolModelInfo[]> | undefined;
    let controller!: ModelCatalogController;
    controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: () => {
        liveCalls += 1;
        if (liveCalls === 1) reentrant = controller.liveModels("force");
        return liveResult.promise;
      },
    });

    const first = controller.liveModels();
    await vi.waitFor(() => expect(liveCalls).toBe(1));
    expect(reentrant).toBeDefined();

    liveResult.resolve([model("cursor/live")]);
    await expect(Promise.all([first, reentrant])).resolves.toEqual([
      [model("cursor/live")],
      [model("cursor/live")],
    ]);
    expect(liveCalls).toBe(1);
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

  it("falls back to refreshed static models when a forced live refresh fails", async () => {
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model(`cursor/static-${++staticCalls}`)],
      refreshModels: async () => {
        liveCalls += 1;
        if (liveCalls === 2) throw new Error("live unavailable");
        return [model(`cursor/live-${liveCalls}`)];
      },
    });

    await controller.refresh();
    await vi.waitFor(() =>
      expect(readSignal(controller.current)).toEqual([model("cursor/live-1")]),
    );

    await expect(controller.refresh("force")).resolves.toEqual([
      model("cursor/static-2"),
    ]);
    await vi.waitFor(() =>
      expect(readSignal(controller.refreshing)).toBe(false),
    );
    expect(readSignal(controller.current)).toEqual([model("cursor/static-2")]);
    expect(readSignal(controller.liveLoaded)).toBe(false);

    await expect(controller.liveModels()).resolves.toEqual([
      model("cursor/live-3"),
    ]);
    expect(liveCalls).toBe(3);
  });

  it("preserves live models and applies a cooldown after background refresh failure", async () => {
    let now = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: async () => {
        liveCalls += 1;
        if (liveCalls === 2) throw new Error("live unavailable");
        return [model(`cursor/live-${liveCalls}`)];
      },
    }, { now: () => now, liveTtlMs: 10 });

    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live-1"),
    ]));

    now = 20;
    await expect(controller.refresh()).resolves.toEqual([model("cursor/live-1")]);
    await vi.waitFor(() => expect(readSignal(controller.refreshing)).toBe(false));
    expect(readSignal(controller.current)).toEqual([model("cursor/live-1")]);
    expect(readSignal(controller.liveLoaded)).toBe(true);

    await controller.refresh();
    expect(liveCalls).toBe(2);

    now = 40;
    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live-3"),
    ]));
  });

  it("applies forced fallback when a forced refresh joins background work", async () => {
    const background = deferred<readonly ProtocolModelInfo[]>();
    const liveError = new Error("live unavailable");
    let now = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: () => {
        liveCalls += 1;
        return liveCalls === 1
          ? Promise.resolve([model("cursor/live")])
          : background.promise;
      },
    }, { now: () => now, liveTtlMs: 10 });

    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live"),
    ]));

    now = 20;
    await controller.refresh();
    const forced = controller.liveModels("force");
    expect(liveCalls).toBe(2);
    background.reject(liveError);

    await expect(forced).rejects.toBe(liveError);
    expect(readSignal(controller.current)).toEqual([model("cursor/static")]);
    expect(readSignal(controller.liveLoaded)).toBe(false);
  });

  it("starts new forced work when force arrives after fulfillment handling", async () => {
    const background = deferred<readonly ProtocolModelInfo[]>();
    const forcedFollowup = deferred<readonly ProtocolModelInfo[]>();
    let now = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: () => {
        liveCalls += 1;
        if (liveCalls === 1) return Promise.resolve([model("cursor/live")]);
        return liveCalls === 2 ? background.promise : forcedFollowup.promise;
      },
    }, { now: () => now, liveTtlMs: 10 });

    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live"),
    ]));

    now = 20;
    await controller.refresh();
    let lateForced: Promise<readonly ProtocolModelInfo[]> | undefined;
    void background.promise.then(() => {
      lateForced = controller.liveModels("force");
    });
    background.resolve([model("cursor/background")]);

    await vi.waitFor(() => expect(liveCalls).toBe(3));
    await vi.waitFor(() => expect(lateForced).toBeDefined());
    forcedFollowup.resolve([model("cursor/forced")]);
    await expect(lateForced).resolves.toEqual([model("cursor/forced")]);
    expect(readSignal(controller.current)).toEqual([model("cursor/forced")]);
    expect(readSignal(controller.liveLoaded)).toBe(true);
  });

  it("starts new forced work when force arrives after rejection handling", async () => {
    const background = deferred<readonly ProtocolModelInfo[]>();
    const forcedFollowup = deferred<readonly ProtocolModelInfo[]>();
    const backgroundError = new Error("background unavailable");
    const forcedError = new Error("forced unavailable");
    let now = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => [model("cursor/static")],
      refreshModels: () => {
        liveCalls += 1;
        if (liveCalls === 1) return Promise.resolve([model("cursor/live")]);
        return liveCalls === 2 ? background.promise : forcedFollowup.promise;
      },
    }, { now: () => now, liveTtlMs: 10 });

    await controller.refresh();
    await vi.waitFor(() => expect(readSignal(controller.current)).toEqual([
      model("cursor/live"),
    ]));

    now = 20;
    await controller.refresh();
    let lateForced: Promise<readonly ProtocolModelInfo[]> | undefined;
    void background.promise.catch(() => {
      lateForced = controller.liveModels("force");
    });
    background.reject(backgroundError);

    await vi.waitFor(() => expect(liveCalls).toBe(3));
    await vi.waitFor(() => expect(lateForced).toBeDefined());
    const forcedRejected = expect(lateForced).rejects.toBe(forcedError);
    forcedFollowup.reject(forcedError);
    await forcedRejected;
    expect(readSignal(controller.current)).toEqual([model("cursor/static")]);
    expect(readSignal(controller.liveLoaded)).toBe(false);
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
