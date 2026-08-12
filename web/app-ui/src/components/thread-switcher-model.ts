export interface ThreadSwitcherEntry {
  readonly id: string;
  readonly parentThreadId: string | null | undefined;
  readonly title: string;
  readonly detail: string;
  readonly closed: boolean;
  readonly pinned: boolean;
  readonly active: boolean;
  readonly needsAttention: boolean;
}

export type ThreadSwitcherFilter = "all" | "running" | "attention" | "removed";

export interface ThreadSwitcherRow {
  readonly entry: ThreadSwitcherEntry;
  readonly depth: number;
}

const normalizedQuery = (query: string): string =>
  query.trim().toLocaleLowerCase();

/**
 * Produces a stable, cycle-safe pre-order tree for one switcher section.
 * Parents outside the section become roots, which keeps open and removed
 * threads independently understandable.
 */
export const threadSwitcherRows = (
  entries: readonly ThreadSwitcherEntry[],
  query: string,
  filter: ThreadSwitcherFilter = "all",
): readonly ThreadSwitcherRow[] => {
  const entryById = new Map(entries.map((entry) => [entry.id, entry]));
  const children = new Map<string, ThreadSwitcherEntry[]>();
  const roots: ThreadSwitcherEntry[] = [];
  for (const entry of entries) {
    const parentId = entry.parentThreadId;
    if (
      parentId === undefined
      || parentId === null
      || parentId === entry.id
      || !entryById.has(parentId)
    ) {
      roots.push(entry);
      continue;
    }
    const siblings = children.get(parentId) ?? [];
    siblings.push(entry);
    children.set(parentId, siblings);
  }

  const needle = normalizedQuery(query);
  const matches = (entry: ThreadSwitcherEntry): boolean => {
    const matchesQuery = needle === ""
      || (entry.title + " " + entry.detail).toLocaleLowerCase().includes(needle);
    const matchesFilter = filter === "all"
      || (filter === "running" && entry.active)
      || (filter === "attention" && entry.needsAttention)
      || (filter === "removed" && entry.closed);
    return matchesQuery && matchesFilter;
  };
  // Each entry has at most one parent. Propagate matches upward once instead
  // of recursively rescanning every descendant subtree for every row.
  const visible = new Set(entries.filter(matches).map((entry) => entry.id));
  const pending = [...visible];
  for (let index = 0; index < pending.length; index += 1) {
    const entry = entryById.get(pending[index] ?? "");
    const parentId = entry?.parentThreadId;
    if (
      parentId === undefined
      || parentId === null
      || parentId === entry?.id
      || !entryById.has(parentId)
      || visible.has(parentId)
    ) continue;
    visible.add(parentId);
    pending.push(parentId);
  }

  const rows: ThreadSwitcherRow[] = [];
  const visited = new Set<string>();
  const append = (entry: ThreadSwitcherEntry, depth: number): void => {
    const stack: Array<{ readonly entry: ThreadSwitcherEntry; readonly depth: number }> = [
      { entry, depth },
    ];
    while (stack.length > 0) {
      const current = stack.pop();
      if (
        current === undefined
        || visited.has(current.entry.id)
        || !visible.has(current.entry.id)
      ) continue;
      visited.add(current.entry.id);
      rows.push(current);
      const descendants = children.get(current.entry.id) ?? [];
      for (let index = descendants.length - 1; index >= 0; index -= 1) {
        const child = descendants[index];
        if (child !== undefined) stack.push({ entry: child, depth: current.depth + 1 });
      }
    }
  };
  for (const root of roots) append(root, 0);
  // A malformed cycle has no root. Keep every thread reachable rather than
  // silently dropping it from the authoritative switcher.
  for (const entry of entries) append(entry, 0);
  return rows;
};

/** Number of durable tabs that fit after reserving a slot for provisional
 * thread setup. A one-tab layout must be allowed to reserve all its space. */
export const durableThreadTabCapacity = (
  totalCapacity: number,
  provisionalOpen: boolean,
): number => Math.max(0, totalCapacity - (provisionalOpen ? 1 : 0));

/**
 * Selects the bounded desktop working set. Pinned threads lead while the
 * current thread always receives a slot; recent and durable open threads fill
 * the remainder.
 */
export const threadWorkingSet = (
  openThreadIds: readonly string[],
  currentThreadId: string,
  pinnedThreadIds: readonly string[],
  recentThreadIds: readonly string[],
  capacity: number,
): readonly string[] => {
  if (capacity <= 0 || openThreadIds.length === 0) return [];
  const open = new Set(openThreadIds);
  const selected: string[] = [];
  const append = (id: string): void => {
    if (!open.has(id) || selected.includes(id) || selected.length >= capacity) return;
    selected.push(id);
  };
  const currentPinned = pinnedThreadIds.includes(currentThreadId);
  if (currentPinned) append(currentThreadId);
  const pinnedCapacity = open.has(currentThreadId) && !currentPinned
    ? Math.max(0, capacity - 1)
    : capacity;
  for (const id of pinnedThreadIds) {
    if (selected.length >= pinnedCapacity && !currentPinned) break;
    append(id);
  }
  append(currentThreadId);
  for (const id of recentThreadIds) append(id);
  for (const id of openThreadIds) append(id);
  return selected;
};
