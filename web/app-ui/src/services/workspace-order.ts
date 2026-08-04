import type { ProtocolWorkspace } from "./protocol-client.js";
import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

const STORAGE_KEY = "trouve.workspace-order.v1";
const MAX_WORKSPACES = 1_000;
const MAX_ID_LENGTH = 256;

export interface WorkspaceOrderStorage {
  load(): readonly string[];
  save(order: readonly string[]): void;
}

export const normalizeWorkspaceOrder = (value: unknown): readonly string[] => {
  if (!Array.isArray(value)) return Object.freeze([]);
  const seen = new Set<string>();
  const order: string[] = [];
  for (const entry of value.slice(0, MAX_WORKSPACES)) {
    if (
      typeof entry !== "string" ||
      entry.length === 0 ||
      entry.length > MAX_ID_LENGTH ||
      seen.has(entry)
    ) continue;
    seen.add(entry);
    order.push(entry);
  }
  return Object.freeze(order);
};

export const browserWorkspaceOrderStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): WorkspaceOrderStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      return raw === null ? Object.freeze([]) : normalizeWorkspaceOrder(JSON.parse(raw));
    } catch {
      return Object.freeze([]);
    }
  },
  save: (order) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(order));
    } catch {
      // Ordering still applies for this frontend lifetime.
    }
  },
});

export class WorkspaceOrderController {
  readonly #storage: WorkspaceOrderStorage | undefined;
  readonly #order = createSignal<readonly string[]>(Object.freeze([]));
  readonly order: ReadonlySignal<readonly string[]> = this.#order;

  constructor(storage?: WorkspaceOrderStorage) {
    this.#storage = storage;
    this.#order.set(normalizeWorkspaceOrder(storage?.load()));
  }

  replace(value: unknown, persist = true): readonly string[] {
    const next = normalizeWorkspaceOrder(value);
    this.#order.set(next);
    if (persist) this.#storage?.save(next);
    return next;
  }

  ordered<T extends Pick<ProtocolWorkspace, "id">>(workspaces: readonly T[]): readonly T[] {
    const positions = new Map(this.#order.get().map((id, index) => [id, index]));
    return [...workspaces].sort((left, right) => {
      const leftPosition = positions.get(left.id);
      const rightPosition = positions.get(right.id);
      if (leftPosition === undefined && rightPosition === undefined) return 0;
      if (leftPosition === undefined) return 1;
      if (rightPosition === undefined) return -1;
      return leftPosition - rightPosition;
    });
  }

  reconcile<T extends Pick<ProtocolWorkspace, "id">>(workspaces: readonly T[]): readonly T[] {
    const ordered = this.ordered(workspaces);
    const next = ordered.map(({ id }) => id);
    const current = this.#order.get();
    if (next.length !== current.length || next.some((id, index) => id !== current[index])) {
      this.replace(next);
    }
    return ordered;
  }

  move(workspaceId: string, offset: number): boolean {
    const current = [...this.#order.get()];
    const from = current.indexOf(workspaceId);
    if (from < 0 || !Number.isInteger(offset) || offset === 0) return false;
    const to = Math.max(0, Math.min(current.length - 1, from + offset));
    if (to === from) return false;
    current.splice(from, 1);
    current.splice(to, 0, workspaceId);
    this.replace(current);
    return true;
  }

  drop(workspaceId: string, targetId: string, after: boolean): boolean {
    if (workspaceId === targetId) return false;
    const current = [...this.#order.get()];
    const from = current.indexOf(workspaceId);
    const target = current.indexOf(targetId);
    if (from < 0 || target < 0) return false;
    current.splice(from, 1);
    const adjustedTarget = current.indexOf(targetId);
    current.splice(adjustedTarget + (after ? 1 : 0), 0, workspaceId);
    this.replace(current);
    return true;
  }
}

export const createBrowserWorkspaceOrderController = (
  persistLocally: boolean,
): WorkspaceOrderController => {
  if (!persistLocally) return new WorkspaceOrderController();
  try {
    return new WorkspaceOrderController(
      browserWorkspaceOrderStorage(globalThis.localStorage),
    );
  } catch {
    return new WorkspaceOrderController();
  }
};
