import { describe, expect, it } from "vitest";

import { organizeWorkspaceList, type WorkspaceListEntry } from "./workspace-list-model.js";

const workspace = (
  id: string,
  repositoryKey: string,
  repositoryName: string,
): WorkspaceListEntry => ({
  id,
  name: id,
  repository_key: repositoryKey,
  repository_name: repositoryName,
});

describe("workspace list model", () => {
  it("coalesces separate workspaces for the same repository", () => {
    const workspaces = [
      workspace("first", "remote:github.com/acme/app", "app"),
      workspace("other", "remote:github.com/acme/other", "other"),
      workspace("clone", "remote:github.com/acme/app", "app"),
    ];

    const repositories = organizeWorkspaceList(workspaces, "repository");
    expect(repositories.map(({ key }) => key)).toEqual([
      "remote:github.com/acme/app",
      "remote:github.com/acme/other",
    ]);
    expect(repositories[0]?.workspaces.map(({ id }) => id)).toEqual(["first", "clone"]);

    const separate = organizeWorkspaceList(workspaces, "workspace");
    expect(separate.map(({ key }) => key)).toEqual(["first", "other", "clone"]);
  });

  it("falls back to workspace identity for blank optional repository metadata", () => {
    const workspaces = [
      { id: "blank", name: "Blank", repository_key: "", repository_name: "" },
    ];

    const groups = organizeWorkspaceList(workspaces, "repository");
    expect(groups.map(({ key }) => key)).toEqual(["workspace:blank"]);
    expect(groups.map(({ label }) => label)).toEqual(["Blank"]);
  });
});
