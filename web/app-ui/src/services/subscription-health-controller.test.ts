import { describe, expect, it, vi } from "vitest";

import type { ProtocolSubscriptionHealth } from "./protocol-client.js";
import {
  requestWithDeadline,
  SubscriptionHealthController,
} from "./subscription-health-controller.js";

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
  it("clears portable deadlines after successful and failed requests settle", async () => {
    vi.useFakeTimers();
    try {
      await expect(requestWithDeadline(30_000, async () => "known providers")).resolves.toBe(
        "known providers",
      );
      expect(vi.getTimerCount()).toBe(0);

      await expect(requestWithDeadline(
        30_000,
        async () => Promise.reject(new Error("health unavailable")),
      )).rejects.toThrow("health unavailable");
      expect(vi.getTimerCount()).toBe(0);

      await expect(requestWithDeadline(30_000, () => {
        throw new Error("request construction failed");
      })).rejects.toThrow("request construction failed");
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

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
    expect(controller.loading.get()).toBe(true);
    await Promise.resolve();
    expect(subscriptionHealth).toHaveBeenCalledTimes(1);
    first.resolve([health("codex")]);
    await expect(one).resolves.toEqual([health("codex")]);
    await expect(two).resolves.toEqual([health("codex")]);
    expect(controller.loading.get()).toBe(false);

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
    expect(controller.loading.get()).toBe(true);
    current.resolve([health("current")]);
    await expect(currentRequest).resolves.toEqual([health("current")]);
    expect(controller.loading.get()).toBe(false);
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

  it("aborts a stalled probe and allows a forced replacement", async () => {
    const subscriptionHealth = vi.fn((signal?: AbortSignal) =>
      new Promise<readonly ProtocolSubscriptionHealth[]>((_resolve, reject) => {
        signal?.addEventListener("abort", () => reject(new Error("aborted")));
      })
    );
    const controller = new SubscriptionHealthController(
      { subscriptionHealth },
      { requestTimeoutMs: 5 },
    );

    await expect(controller.refresh()).rejects.toThrow("aborted");
    expect(controller.loading.get()).toBe(false);

    subscriptionHealth.mockResolvedValueOnce([health("recovered")]);
    await expect(controller.refresh("force")).resolves.toEqual([health("recovered")]);
    expect(subscriptionHealth).toHaveBeenCalledTimes(2);
  });

  it("settles loading after synchronous request construction fails", async () => {
    const subscriptionHealth = vi.fn()
      .mockImplementationOnce(() => {
        throw new Error("unsupported request option");
      })
      .mockResolvedValueOnce([health("recovered")]);
    const controller = new SubscriptionHealthController({ subscriptionHealth });

    await expect(controller.refresh()).rejects.toThrow("unsupported request option");
    expect(controller.loading.get()).toBe(false);
    await expect(controller.refresh("force")).resolves.toEqual([health("recovered")]);
  });
});
