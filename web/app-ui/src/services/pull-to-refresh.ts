export interface PullToRefreshState {
  readonly distance: number;
  readonly armed: boolean;
}

const IDLE_STATE: PullToRefreshState = Object.freeze({ distance: 0, armed: false });

/** Small pointer-gesture model for the PWA inbox. It deliberately refuses a
 * horizontal gesture so workspace controls and browser back navigation keep
 * their normal behavior. */
export class PullToRefreshGesture {
  readonly #threshold: number;
  readonly #maximum: number;
  #origin: { readonly x: number; readonly y: number } | undefined;
  #state: PullToRefreshState = IDLE_STATE;

  constructor(threshold = 64, maximum = 96) {
    this.#threshold = Math.max(1, threshold);
    this.#maximum = Math.max(this.#threshold, maximum);
  }

  get state(): PullToRefreshState {
    return this.#state;
  }

  begin(x: number, y: number, atScrollStart: boolean): boolean {
    this.cancel();
    if (!atScrollStart || !Number.isFinite(x) || !Number.isFinite(y)) return false;
    this.#origin = { x, y };
    return true;
  }

  move(x: number, y: number): PullToRefreshState {
    const origin = this.#origin;
    if (origin === undefined) return IDLE_STATE;
    const horizontal = Math.abs(x - origin.x);
    const vertical = y - origin.y;
    if (vertical <= 0 || horizontal > vertical) {
      this.cancel();
      return IDLE_STATE;
    }
    // Resistance keeps the content attached to the finger without allowing
    // an unbounded transform on tall phone screens.
    const distance = Math.min(this.#maximum, vertical * 0.55);
    this.#state = Object.freeze({
      distance,
      armed: distance >= this.#threshold,
    });
    return this.#state;
  }

  finish(): boolean {
    const armed = this.#state.armed;
    this.cancel();
    return armed;
  }

  cancel(): void {
    this.#origin = undefined;
    this.#state = IDLE_STATE;
  }
}
