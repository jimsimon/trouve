import { describe, expect, it, vi } from "vitest";

import type { ProtocolSubscriptionHealth } from "./protocol-client.js";
import { SubscriptionHealthController } from "./subscription-health-controller.js";

const deferred = <T>() => {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept;
    reject = decline;
  });
  return { promise, resolve, reject };
};

const health = (providerId: string): ProtocolSubscriptionHealth => ({
  provider_id: providerId,
  status: "ok",
  plan: "",
  credits: "",
  note: "",
  windows: [],
});

describe("SubscriptionHealthController", () => {
  it("shares in-flight work and throttles ordinary refreshes for 30 seconds", async () => {
    let now = 1_000;
    const first = deferred<readonly ProtocolSubscriptionHealth[]>();
    const subscriptionHealth = vi.fn(() => first.promise);
    const controller = new SubscriptionHealthController(
      { subscriptionHealth },
      { now: () => now },
    );

    const one = controller.refresh();
    const two = controller.refresh();
    expect(subscriptionHealth).toHaveBeenCalledTimes(1);
    first.resolve([health("codex")]);
    await expect(one).resolves.toEqual([health("codex")]);
    await expect(two).resolves.toEqual([health("codex")]);

    now += 29_999;
    await expect(controller.refresh()).resolves.toEqual([health("codex")]);
    expect(subscriptionHealth).toHaveBeenCalledTimes(1);
    now += 1;
    subscriptionHealth.mockResolvedValueOnce([health("claude")]);
    await expect(controller.refresh()).resolves.toEqual([health("claude")]);
    expect(subscriptionHealth).toHaveBeenCalledTimes(2);
  });

  it("lets a force refresh invalidate an older in-flight response", async () => {
    const old = deferred<readonly ProtocolSubscriptionHealth[]>();
    const current = deferred<readonly ProtocolSubscriptionHealth[]>();
    const subscriptionHealth = vi.fn()
      .mockReturnValueOnce(old.promise)
      .mockReturnValueOnce(current.promise);
    const controller = new SubscriptionHealthController({ subscriptionHealth });

    const oldRequest = controller.refresh();
    const currentRequest = controller.refresh("force");
    current.resolve([health("current")]);
    await expect(currentRequest).resolves.toEqual([health("current")]);
    old.resolve([health("stale")]);
    await expect(oldRequest).resolves.toEqual([health("current")]);
    expect(controller.current.get()).toEqual([health("current")]);
  });

  it("throttles a failed ordinary probe until its freshness boundary", async () => {
    let now = 5_000;
    const subscriptionHealth = vi.fn()
      .mockRejectedValueOnce(new Error("unavailable"))
      .mockResolvedValueOnce([health("recovered")]);
    const controller = new SubscriptionHealthController(
      { subscriptionHealth },
      { now: () => now },
    );

    await expect(controller.refresh()).rejects.toThrow("unavailable");
    await expect(controller.refresh()).resolves.toEqual([]);
    expect(subscriptionHealth).toHaveBeenCalledTimes(1);
    now += 30_000;
    await expect(controller.refresh()).resolves.toEqual([health("recovered")]);
  });
});
