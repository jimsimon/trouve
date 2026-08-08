import { createSignal, type ReadonlySignal } from "../state/reactivity.js";
import type { TimerScheduler } from "./cursor-event-stream.js";

export type TerminalOutputState =
  | "idle"
  | "connecting"
  | "open"
  | "reconnecting"
  | "exited"
  | "closed";

export interface TerminalEventSourceLike {
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  onerror: ((event: Event) => void) | null;
  addEventListener(type: "exit", listener: (event: MessageEvent<string>) => void): void;
  close(): void;
}

export type TerminalEventSourceFactory = (url: string) => TerminalEventSourceLike;

export interface TerminalOutputDiagnostic {
  readonly kind: "invalid-offset" | "invalid-base64" | "non-contiguous-offset";
  readonly offset: number;
}

const terminalScheduler: TimerScheduler = {
  set: (delayMs, callback) => globalThis.setTimeout(callback, delayMs),
  clear: (handle) => globalThis.clearTimeout(handle as ReturnType<typeof setTimeout>),
};

const decodeBase64 = (value: string): Uint8Array => {
  const binary = globalThis.atob(value);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
};

/** Resume-capable adapter for ephemeral PTY byte SSE. A streaming UTF-8
 * decoder preserves characters split across arbitrary server chunks. */
export class TerminalOutputStream {
  readonly #path: string;
  readonly #factory: TerminalEventSourceFactory;
  readonly #onData: (data: string) => void;
  readonly #onExit: () => void;
  readonly #onDiagnostic: (diagnostic: TerminalOutputDiagnostic) => void;
  readonly #state = createSignal<TerminalOutputState>("idle");
  readonly #offset = createSignal(0);
  readonly #decoder = new TextDecoder();
  readonly #scheduler: TimerScheduler;
  readonly #baseDelayMs: number;
  readonly #maxDelayMs: number;
  readonly #stableConnectionMs: number;

  readonly state: ReadonlySignal<TerminalOutputState> = this.#state;
  readonly offset: ReadonlySignal<number> = this.#offset;

  #source: TerminalEventSourceLike | undefined;
  #running = false;
  #retry: unknown;
  #stableRetry: unknown;
  #attempt = 0;

  constructor(options: {
    readonly path: string;
    readonly after?: number;
    readonly onData: (data: string) => void;
    readonly onExit?: () => void;
    readonly onDiagnostic?: (diagnostic: TerminalOutputDiagnostic) => void;
    readonly eventSourceFactory?: TerminalEventSourceFactory;
    readonly scheduler?: TimerScheduler;
    readonly baseDelayMs?: number;
    readonly maxDelayMs?: number;
    readonly stableConnectionMs?: number;
  }) {
    const after = options.after ?? 0;
    if (!Number.isSafeInteger(after) || after < 0) {
      throw new RangeError("terminal offset must be a non-negative safe integer");
    }
    this.#path = options.path;
    this.#offset.set(after);
    this.#onData = options.onData;
    this.#onExit = options.onExit ?? (() => undefined);
    this.#onDiagnostic = options.onDiagnostic ?? (() => undefined);
    this.#factory =
      options.eventSourceFactory ?? ((url) => new EventSource(url) as TerminalEventSourceLike);
    this.#scheduler = options.scheduler ?? terminalScheduler;
    this.#baseDelayMs = options.baseDelayMs ?? 500;
    this.#maxDelayMs = options.maxDelayMs ?? 10_000;
    this.#stableConnectionMs = options.stableConnectionMs ?? 1_000;
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#open("connecting");
  }

  close(): void {
    this.#running = false;
    if (this.#retry !== undefined) this.#scheduler.clear(this.#retry);
    this.#retry = undefined;
    this.#clearStableRetry();
    this.#source?.close();
    this.#source = undefined;
    const tail = this.#decoder.decode();
    if (tail !== "") this.#onData(tail);
    this.#state.set("closed");
  }

  #open(state: Extract<TerminalOutputState, "connecting" | "reconnecting">): void {
    if (!this.#running) return;
    this.#state.set(state);
    const url = new URL(this.#path);
    url.searchParams.set("after", String(this.#offset.get()));
    const source = this.#factory(url.href);
    this.#source = source;
    source.onopen = () => {
      if (source === this.#source && this.#running) {
        this.#state.set("open");
        this.#clearStableRetry();
        this.#stableRetry = this.#scheduler.set(this.#stableConnectionMs, () => {
          this.#stableRetry = undefined;
          if (source === this.#source && this.#running) this.#attempt = 0;
        });
      }
    };
    source.onmessage = (message) => {
      if (source !== this.#source || !this.#running) return;
      this.#receive(message);
    };
    source.addEventListener("exit", () => {
      if (source !== this.#source || !this.#running) return;
      const tail = this.#decoder.decode();
      if (tail !== "") this.#onData(tail);
      this.#running = false;
      if (this.#retry !== undefined) this.#scheduler.clear(this.#retry);
      this.#retry = undefined;
      this.#clearStableRetry();
      source.close();
      this.#state.set("exited");
      this.#onExit();
    });
    source.onerror = () => {
      if (source !== this.#source || !this.#running) return;
      this.#state.set("reconnecting");
      if (source.readyState === 2) this.#scheduleReconnect(source);
    };
  }

  #receive(message: MessageEvent<string>): void {
    const previousOffset = this.#offset.get();
    let offset: number | undefined;
    if (message.lastEventId !== "") {
      offset = Number(message.lastEventId);
      if (!Number.isSafeInteger(offset) || offset < 0) {
        this.#onDiagnostic({ kind: "invalid-offset", offset: previousOffset });
        return;
      }
      if (offset <= previousOffset) return;
    }
    let bytes: Uint8Array;
    try {
      bytes = decodeBase64(message.data);
    } catch {
      this.#onDiagnostic({ kind: "invalid-base64", offset: previousOffset });
      return;
    }
    if (
      message.lastEventId !== "" &&
      offset !== previousOffset + bytes.byteLength
    ) {
      // Flush and discard any partial scalar before resuming from the new
      // frame. Keep the last accepted offset so the replacement stream can
      // replay every missing byte instead of silently skipping the gap.
      void this.#decoder.decode();
      this.#onDiagnostic({ kind: "non-contiguous-offset", offset: previousOffset });
      const source = this.#source;
      if (source !== undefined) {
        this.#state.set("reconnecting");
        this.#scheduleReconnect(source);
      }
      return;
    }
    // Some embedded engines deliver the SSE data but leave lastEventId empty.
    // Terminal events are contiguous byte chunks, so their decoded length is
    // a safe forward offset when the engine omits the server-provided id.
    offset ??= previousOffset + bytes.byteLength;
    this.#offset.set(offset);
    const text = this.#decoder.decode(bytes, { stream: true });
    if (text !== "") this.#onData(text);
  }

  #scheduleReconnect(source: TerminalEventSourceLike): void {
    if (this.#retry !== undefined) return;
    this.#clearStableRetry();
    source.close();
    const delay = Math.min(
      this.#baseDelayMs * 2 ** this.#attempt,
      this.#maxDelayMs,
    );
    this.#attempt += 1;
    this.#retry = this.#scheduler.set(delay, () => {
      this.#retry = undefined;
      if (source === this.#source) this.#source = undefined;
      this.#open("reconnecting");
    });
  }

  #clearStableRetry(): void {
    if (this.#stableRetry === undefined) return;
    this.#scheduler.clear(this.#stableRetry);
    this.#stableRetry = undefined;
  }
}
