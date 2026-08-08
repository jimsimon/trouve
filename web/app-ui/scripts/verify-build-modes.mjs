import { existsSync, readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

const bundleText = (mode) => {
  const directory = resolve("dist", mode, "assets");
  return readdirSync(directory)
    .filter((name) => name.endsWith(".js"))
    .map((name) => readFileSync(resolve(directory, name), "utf8"))
    .join("\n");
};

const artifactFiles = (directory) =>
  readdirSync(directory, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => resolve(entry.parentPath, entry.name));

const desktop = bundleText("desktop");
const pwa = bundleText("pwa");
const desktopRoot = resolve("dist", "desktop");
const pwaRoot = resolve("dist", "pwa");
const serviceWorkerPath = resolve(pwaRoot, "service-worker.js");
const metadataPath = resolve(pwaRoot, "pwa-meta.json");
const manifestPath = resolve(pwaRoot, "manifest.webmanifest");
const productIconPath = resolve(pwaRoot, "icons", "trouve-512.png");
const vectorIconPath = resolve(pwaRoot, "icons", "trouve.svg");

for (const root of [desktopRoot, pwaRoot]) {
  const sourceMaps = artifactFiles(root).filter((path) => path.endsWith(".map"));
  if (sourceMaps.length > 0) {
    throw new Error(
      `production artifact must not contain source maps: ${sourceMaps.join(", ")}`,
    );
  }
}

if (desktop.includes("serviceWorker.register")) {
  throw new Error("desktop bundle must not register the PWA service worker");
}
if (existsSync(resolve(desktopRoot, "service-worker.js"))) {
  throw new Error("desktop artifact must not contain a PWA service worker");
}
if (existsSync(resolve(desktopRoot, "pwa-meta.json"))) {
  throw new Error("desktop artifact must not contain PWA release metadata");
}
if (!pwa.includes("serviceWorker.register")) {
  throw new Error("PWA bundle is missing service-worker registration");
}
if (!existsSync(serviceWorkerPath)) {
  throw new Error("PWA artifact is missing service-worker.js");
}
if (!existsSync(metadataPath)) {
  throw new Error("PWA artifact is missing pwa-meta.json");
}
if (!existsSync(vectorIconPath)) {
  throw new Error("PWA artifact is missing icons/trouve.svg");
}

const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
if (
  manifest.id !== "/" ||
  manifest.start_url !== "/" ||
  manifest.scope !== "/" ||
  manifest.display !== "standalone" ||
  !Array.isArray(manifest.icons) ||
  !manifest.icons.some(
    (icon) =>
      icon.src === "/icons/trouve-512.png" &&
      icon.sizes === "512x512" &&
      icon.type === "image/png" &&
      String(icon.purpose).includes("maskable"),
  )
) {
  throw new Error("PWA manifest scope, display, or install icon is invalid");
}
const productIcon = readFileSync(productIconPath);
if (
  productIcon.subarray(1, 4).toString("ascii") !== "PNG" ||
  productIcon.readUInt32BE(16) !== 512 ||
  productIcon.readUInt32BE(20) !== 512
) {
  throw new Error("PWA product icon must be a real 512x512 PNG");
}

const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
if (
  metadata.deployment !== "pwa" ||
  typeof metadata.frontend_version !== "string" ||
  metadata.frontend_version.length === 0 ||
  !/^[0-9a-f]{7,64}$/.test(metadata.source_revision)
) {
  throw new Error("PWA release metadata is incomplete or malformed");
}

const serviceWorker = readFileSync(serviceWorkerPath, "utf8");
const expectedCache = `trouve-static-${metadata.frontend_version}-${metadata.source_revision}`;
if (!serviceWorker.includes(expectedCache)) {
  throw new Error("PWA service worker cache is not tied to the release identity");
}
if (!serviceWorker.includes("activate-update")) {
  throw new Error("PWA update activation must remain explicitly user-triggered");
}
if (!serviceWorker.includes("trouve-static-")) {
  throw new Error("PWA cache cleanup must remain scoped to trouve static caches");
}
if (serviceWorker.includes("caches.match(")) {
  throw new Error("PWA fallback must read only from its release-specific static cache");
}
if (!serviceWorker.includes("/icons/trouve-512.png")) {
  throw new Error("PWA install icon must be available in the offline shell");
}

console.log(
  `desktop/PWA boundary and controlled update verified (${metadata.frontend_version} ${metadata.source_revision})`,
);
