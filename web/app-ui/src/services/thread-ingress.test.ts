import { describe, expect, it, vi } from "vitest";

import { AppStore } from "../state/app-store.js";
import type { CursorEventStream } from "./cursor-event-stream.js";
import {
  ThreadIngress,
  ThreadReplayBatcher,
  type ThreadProtocol,
  type ThreadReplayScheduler,
} from "./thread-ingress.js";
import type {
  ProtocolEventEnvelope,
  ProtocolIngressEvent,
  ProtocolThreadViewSnapshot,
} from "./protocol-client.js";

const thread = (id: string, sessionId = "se_1") => ({
  id,
  session_id: sessionId,
  model: "openai/gpt-5.6",
  mode: "code",
  permission_mode: "ask" as const,
  created_at: `2026-08-01T12:00:0${id.endsWith("2") ? "2" : "1"}Z`,
});

const viewSnapshot = (
  cursor = 0,
  items: ProtocolThreadViewSnapshot["items"] = [],
) => ({
  cursor,
  value: {
    item_offset: 0,
    total_items: items.length,
    has_older: false,
    items,
  },
});

describe("ThreadIngress", () => {
  it("opens the conversation when auxiliary thread statuses are unavailable", async () => {
    const store = new AppStore();
    const start = vi.fn();
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1")]),
      threadStatuses: vi.fn(async () => Promise.reject(new Error("unsupported"))),
      threadView: vi.fn(async () => viewSnapshot()),
      threadEvents: vi.fn(async () => ({
        start,
        close: vi.fn(),
      }) as unknown as CursorEventStream<ProtocolIngressEvent>),
    };
    const ingress = new ThreadIngress(protocol, store);
    await expect(ingress.openSession("se_1", "th_1")).resolves.toBe("th_1");
    expect(store.thread("th_1")).toBeDefined();
    expect(start).toHaveBeenCalledOnce();
  });

  it("seeds the requested thread from its folded snapshot before folding live events", async () => {
    const store = new AppStore();
    let receivedOptions:
      | Parameters<ThreadProtocol["threadEvents"]>[1]
      | undefined;
    const start = vi.fn();
    const close = vi.fn();
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1"), thread("th_2")]),
      threadView: vi.fn(async () => viewSnapshot(11, [{
        kind: "user",
        turn: 1,
        content: "snapshot",
        attachments: [],
      }])),
      threadEvents: vi.fn(async (_threadId, options) => {
        receivedOptions = options;
        return { start, close } as unknown as CursorEventStream<ProtocolIngressEvent>;
      }),
    };
    const ingress = new ThreadIngress(protocol, store, {
      now: () => Date.parse("2026-08-01T12:00:12Z"),
    });

    await expect(ingress.openSession("se_1", "th_2")).resolves.toBe("th_2");
    expect(protocol.threadView).toHaveBeenCalledWith("th_2");
    expect(receivedOptions?.after).toBe(11);
    expect(start).toHaveBeenCalledOnce();

    receivedOptions?.onEvent({
      kind: "known",
      cursor: 12,
      envelope: {
        cursor: 12,
        scope: { thread: "th_2" },
        ts: "2026-08-01T12:00:12Z",
        type: "user.message",
        turn: 1,
        content: "hello",
        attachments: [],
      },
    });
    expect(store.threadView("th_2").items).toMatchObject([
      { kind: "user", content: "snapshot" },
      { kind: "user", content: "hello" },
    ]);
  });

  it("falls back to the latest thread and closes the prior stream on session switch", async () => {
    const store = new AppStore();
    const closes: ReturnType<typeof vi.fn>[] = [];
    const protocol: ThreadProtocol = {
      threads: vi.fn(async (sessionId) =>
        sessionId === "se_1"
          ? [thread("th_1"), thread("th_2")]
          : [thread("th_3", "se_2")],
      ),
      threadView: vi.fn(async () => viewSnapshot()),
      threadEvents: vi.fn(async () => {
        const close = vi.fn();
        closes.push(close);
        return {
          start: vi.fn(),
          close,
        } as unknown as CursorEventStream<ProtocolIngressEvent>;
      }),
    };
    const ingress = new ThreadIngress(protocol, store, {
      now: () => Date.parse("2026-08-01T12:00:02Z"),
    });

    await expect(ingress.openSession("se_1", "missing")).resolves.toBe("th_2");
    await expect(ingress.openSession("se_2")).resolves.toBe("th_3");
    expect(closes[0]).toHaveBeenCalledOnce();
    ingress.close();
    expect(closes[1]).toHaveBeenCalledOnce();
  });

  it("does not reopen closed tabs when a session has no explicit thread route", async () => {
    const store = new AppStore();
    const close = vi.fn();
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1"), thread("th_2")]),
      threadView: vi.fn(async () => viewSnapshot()),
      threadEvents: vi.fn(async () => ({
        start: vi.fn(),
        close,
      }) as unknown as CursorEventStream<ProtocolIngressEvent>),
    };
    const ingress = new ThreadIngress(protocol, store);

    await expect(ingress.openSession("se_1", undefined, ["th_2"]))
      .resolves.toBe("th_1");
    expect(protocol.threadView).toHaveBeenCalledWith("th_1");

    await expect(ingress.openSession("se_1", undefined, ["th_1", "th_2"]))
      .resolves.toBeUndefined();
    expect(close).toHaveBeenCalledOnce();
    expect(protocol.threadView).toHaveBeenCalledTimes(1);
  });

  it("discards a delayed snapshot after navigation changes generation", async () => {
    const store = new AppStore();
    let resolveFirst:
      | ((snapshot: ReturnType<typeof viewSnapshot>) => void)
      | undefined;
    const firstSnapshot = new Promise<ReturnType<typeof viewSnapshot>>((resolve) => {
      resolveFirst = resolve;
    });
    const protocol: ThreadProtocol = {
      threads: vi.fn(async (sessionId) => [
        thread(sessionId === "se_1" ? "th_1" : "th_2", sessionId),
      ]),
      threadView: vi.fn(async (threadId) =>
        threadId === "th_1" ? firstSnapshot : viewSnapshot(20)),
      threadEvents: vi.fn(async () => ({
        start: vi.fn(),
        close: vi.fn(),
      }) as unknown as CursorEventStream<ProtocolIngressEvent>),
    };
    const ingress = new ThreadIngress(protocol, store);

    const firstOpen = ingress.openSession("se_1", "th_1");
    await vi.waitFor(() => expect(protocol.threadView).toHaveBeenCalledWith("th_1"));
    await expect(ingress.openSession("se_2", "th_2")).resolves.toBe("th_2");
    resolveFirst?.(viewSnapshot(10, [{
      kind: "user",
      turn: 1,
      content: "stale",
      attachments: [],
    }]));
    await expect(firstOpen).resolves.toBeUndefined();

    expect(protocol.threadEvents).toHaveBeenCalledOnce();
    expect(protocol.threadEvents).toHaveBeenCalledWith(
      "th_2",
      expect.objectContaining({ after: 20 }),
    );
    expect(store.threadView("th_1").items).toEqual([]);
  });

  it("does not fall back to cursor-zero replay when snapshot loading fails", async () => {
    const store = new AppStore();
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1")]),
      threadView: vi.fn(async () => {
        throw new Error("snapshot unavailable");
      }),
      threadEvents: vi.fn(),
    };
    const ingress = new ThreadIngress(protocol, store);

    await expect(ingress.openSession("se_1", "th_1")).rejects.toThrow(
      "snapshot unavailable",
    );
    expect(protocol.threadEvents).not.toHaveBeenCalled();
    expect(ingress.state.get()).toBe("error");
  });

  it("reconnects when the same thread is opened again so events use the current generation", async () => {
    const store = new AppStore();
    const eventHandlers: Array<(event: ProtocolIngressEvent) => void> = [];
    const closes: ReturnType<typeof vi.fn>[] = [];
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1")]),
      threadView: vi.fn(async () => viewSnapshot()),
      threadEvents: vi.fn(async (_threadId, options) => {
        eventHandlers.push(options.onEvent);
        const close = vi.fn();
        closes.push(close);
        return {
          start: vi.fn(),
          close,
        } as unknown as CursorEventStream<ProtocolIngressEvent>;
      }),
    };
    const ingress = new ThreadIngress(protocol, store, {
      now: () => Date.parse("2026-08-01T12:00:02Z"),
    });

    await ingress.openSession("se_1", "th_1");
    await ingress.openSession("se_1", "th_1");

    expect(protocol.threadEvents).toHaveBeenCalledTimes(2);
    expect(closes[0]).toHaveBeenCalledOnce();

    const message = (cursor: number, content: string): ProtocolIngressEvent => ({
      kind: "known",
      cursor,
      envelope: {
        cursor,
        scope: { thread: "th_1" },
        ts: `2026-08-01T12:00:${String(cursor).padStart(2, "0")}Z`,
        type: "user.message",
        turn: 1,
        content,
        attachments: [],
      },
    });
    eventHandlers[0]?.(message(1, "stale"));
    eventHandlers[1]?.(message(2, "current"));

    expect(store.threadView("th_1").items).toMatchObject([
      { kind: "user", content: "current" },
    ]);
  });

  it("reconnects the active cursor stream after foreground and online transitions", async () => {
    const store = new AppStore();
    let visibilityState: DocumentVisibilityState = "hidden";
    let foreground: (() => void) | undefined;
    let onlineAgain: (() => void) | undefined;
    const visibility = {
      get visibilityState(): DocumentVisibilityState {
        return visibilityState;
      },
      addEventListener: vi.fn((_type: "visibilitychange", listener: () => void) => {
        foreground = listener;
      }),
      removeEventListener: vi.fn(),
    };
    const online = {
      addEventListener: vi.fn((_type: "online", listener: () => void) => {
        onlineAgain = listener;
      }),
      removeEventListener: vi.fn(),
    };
    const reconnectNow = vi.fn();
    const close = vi.fn();
    const protocol: ThreadProtocol = {
      threads: vi.fn(async () => [thread("th_1")]),
      threadView: vi.fn(async () => viewSnapshot()),
      threadEvents: vi.fn(async () => ({
        start: vi.fn(),
        reconnectNow,
        close,
      }) as unknown as CursorEventStream<ProtocolIngressEvent>),
    };
    const ingress = new ThreadIngress(protocol, store, { visibility, online });

    await ingress.openSession("se_1", "th_1");
    foreground?.();
    expect(reconnectNow).not.toHaveBeenCalled();

    visibilityState = "visible";
    foreground?.();
    onlineAgain?.();
    expect(reconnectNow).toHaveBeenCalledTimes(2);

    ingress.close();
    expect(close).toHaveBeenCalledOnce();
    expect(visibility.removeEventListener).toHaveBeenCalledOnce();
    expect(online.removeEventListener).toHaveBeenCalledOnce();
  });
});

