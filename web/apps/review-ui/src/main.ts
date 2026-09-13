import "@trouve-ai/ui-foundation/styles/themes.css";
import "@trouve-ai/ui-foundation/styles/tokens.css";
import "@trouve-ai/ui-foundation/styles/base.css";
import "@trouve-ai/transcript/styles/transcript.css";
import "@trouve-ai/code-review/styles.css";
import "./styles/site.css";

import { configureContentWorker } from "@trouve-ai/content-rendering/content-worker-client";
import {
  hashForRoute,
  NAVIGATE_EVENT,
  routeFromHash,
  THEME_CHANGE_EVENT,
  type NavigateEvent,
  type ThemeChangeEvent,
} from "@trouve-ai/code-review/app";
import { observeSignal } from "@trouve-ai/ui-foundation/reactivity";
import { createBrowserThemeController } from "@trouve-ai/ui-foundation/theme-controller";

// The self-hosted review site: the shared dashboard element served
// same-origin behind nginx, with the same `#/section[/id]` hash routes the
// Preact application used, so existing bookmarks and links keep working.
const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("missing #app");

// Markdown, diffs, and highlighting for task transcripts render off-thread
// through the same bounded worker protocol the desktop uses.
configureContentWorker(
  () => new Worker(new URL("./content-worker.ts", import.meta.url), { type: "module", name: "trouve-content" }),
);

// The site owns its theme the way the desktop shell does: the same
// controller, the same `data-theme` themes, persisted in this browser.
const themes = createBrowserThemeController(true);
const applyTheme = (theme: string): void => {
  document.documentElement.dataset["theme"] = theme;
  document.documentElement.style.colorScheme = theme.includes("light") ? "light" : "dark";
};
applyTheme(themes.theme.get());
observeSignal(themes.theme, applyTheme);

const app = document.createElement("trouve-code-review-app");
app.route = routeFromHash(window.location.hash);
app.themePreference = themes.preference.get();
observeSignal(themes.preference, (preference) => {
  app.themePreference = preference;
});
app.addEventListener(THEME_CHANGE_EVENT, (event: Event) => {
  themes.setPreference((event as ThemeChangeEvent).detail.preference);
});
app.addEventListener(NAVIGATE_EVENT, (event: NavigateEvent) => {
  window.location.hash = hashForRoute(event.detail.section, event.detail.id);
});
window.addEventListener("hashchange", () => {
  app.route = routeFromHash(window.location.hash);
});
if (!window.location.hash) window.location.hash = hashForRoute("overview");
root.append(app);
