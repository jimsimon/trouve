import { describe, expect, it } from "vitest";

import { WORKSPACE_STATUS_FILTERS } from "../components/workspace-session-list-model.js";
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

  it("ignores invalid toggles and does not persist repeated removals", () => {
    const saved: unknown[] = [];
    const controller = new WorkspaceListPreferencesController({
      load: () => undefined,
      save: (value) => saved.push(value),
    });
    controller.toggleFilter("", "status", 0);
    controller.toggleFilter("ws-1", "status", -1);
    controller.toggleFilter("ws-1", "status", 0.5);
    controller.toggleFilter("ws-1", "status", WORKSPACE_STATUS_FILTERS.length);
    expect(saved).toHaveLength(0);

    controller.toggleFilter("ws-1", "status", 0);
    controller.removeWorkspace("ws-1");
    controller.removeWorkspace("ws-1");
    expect(saved).toHaveLength(2);
    expect(controller.current.get().filters).toEqual({});
  });

  it("normalizes workspace filters into a null-prototype record", () => {
    const preferences = normalizeWorkspaceListPreferences(JSON.parse(
      '{"filters":{"__proto__":{"status":0,"pullRequest":0}}}',
    ));
    expect(Object.getPrototypeOf(preferences.filters)).toBeNull();
    expect(preferences.filters["__proto__"]).toEqual({ status: 0, pullRequest: 0 });
  });
});
