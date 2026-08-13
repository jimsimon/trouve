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
  const promise = new Promise<T>((accept) => {
    resolve = accept;
  });
  return { promise, resolve };
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
    expect(readSignal(controller.refreshing)).toBe(true);

    live.resolve([model("cursor/default"), model("cursor/gpt-5.6")]);
    await live.promise;
    await Promise.resolve();

    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "cursor/default",
      "cursor/gpt-5.6",
    ]);
    expect(readSignal(controller.refreshing)).toBe(false);
    await expect(controller.refresh()).resolves.toHaveLength(2);
    expect(staticCalls).toBe(1);
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
    let staticCalls = 0;
    let liveCalls = 0;
    const controller = new ModelCatalogController({
      models: async () => {
        staticCalls += 1;
        return staticCalls === 1
          ? [model("local/offline")]
          : [model("codex/gpt-5.6-sol")];
      },
      refreshModels: async () => {
        liveCalls += 1;
        return liveCalls === 1
          ? [model("local/offline")]
          : [model("codex/gpt-5.6-sol"), model("cursor/gpt-5.6")];
      },
    });

    await controller.refresh();
    await vi.waitFor(() => expect(liveCalls).toBe(1));
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "local/offline",
    ]);

    await controller.refresh("force");
    await vi.waitFor(() => expect(liveCalls).toBe(2));

    expect(staticCalls).toBe(2);
    expect(readSignal(controller.current).map(({ id }) => id)).toEqual([
      "codex/gpt-5.6-sol",
      "cursor/gpt-5.6",
    ]);
  });
});
