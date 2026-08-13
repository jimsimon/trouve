import { describe, expect, it } from "vitest";

import {
  normalizeWorkspaceListPreferences,
  WorkspaceListPreferencesController,
} from "./workspace-list-preferences.js";

describe("workspace list preferences", () => {
  it("normalizes invalid values and bounded filter masks", () => {
    expect(normalizeWorkspaceListPreferences({
      grouping: "future",
      ordering: "created",
      showBranches: false,
      filters: { ws: { status: 63, pullRequest: -1 } },
    })).toEqual({
      grouping: "repository",
      ordering: "created",
      showBranches: false,
      showStatus: true,
      filters: { ws: { status: 31, pullRequest: 31 } },
    });
  });

  it("persists global choices and independent workspace filters", () => {
    const saved: unknown[] = [];
    const controller = new WorkspaceListPreferencesController({
      load: () => undefined,
      save: (value) => saved.push(value),
    });
    controller.update({ grouping: "status", showStatus: false });
    controller.toggleFilter("ws-1", "status", 2);
    controller.toggleFilter("ws-1", "pullRequest", 4);
    expect(controller.current.get()).toMatchObject({
      grouping: "status",
      showStatus: false,
      filters: { "ws-1": { status: 27, pullRequest: 15 } },
    });
    expect(saved).toHaveLength(3);
  });
});
