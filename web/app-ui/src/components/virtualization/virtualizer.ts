export interface VirtualItem {
  readonly id: string;
  readonly estimatedHeight?: number;
  readonly heavyweight?: boolean;
}

export type VirtualizationMode = "virtual" | "accessible";

export interface PositionedVirtualItem<T extends VirtualItem> {
  readonly item: T;
  readonly index: number;
  readonly start: number;
  readonly height: number;
  readonly end: number;
}

export interface VirtualWindow<T extends VirtualItem> {
  readonly items: readonly PositionedVirtualItem<T>[];
  readonly paddingBefore: number;
  readonly paddingAfter: number;
  readonly totalHeight: number;
  readonly scrollTop: number;
  readonly followingTail: boolean;
}

export interface ScrollCorrection {
  readonly previousScrollTop: number;
  readonly scrollTop: number;
  readonly delta: number;
}

export interface VirtualScrollBookmark {
  readonly id: string;
  readonly offset: number;
}

type Anchor = VirtualScrollBookmark;

const correction = (previousScrollTop: number, scrollTop: number): ScrollCorrection => ({
  previousScrollTop,
  scrollTop,
  delta: scrollTop - previousScrollTop,
});

const checkedHeight = (height: number, label: string): number => {
  if (!Number.isFinite(height) || height <= 0) {
    throw new RangeError(`${label} must be a positive finite number`);
  }
  return height;
};

/** Framework-independent virtualization state. DOM measurement, scrolling,
 * and ResizeObserver ownership remain with a thin Lit controller; this model
 * makes anchoring and follow-tail behavior deterministic and fixture-testable. */
export class Virtualizer<T extends VirtualItem> {
  readonly #defaultEstimatedHeight: number;
  readonly #overscanPx: number;
  readonly #tailTolerancePx: number;
  readonly #measurements = new Map<string, number>();
  #items: readonly T[] = [];
  #scrollTop = 0;
  #viewportHeight = 0;
  #mode: VirtualizationMode;
  #followingTail = true;
  #anchor: Anchor | undefined;

