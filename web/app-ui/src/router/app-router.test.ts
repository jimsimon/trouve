import { describe, expect, it, vi } from "vitest";

import { AppRouter, parseRoute, routeHref, routeKey } from "./app-router.js";
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
      inspection: "info",
    });
    expect(parseRoute("/workspaces/ws/sessions/se/inspect/plan")).toEqual({
      kind: "session",
      workspaceId: "ws",
      sessionId: "se",
      inspection: "info",
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

  it("uses semantic route keys for independently parsed equivalent routes", () => {
    expect(routeKey(parseRoute("/settings/providers")))
      .toBe(routeKey({ kind: "settings", section: "providers" }));
    expect(routeKey({ kind: "not-found", pathname: "/unknown" }))
      .toBe("not-found:/unknown");
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

  it("notifies subscribers when application or browser navigation changes the route", () => {
    let pathname = "/inbox";
    let browserNavigation: (() => void) | undefined;
    const router = new AppRouter({
      pathname: () => pathname,
      push: (href) => {
        pathname = href;
      },
      replace: vi.fn(),
      listen: (listener) => {
        browserNavigation = listener;
        return () => undefined;
      },
    });
    const listener = vi.fn();
    const unsubscribe = router.subscribe(listener);

    router.navigate({ kind: "settings" });
    pathname = "/reviews";
    browserNavigation?.();

    expect(listener).toHaveBeenNthCalledWith(1, { kind: "settings" });
    expect(listener).toHaveBeenNthCalledWith(2, { kind: "reviews" });

    unsubscribe();
    router.navigate({ kind: "inbox" });
    expect(listener).toHaveBeenCalledTimes(2);
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
      inspection: "info",
    });
    expect(push).toHaveBeenLastCalledWith(
      "/workspaces/ws/sessions/se-2/threads/th-3/inspect/info",
    );
  });
});
