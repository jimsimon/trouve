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
    return workspaces.map((workspace) => ({
      key: workspace.id,
      label: workspace.name,
      workspaces: [workspace],
      repository: false,
    }));
  }

  const groups = new Map<string, { label: string; workspaces: T[] }>();
  for (const workspace of workspaces) {
    const repositoryKey = workspace.repository_key?.trim() || "workspace:" + workspace.id;
    const group = groups.get(repositoryKey) ?? {
      label: workspace.repository_name?.trim() || workspace.name,
      workspaces: [],
    };
    group.workspaces.push(workspace);
    groups.set(repositoryKey, group);
  }
  return [...groups].map(([key, group]) => ({
    key,
    label: group.label,
    workspaces: group.workspaces,
    repository: true,
  }));
};
