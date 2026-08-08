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
  close = vi.fn();

  addEventListener(
    _type: "exit",
    listener: (event: MessageEvent<string>) => void,
  ): void {
    this.exit = listener;
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

  it("ignores duplicate offsets, reports invalid chunks, and closes on exit", () => {
    const source = new FakeSource();
    const output = vi.fn();
    const diagnostic = vi.fn();
    const exited = vi.fn();
    const stream = new TerminalOutputStream({
      path: "http://127.0.0.1:43127/v1/terminals/term/output",
      onData: output,
      onExit: exited,
      onDiagnostic: diagnostic,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "2" }));
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "2" }));
    source.onmessage?.(new MessageEvent("message", { data: "%%%", lastEventId: "3" }));
    source.exit?.(new MessageEvent("exit"));

    expect(output).toHaveBeenCalledOnce();
    expect(diagnostic).toHaveBeenCalledWith({ kind: "invalid-base64", offset: 2 });
    expect(exited).toHaveBeenCalledOnce();
    expect(source.close).toHaveBeenCalledOnce();
  });

  it("discards a partial scalar at a non-contiguous byte offset", () => {
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
      scheduler,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "4oI=", lastEventId: "2" }));
    source.onmessage?.(new MessageEvent("message", { data: "b2s=", lastEventId: "5" }));

    expect(output).toEqual([]);
    expect(stream.offset.get()).toBe(2);
    expect(diagnostic).toHaveBeenCalledWith({
      kind: "non-contiguous-offset",
      offset: 2,
    });
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
