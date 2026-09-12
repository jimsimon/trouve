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
  readonly #indexById = new Map<string, number>();
  #items: readonly T[] = [];
  #heights: number[] = [];
  #heightTree: number[] = [0];
  #scrollTop = 0;
  #viewportHeight = 0;
  #mode: VirtualizationMode;
  #followingTail = true;
  #anchor: Anchor | undefined;
  #window: VirtualWindow<T> | undefined;

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
    if (this.#mode === mode) return;
    this.#mode = mode;
    this.#window = undefined;
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
    this.#rebuildGeometry();

    if (this.#followingTail) {
      this.#scrollTop = this.#tailScrollTop();
    } else if (anchor !== undefined) {
      const anchorIndex = this.#indexById.get(anchor.id) ?? -1;
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
    this.#window = undefined;
    return correction(previousScrollTop, this.#scrollTop);
  }

  setViewport(
    scrollTop: number,
    viewportHeight: number,
    options: {
      readonly userInitiated?: boolean;
      readonly atTail?: boolean;
    } = {},
  ): void {
    if (!Number.isFinite(scrollTop) || scrollTop < 0) {
      throw new RangeError("scrollTop must be a non-negative finite number");
    }
    if (!Number.isFinite(viewportHeight) || viewportHeight < 0) {
      throw new RangeError("viewportHeight must be a non-negative finite number");
    }
    this.#viewportHeight = viewportHeight;
    this.#scrollTop = this.#clampScroll(scrollTop);
    const tailGap = this.#tailScrollTop() - this.#scrollTop;
    if (options.userInitiated === true) {
      // Match the retained desktop behavior: a deliberate scroll away parks
      // history, while reaching the actual bottom resumes live-tail pinning.
      // Keep the rejoin tolerance tight so a merely near-tail reader is not
      // pulled away from the position they chose.
      // The rendered DOM is authoritative when it is available because live
      // row measurements can briefly differ from the virtual height model.
      this.#followingTail = options.atTail ?? tailGap <= 1;
    } else if (this.#followingTail && tailGap > this.#tailTolerancePx) {
      this.#followingTail = false;
    }
    this.#anchor = this.#captureAnchor();
    this.#window = undefined;
  }

  /** Update only the viewport size. A live-tail resize must remain anchored
   * to the new bottom, while parked history keeps its existing top anchor. */
  resizeViewport(viewportHeight: number): ScrollCorrection {
    if (!Number.isFinite(viewportHeight) || viewportHeight < 0) {
      throw new RangeError("viewportHeight must be a non-negative finite number");
    }
    const previousScrollTop = this.#scrollTop;
    this.#viewportHeight = viewportHeight;
    this.#scrollTop = this.#followingTail
      ? this.#tailScrollTop()
      : this.#clampScroll(this.#scrollTop);
    this.#anchor = this.#captureAnchor();
    this.#window = undefined;
    return correction(previousScrollTop, this.#scrollTop);
  }

  enableFollowTail(): ScrollCorrection {
    const previousScrollTop = this.#scrollTop;
    this.#followingTail = true;
    this.#scrollTop = this.#tailScrollTop();
    this.#anchor = this.#captureAnchor();
    this.#window = undefined;
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
    const anchorIndex = this.#indexById.get(anchor.id) ?? -1;
    if (anchorIndex >= 0) {
      this.#scrollTop = this.#clampScroll(
        this.#offsetAt(anchorIndex) + anchor.offset,
      );
    } else {
      this.#scrollTop = this.#clampScroll(this.#scrollTop);
    }
    this.#window = undefined;
    return correction(previousScrollTop, this.#scrollTop);
  }

  hasMeasurement(id: string): boolean {
    return this.#measurements.has(id);
  }

  measure(id: string, height: number): ScrollCorrection {
    checkedHeight(height, `measured height for ${id}`);
    const index = this.#indexById.get(id) ?? -1;
    if (index < 0) throw new TypeError(`cannot measure unknown virtual item: ${id}`);
    const previousHeight = this.#heightAt(index);
    const previousScrollTop = this.#scrollTop;
    this.#measurements.set(id, height);
    this.#updateHeight(index, height);
    this.#window = undefined;
    const delta = height - previousHeight;
    if (delta === 0) return correction(previousScrollTop, this.#scrollTop);

    if (this.#followingTail) {
      this.#scrollTop = this.#tailScrollTop();
    } else {
      const anchor = this.#anchor ?? this.#captureAnchor();
      const anchorIndex =
        anchor === undefined
          ? -1
          : this.#indexById.get(anchor.id) ?? -1;
      if (anchorIndex >= 0 && index < anchorIndex) {
        this.#scrollTop = this.#clampScroll(this.#scrollTop + delta);
      } else {
        this.#scrollTop = this.#clampScroll(this.#scrollTop);
      }
    }
    return correction(previousScrollTop, this.#scrollTop);
  }

  window(): VirtualWindow<T> {
    if (this.#window !== undefined) return this.#window;
    const totalHeight = this.#totalHeight();
    if (this.#mode === "accessible") {
      this.#window = {
        items: this.#positionedRange(0, this.#items.length),
        paddingBefore: 0,
        paddingAfter: 0,
        totalHeight,
        scrollTop: this.#scrollTop,
        followingTail: this.#followingTail,
      };
      return this.#window;
    }

    const from = Math.max(0, this.#scrollTop - this.#overscanPx);
    const through = Math.min(
      totalHeight,
      this.#scrollTop + this.#viewportHeight + this.#overscanPx,
    );
    const startIndex = this.#indexAtOffset(from);
    const start = this.#offsetAt(startIndex);
    const throughIndex = this.#indexAtOffset(through);
    const endIndex = Math.min(
      this.#items.length,
      throughIndex + (this.#offsetAt(throughIndex) < through ? 1 : 0),
    );
    const end = this.#offsetAt(endIndex);
    this.#window = {
      items: this.#positionedRange(startIndex, endIndex),
      paddingBefore: start,
      paddingAfter: Math.max(0, totalHeight - end),
      totalHeight,
      scrollTop: this.#scrollTop,
      followingTail: this.#followingTail,
    };
    return this.#window;
  }

  isMounted(id: string): boolean {
    if (this.#mode === "accessible") return this.#indexById.has(id);
    return this.window().items.some(({ item }) => item.id === id);
  }

  shouldUnmountHeavyweight(id: string): boolean {
    const index = this.#indexById.get(id);
    const item = index === undefined ? undefined : this.#items[index];
    return item?.heavyweight === true && !this.isMounted(id);
  }

  #captureAnchor(): Anchor | undefined {
    if (this.#items.length === 0) return undefined;
    const index = this.#indexAtOffset(this.#scrollTop);
    const item = this.#items[index];
    if (item !== undefined) {
      return { id: item.id, offset: this.#scrollTop - this.#offsetAt(index) };
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
    return this.#heights[index] ?? 0;
  }

  #offsetAt(index: number): number {
    let offset = 0;
    for (
      let cursor = Math.min(Math.max(0, Math.floor(index)), this.#items.length);
      cursor > 0;
      cursor -= cursor & -cursor
    ) {
      offset += this.#heightTree[cursor] ?? 0;
    }
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

  /** Rebuild indexed geometry after an item-list change. Scroll events then
   * locate the visible window and anchor in O(log n), instead of repeatedly
   * walking every historical turn from the beginning. */
  #rebuildGeometry(): void {
    this.#indexById.clear();
    this.#heights = new Array<number>(this.#items.length);
    this.#heightTree = new Array<number>(this.#items.length + 1).fill(0);
    for (let index = 0; index < this.#items.length; index += 1) {
      const item = this.#items[index];
      if (item === undefined) continue;
      this.#indexById.set(item.id, index);
      const height = this.#measurements.get(item.id)
        ?? item.estimatedHeight
        ?? this.#defaultEstimatedHeight;
      this.#heights[index] = height;
      const cursor = index + 1;
      this.#heightTree[cursor] = (this.#heightTree[cursor] ?? 0) + height;
      const parent = cursor + (cursor & -cursor);
      if (parent < this.#heightTree.length) {
        this.#heightTree[parent] = (this.#heightTree[parent] ?? 0)
          + (this.#heightTree[cursor] ?? 0);
      }
    }
  }

  #updateHeight(index: number, height: number): void {
    const previous = this.#heights[index];
    if (previous === undefined || previous === height) return;
    const delta = height - previous;
    this.#heights[index] = height;
    for (
      let cursor = index + 1;
      cursor < this.#heightTree.length;
      cursor += cursor & -cursor
    ) {
      this.#heightTree[cursor] = (this.#heightTree[cursor] ?? 0) + delta;
    }
  }

  /** Return the first item whose end lies beyond `offset`. The Fenwick-tree
   * search mirrors the former `end <= offset` scan at exact row boundaries. */
  #indexAtOffset(offset: number): number {
    const itemCount = this.#items.length;
    if (itemCount === 0) return 0;
    let index = 0;
    let accumulated = 0;
    let step = 1;
    while (step * 2 <= itemCount) step *= 2;
    for (; step > 0; step = Math.floor(step / 2)) {
      const next = index + step;
      const height = this.#heightTree[next];
      if (next <= itemCount && height !== undefined && accumulated + height <= offset) {
        index = next;
        accumulated += height;
      }
    }
    return index;
  }
}
