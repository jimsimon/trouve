import { nothing, type PropertyValues, type TemplateResult } from "lit";

import { fontAwesomeIcon } from "@trouve-ai/ui-foundation/font-awesome-icon";
import {
  isThemePreference,
  THEME_NAMES,
  type ThemePreference,
} from "@trouve-ai/ui-foundation/theme-controller";

import packageMetadata from "../package.json";
import { createReviewApi, type ReviewApi } from "./api";
import { errorMessage, ReviewElement } from "./element";
import "./jobs-page";
import "./overview-page";
import {
  AUTOMATIC_RETRY_MS,
  DASHBOARD_FALLBACK_REFRESH_MS,
  SERVER_EVENT_REFRESH_DEBOUNCE_MS,
} from "./presentation";
import "./repositories-page";
import { reviewersPage } from "./reviewers-page";
import { dispatchNavigate, sections, type Route, type Section } from "./route";
import { settingsPage } from "./settings-page";
import "./stats-page";
import { html } from "./template";
import type {
  CodeReviewSettings,
  Dashboard,
  Model,
  PersonaInfo,
  ProvidersResponse,
} from "./types";

export {
  hashForRoute,
  isSection,
  NAVIGATE_EVENT,
  routeFromHash,
  sections,
  type NavigateDetail,
  type NavigateEvent,
  type Route,
  type Section,
} from "./route";

export const THEME_CHANGE_EVENT = "trouve-code-review-theme-change";

export type ThemeChangeEvent = CustomEvent<{ readonly preference: ThemePreference }>;

/**
 * The code review dashboard. Hosts mount it, hand it the current route and
 * either a review API client or a `base-url`, and listen for
 * `trouve-code-review-navigate` events to move between routes.
 *
 * The dashboard never applies a theme itself: it is drawn from the
 * `--trouve-*` tokens of whichever `data-theme` its host set. A self-hosted
 * shell that owns the theme passes `themePreference` to show the picker and
 * listens for `trouve-code-review-theme-change`; a host whose own settings
 * own appearance leaves it unset.
 */
