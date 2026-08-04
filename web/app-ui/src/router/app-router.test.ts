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
});