  constructor(options: {
    readonly estimatedHeight: number;
    readonly overscanPx?: number;
    readonly tailTolerancePx?: number;
    readonly mode?: VirtualizationMode;
  }) {
    this.#defaultEstimatedHeight = checkedHeight(
      options.estimatedHeight,
      "estimatedHeight",
    );
    this.#overscanPx = Math.max(0, options.overscanPx ?? options.estimatedHeight * 3);
    this.#tailTolerancePx = Math.max(0, options.tailTolerancePx ?? 24);
    this.#mode = options.mode ?? "virtual";
  }

  setMode(mode: VirtualizationMode): void {
    this.#mode = mode;
  }

  setItems(items: readonly T[]): ScrollCorrection {
    const ids = new Set<string>();
    for (const item of items) {
      if (item.id.length === 0) throw new TypeError("virtual item id must not be empty");
      if (ids.has(item.id)) throw new TypeError(`duplicate virtual item id: ${item.id}`);
      ids.add(item.id);
      if (item.estimatedHeight !== undefined) {
        checkedHeight(item.estimatedHeight, `estimated height for ${item.id}`);
      }
    }

    const previousScrollTop = this.#scrollTop;
    const anchor = this.#anchor ?? this.#captureAnchor();
    this.#items = items;
    for (const id of this.#measurements.keys()) {
      if (!ids.has(id)) this.#measurements.delete(id);
    }

    if (this.#followingTail) {
      this.#scrollTop = this.#tailScrollTop();
    } else if (anchor !== undefined) {
      const anchorIndex = this.#items.findIndex((item) => item.id === anchor.id);
      if (anchorIndex >= 0) {
        this.#scrollTop = this.#clampScroll(this.#offsetAt(anchorIndex) + anchor.offset);
        this.#anchor = anchor;
      } else {
        this.#scrollTop = this.#clampScroll(this.#scrollTop);
        this.#anchor = this.#captureAnchor();
      }
    } else {
      this.#scrollTop = this.#clampScroll(this.#scrollTop);
      this.#anchor = this.#captureAnchor();
    }
    return correction(previousScrollTop, this.#scrollTop);
  }

  setViewport(scrollTop: number, viewportHeight: number): void {
    if (!Number.isFinite(scrollTop) || scrollTop < 0) {
      throw new RangeError("scrollTop must be a non-negative finite number");
    }
    if (!Number.isFinite(viewportHeight) || viewportHeight < 0) {
      throw new RangeError("viewportHeight must be a non-negative finite number");
    }
    this.#viewportHeight = viewportHeight;
    this.#scrollTop = this.#clampScroll(scrollTop);
    if (
      this.#followingTail &&
      this.#tailScrollTop() - this.#scrollTop > this.#tailTolerancePx
    ) {
      // Once the user intentionally leaves the tail, only enableFollowTail()
      // resumes it. Merely scrolling near the bottom does not steal control.
      this.#followingTail = false;
    }
    this.#anchor = this.#captureAnchor();
  }

  enableFollowTail(): ScrollCorrection {
    const previousScrollTop = this.#scrollTop;
    this.#followingTail = true;
    this.#scrollTop = this.#tailScrollTop();
    this.#anchor = this.#captureAnchor();
    return correction(previousScrollTop, this.#scrollTop);
  }

  /** Returns no bookmark at the live tail. Parked history is represented by
   * a stable item id and offset, never by layout-dependent raw pixels. */
  bookmark(): VirtualScrollBookmark | undefined {
    if (this.#followingTail) return undefined;
    const anchor = this.#captureAnchor() ?? this.#anchor;
    return anchor === undefined ? undefined : Object.freeze({ ...anchor });
  }

  restoreBookmark(bookmark: VirtualScrollBookmark): ScrollCorrection {
    if (bookmark.id.length === 0) {
      throw new TypeError("bookmark id must not be empty");
    }
    if (!Number.isFinite(bookmark.offset) || bookmark.offset < 0) {
      throw new RangeError("bookmark offset must be a non-negative finite number");
    }
    const previousScrollTop = this.#scrollTop;
    const anchor = Object.freeze({ id: bookmark.id, offset: bookmark.offset });
    this.#followingTail = false;
    this.#anchor = anchor;
    const anchorIndex = this.#items.findIndex((item) => item.id === anchor.id);
    if (anchorIndex >= 0) {
      this.#scrollTop = this.#clampScroll(
        this.#offsetAt(anchorIndex) + anchor.offset,
      );
    } else {
      this.#scrollTop = this.#clampScroll(this.#scrollTop);
    }
    return correction(previousScrollTop, this.#scrollTop);
  }

  measure(id: string, height: number): ScrollCorrection {
    checkedHeight(height, `measured height for ${id}`);
    const index = this.#items.findIndex((item) => item.id === id);
    if (index < 0) throw new TypeError(`cannot measure unknown virtual item: ${id}`);
    const previousHeight = this.#heightAt(index);
    const previousScrollTop = this.#scrollTop;
    this.#measurements.set(id, height);
    const delta = height - previousHeight;
    if (delta === 0) return correction(previousScrollTop, this.#scrollTop);

    if (this.#followingTail) {
      this.#scrollTop = this.#tailScrollTop();
    } else {
      const anchor = this.#anchor ?? this.#captureAnchor();
      const anchorIndex =
        anchor === undefined
          ? -1
          : this.#items.findIndex((item) => item.id === anchor.id);
      if (anchorIndex >= 0 && index < anchorIndex) {
        this.#scrollTop = this.#clampScroll(this.#scrollTop + delta);
      } else {
        this.#scrollTop = this.#clampScroll(this.#scrollTop);
      }
    }
    return correction(previousScrollTop, this.#scrollTop);
  }

  window(): VirtualWindow<T> {
    const totalHeight = this.#totalHeight();
    if (this.#mode === "accessible") {
      return {
        items: this.#positionedRange(0, this.#items.length),
        paddingBefore: 0,
        paddingAfter: 0,
        totalHeight,
        scrollTop: this.#scrollTop,
        followingTail: this.#followingTail,
      };
    }

    const from = Math.max(0, this.#scrollTop - this.#overscanPx);
    const through = Math.min(
      totalHeight,
      this.#scrollTop + this.#viewportHeight + this.#overscanPx,
    );
    let startIndex = 0;
    let start = 0;
    while (
      startIndex < this.#items.length &&
      start + this.#heightAt(startIndex) <= from
    ) {
      start += this.#heightAt(startIndex);
      startIndex += 1;
    }
    let endIndex = startIndex;
    let end = start;
    while (endIndex < this.#items.length && end < through) {
      end += this.#heightAt(endIndex);
      endIndex += 1;
    }
    return {
      items: this.#positionedRange(startIndex, endIndex),
      paddingBefore: start,
      paddingAfter: Math.max(0, totalHeight - end),
      totalHeight,
      scrollTop: this.#scrollTop,
      followingTail: this.#followingTail,
    };
  }

  isMounted(id: string): boolean {
    if (this.#mode === "accessible") return this.#items.some((item) => item.id === id);
    return this.window().items.some(({ item }) => item.id === id);
  }

  shouldUnmountHeavyweight(id: string): boolean {
    const item = this.#items.find((candidate) => candidate.id === id);
    return item?.heavyweight === true && !this.isMounted(id);
  }

  #captureAnchor(): Anchor | undefined {
    if (this.#items.length === 0) return undefined;
    let start = 0;
    for (let index = 0; index < this.#items.length; index += 1) {
      const item = this.#items[index];
      if (item === undefined) break;
      const end = start + this.#heightAt(index);
      if (end > this.#scrollTop) {
        return { id: item.id, offset: this.#scrollTop - start };
      }
      start = end;
    }
    const last = this.#items.at(-1);
    return last === undefined
      ? undefined
      : { id: last.id, offset: this.#scrollTop - this.#offsetAt(this.#items.length - 1) };
  }

  #positionedRange(startIndex: number, endIndex: number): readonly PositionedVirtualItem<T>[] {
    const positioned: PositionedVirtualItem<T>[] = [];
    let start = this.#offsetAt(startIndex);
    for (let index = startIndex; index < endIndex; index += 1) {
      const item = this.#items[index];
      if (item === undefined) break;
      const height = this.#heightAt(index);
      positioned.push({ item, index, start, height, end: start + height });
      start += height;
    }
    return positioned;
  }

  #heightAt(index: number): number {
    const item = this.#items[index];
    if (item === undefined) return 0;
    return (
      this.#measurements.get(item.id) ??
      item.estimatedHeight ??
      this.#defaultEstimatedHeight
    );
  }

  #offsetAt(index: number): number {
    let offset = 0;
    const limit = Math.min(index, this.#items.length);
    for (let cursor = 0; cursor < limit; cursor += 1) offset += this.#heightAt(cursor);
    return offset;
  }

  #totalHeight(): number {
    return this.#offsetAt(this.#items.length);
  }

  #tailScrollTop(): number {
    return Math.max(0, this.#totalHeight() - this.#viewportHeight);
  }

  #clampScroll(scrollTop: number): number {
    return Math.min(Math.max(0, scrollTop), this.#tailScrollTop());
  }
}
