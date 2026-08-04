import { describe, expect, it, vi } from "vitest";

import {
  CursorEventStream,
  type EventSourceLike,
  type TimerScheduler,
} from "./cursor-event-stream.js";
import { readSignal } from "../state/reactivity.js";

interface TestEvent {
  readonly cursor: number;
  readonly type: string;
}

class FakeEventSource implements EventSourceLike {
  readyState = 0;
  onopen: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  readonly close = vi.fn(() => {
    this.readyState = 2;
  });

  open(): void {
    this.readyState = 1;
    this.onopen?.(new Event("open"));
  }

  message(value: unknown, lastEventId = ""): void {
    this.onmessage?.(
      new MessageEvent("message", { data: JSON.stringify(value), lastEventId }),
    );
  }

  failClosed(): void {
    this.readyState = 2;
    this.onerror?.(new Event("error"));
  }
}

const parse = (value: unknown): TestEvent => {
  if (
    typeof value !== "object" ||
    value === null ||
    typeof (value as { cursor?: unknown }).cursor !== "number" ||
    typeof (value as { type?: unknown }).type !== "string"
  ) {
    throw new TypeError("invalid test event");
  }
  return value as TestEvent;
};

describe("CursorEventStream", () => {
  it("resumes after the supplied cursor and rejects duplicates or mismatched SSE ids", () => {
    const sources: FakeEventSource[] = [];
    const urls: string[] = [];
    const events: TestEvent[] = [];
    const diagnostics = vi.fn();
    const stream = new CursorEventStream({
      path: "/v1/events?scope=server",
      origin: "https://trouve.example",
      after: 7,
      parse,
      onEvent: (event) => events.push(event),
      onDiagnostic: diagnostics,
      eventSourceFactory: (url) => {
        urls.push(url);
        const source = new FakeEventSource();
        sources.push(source);
        return source;
      },
    });
    stream.start();
    expect(urls).toEqual(["https://trouve.example/v1/events?scope=server&after=7"]);
    sources[0]?.open();
    sources[0]?.message({ cursor: 8, type: "session.updated" }, "8");
    sources[0]?.message({ cursor: 8, type: "session.updated" }, "8");
    sources[0]?.message({ cursor: 9, type: "session.updated" }, "10");
    expect(events).toEqual([{ cursor: 8, type: "session.updated" }]);
    expect(readSignal(stream.cursor)).toBe(8);
    expect(diagnostics).toHaveBeenCalledWith({ kind: "event-id-mismatch", cursor: 8 });
  });

  it("recreates a closed source from the last accepted cursor", () => {
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
    const sources: FakeEventSource[] = [];
    const urls: string[] = [];
    const stream = new CursorEventStream({
      path: "/v1/events",
      origin: "https://trouve.example",
      after: 2,
      parse,
      onEvent: () => undefined,
      scheduler,
      eventSourceFactory: (url) => {
        urls.push(url);
        const source = new FakeEventSource();
        sources.push(source);
        return source;
      },
    });
    stream.start();
    sources[0]?.message({ cursor: 5, type: "session.updated" }, "5");
    sources[0]?.failClosed();
    expect(delays).toEqual([250]);
    callbacks[0]?.();
    expect(urls[1]).toBe("https://trouve.example/v1/events?after=5");
    stream.close();
    expect(readSignal(stream.state)).toBe("closed");
  });

  it("drops malformed data without putting payload content in diagnostics", () => {
    const source = new FakeEventSource();
    const diagnostics = vi.fn();
    const stream = new CursorEventStream({
      path: "/v1/events",
      origin: "https://trouve.example",
      after: 0,
      parse,
      onEvent: vi.fn(),
      onDiagnostic: diagnostics,
      eventSourceFactory: () => source,
    });
    stream.start();
    source.onmessage?.(new MessageEvent("message", { data: "repository secret" }));
    source.message({ cursor: "bad", prompt: "secret" });
    expect(diagnostics.mock.calls).toEqual([
      [{ kind: "invalid-json", cursor: 0 }],
      [{ kind: "invalid-payload", cursor: 0 }],
    ]);
  });
});
