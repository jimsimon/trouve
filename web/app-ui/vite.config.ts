import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vitest/config";
import type { Plugin } from "vite";

function isLoopbackHostname(hostname: string): boolean {
  if (hostname === "localhost" || hostname === "[::1]") {
    return true;
  }
  const octets = hostname.split(".");
  return octets.length === 4
    && octets.every((octet) => /^\d{1,3}$/.test(octet) && Number(octet) <= 255)
    && Number(octets[0]) === 127;
}

const desktopDevelopmentServer = (() => {
  const configured = process.env["TROUVE_APP_UI_DEV_URL"] ?? "http://127.0.0.1:5173";
  const url = new URL(configured);
  if (
    url.protocol !== "http:"
    || !isLoopbackHostname(url.hostname)
    || url.username !== ""
    || url.password !== ""
    || url.pathname !== "/"
    || url.search !== ""
    || url.hash !== ""
  ) {
    throw new Error(
      "TROUVE_APP_UI_DEV_URL must be a credential-free loopback HTTP origin",
    );
  }
  const port = Number(url.port || "80");
  if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) {
    throw new Error("TROUVE_APP_UI_DEV_URL must contain a valid TCP port");
  }
  return {
    host: url.hostname === "[::1]" ? "::1" : url.hostname,
    port,
  };
})();

export default defineConfig(({ mode }) => {
  const packageJson = JSON.parse(
    readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf8"),
  ) as { version: string };
  const sourceRevision =
    process.env["GITHUB_SHA"]?.slice(0, 12) ??
    execFileSync("git", ["rev-parse", "--short=12", "HEAD"], {
      cwd: fileURLToPath(new URL("../..", import.meta.url)),
      encoding: "utf8",
    }).trim();
  const pwaCacheName = `trouve-static-${packageJson.version}-${sourceRevision}`;
  const input: Record<string, string> = {
    app: fileURLToPath(new URL("./index.html", import.meta.url)),
    gallery: fileURLToPath(new URL("./gallery.html", import.meta.url)),
  };
  if (mode === "pwa") {
    input["service-worker"] = fileURLToPath(
      new URL("./src/pwa/service-worker.ts", import.meta.url),
    );
  }
  const iconSource = readFileSync(
    fileURLToPath(
      new URL("../../crates/trouve-app/assets/trouve.png", import.meta.url),
    ),
  );
  const productIconPlugin: Plugin = {
    name: "trouve-shared-product-icon",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "icons/trouve-512.png",
        source: iconSource,
      });
    },
  };
  const pwaMetadataPlugin: Plugin = {
    name: "trouve-pwa-metadata",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "pwa-meta.json",
        source: `${JSON.stringify(
          {
            frontend_version: packageJson.version,
            source_revision: sourceRevision,
            deployment: "pwa",
          },
          null,
          2,
        )}\n`,
      });
    },
  };
  return {
    resolve: {
      alias: {
        // This package publishes a DOM-free `worker` entry, but Vite's shared
        // browser condition otherwise selects index.dom.js for worker graphs.
        // The DOM build calls document.createElement at module evaluation and
        // terminates a DedicatedWorker before it can accept any requests.
        "decode-named-character-reference": fileURLToPath(
          new URL(
            "./node_modules/decode-named-character-reference/index.js",
            import.meta.url,
          ),
        ),
      },
    },
    define: {
      __TROUVE_FRONTEND_VERSION__: JSON.stringify(packageJson.version),
      __TROUVE_SOURCE_REVISION__: JSON.stringify(sourceRevision),
      __TROUVE_PWA_CACHE_NAME__: JSON.stringify(pwaCacheName),
    },
    plugins: [productIconPlugin, ...(mode === "pwa" ? [pwaMetadataPlugin] : [])],
    server: {
      host: desktopDevelopmentServer.host,
      port: desktopDevelopmentServer.port,
      strictPort: true,
      // The desktop gateway proxies Vite's HTTP modules so host capabilities
      // and `/v1` remain same-origin. The HMR socket connects directly to this
      // exact loopback server and is allowlisted by the gateway's dev CSP.
      hmr: {
        protocol: "ws",
        host: desktopDevelopmentServer.host,
        port: desktopDevelopmentServer.port,
        clientPort: desktopDevelopmentServer.port,
      },
    },
    build: {
      outDir: mode === "pwa" ? "dist/pwa" : "dist/desktop",
      target: "es2022",
      sourcemap: false,
      rollupOptions: {
        input,
        output: {
          entryFileNames: (chunk) =>
            chunk.name === "service-worker"
              ? "service-worker.js"
              : "assets/[name]-[hash].js",
          chunkFileNames: "assets/[name]-[hash].js",
          assetFileNames: "assets/[name]-[hash][extname]",
        },
      },
    },
    test: {
      environment: "node",
      include: ["src/**/*.test.ts"],
    },
  };
});
