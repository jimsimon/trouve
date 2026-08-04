import { describe, expect, it } from "vitest";

import {
  BrowserWakeLockCoordinator,
  browserWakeLockCapability,
  type WakeLockDocumentLike,
  type WakeLockSentinelLike,
} from "./browser-wake-lock.js";

class FakeDocument implements WakeLockDocumentLike {
  visibilityState: DocumentVisibilityState = "visible";
  readonly listeners = new Set<() => void>();
  addEventListener(_type: "visibilitychange", listener: () => void): void {
    this.listeners.add(listener);
  }
  removeEventListener(_type: "visibilitychange", listener: () => void): void {
    this.listeners.delete(listener);
  }
  setVisibility(value: DocumentVisibilityState): void {
    this.visibilityState = value;
    for (const listener of this.listeners) listener();
  }
}

class FakeSentinel implements WakeLockSentinelLike {
  released = false;
  readonly listeners: (() => void)[] = [];
  async release(): Promise<void> {
    if (this.released) return;
    this.released = true;
    for (const listener of this.listeners) listener();
  }
  addEventListener(_type: "release", listener: () => void): void {
    this.listeners.push(listener);
  }
}

const settle = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

describe("BrowserWakeLockCoordinator", () => {
  it("reports only a real Wake Lock API", () => {
    expect(browserWakeLockCapability({})).toBe(false);
    expect(browserWakeLockCapability({ wakeLock: { request: async () => new FakeSentinel() } })).toBe(true);
  });

  it("holds only while desired, started, and visible", async () => {
    const document = new FakeDocument();
    const sentinels: FakeSentinel[] = [];
    const coordinator = new BrowserWakeLockCoordinator({
      wakeLock: {
        request: async () => {
          const sentinel = new FakeSentinel();
          sentinels.push(sentinel);
          return sentinel;
        },
      },
    }, document);

    coordinator.start();
    coordinator.setDesired(true);
    await settle();
    expect(coordinator.held).toBe(true);
    expect(sentinels).toHaveLength(1);

    document.setVisibility("hidden");
    await settle();
    expect(sentinels[0]?.released).toBe(true);
    expect(coordinator.held).toBe(false);

    document.setVisibility("visible");
    await settle();
    expect(sentinels).toHaveLength(2);
    expect(coordinator.held).toBe(true);

    coordinator.setDesired(false);
    await settle();
    expect(sentinels[1]?.released).toBe(true);
    coordinator.stop();
    expect(document.listeners.size).toBe(0);
  });

  it("releases a request that resolves after the desired state changes", async () => {
    const document = new FakeDocument();
    const sentinel = new FakeSentinel();
    let resolveRequest!: (value: WakeLockSentinelLike) => void;
    const coordinator = new BrowserWakeLockCoordinator({
      wakeLock: {
        request: () => new Promise((resolve) => { resolveRequest = resolve; }),
      },
    }, document);
    coordinator.start();
    coordinator.setDesired(true);
    coordinator.setDesired(false);
    resolveRequest(sentinel);
    await settle();
    expect(sentinel.released).toBe(true);
    expect(coordinator.held).toBe(false);
  });
});