const messageEnvelope = (
  cursor: number,
  timestamp: string,
): ProtocolEventEnvelope => ({
  cursor,
  scope: { thread: "th_1" },
  ts: timestamp,
  type: "user.message",
  turn: 1,
  content: `message ${cursor}`,
  attachments: [],
});

describe("ThreadReplayBatcher", () => {
  it("coalesces persisted history into one ordered application", () => {
    let scheduled: (() => void) | undefined;
    const scheduler: ThreadReplayScheduler = {
      set: vi.fn((_delay, callback) => {
        scheduled = callback;
        return 1;
      }),
      clear: vi.fn(),
    };
    const apply = vi.fn();
    const batcher = new ThreadReplayBatcher(apply, {
      now: () => Date.parse("2026-08-01T12:01:00Z"),
      scheduler,
    });
    batcher.receive(messageEnvelope(1, "2026-08-01T12:00:00Z"));
    batcher.receive(messageEnvelope(2, "2026-08-01T12:00:01Z"));

    expect(apply).not.toHaveBeenCalled();
    expect(scheduler.set).toHaveBeenCalledOnce();
    scheduled?.();
    expect(apply).toHaveBeenCalledOnce();
    expect(apply.mock.calls[0]?.[0]).toEqual([
      expect.objectContaining({ cursor: 1 }),
      expect.objectContaining({ cursor: 2 }),
    ]);
  });

  it("flushes a replay prefix with the first live event and drops disposed work", () => {
    const callbacks: Array<() => void> = [];
    const scheduler: ThreadReplayScheduler = {
      set: (_delay, callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      clear: vi.fn(),
    };
    const apply = vi.fn();
    const batcher = new ThreadReplayBatcher(apply, {
      now: () => Date.parse("2026-08-01T12:01:00Z"),
      scheduler,
    });
    batcher.receive(messageEnvelope(1, "2026-08-01T12:00:00Z"));
    batcher.receive(messageEnvelope(2, "2026-08-01T12:00:59Z"));
    expect(apply).toHaveBeenCalledOnce();
    expect(apply.mock.calls[0]?.[0]).toEqual([
      expect.objectContaining({ cursor: 1 }),
      expect.objectContaining({ cursor: 2 }),
    ]);

    batcher.receive(messageEnvelope(3, "2026-08-01T12:00:00Z"));
    batcher.dispose();
    callbacks.at(-1)?.();
    expect(apply).toHaveBeenCalledOnce();
  });
});
