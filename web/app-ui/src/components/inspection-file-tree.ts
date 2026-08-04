import type { ProtocolDirEntry } from "../services/protocol-client.js";

export type FileTreeDirectoryStatus =
  | "unloaded"
  | "loading"
  | "loaded"
  | "error";

export interface FileTreeDirectorySnapshot {
  readonly status: FileTreeDirectoryStatus;
  readonly entries: readonly ProtocolDirEntry[];
}

export interface FileTreeRow {
  readonly path: string;
  readonly parentPath: string;
  readonly name: string;
  readonly isDirectory: boolean;
  /** Zero-based visual indentation. */
  readonly depth: number;
  /** One-based ARIA tree level. */
  readonly level: number;
  readonly positionInSet: number;
  readonly setSize: number;
  readonly expanded: boolean;
  readonly directoryStatus: FileTreeDirectoryStatus | undefined;
}

export type FileTreeKeyAction =
  | { readonly kind: "focus"; readonly path: string }
  | { readonly kind: "expand"; readonly path: string }
  | { readonly kind: "collapse"; readonly path: string }
  | { readonly kind: "activate"; readonly path: string };

const EMPTY_ENTRIES: readonly ProtocolDirEntry[] = Object.freeze([]);

const parentPath = (path: string): string => {
  const separator = path.lastIndexOf("/");
  return separator < 0 ? "." : path.slice(0, separator);
};

const compareEntries = (left: ProtocolDirEntry, right: ProtocolDirEntry): number => {
  if (left.is_dir !== right.is_dir) return left.is_dir ? -1 : 1;
  if (left.name < right.name) return -1;
  if (left.name > right.name) return 1;
  return 0;
};

const joinedPath = (directory: string, name: string): string =>
  directory === "." ? name : `${directory}/${name}`;

/**
 * Cached, lazily populated projection for the Files inspection tree.
 *
 * Network requests stay in the component. This model owns only deterministic
 * directory states, visible-row flattening, roving focus, and recovery when a
 * refresh or collapse removes the active descendant.
 */
export class InspectionFileTreeModel {
  readonly #directories = new Map<string, FileTreeDirectorySnapshot>();
  readonly #expanded = new Set<string>();
  #activePath: string | undefined;

  get activePath(): string | undefined {
    return this.#activePath;
  }

  get rows(): readonly FileTreeRow[] {
    const rows: FileTreeRow[] = [];
    const walk = (directory: string, depth: number): void => {
      const state = this.directory(directory);
      if (state.status !== "loaded") return;
      const setSize = state.entries.length;
      state.entries.forEach((entry, index) => {
        const path = joinedPath(directory, entry.name);
        const expanded = entry.is_dir && this.#expanded.has(path);
        rows.push({
          path,
          parentPath: directory,
          name: entry.name,
          isDirectory: entry.is_dir,
          depth,
          level: depth + 1,
          positionInSet: index + 1,
          setSize,
          expanded,
          directoryStatus: entry.is_dir
            ? this.directory(path).status
            : undefined,
        });
        if (expanded) walk(path, depth + 1);
      });
    };
    walk(".", 0);
    return rows;
  }

  get loading(): boolean {
    return [...this.#directories.values()].some(
      (directory) => directory.status === "loading",
    );
  }

  directory(path: string): FileTreeDirectorySnapshot {
    return this.#directories.get(path) ?? {
      status: "unloaded",
      entries: EMPTY_ENTRIES,
    };
  }

  row(path: string): FileTreeRow | undefined {
    return this.rows.find((row) => row.path === path);
  }

  /** Clear cached listings while retaining a path to recover after reload. */
  reset(preferredPath = this.#activePath): void {
    this.#directories.clear();
    this.#expanded.clear();
    this.#activePath = preferredPath;
  }

  /** Reset all state when the owning session changes. */
  clear(): void {
    this.#directories.clear();
    this.#expanded.clear();
    this.#activePath = undefined;
  }

  beginLoading(path: string): void {
    this.#directories.set(path, {
      status: "loading",
      entries: EMPTY_ENTRIES,
    });
    this.#recoverActivePath();
  }

  resolveDirectory(path: string, entries: readonly ProtocolDirEntry[]): void {
    this.#directories.set(path, {
      status: "loaded",
      entries: [...entries].sort(compareEntries),
    });
    this.#recoverActivePath();
  }

  failDirectory(path: string): void {
    this.#directories.set(path, {
      status: "error",
      entries: EMPTY_ENTRIES,
    });
    this.#recoverActivePath();
  }

  needsLoad(path: string): boolean {
    const status = this.directory(path).status;
    return status === "unloaded" || status === "error";
  }

  setActive(path: string): boolean {
    if (this.row(path) === undefined) return false;
    this.#activePath = path;
    return true;
  }

  expand(path: string): boolean {
    const row = this.row(path);
    if (row === undefined || !row.isDirectory) return false;
    this.#expanded.add(path);
    this.#activePath = path;
    return true;
  }

  collapse(path: string): boolean {
    const row = this.row(path);
    if (row === undefined || !row.isDirectory || !row.expanded) return false;
    this.#expanded.delete(path);
    if (
      this.#activePath !== undefined &&
      this.#activePath.startsWith(`${path}/`)
    ) {
      this.#activePath = path;
    }
    this.#recoverActivePath();
    return true;
  }

  toggle(path: string): "expanded" | "collapsed" | undefined {
    const row = this.row(path);
    if (row === undefined || !row.isDirectory) return undefined;
    if (row.expanded) {
      this.collapse(path);
      return "collapsed";
    }
    this.expand(path);
    return "expanded";
  }

  actionForKey(key: string, currentPath: string): FileTreeKeyAction | undefined {
    const rows = this.rows;
    const currentIndex = rows.findIndex((row) => row.path === currentPath);
    const current = rows[currentIndex];
    if (current === undefined) return undefined;

    if (key === "ArrowUp") {
      return { kind: "focus", path: rows[Math.max(0, currentIndex - 1)]!.path };
    }
    if (key === "ArrowDown") {
      return {
        kind: "focus",
        path: rows[Math.min(rows.length - 1, currentIndex + 1)]!.path,
      };
    }
    if (key === "Home") return { kind: "focus", path: rows[0]!.path };
    if (key === "End") {
      return { kind: "focus", path: rows[rows.length - 1]!.path };
    }
    if (key === "ArrowRight" && current.isDirectory) {
      if (!current.expanded) return { kind: "expand", path: current.path };
      const firstChild = rows[currentIndex + 1];
      return firstChild?.parentPath === current.path
        ? { kind: "focus", path: firstChild.path }
        : undefined;
    }
    if (key === "ArrowLeft") {
      if (current.isDirectory && current.expanded) {
        return { kind: "collapse", path: current.path };
      }
      return current.parentPath === "."
        ? undefined
        : { kind: "focus", path: current.parentPath };
    }
    if (key === "Enter" || key === " ") {
      return { kind: "activate", path: current.path };
    }
    return undefined;
  }

  #recoverActivePath(): void {
    const rows = this.rows;
    if (rows.length === 0) {
      // Keep the preferred path while the root listing is loading. It can be
      // recovered to that path, a visible ancestor, or the first root item
      // once rows exist.
      return;
    }
    const visible = new Set(rows.map((row) => row.path));
    let candidate = this.#activePath;
    while (candidate !== undefined && candidate !== ".") {
      if (visible.has(candidate)) {
        this.#activePath = candidate;
        return;
      }
      candidate = parentPath(candidate);
    }
    this.#activePath = rows[0]!.path;
  }
}
