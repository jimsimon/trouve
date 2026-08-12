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

const confirmAutomaticClose = async (): Promise<boolean> => true;

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
        confirmAutomaticClose,
        onCloseRequested: (_request, nextActions) => {
          actions = nextActions;
        },
      },
    );

    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
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
      authoritative: true,
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

  it("does not infer idle from the empty store before protocol bootstrap", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    let actions: DesktopCloseActions | undefined;
    const resolveClose = vi.fn(async () => undefined);
    const onCloseRequested = vi.fn((_request, nextActions: DesktopCloseActions) => {
      actions = nextActions;
    });
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested },
    );
    coordinator.start();

    receive(batch(1, { requestId: 17, waitingForIdle: false }));
    expect(onCloseRequested).toHaveBeenCalledOnce();
    expect(resolveClose).not.toHaveBeenCalled();

    await actions!.quitWhenIdle();
    expect(resolveClose).toHaveBeenCalledWith(17, "quit_when_idle");
    expect(resolveClose).not.toHaveBeenCalledWith(17, "quit_now");

    coordinator.updateActivity({
      authoritative: false,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();
    expect(resolveClose).not.toHaveBeenCalledWith(17, "quit_now");

    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();
    expect(resolveClose).toHaveBeenLastCalledWith(17, "quit_now");
  });

  it("prompts when bootstrapped idle activity cannot be reconciled after disconnect", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const disconnected = new Error("stream disconnected");
    const resolveClose = vi.fn(async () => undefined);
    const onCloseRequested = vi.fn();
    const onDiagnostic = vi.fn();
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      {
        confirmAutomaticClose: vi.fn(async () => Promise.reject(disconnected)),
        onCloseRequested,
        onDiagnostic,
      },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });

    receive(batch(1, { requestId: 18, waitingForIdle: false }));
    await settle();

    expect(resolveClose).not.toHaveBeenCalled();
    expect(onCloseRequested).toHaveBeenCalledOnce();
    expect(onDiagnostic).toHaveBeenCalledWith(disconnected);
  });

  it("auto-closes fresh idle activity only after close-time reconciliation", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const resolveClose = vi.fn(async () => undefined);
    const onCloseRequested = vi.fn();
    const reconcileActivity = vi.fn(async () => true);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose: reconcileActivity, onCloseRequested },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });

    receive(batch(1, { requestId: 18, waitingForIdle: false }));
    receive(batch(2, { requestId: 18, waitingForIdle: false }));
    await settle();
    expect(resolveClose).toHaveBeenCalledWith(18, "quit_now");
    expect(resolveClose).toHaveBeenCalledTimes(1);
    expect(reconcileActivity).toHaveBeenCalledOnce();
    expect(onCloseRequested).not.toHaveBeenCalled();
  });

  it("acknowledges each exact close request without choosing a decision", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const acknowledgeClose = vi.fn(async () => undefined);
    const resolveClose = vi.fn(async () => undefined);
    const onCloseRequested = vi.fn();
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        acknowledgeClose,
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested },
    );
    coordinator.start();

    receive(batch(1, { requestId: 41, waitingForIdle: false }));
    receive(batch(2, { requestId: 41, waitingForIdle: false }));
    await settle();

    expect(acknowledgeClose).toHaveBeenCalledOnce();
    expect(acknowledgeClose).toHaveBeenCalledWith(41);
    expect(resolveClose).not.toHaveBeenCalled();
    expect(onCloseRequested).toHaveBeenCalledOnce();
    coordinator.stop();
  });

  it("does not retry an in-flight acknowledgement after stop", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    let rejectAcknowledgement!: (error: unknown) => void;
    const acknowledgeClose = vi.fn(
      () => new Promise<void>((_resolve, reject) => {
        rejectAcknowledgement = reject;
      }),
    );
    const scheduleRetry = vi.fn((): unknown => "retry");
    const onDiagnostic = vi.fn();
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        acknowledgeClose,
        resolveClose: async () => undefined,
        setSleepInhibition: async () => undefined,
      },
      {
        confirmAutomaticClose,
        onCloseRequested: vi.fn(),
        onDiagnostic,
        closeRetryScheduler: { set: scheduleRetry, clear: vi.fn() },
      },
    );
    coordinator.start();
    receive(batch(1, { requestId: 43, waitingForIdle: false }));
    expect(acknowledgeClose).toHaveBeenCalledOnce();

    coordinator.stop();
    rejectAcknowledgement(new Error("late acknowledgement failure"));
    await settle();

    expect(scheduleRetry).not.toHaveBeenCalled();
    expect(onDiagnostic).not.toHaveBeenCalled();
    expect(acknowledgeClose).toHaveBeenCalledTimes(1);
  });

  it("re-presents a fresh close after automatic quit resolution fails", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    let actions: DesktopCloseActions | undefined;
    const unavailable = new Error("host resolution unavailable");
    const resolveClose = vi
      .fn<(requestId: number, decision: "cancel" | "quit_now" | "quit_when_idle") => Promise<void>>()
      .mockRejectedValueOnce(unavailable)
      .mockResolvedValue(undefined);
    const onCloseRequested = vi.fn((_request, nextActions: DesktopCloseActions) => {
      actions = nextActions;
    });
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });

    receive(batch(1, { requestId: 42, waitingForIdle: false }));
    receive(batch(2, { requestId: 42, waitingForIdle: false }));
    await settle();

    expect(resolveClose).toHaveBeenCalledTimes(1);
    expect(onCloseRequested).toHaveBeenCalledOnce();
    await actions!.quitNow();
    expect(resolveClose).toHaveBeenCalledTimes(2);
    expect(resolveClose).toHaveBeenLastCalledWith(42, "quit_now");
    coordinator.stop();
  });

  it("rechecks authority before a queued idle close runs", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    const resolveClose = vi.fn(async () => undefined);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested: vi.fn() },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });

    receive(batch(1, { requestId: 19, waitingForIdle: true }));
    coordinator.updateActivity({
      authoritative: false,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();

    expect(resolveClose).not.toHaveBeenCalled();
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
      { confirmAutomaticClose, onCloseRequested: vi.fn() },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    receive(batch(9, { requestId: 31, waitingForIdle: true }));
    await settle();
    expect(resolveClose).toHaveBeenCalledWith(31, "quit_now");
  });

  it("retries a transient quit-when-idle reconciliation failure with backoff", async () => {
    let receive!: (batch: HostLifecycleBatch) => void;
    let retry!: () => void;
    const disconnected = new Error("temporarily disconnected");
    const confirmClose = vi
      .fn<() => Promise<boolean>>()
      .mockRejectedValueOnce(disconnected)
      .mockResolvedValue(true);
    const resolveClose = vi.fn(async () => undefined);
    const scheduleRetry = vi.fn((delayMs: number, callback: () => void): unknown => {
      expect(delayMs).toBe(1_000);
      retry = callback;
      return "retry-1";
    });
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      {
        confirmAutomaticClose: confirmClose,
        onCloseRequested: vi.fn(),
        closeRetryScheduler: {
          set: scheduleRetry,
          clear: vi.fn(),
        },
      },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });

    receive(batch(1, { requestId: 32, waitingForIdle: true }));
    await settle();
    expect(confirmClose).toHaveBeenCalledTimes(1);
    expect(resolveClose).not.toHaveBeenCalled();
    expect(scheduleRetry).toHaveBeenCalledOnce();

    retry();
    await settle();
    expect(confirmClose).toHaveBeenCalledTimes(2);
    expect(resolveClose).toHaveBeenCalledWith(32, "quit_now");
  });

  it("cancels a close and announces each request only once", async () => {
    let receive!: (value: HostLifecycleBatch) => void;
    let actions: DesktopCloseActions | undefined;
    const onCloseRequested = vi.fn((_request, nextActions: DesktopCloseActions) => {
      actions = nextActions;
    });
    const resolveClose = vi.fn(async () => undefined);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested },
    );
    coordinator.start();
    receive(batch(1, { requestId: 8, waitingForIdle: false }));
    receive(batch(2, { requestId: 8, waitingForIdle: false }));
    expect(onCloseRequested).toHaveBeenCalledOnce();
    await actions!.cancel();
    expect(resolveClose).toHaveBeenCalledWith(8, "cancel");
  });

  it("allows a repeated close announcement when the UI callback throws", () => {
    let receive!: (value: HostLifecycleBatch) => void;
    const onCloseRequested = vi.fn(() => {
      throw new Error("dialog failed");
    });
    const onDiagnostic = vi.fn();
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose: async () => undefined,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested, onDiagnostic },
    );
    coordinator.start();
    receive(batch(1, { requestId: 9, waitingForIdle: false }));
    receive(batch(2, { requestId: 9, waitingForIdle: false }));
    expect(onCloseRequested).toHaveBeenCalledTimes(2);
    expect(onDiagnostic).toHaveBeenCalledTimes(2);
  });

  it("does not spin on a failing automatic quit-now request", async () => {
    let receive!: (value: HostLifecycleBatch) => void;
    const resolveClose = vi.fn(async () => {
      throw new Error("host unavailable");
    });
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async (callback) => {
          receive = callback;
          await new Promise<void>(() => undefined);
        },
        resolveClose,
        setSleepInhibition: async () => undefined,
      },
      { confirmAutomaticClose, onCloseRequested: vi.fn() },
    );
    coordinator.start();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    receive(batch(1, { requestId: 10, waitingForIdle: true }));
    await settle();
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();
    expect(resolveClose).toHaveBeenCalledTimes(1);

    coordinator.updateActivity({
      authoritative: true,
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    });
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    await settle();
    expect(resolveClose).toHaveBeenCalledTimes(2);
    coordinator.stop();
  });

  it("does not inhibit sleep when the general preference is disabled", async () => {
    const setSleepInhibition = vi.fn(async (_active: boolean) => undefined);
    const coordinator = new DesktopHostCoordinator(
      {
        watchLifecycle: async () => new Promise<void>(() => undefined),
        resolveClose: async () => undefined,
        setSleepInhibition,
      },
      { confirmAutomaticClose, onCloseRequested: vi.fn() },
    );
    coordinator.updateActivity({
      authoritative: true,
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
      { confirmAutomaticClose, onCloseRequested: vi.fn(), onDiagnostic },
    );

    const active = {
      authoritative: true,
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
      { confirmAutomaticClose, onCloseRequested: vi.fn() },
    );

    coordinator.updateActivity({
      authoritative: true,
      idle: false,
      workRunning: true,
      preventSleepWhileRunning: true,
    });
    coordinator.updateActivity({
      authoritative: true,
      idle: true,
      workRunning: false,
      preventSleepWhileRunning: true,
    });
    coordinator.updateActivity({
      authoritative: true,
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
