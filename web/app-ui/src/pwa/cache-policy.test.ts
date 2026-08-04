import { afterAll, beforeEach, describe, expect, it } from "vitest";

import { appShellAssetPaths, cachePolicyFor } from "./cache-policy.js";

const originalSelf = Object.getOwnPropertyDescriptor(globalThis, "self");

beforeEach(() => {
  Object.defineProperty(globalThis, "self", {
    configurable: true,
    value: { location: { origin: "https://trouve.example" } },
  });
});

afterAll(() => {
  if (originalSelf === undefined) {
    Reflect.deleteProperty(globalThis, "self");
  } else {
    Object.defineProperty(globalThis, "self", originalSelf);
  }
});

describe("PWA cache policy", () => {
  it("never caches protocol, auth, SSE, mutations, or other origins", () => {
    expect(cachePolicyFor(new Request("https://trouve.example/v1/sessions"))).toBe("network-only");
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/v1", { headers: { accept: "text/html" } }),
      ),
    ).toBe("network-only");
    expect(cachePolicyFor(new Request("https://trouve.example/auth/callback"))).toBe("network-only");
    expect(cachePolicyFor(new Request("https://trouve.example/__trouve/host/v1/preferences"))).toBe(
      "network-only",
    );
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/assets/events-12345678.js", {
          headers: { accept: "text/event-stream" },
        }),
      ),
    ).toBe("network-only");
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/assets/app-12345678.js", { method: "POST" }),
      ),
    ).toBe("network-only");
    expect(cachePolicyFor(new Request("https://elsewhere.example/assets/app-12345678.js"))).toBe(
      "network-only",
    );
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/assets/app-12345678.js?credential=secret"),
      ),
    ).toBe("network-only");
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/assets/app-12345678.js", {
          headers: { authorization: "Bearer secret" },
        }),
      ),
    ).toBe("network-only");
  });

  it("allows only immutable hashed same-origin assets", () => {
    expect(cachePolicyFor(new Request("https://trouve.example/assets/app-12345678.js"))).toBe(
      "static-asset",
    );
    expect(cachePolicyFor(new Request("https://trouve.example/index.html"))).toBe("network-only");
  });

  it("uses a static shell only for safe same-origin document navigation", () => {
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/workspaces/ws/sessions/se", {
          headers: { accept: "text/html,application/xhtml+xml" },
        }),
      ),
    ).toBe("app-shell-navigation");
    expect(
      cachePolicyFor(
        new Request("https://trouve.example/oauth/callback", {
          headers: { accept: "text/html" },
        }),
      ),
    ).toBe("network-only");
  });

  it("extracts only immutable Vite assets for shell precaching", () => {
    expect(
      appShellAssetPaths(`
        <link href="/assets/app-12345678.css">
        <script src="/assets/app-abcdefgh.js"></script>
        <img src="/repository/private.png">
        <script src="https://elsewhere.example/assets/bad-12345678.js"></script>
      `),
    ).toEqual(["/assets/app-12345678.css", "/assets/app-abcdefgh.js"]);
  });
});
