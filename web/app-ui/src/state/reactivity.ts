// Keep the experimental Lit signals integration isolated in this module.
// Application modules depend on these owned names so the implementation can
// be replaced without rewriting every component.
import { SignalWatcher, watch } from "@lit-labs/signals";
import type { ReactiveElement } from "lit";
import { Signal } from "signal-polyfill";

export type ReadonlySignal<T> = Signal.State<T> | Signal.Computed<T>;

export const createSignal = <T>(initialValue: T): Signal.State<T> =>
  new Signal.State(initialValue);

export const createComputed = <T>(compute: () => T): Signal.Computed<T> =>
  new Signal.Computed(compute);

export const readSignal = <T>(signal: ReadonlySignal<T>): T => signal.get();

export const renderSignal = <T>(signal: ReadonlySignal<T>) => watch(signal);

// The upstream mixin accepts arbitrary constructor signatures.
// eslint is not part of this package; keep the compatibility type local here.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type ReactiveElementConstructor = new (...args: any[]) => ReactiveElement;

let activeHostWatchers = 0;

/** Test/diagnostic hook. It deliberately reports coarse host watchers rather
 * than signal-source counts so callers never depend on signal-polyfill
 * introspection internals. */
export const reactivityDebugSnapshot = (): Readonly<{
  activeHostWatchers: number;
}> => Object.freeze({ activeHostWatchers });

export const withSignalTracking = <T extends ReactiveElementConstructor>(Base: T) => {
  const Watched = SignalWatcher(Base);
  class TrouveSignalTrackingElement extends Watched {
    #countedWatcher = false;

    override connectedCallback(): void {
      super.connectedCallback();
      if (!this.#countedWatcher) {
        this.#countedWatcher = true;
        activeHostWatchers += 1;
      }
    }

    override disconnectedCallback(): void {
      super.disconnectedCallback();
      queueMicrotask(() => {
        if (!this.isConnected && this.#countedWatcher) {
          this.#countedWatcher = false;
          activeHostWatchers -= 1;
        }
      });
    }
  }
  return TrouveSignalTrackingElement as typeof Watched;
};
