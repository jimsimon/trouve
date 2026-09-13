import { describe, expect, it } from "vitest";

import { createComputed, createSignal, observeSignal } from "./reactivity.js";

const tick = (): Promise<void> => new Promise((resolve) => queueMicrotask(resolve));

describe("observeSignal", () => {
  it("delivers each distinct value once per batch of writes, then stops after dispose", async () => {
    const count = createSignal(0);
    const doubled = createComputed(() => count.get() * 2);
    const seen: number[] = [];
    const dispose = observeSignal(doubled, (value) => seen.push(value));

    count.set(1);
    count.set(2);
    await tick();
    expect(seen).toEqual([4]);

    count.set(2);
    await tick();
    expect(seen).toEqual([4]);

    count.set(3);
    await tick();
    expect(seen).toEqual([4, 6]);

    dispose();
    count.set(4);
    await tick();
    expect(seen).toEqual([4, 6]);
  });
});
