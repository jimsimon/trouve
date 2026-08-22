export interface FailedWriteState<T> {
  readonly current: boolean;
  readonly confirmed: T | undefined;
}

/** Tracks optimistic writes against the newest host-confirmed snapshot. */
export class ConfirmedWriteTracker<T> {
  #generation = 0;
  #confirmedGeneration = 0;
  #confirmed: T | undefined;

  load(value: T): void {
    this.#confirmed = value;
    this.#confirmedGeneration = this.#generation;
  }

  begin(): number {
    this.#generation += 1;
    return this.#generation;
  }

  succeed(generation: number, value: T): boolean {
    if (generation >= this.#confirmedGeneration) {
      this.#confirmed = value;
      this.#confirmedGeneration = generation;
    }
    return generation === this.#generation;
  }

  fail(generation: number): FailedWriteState<T> {
    return {
      current: generation === this.#generation,
      confirmed: this.#confirmed,
    };
  }
}
