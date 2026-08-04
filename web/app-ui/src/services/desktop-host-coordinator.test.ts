import { describe, expect, it, vi } from "vitest";

import type {
  HostLifecycleBatch,
  WatchHostLifecycleOptions,
} from "./host-client.js";
import {
  DesktopHostCoordinator,
  type DesktopCloseActions,
} from "./desktop-host-coordinator.js";

const batch = (
  cursor: number,
  pendingClose?: { readonly requestId: number; readonly waitingForIdle: boolean },
): HostLifecycleBatch => ({
  cursor,
  state: {
    focused: true,
    visible: true,
    occluded: false,
    pendingClose,
  },
  events: [],
});

const settle = async (): Promise<void> => {
  await new Promise((resolve) => setTimeout(resolve, 0));
};

describe("DesktopHostCoordinator", () => {
  it("keeps close UI and protocol-backed idle policy in the frontend", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const watchLifecycle = vi.fn(
      (
        callback: (value: HostLifecycleBatch) => void,
        options: WatchHostLifecycleOptions,
      ) => {
        receive = callback;
        return new Promise<void>((resolve) => {
          options.signal?.addEventListener("abort", () => resolve(), { once: true });
        });
      },
    );
    const resolveClose = vi.fn(
      async (_requestId: number, _decision: "cancel" | "quit_now" | "quit_when_idle") =>
        undefined,
    );
    const setSleepInhibition = vi.fn(async (_active: boolean) => undefined);
    let actions: DesktopCloseActions | undefined;
    const coordinator = new DesktopHostCoordinator(
      { watchLifecycle, resolveClose, setSleepInhibition },
      {
        onCloseRequested: (_request, nextActions) => {
          actions = nextActions;
        },
      },
    );

    coordinator.start();
    coordinator.updateActivity({
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    });
    receive(batch(1, { requestId: 7, waitingForIdle: false }));
    expect(actions).toBeDefined();
    await actions!.quitWhenIdle();
    expect(resolveClose).toHaveBeenCalledWith(7, "quit_when_idle");
    expect(resolveClose).not.toHaveBeenCalledWith(7, "quit_now");

    coordinator.updateActivity({
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();
    expect(resolveClose).toHaveBeenLastCalledWith(7, "quit_now");
    expect(setSleepInhibition.mock.calls.map(([active]) => active)).toEqual([
      true,
      false,
    ]);
    coordinator.stop();
  });

  it("recovers a retained quit-when-idle request from lifecycle state", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const resolveClose = vi.fn(
      async (_requestId: number, _decision: "cancel" | "quit_now" | "quit_when_idle") =>
        undefined,
    );
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { onCloseRequested: vi.fn() },
    );
    coordinator.start();
    coordinator.updateActivity({
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    receive(batch(9, { requestId: 31, waitingForIdle: true }));
    await settle();
    expect(resolveClose).toHaveBeenCalledWith(31, "quit_now");
  });

  it("does not inhibit sleep when the general preference is disabled", async () => {
    const setSleepInhibition = vi.fn(async (_active: boolean) => undefined);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async () => new Promise<void>(() => undefined),
        resolveClose: async () => undefined,
        setSleepInhibition,
      },
      { onCloseRequested: vi.fn() },
    );
    coordinator.updateActivity({
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: false,
    });
    await settle();
    expect(setSleepInhibition).not.toHaveBeenCalled();
  });

  it("retries a failed sleep request only after an idle-to-active transition", async () => {
    const unavailable = new Error("sleep inhibition is unavailable");
    const setSleepInhibition = vi
      .fn<(active: boolean) => Promise<void>>()
      .mockRejectedValueOnce(unavailable)
      .mockResolvedValue(undefined);
    const onDiagnostic = vi.fn();
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async () => new Promise<void>(() => undefined),
        resolveClose: async () => undefined,
        setSleepInhibition,
      },
      { onCloseRequested: vi.fn(), onDiagnostic },
    );

    const active = {
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    } as const;
    coordinator.updateActivity(active);
    await settle();
    expect(setSleepInhibition).toHaveBeenCalledTimes(1);
    expect(onDiagnostic).toHaveBeenCalledWith(unavailable);

    // Ordinary renders report the same state repeatedly. They must not turn a
    // native failure into a tight retry loop.
    coordinator.updateActivity(active);
    coordinator.updateActivity(active);
    await settle();
    expect(setSleepInhibition).toHaveBeenCalledTimes(1);

    coordinator.updateActivity({ ...active, idle: true, workRunning: false });
    coordinator.updateActivity(active);
    await settle();
    expect(setSleepInhibition.mock.calls.map(([value]) => value)).toEqual([
      true,
      true,
    ]);
  });

  it("applies a desired-state transition that occurs during a failed request", async () => {
    let rejectRequest!: (error: unknown) => void;
    const setSleepInhibition = vi
      .fn<(active: boolean) => Promise<void>>()
      .mockImplementationOnce(
        () =>
          new Promise<void>((_resolve, reject) => {
            rejectRequest = reject;
          }),
      )
      .mockResolvedValue(undefined);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async () => new Promise<void>(() => undefined),
        resolveClose: async () => undefined,
        setSleepInhibition,
      },
      { onCloseRequested: vi.fn() },
    );

    coordinator.updateActivity({
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    });
    coordinator.updateActivity({
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    coordinator.updateActivity({
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    });
    rejectRequest(new Error("lost race"));
    await settle();

    expect(setSleepInhibition.mock.calls.map(([value]) => value)).toEqual([
      true,
      true,
    ]);
  });
});
