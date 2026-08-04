import { describe, expect, it } from "vitest";

import {
  emptyWorkspaceRegistrationDraft,
  pickAndRegisterWorkspace,
  validateWorkspaceRegistration,
  workspaceRegistrationRequest,
} from "./workspace-settings-model.js";

describe("workspace registration form model", () => {
  it("requires a server-host repository path", () => {
    expect(validateWorkspaceRegistration(emptyWorkspaceRegistrationDraft())).toEqual({
      path: "Enter an absolute repository path on the server host.",
    });
    expect(() => workspaceRegistrationRequest({ path: "   ", name: "ignored" }))
      .toThrow("server host");
  });

  it("trims values and omits an empty optional name", () => {
    expect(
      workspaceRegistrationRequest({ path: "  /srv/repos/trouve  ", name: "   " }),
    ).toEqual({ path: "/srv/repos/trouve" });
    expect(
      workspaceRegistrationRequest({ path: "/srv/repos/trouve", name: "  Main repo  " }),
    ).toEqual({ path: "/srv/repos/trouve", name: "Main repo" });
  });

  it("leaves host-specific path validation to the server", () => {
    expect(
      workspaceRegistrationRequest({ path: "C:\\repos\\trouve", name: "Windows host" }),
    ).toMatchObject({ path: "C:\\repos\\trouve" });
    expect(
      workspaceRegistrationRequest({ path: "\\\\server\\repos\\trouve", name: "" }),
    ).toMatchObject({ path: "\\\\server\\repos\\trouve" });
  });

  it("registers a native-picked path through the protocol without rewriting it", async () => {
    const requests: unknown[] = [];
    const workspace = { id: "ws-1", name: "trouve", path: "/srv/repos/trouve " };
    const result = await pickAndRegisterWorkspace(
      { pickDirectory: async () => "/srv/repos/trouve " },
      {
        registerWorkspace: async (request) => {
          requests.push(request);
          return workspace;
        },
      },
    );
    expect(result).toEqual(workspace);
    expect(requests).toEqual([{ path: "/srv/repos/trouve " }]);
  });

  it("does not mutate protocol state when the native picker is cancelled", async () => {
    let registrations = 0;
    await expect(
      pickAndRegisterWorkspace(
        { pickDirectory: async () => undefined },
        {
          registerWorkspace: async () => {
            registrations += 1;
            throw new Error("must not register");
          },
        },
      ),
    ).resolves.toBeUndefined();
    expect(registrations).toBe(0);
  });

  it("rejects malformed picker paths before registration", async () => {
    let registrations = 0;
    await expect(
      pickAndRegisterWorkspace(
        { pickDirectory: async () => "/srv/repos/secret\npath" },
        {
          registerWorkspace: async () => {
            registrations += 1;
            throw new Error("must not register");
          },
        },
      ),
    ).rejects.toThrow("invalid path");
    expect(registrations).toBe(0);
  });
});
