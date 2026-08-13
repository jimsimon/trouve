import { describe, expect, it, vi } from "vitest";

import {
  TerminalOutputStream,
  type TerminalEventSourceLike,
} from "./terminal-output-stream.js";
import type { TimerScheduler } from "./cursor-event-stream.js";

class FakeSource implements TerminalEventSourceLike {
  readyState = 1;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  exit: ((event: MessageEvent<string>) => void) | undefined;
  replayStart: ((event: MessageEvent<string>) => void) | undefined;
  close = vi.fn();

  addEventListener(
    type: "exit" | "replay-start",
    listener: (event: MessageEvent<string>) => void,
  ): void {
    if (type === "exit") this.exit = listener;
    else this.replayStart = listener;
  }
}

describe("TerminalOutputStream", () => {
  it("resumes from a byte offset and decodes UTF-8 split across SSE chunks", () => {
    const source = new FakeSource();
    const urls: string[] = [];
    const output: string[] = [];
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 7,
      onData: (data) => output.push(data),
      eventSourceFactory: (url) => {
        urls.push(url);
        return source;
      },
    });
    stream.start();
    expect(urls[0]).toContain("after=7");

    source.onmessage?.(
      new MessageEvent("message", { data: "4oI=", lastEventId: "9" }),
    );
    source.onmessage?.(
      new MessageEvent("message", { data: "rCBvaw==", lastEventId: "13" }),
    );
    expect(output.join("")).toBe("€ ok");
    expect(stream.offset.get()).toBe(13);
  });

  it("accepts contiguous PTY bytes when an embedded engine omits lastEventId", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 7,
      onData: (data) => output.push(data),
      eventSourceFactory: () => source,
    });
    stream.start();

    source.onmessage?.(
      new MessageEvent("message", { data: "b2s=", lastEventId: "" }),
    );

    expect(output.join("")).toBe("ok");
    expect(stream.offset.get()).toBe(9);
  });

  it("ignores exact and stale duplicates before an explicit retained-backlog clamp", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const diagnostic = vi.fn();
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 2,
      onData: (data) => output.push(data),
      onDiagnostic: diagnostic,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "%%%", lastEventId: "2" }));
    source.onmessage?.(new MessageEvent("message", { data: "%%%", lastEventId: "1" }));
    source.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":5}', lastEventId: "" }),
    );
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "7" }));

    expect(output).toEqual(["ok"]);
    expect(stream.offset.get()).toBe(7);
    expect(diagnostic).toHaveBeenCalledOnce();
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
    expect(source.close).not.toHaveBeenCalled();
    stream.close();
  });

  it("uses the absolute replay start when lastEventId is missing after a backlog clamp", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const diagnostic = vi.fn();
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 2,
      onData: (data) => output.push(data),
      onDiagnostic: diagnostic,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":5}', lastEventId: "" }),
    );
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "" }));

    expect(output).toEqual(["ok"]);
    expect(stream.offset.get()).toBe(7);
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
    expect(source.close).not.toHaveBeenCalled();
    stream.close();
  });

  it("reconnects from the last contiguous offset after a later gap", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const diagnostic = vi.fn();
    const delays: number[] = [];
    const scheduler: TimerScheduler = {
      set: (delay, callback) => {
        delays.push(delay);
        return callback;
      },
      clear: vi.fn(),
    };
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: (data) => output.push(data),
      onDiagnostic: diagnostic,
      eventSourceFactory: () => source,
      scheduler,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "2" }));
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "5" }));

    expect(output).toEqual(["ok"]);
    expect(stream.offset.get()).toBe(2);
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
    expect(source.close).toHaveBeenCalledOnce();
    expect(delays).toEqual([500]);
    stream.close();
  });

  it("retains a split UTF-8 scalar across malformed-gap recovery", () => {
    const sources: FakeSource[] = [];
    const callbacks: Array<() => void> = [];
    const output: string[] = [];
    const scheduler: TimerScheduler = {
      set: (_delay, callback) => {
        callbacks.push(callback);
        return callback;
      },
      clear: vi.fn(),
    };
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: (data) => output.push(data),
      scheduler,
      eventSourceFactory: () => {
        const source = new FakeSource();
        sources.push(source);
        return source;
      },
    });
    stream.start();
    sources[0]?.onmessage?.(
      new MessageEvent("message", { data: "4oI=", lastEventId: "2" }),
    );
    sources[0]?.onmessage?.(
      new MessageEvent("message", { data: "eA==", lastEventId: "4" }),
    );
    expect(output).toEqual([]);
    expect(stream.offset.get()).toBe(2);

    callbacks[0]?.();
    sources[1]?.onmessage?.(
      new MessageEvent("message", { data: "rA==", lastEventId: "3" }),
    );

    expect(output).toEqual(["€"]);
    expect(stream.offset.get()).toBe(3);
    stream.close();
  });

  it("trims id-less replay overlap and completes split UTF-8 after automatic reconnect", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: (data) => output.push(data),
      eventSourceFactory: () => source,
    });
    stream.start();
    source.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":0}', lastEventId: "" }),
    );
    source.onmessage?.(
      new MessageEvent("message", { data: "4oI=", lastEventId: "" }),
    );
    expect(stream.offset.get()).toBe(2);

    // The native EventSource reconnects its original URL, so the server can
    // replay bytes already rendered. The marker lets us trim those bytes even
    // in an embedding that does not populate lastEventId.
    source.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":0}', lastEventId: "" }),
    );
    source.onmessage?.(
      new MessageEvent("message", { data: "4oKs", lastEventId: "" }),
    );

    expect(output).toEqual(["€"]);
    expect(stream.offset.get()).toBe(3);
    stream.close();
  });

  it("resets a partial UTF-8 scalar when reconnect replay starts after a clamp", () => {
    const sources: FakeSource[] = [];
    const callbacks: Array<() => void> = [];
    const output: string[] = [];
    const diagnostic = vi.fn();
    const scheduler: TimerScheduler = {
      set: (_delay, callback) => {
        callbacks.push(callback);
        return callback;
      },
      clear: vi.fn(),
    };
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: (data) => output.push(data),
      onDiagnostic: diagnostic,
      scheduler,
      eventSourceFactory: () => {
        const source = new FakeSource();
        sources.push(source);
        return source;
      },
    });
    stream.start();
    sources[0]?.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":0}', lastEventId: "" }),
    );
    sources[0]?.onmessage?.(
      new MessageEvent("message", { data: "4oI=", lastEventId: "" }),
    );
    sources[0]!.readyState = 2;
    sources[0]?.onerror?.(new Event("error"));
    callbacks[0]?.();

    sources[1]?.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":5}', lastEventId: "" }),
    );
    sources[1]?.onmessage?.(
      new MessageEvent("message", { data: "b2s=", lastEventId: "" }),
    );

    expect(output).toEqual(["ok"]);
    expect(stream.offset.get()).toBe(7);
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
    stream.close();
  });

  it("delivers id-less replay before an already-exited terminal", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const exited = vi.fn();
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 5,
      onData: (data) => output.push(data),
      onExit: exited,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.replayStart?.(
      new MessageEvent("replay-start", { data: '{"offset":5}', lastEventId: "" }),
    );
    source.onmessage?.(
      new MessageEvent("message", { data: "b2s=", lastEventId: "" }),
    );
    source.exit?.(new MessageEvent("exit"));

    expect(output).toEqual(["ok"]);
    expect(stream.offset.get()).toBe(7);
    expect(stream.state.get()).toBe("exited");
    expect(exited).toHaveBeenCalledOnce();
  });

  it.each([
    { data: "b2s=", lastEventId: "not-an-offset", kind: "invalid-offset" },
    { data: "%%%", lastEventId: "4", kind: "invalid-base64" },
  ] as const)(
    "reconnects from the last good offset after $kind and ignores a stale exit",
    ({ data, lastEventId, kind }) => {
      const sources: FakeSource[] = [];
      const urls: string[] = [];
      const callbacks: Array<() => void> = [];
      const exited = vi.fn();
      const diagnostic = vi.fn();
      const scheduler: TimerScheduler = {
        set: (_delay, callback) => {
          callbacks.push(callback);
          return callback;
        },
        clear: vi.fn(),
      };
      const stream = new TerminalOutputStream({
        path: "http://127.0.0.1:43127/v1/terminals/term/output",
        after: 2,
        onData: () => undefined,
        onExit: exited,
        onDiagnostic: diagnostic,
        scheduler,
        eventSourceFactory: (url) => {
          urls.push(url);
          const source = new FakeSource();
          sources.push(source);
          return source;
        },
      });
      stream.start();

      sources[0]?.onmessage?.(new MessageEvent("message", { data, lastEventId }));
      sources[0]?.exit?.(new MessageEvent("exit"));

      expect(diagnostic).toHaveBeenCalledWith({ kind, offset: 2 });
      expect(stream.offset.get()).toBe(2);
      expect(exited).not.toHaveBeenCalled();
      expect(sources[0]?.close).toHaveBeenCalledOnce();
      callbacks[0]?.();
      expect(urls[1]).toContain("after=2");
      stream.close();
    },
  );

  it("rejects a forward gap without an explicit replay marker", () => {
    const source = new FakeSource();
    const output: string[] = [];
    const callbacks: Array<() => void> = [];
    const scheduler: TimerScheduler = {
      set: (_delay, callback) => {
        callbacks.push(callback);
        return callback;
      },
      clear: vi.fn(),
    };
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 2,
      onData: (data) => output.push(data),
      eventSourceFactory: () => source,
      scheduler,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "bw==", lastEventId: "7" }));

    expect(output).toEqual([]);
    expect(stream.offset.get()).toBe(2);
    expect(source.close).toHaveBeenCalledOnce();
    expect(callbacks).toHaveLength(1);
    stream.close();
  });

  it("rejects an overlapping non-contiguous byte frame", () => {
    const source = new FakeSource();
    const output = vi.fn();
    const diagnostic = vi.fn();
    const delays: number[] = [];
    const scheduler: TimerScheduler = {
      set: (delay, callback) => {
        delays.push(delay);
        return callback;
      },
      clear: vi.fn(),
    };
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      after: 2,
      onData: output,
      onDiagnostic: diagnostic,
      eventSourceFactory: () => source,
      scheduler,
    });
    stream.start();
    source.onmessage?.(
      new MessageEvent("message", { data: "b2s=", lastEventId: "3" }),
    );

    expect(output).not.toHaveBeenCalled();
    expect(stream.offset.get()).toBe(2);
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
    expect(source.close).toHaveBeenCalledOnce();
    expect(delays).toEqual([500]);
    stream.close();
  });

  it("uses injectable bounded exponential backoff for closed sources", () => {
    const callbacks: Array<() => void> = [];
    const delays: number[] = [];
    const scheduler: TimerScheduler = {
      set: (delay, callback) => {
        delays.push(delay);
        callbacks.push(callback);
        return callback;
      },
      clear: vi.fn(),
    };
    const sources: FakeSource[] = [];
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: () => undefined,
      scheduler,
      baseDelayMs: 100,
      maxDelayMs: 150,
      eventSourceFactory: () => {
        const source = new FakeSource();
        source.readyState = 2;
        sources.push(source);
        return source;
      },
    });
    stream.start();
    sources[0]!.onerror?.(new Event("error"));
    callbacks[0]!();
    sources[1]!.onerror?.(new Event("error"));
    expect(delays).toEqual([100, 150]);
    stream.close();
  });
});
