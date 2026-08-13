import "@awesome.me/webawesome/dist/styles/themes/default.css";
import "@awesome.me/webawesome/dist/components/button/button.js";

import "./styles/themes.css";
import "./styles/tokens.css";
import "./styles/app.css";
import "./app/trouve-app.js";
import { installNativeTextHistoryShortcuts } from "./services/native-text-history.js";

installNativeTextHistoryShortcuts();

const pwaTarget = import.meta.env.MODE === "pwa";
if (pwaTarget && "serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    void navigator.serviceWorker
      .register("/service-worker.js", {
        scope: "/",
        type: "module",
      })
      .then((registration) => {
        const announce = (): void => {
          if (registration.waiting === null) return;
          window.dispatchEvent(
            new CustomEvent("trouve-pwa-update-ready", {
              detail: {
                activate: () =>
                  registration.waiting?.postMessage({ type: "activate-update" }),
              },
            }),
          );
        };
        announce();
        registration.addEventListener("updatefound", () => {
          const worker = registration.installing;
          worker?.addEventListener("statechange", () => {
            if (worker.state === "installed" && navigator.serviceWorker.controller !== null) {
              announce();
            }
          });
        });
        let reloading = false;
        navigator.serviceWorker.addEventListener("controllerchange", () => {
          if (reloading) return;
          reloading = true;
          globalThis.location.reload();
        });
      })
      .catch(() => {
        // Offline, policy, and restricted-context failures leave the PWA
        // usable for this visit without producing an unhandled rejection.
      });
  });
}