export class CodeReviewApp extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    baseUrl: { attribute: "base-url" },
    route: { attribute: false },
    themePreference: { attribute: false },
    dashboard: { state: true },
    providers: { state: true },
    reviewSettings: { state: true },
    models: { state: true },
    personaInfos: { state: true },
    dashboardError: { state: true },
    configurationError: { state: true },
    loading: { state: true },
    serverEventAfter: { state: true },
  };

  /** Explicit client; when omitted one is built from `baseUrl`. */
  api: ReviewApi | undefined = undefined;
  baseUrl = "";
  route: Route = { section: "overview", jobId: "" };
  /** Current theme preference when this shell owns theming; hides the picker when unset. */
  themePreference: ThemePreference | undefined = undefined;

  private dashboard: Dashboard | null = null;
  private providers: ProvidersResponse | null = null;
  private reviewSettings: CodeReviewSettings | null = null;
  private models: Model[] = [];
  private personaInfos: PersonaInfo[] = [];
  private dashboardError = "";
  private configurationError = "";
  private loading = true;
  private serverEventAfter: number | null = null;

  private derivedApi: ReviewApi | undefined;
  private serverEventCursor = 0;
  private readonly dashboardLoad: { promise: Promise<void> | null; reloadRequested: boolean } = {
    promise: null,
    reloadRequested: false,
  };
  private configurationLoad: Promise<void> | null = null;

  private readonly mountEffect = this.effect();
  private readonly configurationEffect = this.effect();
  private readonly fallbackRefreshEffect = this.effect();
  private readonly dashboardRetryEffect = this.effect();
  private readonly configurationRetryEffect = this.effect();
  private readonly serverEventsEffect = this.effect();

  private get client(): ReviewApi {
    if (this.api) return this.api;
    this.derivedApi ??= createReviewApi({ baseUrl: this.baseUrl });
    return this.derivedApi;
  }

  protected override willUpdate(changed: PropertyValues<this>): void {
    if (changed.has("baseUrl")) this.derivedApi = undefined;
  }

  private loadDashboard(quiet = false): Promise<void> {
    const state = this.dashboardLoad;
    if (state.promise) {
      state.reloadRequested = true;
      return state.promise;
    }
    const request = (async (): Promise<void> => {
      if (!quiet) this.loading = true;
      try {
        do {
          state.reloadRequested = false;
          try {
            const snapshot = await this.client.getDashboard();
            this.serverEventCursor = Math.max(this.serverEventCursor, snapshot.cursor);
            this.serverEventAfter = this.serverEventAfter ?? snapshot.cursor;
            this.dashboard = snapshot.dashboard;
            this.dashboardError = "";
          } catch (cause) {
            this.dashboardError = errorMessage(cause);
          }
        } while (state.reloadRequested);
      } finally {
        if (!quiet) this.loading = false;
      }
    })();
    state.promise = request;
    void request.finally(() => {
      if (state.promise === request) state.promise = null;
    });
    return request;
  }

  private loadConfiguration(): Promise<void> {
    if (this.configurationLoad) return this.configurationLoad;
    const api = this.client;
    const request = (async (): Promise<void> => {
      const results = await Promise.allSettled([
        api.getProviders().then((providers) => {
          this.providers = providers;
        }),
        api.getReviewSettings().then((settings) => {
          this.reviewSettings = settings;
        }),
        api.getModels().then((models) => {
          this.models = models;
        }),
        api.getPersonaInfos().then((infos) => {
          this.personaInfos = infos;
        }),
      ]);
      const errors = results
        .filter((result): result is PromiseRejectedResult => result.status === "rejected")
        .map(({ reason }) => errorMessage(reason));
      this.configurationError = errors.join("; ");
    })();
    this.configurationLoad = request;
    void request.finally(() => {
      if (this.configurationLoad === request) this.configurationLoad = null;
    });
    return request;
  }

  private get needsConfiguration(): boolean {
    const { section } = this.route;
    return section === "repositories" || section === "reviewers" || section === "settings";
  }

  protected override updated(): void {
    this.mountEffect.run([], () => {
      void this.loadDashboard();
    });
    const needsConfiguration = this.needsConfiguration;
    this.configurationEffect.run([needsConfiguration], () => {
      if (needsConfiguration) void this.loadConfiguration();
    });
    this.fallbackRefreshEffect.run([], () => {
      const timer = window.setInterval(() => {
        if (document.visibilityState === "visible") void this.loadDashboard(true);
      }, DASHBOARD_FALLBACK_REFRESH_MS);
      return () => window.clearInterval(timer);
    });
    const dashboardError = this.dashboardError;
    this.dashboardRetryEffect.run([dashboardError], () => {
      if (!dashboardError) return;
      const timer = window.setInterval(() => {
        if (document.visibilityState === "visible") void this.loadDashboard(true);
      }, AUTOMATIC_RETRY_MS);
      return () => window.clearInterval(timer);
    });
    const configurationError = this.configurationError;
    this.configurationRetryEffect.run([configurationError, needsConfiguration], () => {
      if (!needsConfiguration || !configurationError) return;
      const timer = window.setInterval(() => {
        if (document.visibilityState === "visible") void this.loadConfiguration();
      }, AUTOMATIC_RETRY_MS);
      return () => window.clearInterval(timer);
    });
    const serverEventAfter = this.serverEventAfter;
    this.serverEventsEffect.run([serverEventAfter], () => {
      if (serverEventAfter === null) return;
      let refreshTimer: number | undefined;
      const close = this.client.openServerEvents(serverEventAfter, (event) => {
        if (event.cursor <= this.serverEventCursor) return;
        this.serverEventCursor = event.cursor;
        if (refreshTimer !== undefined) window.clearTimeout(refreshTimer);
        refreshTimer = window.setTimeout(() => {
          refreshTimer = undefined;
          void this.loadDashboard(true);
        }, SERVER_EVENT_REFRESH_DEBOUNCE_MS);
      });
      return () => {
        if (refreshTimer !== undefined) window.clearTimeout(refreshTimer);
        close();
      };
    });
  }

  private readonly refreshDashboard = (): void => {
    void this.loadDashboard(true);
  };

  private readonly refreshEverything = (): void => {
    void this.loadDashboard(true);
    void this.loadConfiguration();
  };

  /**
   * Sidebar links keep their `#/section` hrefs, but a plain left click is
   * turned into a navigate event so the host decides how to apply it.
   */
  private onSectionLinkClick(event: MouseEvent, section: Section): void {
    if (event.defaultPrevented || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    dispatchNavigate(this, section);
  }

  private onThemeChange(event: Event): void {
    const value = (event.currentTarget as HTMLSelectElement).value;
    if (!isThemePreference(value)) return;
    this.dispatchEvent(
      new CustomEvent(THEME_CHANGE_EVENT, {
        bubbles: true,
        composed: true,
        detail: { preference: value },
      }),
    );
  }

  private renderPage(dashboard: Dashboard): TemplateResult | typeof nothing {
    const route = this.route;
    const api = this.client;
    switch (route.section) {
      case "overview":
        return html`<trouve-code-review-overview
          .dashboard=${dashboard}
        ></trouve-code-review-overview>`;
      case "jobs":
        return html`<trouve-code-review-jobs
          .api=${api}
          .dashboard=${dashboard}
          .selectedId=${route.jobId}
          .onChanged=${this.refreshDashboard}
        ></trouve-code-review-jobs>`;
      case "repositories":
        return html`<trouve-code-review-repositories
          .api=${api}
          .dashboard=${dashboard}
          .models=${this.models}
          .onChanged=${this.refreshDashboard}
        ></trouve-code-review-repositories>`;
      case "reviewers":
        return reviewersPage({
          api,
          reviewers: dashboard.reviewers,
          models: this.models,
          defaultModel: this.providers?.default_model,
          onChanged: this.refreshDashboard,
        });
      case "stats":
        return html`<trouve-code-review-stats
          .api=${api}
          .repositories=${dashboard.repositories}
        ></trouve-code-review-stats>`;
      case "settings":
        return settingsPage({
          api,
          app: dashboard.app,
          providers: this.providers,
          reviewSettings: this.reviewSettings,
          models: this.models,
          reviewPersonaInfo: this.personaInfos.find(({ persona }) => persona.id === "review"),
          onChanged: this.refreshEverything,
        });
      default:
        return nothing;
    }
  }

  protected override render(): TemplateResult {
    const dashboard = this.dashboard;
    const route = this.route;
    const error = this.dashboardError || (this.needsConfiguration ? this.configurationError : "");
    const content = dashboard ? html`<section>${this.renderPage(dashboard)}</section>` : nothing;
    return html`<div class="shell">
      <aside class="sidebar">
        <a
          class="brand"
          href="#/overview"
          aria-label="trouve review dashboard"
          @click=${(event: MouseEvent) => this.onSectionLinkClick(event, "overview")}
        >
          <span class="brand-mark">t</span>
          <span>
            <strong>trouve</strong>
            <small>code reviews · v${packageMetadata.version}</small>
          </span>
        </a>
        <nav aria-label="Dashboard sections">
          ${sections.map(
            ({ id, label, icon }) => html`<a
              href="#/${id}"
              class=${route.section === id ? "active" : ""}
              aria-current=${route.section === id ? "page" : nothing}
              @click=${(event: MouseEvent) => this.onSectionLinkClick(event, id)}
            >
              ${fontAwesomeIcon(icon)}<span>${label}</span>${id === "jobs" && dashboard
                ? html`<b>${dashboard.jobs.filter((job) => job.status === "running").length}</b>`
                : nothing}
            </a>`,
          )}
        </nav>
        <div class="sidebar-footer">
          ${this.themePreference
            ? html`<label class="theme-picker">
                ${fontAwesomeIcon("circle-half-stroke", { label: "Theme" })}
                <select aria-label="Theme" .value=${this.themePreference} @change=${this.onThemeChange}>
                  <option value="system">System</option>
                  ${THEME_NAMES.map(
                    (name) => html`<option value=${name}>${name.replaceAll("-", " ")}</option>`,
                  )}
                </select>
              </label>`
            : nothing}
          <div class="sidebar-health">
            <i class=${dashboard?.app.configured ? "online" : ""}></i>
            <span>${dashboard?.app.configured ? "GitHub App online" : "GitHub App not configured"}</span>
          </div>
        </div>
      </aside>
      <main class="content">
        ${error
          ? html`<div class="banner error" role="alert">${error}<span>Retrying automatically.</span></div>`
          : nothing}
        ${this.loading && !dashboard
          ? html`<div class="loading">Loading review operations…</div>`
          : content}
      </main>
    </div>`;
  }
}

customElements.define("trouve-code-review-app", CodeReviewApp);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-app": CodeReviewApp;
  }
}
