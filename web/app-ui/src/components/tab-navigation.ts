const isItemIndex = (index: number, itemCount: number): boolean =>
  Number.isInteger(index) && index >= 0 && index < itemCount;

/** Keep one selected tab in the sequential focus order, falling back to the
 * first tab while route-backed selection is still settling. */
export const rovingTabIndex = (
  itemIndex: number,
  selectedIndex: number,
  itemCount: number,
): 0 | -1 => {
  if (
    !Number.isInteger(itemCount) ||
    itemCount <= 0 ||
    !isItemIndex(itemIndex, itemCount)
  ) {
    return -1;
  }
  const tabStop = isItemIndex(selectedIndex, itemCount) ? selectedIndex : 0;
  return itemIndex === tabStop ? 0 : -1;
};

/** Resolve the automatic-activation keys for a horizontal ARIA tablist. */
export const nextHorizontalTabIndex = (
  key: string,
  currentIndex: number,
  itemCount: number,
): number | undefined => {
  if (
    !Number.isInteger(itemCount) ||
    itemCount <= 0 ||
    !isItemIndex(currentIndex, itemCount)
  ) {
    return undefined;
  }
  if (key === "Home") return 0;
  if (key === "End") return itemCount - 1;
  if (key === "ArrowLeft") return (currentIndex - 1 + itemCount) % itemCount;
  if (key === "ArrowRight") return (currentIndex + 1) % itemCount;
  return undefined;
};
