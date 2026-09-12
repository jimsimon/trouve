import type { FontAwesomeIconName } from "@trouve-ai/ui-foundation/font-awesome-icon";

export type Section = "overview" | "jobs" | "repositories" | "reviewers" | "stats" | "settings";

export interface Route {
  section: Section;
  jobId: string;
}

/** Sidebar sections; icons come from the shared Font Awesome subset. */
export const sections: ReadonlyArray<{ id: Section; label: string; icon: FontAwesomeIconName }> = [
  { id: "overview", label: "Overview", icon: "gauge-high" },
  { id: "jobs", label: "Review jobs", icon: "code-pull-request" },
  { id: "repositories", label: "Repositories", icon: "folder-tree" },
  { id: "reviewers", label: "Reviewer personas", icon: "users" },
  { id: "stats", label: "Statistics", icon: "chart-line" },
  { id: "settings", label: "Settings", icon: "gear" },
];

export function isSection(value: unknown): value is Section {
  return sections.some(({ id }) => id === value);
}

/** Parse a `#/section` or `#/section/id` hash; unknown sections fall back to the overview. */
export function routeFromHash(hash: string): Route {
  const [rawSection, rawId = ""] = hash.replace(/^#\/?/, "").split("/");
  const section = isSection(rawSection) ? rawSection : "overview";
  return { section, jobId: section === "jobs" ? rawId : "" };
}

/** The `location.hash` value (without the leading `#`) for a route. */
export function hashForRoute(section: Section, id = ""): string {
  return `/${section}${id ? `/${id}` : ""}`;
}

export const NAVIGATE_EVENT = "trouve-code-review-navigate";

export interface NavigateDetail {
  section: Section;
  id: string;
}

export type NavigateEvent = CustomEvent<NavigateDetail>;

/**
 * Ask the host shell to move to another dashboard route. The event bubbles so
 * nested screens can request navigation without owning `window.location`.
 */
export function dispatchNavigate(target: EventTarget, section: Section, id = ""): void {
  target.dispatchEvent(
    new CustomEvent<NavigateDetail>(NAVIGATE_EVENT, {
      bubbles: true,
      composed: true,
      detail: { section, id },
    }),
  );
}

declare global {
  interface HTMLElementEventMap {
    "trouve-code-review-navigate": NavigateEvent;
  }
}
