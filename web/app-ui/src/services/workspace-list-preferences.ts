import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

const STORAGE_KEY = "trouve.workspace-list-preferences.v1";
const ALL_FILTERS = 0b1_1111;

export type WorkspaceListGrouping = "repository" | "workspace" | "updated" | "status";
export type WorkspaceListOrdering = "updated" | "status" | "created";

export interface WorkspaceListFilterPreferences {
  readonly status: number;
  readonly pullRequest: number;
}

export interface WorkspaceListPreferences {
  readonly grouping: WorkspaceListGrouping;
  readonly ordering: WorkspaceListOrdering;
  readonly showBranches: boolean;
  readonly showStatus: boolean;
  readonly filters: Readonly<Record<string, WorkspaceListFilterPreferences>>;
}

export interface WorkspaceListPreferencesStorage {
  load(): unknown;
  save(value: WorkspaceListPreferences): void;
}

export const DEFAULT_WORKSPACE_LIST_PREFERENCES: WorkspaceListPreferences = Object.freeze({
  grouping: "repository",
  ordering: "updated",
  showBranches: true,
  showStatus: true,
  filters: Object.freeze({}),
});

const grouping = (value: unknown): WorkspaceListGrouping =>
  value === "workspace" || value === "updated" || value === "status"
    ? value
    : "repository";

const ordering = (value: unknown): WorkspaceListOrdering =>
  value === "status" || value === "created" ? value : "updated";

const mask = (value: unknown): number =>
  typeof value === "number" && Number.isInteger(value)
    ? value & ALL_FILTERS
    : ALL_FILTERS;

export const normalizeWorkspaceListPreferences = (
  value: unknown,
): WorkspaceListPreferences => {
  const source = value !== null && typeof value === "object"
    ? value as Record<string, unknown>
    : {};
  const rawFilters = source["filters"] !== null && typeof source["filters"] === "object"
    ? source["filters"] as Record<string, unknown>
    : {};
  const filters: Record<string, WorkspaceListFilterPreferences> = {};
  for (const [workspaceId, raw] of Object.entries(rawFilters).slice(0, 1_000)) {
    if (workspaceId === "" || workspaceId.length > 256 || raw === null || typeof raw !== "object") {
      continue;
    }
    const entry = raw as Record<string, unknown>;
    filters[workspaceId] = Object.freeze({
      status: mask(entry["status"]),
      pullRequest: mask(entry["pullRequest"]),
    });
  }
  return Object.freeze({
    grouping: grouping(source["grouping"]),
    ordering: ordering(source["ordering"]),
    showBranches: typeof source["showBranches"] === "boolean"
      ? source["showBranches"]
      : true,
    showStatus: typeof source["showStatus"] === "boolean"
      ? source["showStatus"]
      : true,
    filters: Object.freeze(filters),
  });
};

export const browserWorkspaceListPreferencesStorage = (
  storage: Pick<Storage, "getItem" | "setItem">,
): WorkspaceListPreferencesStorage => ({
  load: () => {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      return raw === null ? undefined : JSON.parse(raw);
    } catch {
      return undefined;
    }
  },
  save: (value) => {
    try {
      storage.setItem(STORAGE_KEY, JSON.stringify(value));
    } catch {
      // Preferences still apply for this frontend lifetime.
    }
  },
});

export class WorkspaceListPreferencesController {
  readonly #storage: WorkspaceListPreferencesStorage | undefined;
  readonly #current = createSignal(DEFAULT_WORKSPACE_LIST_PREFERENCES);
  readonly current: ReadonlySignal<WorkspaceListPreferences> = this.#current;

  constructor(storage?: WorkspaceListPreferencesStorage) {
    this.#storage = storage;
    this.#current.set(normalizeWorkspaceListPreferences(storage?.load()));
  }

  update(patch: Partial<Omit<WorkspaceListPreferences, "filters">>): void {
    this.#replace({ ...this.#current.get(), ...patch });
  }

  filtersFor(workspaceId: string): WorkspaceListFilterPreferences {
    return this.#current.get().filters[workspaceId]
      ?? Object.freeze({ status: ALL_FILTERS, pullRequest: ALL_FILTERS });
  }

  toggleFilter(
    workspaceId: string,
    category: keyof WorkspaceListFilterPreferences,
    index: number,
  ): void {
    if (workspaceId === "" || index < 0 || index > 4) return;
    const current = this.#current.get();
    const workspace = this.filtersFor(workspaceId);
    const nextWorkspace = Object.freeze({
      ...workspace,
      [category]: workspace[category] ^ (1 << index),
    });
    this.#replace({
      ...current,
      filters: { ...current.filters, [workspaceId]: nextWorkspace },
    });
  }

  removeWorkspace(workspaceId: string): void {
    const current = this.#current.get();
    if (current.filters[workspaceId] === undefined) return;
    const filters = { ...current.filters };
    delete filters[workspaceId];
    this.#replace({ ...current, filters });
  }

  #replace(value: unknown): void {
    const next = normalizeWorkspaceListPreferences(value);
    this.#current.set(next);
    this.#storage?.save(next);
  }
}

export const createBrowserWorkspaceListPreferencesController = () => {
  try {
    return new WorkspaceListPreferencesController(
      browserWorkspaceListPreferencesStorage(globalThis.localStorage),
    );
  } catch {
    return new WorkspaceListPreferencesController();
  }
};
