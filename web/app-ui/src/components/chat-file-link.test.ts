import { describe, expect, it } from "vitest";

import {
  isApplicationRouteTarget,
  parseChatFileTarget,
  sessionRelativeFilePath,
} from "./chat-file-link.js";

describe("chat file links", () => {
  it("parses the absolute, relative, and line-range forms used by agents", () => {
    expect(parseChatFileTarget("/tmp/worktree/crates/app/src/main.rs:42")).toEqual({
      path: "/tmp/worktree/crates/app/src/main.rs",
      from: 42,
      to: 42,
    });
    expect(parseChatFileTarget("crates/app/src/main.rs:42:7")).toEqual({
      path: "crates/app/src/main.rs",
      from: 42,
      to: 42,
    });
    expect(parseChatFileTarget("crates/app/src/main.rs#L10-L14")).toEqual({
      path: "crates/app/src/main.rs",
      from: 10,
      to: 14,
    });
    expect(parseChatFileTarget("file:///tmp/worktree/README.md")).toEqual({
      path: "/tmp/worktree/README.md",
      from: 0,
      to: 0,
    });
  });

  it("does not turn ordinary URLs, anchors, or labels into file actions", () => {
    expect(parseChatFileTarget("https://example.com/file.rs:42")).toBeUndefined();
    expect(parseChatFileTarget("mailto:user@example.com")).toBeUndefined();
    expect(parseChatFileTarget("#section")).toBeUndefined();
    expect(parseChatFileTarget("README")).toBeUndefined();
  });

  it("keeps known app routes in the router workflow", () => {
    expect(isApplicationRouteTarget("/settings/providers")).toBe(true);
    expect(isApplicationRouteTarget("/workspaces/ws/sessions/se")).toBe(true);
    expect(isApplicationRouteTarget("/tmp/worktree/file.rs")).toBe(false);
  });

  it("contains absolute and relative actions to the selected worktree", () => {
    expect(sessionRelativeFilePath(
      "/tmp/worktree/crates/app/src/main.rs",
      "/tmp/worktree",
    )).toBe("crates/app/src/main.rs");
    expect(sessionRelativeFilePath("crates/app/src/main.rs", "/tmp/worktree")).toBe(
      "crates/app/src/main.rs",
    );
    expect(sessionRelativeFilePath("C:\\worktree\\src\\main.rs", "C:\\worktree")).toBe(
      "src/main.rs",
    );
    expect(sessionRelativeFilePath("/etc/passwd", "/tmp/worktree")).toBeUndefined();
    expect(sessionRelativeFilePath("../outside", "/tmp/worktree")).toBeUndefined();
  });
});
