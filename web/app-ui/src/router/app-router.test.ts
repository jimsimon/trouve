import { describe, expect, it, vi } from "vitest";

import { AppRouter, parseRoute, routeHref } from "./app-router.js";
import { readSignal } from "../state/reactivity.js";

describe("application routes", () => {
  it("round-trips the session and mobile inspection route", () => {
    const route = {
      kind: "session" as const,
      workspaceId: "ws/a",
      sessionId: "se 1",
      threadId: "th#2",
      inspection: "diff" as const,
    };
    const href = routeHref(route);
    expect(parseRoute(href)).toEqual(route);
    expect(
      parseRoute("/workspaces/ws/sessions/se/inspect/pr"),
    ).toEqual({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se",
      inspection: "pr",
    });
    expect(parseRoute("/workspaces/ws/sessions/se/inspect/mcp")).toEqual({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se",
      inspection: "mcp",
    });
  });

  it("rejects malformed escapes and unknown inspection panels", () => {
    expect(parseRoute("/workspaces/%zz/sessions/se-1").kind).toBe("not-found");
    expect(parseRoute("/workspaces/ws/sessions/se/inspect/secrets").kind).toBe("not-found");
  });

  it("round-trips the management routes", () => {
    expect(parseRoute(routeHref({ kind: "reviews" }))).toEqual({ kind: "reviews" });
    expect(parseRoute(routeHref({ kind: "automations" }))).toEqual({
      kind: "automations",
    });
  });

  it("owns navigation without replacing the stable router service", () => {
    let pathname = "/inbox";
    const push = vi.fn((href: string) => {
      pathname = href;
    });
    const router = new AppRouter({
      pathname: () => pathname,
      push,
      replace: vi.fn(),
      listen: () => () => undefined,
    });
    router.navigate({ kind: "settings", section: "providers" });
    expect(push).toHaveBeenCalledWith("/settings/providers");
    expect(readSignal(router.route)).toEqual({ kind: "settings", section: "providers" });
  });

  it("remembers the last settings screen for the router lifetime", () => {
    let pathname = "/inbox";
    const push = vi.fn((href: string) => {
      pathname = href;
    });
    const router = new AppRouter({
      pathname: () => pathname,
      push,
      replace: vi.fn(),
      listen: () => () => undefined,
    });

    router.navigate({ kind: "settings", section: "chat" });
    router.navigate({ kind: "inbox" });
    router.navigate({ kind: "settings" });

    expect(push).toHaveBeenLastCalledWith("/settings/chat");
    expect(readSignal(router.route)).toEqual({ kind: "settings", section: "chat" });
  });

  it("does not persist the settings screen into a new app router", () => {
    let pathname = "/inbox";
    const platform = () => ({
      pathname: () => pathname,
      push: vi.fn((href: string) => {
        pathname = href;
      }),
      replace: vi.fn(),
      listen: () => () => undefined,
    });
    const firstPlatform = platform();
    const firstRouter = new AppRouter(firstPlatform);
    firstRouter.navigate({ kind: "settings", section: "appearance" });
    firstRouter.dispose();

    pathname = "/inbox";
    const restartedPlatform = platform();
    const restartedRouter = new AppRouter(restartedPlatform);
    restartedRouter.navigate({ kind: "settings" });

    expect(restartedPlatform.push).toHaveBeenLastCalledWith("/settings");
    expect(readSignal(restartedRouter.route)).toEqual({ kind: "settings" });
  });

  it("preserves the selected inspection pane across session and thread changes", () => {
    let pathname = "/workspaces/ws/sessions/se-1/threads/th-1/inspect/files";
    const push = vi.fn((href: string) => {
      pathname = href;
    });
    const router = new AppRouter({
      pathname: () => pathname,
      push,
      replace: vi.fn(),
      listen: () => () => undefined,
    });

    router.navigate({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se-1",
      threadId: "th-2",
    });
    expect(push).toHaveBeenLastCalledWith(
      "/workspaces/ws/sessions/se-1/threads/th-2/inspect/files",
    );
    expect(readSignal(router.route)).toEqual({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se-1",
      threadId: "th-2",
      inspection: "files",
    });

    router.navigate({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se-2",
      threadId: "th-3",
    });
    expect(push).toHaveBeenLastCalledWith(
      "/workspaces/ws/sessions/se-2/threads/th-3/inspect/files",
    );

    router.navigate({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se-2",
      threadId: "th-3",
      inspection: "mcp",
    });
    expect(push).toHaveBeenLastCalledWith(
      "/workspaces/ws/sessions/se-2/threads/th-3/inspect/mcp",
    );
  });
});
