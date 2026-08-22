import type { WorkspaceListGrouping } from "../services/workspace-list-preferences.js";

export interface WorkspaceListEntry {
  readonly id: string;
  readonly name: string;
  readonly repository_key?: string | null;
  readonly repository_name?: string | null;
}

export interface WorkspaceListGroup<T extends WorkspaceListEntry> {
  readonly key: string;
  readonly label: string;
  readonly workspaces: readonly T[];
  readonly repository: boolean;
}

const nonBlankString = (value: unknown): string | undefined =>
  typeof value === "string" && value.trim() !== "" ? value : undefined;

/**
 * Resolve top-level repository/workspace organization before rendering child
 * session lists. Repository identity comes from the server so separate clones
 * and linked worktrees can be coalesced without guessing from display names.
 */
export const organizeWorkspaceList = <T extends WorkspaceListEntry>(
  workspaces: readonly T[],
  grouping: WorkspaceListGrouping,
): readonly WorkspaceListGroup<T>[] => {
  if (grouping !== "repository") {
    return Object.freeze(workspaces.map((workspace) => Object.freeze({
      key: workspace.id,
      label: workspace.name,
      workspaces: Object.freeze([workspace]),
      repository: false,
    })));
  }

  const groups = new Map<string, { label: string; workspaces: T[] }>();
  for (const workspace of workspaces) {
    const repositoryKey = nonBlankString(workspace.repository_key) ?? "workspace:" + workspace.id;
    const group = groups.get(repositoryKey) ?? {
      label: nonBlankString(workspace.repository_name) ?? workspace.name,
      workspaces: [],
    };
    group.workspaces.push(workspace);
    groups.set(repositoryKey, group);
  }
  return Object.freeze([...groups].map(([key, group]) => Object.freeze({
    key,
    label: group.label,
    workspaces: Object.freeze(group.workspaces),
    repository: true,
  })));
};
