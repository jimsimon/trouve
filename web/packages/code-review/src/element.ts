import { LitElement, type ReactiveController, type ReactiveControllerHost } from "lit";

export type EffectCleanup = (() => void) | undefined | void;

/**
 * Effect body. `isCurrent` stays true until this run is disposed or replaced
 * by a run with different deps, so asynchronous continuations started by the
 * body (fetches, timers) can check it before publishing state.
 */
export type EffectBody = (isCurrent: () => boolean) => EffectCleanup;

/**
 * One `useEffect`-style slot: `run` re-executes its body only when `deps`
 * changed (compared with `Object.is`), disposing the previous body first.
 * Elements call `run` from `updated()` so the body sees committed DOM, and
 * `dispose` from `disconnectedCallback`.
 */
export class Effect {
  private deps: readonly unknown[] | undefined;
  private cleanup: EffectCleanup;
  private generation = 0;

  run(deps: readonly unknown[], effect: EffectBody): void {
    if (
      this.deps !== undefined &&
      this.deps.length === deps.length &&
      this.deps.every((value, index) => Object.is(value, deps[index]))
    ) {
      return;
    }
    this.dispose();
    this.deps = deps;
    const generation = this.generation;
    this.cleanup = effect(() => generation === this.generation);
  }

  dispose(): void {
    const cleanup = this.cleanup;
    this.cleanup = undefined;
    this.deps = undefined;
    this.generation += 1;
    if (typeof cleanup === "function") cleanup();
  }
}

/**
 * Ownership for overlapping asynchronous loads of one piece of state. Each
 * `begin()` supersedes the previous request; a request publishes its result or
 * error only while its predicate is still true, so a slow response for an
 * earlier filter can never overwrite the data for the current one.
 */
export class LatestRequest {
  private generation = 0;

  begin(): () => boolean {
    const generation = ++this.generation;
    return () => generation === this.generation;
  }
}

/**
 * Base class for every element in this package. They render into light DOM so
 * the shared global stylesheet applies unchanged, and the host box is removed
 * from layout (`display: contents`) so the rendered children participate in
 * their parent's flex/grid/block formatting exactly as the original markup
 * did.
 */
export abstract class ReviewElement extends LitElement {
  private readonly effects: Effect[] = [];

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  /** Allocate an effect slot; call its `run` from `updated()`. */
  protected effect(): Effect {
    const effect = new Effect();
    this.effects.push(effect);
    return effect;
  }

  override connectedCallback(): void {
    super.connectedCallback();
    this.style.display = "contents";
    // Effects were disposed on disconnect; a re-render re-establishes them.
    if (this.hasUpdated) this.requestUpdate();
  }

  override disconnectedCallback(): void {
    super.disconnectedCallback();
    for (const effect of this.effects) effect.dispose();
  }
}

/**
 * The `useClock(active)` hook: while active and the document is visible,
 * `now` advances every second and the host re-renders.
 */
export class Clock implements ReactiveController {
  now = Date.now();
  private active: boolean | undefined;
  private timer: number | undefined;

  constructor(private readonly host: ReactiveControllerHost) {
    host.addController(this);
  }

  hostDisconnected(): void {
    this.stop();
    this.active = undefined;
  }

  /** Mirror the effect dependency on `active`: re-arm only when it changes. */
  sync(active: boolean): void {
    if (this.active === active) return;
    this.stop();
    this.active = active;
    if (!active) return;
    document.addEventListener("visibilitychange", this.syncVisibility);
    this.syncVisibility();
  }

  private readonly syncVisibility = (): void => {
    if (this.timer !== undefined) window.clearInterval(this.timer);
    this.timer = undefined;
    if (document.visibilityState === "visible") {
      this.tick();
      this.timer = window.setInterval(this.tick, 1_000);
    }
  };

  private readonly tick = (): void => {
    this.now = Date.now();
    this.host.requestUpdate();
  };

  private stop(): void {
    document.removeEventListener("visibilitychange", this.syncVisibility);
    if (this.timer !== undefined) window.clearInterval(this.timer);
    this.timer = undefined;
  }
}

/** The `useFlash()` hook: a transient status message cleared 2.5s after each flash. */
export class Flash {
  message = "";

  constructor(private readonly host: ReactiveControllerHost) {}

  readonly flash = (next: string): void => {
    this.message = next;
    this.host.requestUpdate();
    window.setTimeout(() => {
      this.message = "";
      this.host.requestUpdate();
    }, 2_500);
  };
}

export function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

export function targetValue(event: Event): string {
  return (event.currentTarget as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement).value;
}

export function targetChecked(event: Event): boolean {
  return (event.currentTarget as HTMLInputElement).checked;
}
