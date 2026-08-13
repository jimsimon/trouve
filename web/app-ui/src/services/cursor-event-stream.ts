import {
  createSignal,
  type ReadonlySignal,
} from "../state/reactivity.js";

export interface CursorEnvelope {
  readonly cursor: number;
}

export interface EventSourceLike {
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  onerror: ((event: Event) => void) | null;
  close(): void;
}

export type EventSourceFactory = (url: string) => EventSourceLike;
export type EventParser<T extends CursorEnvelope> = (value: unknown) => T;
export type StreamState = "idle" | "connecting" | "open" | "reconnecting" | "closed";

export interface SafeStreamDiagnostic {
  readonly kind: "invalid-json" | "invalid-payload" | "event-id-mismatch";
  readonly cursor: number;
}

export interface TimerScheduler {
  set(delayMs: number, callback: () => void): unknown;
  clear(handle: unknown): void;
}

const defaultScheduler: TimerScheduler = {
  set: (delayMs, callback) => globalThis.setTimeout(callback, delayMs),
  clear: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

const urlAfterCursor = (path: string, cursor: number, origin: string): string => {
  const url = new URL(path, origin);
  url.searchParams.set("after", String(cursor));
  return url.href;
};

/** Owns one cursor-addressed EventSource lifecycle. Event cursors are strictly
 * increasing but intentionally not assumed dense. Browser-native reconnects
 * retain Last-Event-ID; a fully closed source is recreated from our last
 * accepted cursor with bounded exponential backoff. */
export class CursorEventStream<T extends CursorEnvelope> {
  readonly #path: string;
  readonly #origin: string;
  readonly #parse: EventParser<T>;
  readonly #onEvent: (event: T) => void;
  readonly #onOpen: () => void;
  readonly #onDiagnostic: (diagnostic: SafeStreamDiagnostic) => void;
  readonly #eventSourceFactory: EventSourceFactory;
  readonly #scheduler: TimerScheduler;
  readonly #baseDelayMs: number;
  readonly #maxDelayMs: number;
  readonly #stableConnectionMs: number;
  readonly #state = createSignal<StreamState>("idle");
  readonly #cursor = createSignal(0);

  readonly state: ReadonlySignal<StreamState> = this.#state;
  readonly cursor: ReadonlySignal<number> = this.#cursor;

  #source: EventSourceLike | undefined;
  #timer: unknown;
  #stableTimer: unknown;
  #attempt = 0;
  #running = false;

  constructor(options: {
    readonly path: string;
    readonly origin: string;
    readonly after: number;
    readonly parse: EventParser<T>;
    readonly onEvent: (event: T) => void;
    readonly onOpen?: () => void;
    readonly onDiagnostic?: (diagnostic: SafeStreamDiagnostic) => void;
    readonly eventSourceFactory?: EventSourceFactory;
    readonly scheduler?: TimerScheduler;
    readonly baseDelayMs?: number;
    readonly maxDelayMs?: number;
    readonly stableConnectionMs?: number;
  }) {
    if (!Number.isSafeInteger(options.after) || options.after < 0) {
      throw new RangeError("event cursor must be a non-negative safe integer");
    }
    this.#path = options.path;
    this.#origin = options.origin;
    this.#parse = options.parse;
    this.#onEvent = options.onEvent;
    this.#onOpen = options.onOpen ?? (() => undefined);
    this.#onDiagnostic = options.onDiagnostic ?? (() => undefined);
    this.#eventSourceFactory =
      options.eventSourceFactory ?? ((url) => new EventSource(url));
    this.#scheduler = options.scheduler ?? defaultScheduler;
    this.#baseDelayMs = options.baseDelayMs ?? 250;
    this.#maxDelayMs = options.maxDelayMs ?? 10_000;
    this.#stableConnectionMs = options.stableConnectionMs ?? 1_000;
    this.#cursor.set(options.after);
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#open("connecting");
  }

  reconnectNow(): void {
    if (!this.#running) return;
    this.#clearTimer();
    this.#clearStableTimer();
    this.#source?.close();
    this.#source = undefined;
    this.#open("reconnecting");
  }

  close(): void {
    this.#running = false;
    this.#clearTimer();
    this.#clearStableTimer();
    this.#source?.close();
    this.#source = undefined;
    this.#state.set("closed");
  }

  #open(state: Extract<StreamState, "connecting" | "reconnecting">): void {
    if (!this.#running) return;
    this.#state.set(state);
    const source = this.#eventSourceFactory(
      urlAfterCursor(this.#path, this.#cursor.get(), this.#origin),
    );
    this.#source = source;
    source.onopen = () => {
      if (source !== this.#source || !this.#running) return;
      this.#state.set("open");
      this.#onOpen();
      this.#clearStableTimer();
      this.#stableTimer = this.#scheduler.set(this.#stableConnectionMs, () => {
        this.#stableTimer = undefined;
        if (source === this.#source && this.#running) this.#attempt = 0;
      });
    };
    source.onmessage = (message) => {
      if (source !== this.#source || !this.#running) return;
      this.#receive(message);
    };
    source.onerror = () => {
      if (source !== this.#source || !this.#running) return;
      this.#state.set("reconnecting");
      // EventSource owns retries while CONNECTING. readyState 2 is CLOSED.
      if (source.readyState === 2) this.#scheduleRecreate(source);
    };
  }

  #receive(message: MessageEvent<string>): void {
    let raw: unknown;
    try {
      raw = JSON.parse(message.data) as unknown;
    } catch {
      this.#onDiagnostic({ kind: "invalid-json", cursor: this.#cursor.get() });
      return;
    }
    let event: T;
    try {
      event = this.#parse(raw);
    } catch {
      this.#onDiagnostic({ kind: "invalid-payload", cursor: this.#cursor.get() });
      return;
    }
    if (!Number.isSafeInteger(event.cursor) || event.cursor < 0) {
      this.#onDiagnostic({ kind: "invalid-payload", cursor: this.#cursor.get() });
      return;
    }
    if (message.lastEventId !== "" && message.lastEventId !== String(event.cursor)) {
      this.#onDiagnostic({ kind: "event-id-mismatch", cursor: this.#cursor.get() });
      return;
    }
    if (event.cursor <= this.#cursor.get()) return;
    this.#cursor.set(event.cursor);
    this.#onEvent(event);
  }

  #scheduleRecreate(source: EventSourceLike): void {
    if (this.#timer !== undefined) return;
    this.#clearStableTimer();
    source.close();
    const delay = Math.min(this.#baseDelayMs * 2 ** this.#attempt, this.#maxDelayMs);
    this.#attempt += 1;
    this.#timer = this.#scheduler.set(delay, () => {
      this.#timer = undefined;
      if (source === this.#source) this.#source = undefined;
      this.#open("reconnecting");
    });
  }

  #clearTimer(): void {
    if (this.#timer === undefined) return;
    this.#scheduler.clear(this.#timer);
    this.#timer = undefined;
  }

  #clearStableTimer(): void {
    if (this.#stableTimer === undefined) return;
    this.#scheduler.clear(this.#stableTimer);
    this.#stableTimer = undefined;
  }
}
