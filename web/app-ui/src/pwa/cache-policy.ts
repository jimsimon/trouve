export type CachePolicy = "network-only" | "static-asset" | "app-shell-navigation";

const USER_DATA_PREFIXES = [
  "/v1/",
  "/auth/",
  "/oauth/",
  "/github/webhooks",
  "/__trouve/host/",
] as const;

const HASHED_ASSET = /^\/assets\/[a-zA-Z0-9_.-]+-[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9]+$/;

export const cachePolicyFor = (request: Request): CachePolicy => {
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin) return "network-only";
  if (url.search !== "" || request.headers.has("authorization")) return "network-only";
  if (
    USER_DATA_PREFIXES.some(
      (prefix) =>
        url.pathname === prefix.replace(/\/$/, "") || url.pathname.startsWith(prefix),
    )
  ) {
    return "network-only";
  }
  if (request.headers.get("accept")?.includes("text/event-stream")) {
    return "network-only";
  }
  if (request.headers.get("accept")?.includes("text/html")) {
    return "app-shell-navigation";
  }
  return HASHED_ASSET.test(url.pathname) ? "static-asset" : "network-only";
};

/** Extract only Vite's immutable same-origin asset paths from the static shell.
 * This is intentionally not a general-purpose HTML URL crawler. */
export const appShellAssetPaths = (html: string): readonly string[] => {
  const paths = new Set<string>();
  const references = html.matchAll(/(?:src|href)=["'](\/assets\/[^"']+)["']/g);
  for (const match of references) {
    const path = match[1];
    if (path !== undefined && HASHED_ASSET.test(path)) paths.add(path);
  }
  return [...paths];
};

export const STATIC_CACHE_NAME = __TROUVE_PWA_CACHE_NAME__;
