import { createSignal, type ReadonlySignal } from "../state/reactivity.js";

export type InspectionPanel = "info" | "terminal" | "diff" | "plan" | "files" | "pr";

export type AppRoute =
  | { readonly kind: "inbox" }
  | { readonly kind: "reviews" }
  | { readonly kind: "automations" }
  | {
      readonly kind: "session";
      readonly workspaceId: string;
      readonly sessionId: string;
      readonly threadId?: string;
      readonly inspection?: InspectionPanel;
    }
  | { readonly kind: "settings"; readonly section?: string }
  | { readonly kind: "not-found"; readonly pathname: string };

const decodeSegment = (value: string): string | undefined => {
  try {
    const decoded = decodeURIComponent(value);
    return decoded.length > 0 ? decoded : undefined;
  } catch {
    return undefined;
  }
};

const isInspectionPanel = (value: string): value is InspectionPanel =>
  value === "info" ||
  value === "terminal" ||
  value === "diff" ||
  value === "plan" ||
  value === "files" ||
  value === "pr";

export const parseRoute = (pathname: string): AppRoute => {
  const parts = pathname.split("/").filter(Boolean);
  if (parts.length === 0 || (parts.length === 1 && parts[0] === "inbox")) {
    return { kind: "inbox" };
  }
  if (parts.length === 1 && parts[0] === "reviews") return { kind: "reviews" };
  if (parts.length === 1 && parts[0] === "automations") {
    return { kind: "automations" };
  }
  if (parts[0] === "settings" && parts.length <= 2) {
    const encodedSection = parts[1];
    if (encodedSection === undefined) return { kind: "settings" };
    const section = decodeSegment(encodedSection);
    return section === undefined
      ? { kind: "not-found", pathname }
      : { kind: "settings", section };
  }
  if (
    parts[0] === "workspaces" &&
    parts[2] === "sessions" &&
    parts.length >= 4
  ) {
    const workspaceId = parts[1] === undefined ? undefined : decodeSegment(parts[1]);
    const sessionId = parts[3] === undefined ? undefined : decodeSegment(parts[3]);
    if (workspaceId === undefined || sessionId === undefined) {
      return { kind: "not-found", pathname };
    }
    let index = 4;
    let threadId: string | undefined;
    let inspection: InspectionPanel | undefined;
    if (parts[index] === "threads") {
      const encodedThreadId = parts[index + 1];
      threadId = encodedThreadId === undefined ? undefined : decodeSegment(encodedThreadId);
      if (threadId === undefined) return { kind: "not-found", pathname };
      index += 2;
    }
    if (parts[index] === "inspect") {
      const panel = parts[index + 1];
      if (panel === "mcp") {
        // MCP details moved into the session overview. Keep old bookmarks and
        // retained routes useful without preserving a second product pane.
        inspection = "info";
      } else if (panel === undefined || !isInspectionPanel(panel)) {
        return { kind: "not-found", pathname };
      } else {
        inspection = panel;
      }
      index += 2;
    }
    if (index !== parts.length) return { kind: "not-found", pathname };
    return {
      kind: "session",
      workspaceId,
      sessionId,
      ...(threadId === undefined ? {} : { threadId }),
      ...(inspection === undefined ? {} : { inspection }),
    };
  }
  return { kind: "not-found", pathname };
};

export const routeHref = (route: Exclude<AppRoute, { kind: "not-found" }>): string => {
  if (route.kind === "inbox") return "/inbox";
  if (route.kind === "reviews") return "/reviews";
  if (route.kind === "automations") return "/automations";
  if (route.kind === "settings") {
    return route.section === undefined
      ? "/settings"
      : `/settings/${encodeURIComponent(route.section)}`;
  }
  let path = `/workspaces/${encodeURIComponent(route.workspaceId)}/sessions/${encodeURIComponent(route.sessionId)}`;
  if (route.threadId !== undefined) path += `/threads/${encodeURIComponent(route.threadId)}`;
  if (route.inspection !== undefined) path += `/inspect/${route.inspection}`;
  return path;
};

export interface RouterPlatform {
  readonly pathname: () => string;
  readonly push: (href: string) => void;
  readonly replace: (href: string) => void;
  readonly listen: (listener: () => void) => () => void;
}

export class AppRouter {
  readonly #platform: RouterPlatform;
  readonly #route = createSignal<AppRoute>({ kind: "inbox" });
  readonly route: ReadonlySignal<AppRoute> = this.#route;
  readonly #stopListening: () => void;
  #lastSettingsSection: string | undefined;

  constructor(platform: RouterPlatform) {
    this.#platform = platform;
    const initialRoute = parseRoute(platform.pathname());
    this.#rememberSettingsSection(initialRoute);
    this.#route.set(initialRoute);
    this.#stopListening = platform.listen(() => {
      const route = parseRoute(platform.pathname());
      this.#rememberSettingsSection(route);
      this.#route.set(route);
    });
  }

  navigate(route: Exclude<AppRoute, { kind: "not-found" }>, replace = false): void {
    const current = this.#route.get();
    const settingsDestination = route.kind === "settings"
      && route.section === undefined
      && this.#lastSettingsSection !== undefined
      ? { kind: "settings" as const, section: this.#lastSettingsSection }
      : route;
    // Inspection is one app-level selection, not a per-thread/session default.
    // Most chat navigation intentionally constructs only the new identity;
    // retain the visible right pane unless the caller explicitly selects one.
    const destination = settingsDestination.kind === "session"
      && settingsDestination.inspection === undefined
      && current.kind === "session"
      && current.inspection !== undefined
      ? { ...settingsDestination, inspection: current.inspection }
      : settingsDestination;
    this.#rememberSettingsSection(destination);
    const href = routeHref(destination);
    if (replace) this.#platform.replace(href);
    else this.#platform.push(href);
    this.#route.set(destination);
  }

  #rememberSettingsSection(route: AppRoute): void {
    if (route.kind === "settings" && route.section !== undefined) {
      this.#lastSettingsSection = route.section;
    }
  }

  dispose(): void {
    this.#stopListening();
  }
}

export const createBrowserRouter = (): AppRouter =>
  new AppRouter({
    pathname: () => globalThis.location.pathname,
    push: (href) => globalThis.history.pushState(null, "", href),
    replace: (href) => globalThis.history.replaceState(null, "", href),
    listen: (listener) => {
      globalThis.addEventListener("popstate", listener);
      return () => globalThis.removeEventListener("popstate", listener);
    },
  });
