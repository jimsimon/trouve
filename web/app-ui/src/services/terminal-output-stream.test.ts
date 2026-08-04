import { describe, expect, it, vi } from "vitest";

import {
  TerminalOutputStream,
  type TerminalEventSourceLike,
} from "./terminal-output-stream.js";

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
});
