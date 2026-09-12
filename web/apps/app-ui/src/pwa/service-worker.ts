/// <reference lib="webworker" />

import {
  appShellAssetPaths,
  cachePolicyFor,
  STATIC_CACHE_NAME,
} from "./cache-policy.js";

declare const self: ServiceWorkerGlobalScope;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(STATIC_CACHE_NAME).then(async (cache) => {
      const shell = await fetch(new Request("/", { cache: "reload" }));
      if (!shell.ok) throw new Error("unable to fetch the PWA application shell");
      const html = await shell.clone().text();
      await cache.put("/", shell.clone());
      await cache.put("/index.html", shell);
      await cache.addAll([
        "/manifest.webmanifest",
        "/icons/trouve.svg",
        "/icons/trouve-512.png",
        ...appShellAssetPaths(html),
      ]);
    }),
  );
});
self.addEventListener("message", (event) => {
  if ((event.data as { type?: unknown } | null)?.type === "activate-update") {
    event.waitUntil(self.skipWaiting());
  }
});
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys
            .filter(
              (key) => key.startsWith("trouve-static-") && key !== STATIC_CACHE_NAME,
            )
            .map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});
self.addEventListener("fetch", (event) => {
  const policy = cachePolicyFor(event.request);
  if (policy === "network-only") return;
  if (policy === "app-shell-navigation") {
    event.respondWith(
      fetch(event.request).catch(async () => {
        const cache = await caches.open(STATIC_CACHE_NAME);
        const shell = await cache.match("/");
        return shell ?? Response.error();
      }),
    );
    return;
  }
  event.respondWith(
    caches.open(STATIC_CACHE_NAME).then(async (cache) => {
      const cached = await cache.match(event.request);
      if (cached) return cached;
      const response = await fetch(event.request);
      if (response.ok) await cache.put(event.request, response.clone());
      return response;
    }),
  );
});
