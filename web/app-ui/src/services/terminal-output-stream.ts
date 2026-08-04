import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

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
  readonly kind: "invalid-offset" | "invalid-base64";
  readonly offset: number;
}

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

  readonly state: ReadonlySignal<TerminalOutputState> = this.#state;
  readonly offset: ReadonlySignal<number> = this.#offset;

  #source: TerminalEventSourceLike | undefined;
  #running = false;
  #retry: ReturnType<typeof setTimeout> | undefined;

  constructor(options: {
    readonly path: string;
    readonly after?: number;
    readonly onData: (data: string) => void;
    readonly onExit?: () => void;
    readonly onDiagnostic?: (diagnostic: TerminalOutputDiagnostic) => void;
    readonly eventSourceFactory?: TerminalEventSourceFactory;
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
  }

  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#open("connecting");
  }

  close(): void {
    this.#running = false;
    if (this.#retry !== undefined) clearTimeout(this.#retry);
    this.#retry = undefined;
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
      if (source === this.#source && this.#running) this.#state.set("open");
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
      source.close();
      this.#state.set("exited");
      this.#onExit();
    });
    source.onerror = () => {
      if (source !== this.#source || !this.#running) return;
      this.#state.set("reconnecting");
      if (source.readyState !== 2 || this.#retry !== undefined) return;
      source.close();
      this.#retry = setTimeout(() => {
        this.#retry = undefined;
        if (source === this.#source) this.#source = undefined;
        this.#open("reconnecting");
      }, 500);
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
    // Some embedded engines deliver the SSE data but leave lastEventId empty.
    // Terminal events are contiguous byte chunks, so their decoded length is
    // a safe forward offset when the engine omits the server-provided id.
    offset ??= previousOffset + bytes.byteLength;
    this.#offset.set(offset);
    const text = this.#decoder.decode(bytes, { stream: true });
    if (text !== "") this.#onData(text);
  }
}
