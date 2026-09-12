import { nothing, type TemplateResult } from "lit";
import { repeat } from "lit/directives/repeat.js";

import { Clock, ReviewElement } from "./element";
import { dispatchNavigate, type Section } from "./route";
import { emptyState, jobRow, pageHeader, panelTitle } from "./shared-views";
import { html } from "./template";
import type { Dashboard, ReviewJob } from "./types";

/** The `#/overview` screen. Rendered inside the app shell's `<section>`. */
export class OverviewPage extends ReviewElement {
  static override properties = {
    dashboard: { attribute: false },
  };

  dashboard!: Dashboard;

  private readonly clock = new Clock(this);

  private activeJobs(): ReviewJob[] {
    return this.dashboard.jobs.filter(
      (job) => job.status === "running" || job.status === "queued",
    );
  }

  protected override updated(): void {
    this.clock.sync(this.activeJobs().some((job) => job.status === "running"));
  }

  private readonly navigate = (section: Section, id = ""): void =>
    dispatchNavigate(this, section, id);

  protected override render(): TemplateResult {
    const dashboard = this.dashboard;
    const counts = dashboard.jobs.reduce<Record<string, number>>((result, job) => {
      result[job.status] = (result[job.status] ?? 0) + 1;
      return result;
    }, {});
    const active = this.activeJobs();
    const now = this.clock.now;
    const recent = dashboard.jobs.filter((job) => !active.includes(job)).slice(0, 8);
    const metrics: Array<[string, number, string]> = [
      ["Running", counts["running"] ?? 0, "blue"],
      ["Pending", counts["queued"] ?? 0, "amber"],
      ["Succeeded", counts["succeeded"] ?? 0, "green"],
      ["Failed", counts["failed"] ?? 0, "red"],
    ];
    return html`${pageHeader({
        eyebrow: "Operations",
        title: "Review overview",
        description: "Live review activity, queue health, and recent outcomes.",
      })}
      <div class="metric-grid">
        ${metrics.map(
          ([label, value, color]) => html`<article class="metric ${color}">
            <span>${label}</span>
            <strong>${value}</strong>
            <small>in current history</small>
          </article>`,
        )}
      </div>
      <div class="overview-grid">
        <section class="panel">
          ${panelTitle("Active reviews", `${active.length} jobs in flight`)}
          ${active.length
            ? html`<div class="job-list compact-list">
                ${repeat(active, (job) => job.id, (job) => jobRow(job, now, this.navigate))}
              </div>`
            : emptyState("Queue is clear", "New review jobs will appear here immediately.")}
        </section>
        <section class="panel">
          ${panelTitle("Recent outcomes", "Most recently completed")}
          <div class="job-list compact-list">
            ${repeat(recent, (job) => job.id, (job) => jobRow(job, now, this.navigate))}
          </div>
        </section>
      </div>
      ${dashboard.app.last_error
        ? html`<div class="banner error" role="status">
            <strong>Last reconciliation error:</strong> ${dashboard.app.last_error}
          </div>`
        : nothing}`;
  }
}

customElements.define("trouve-code-review-overview", OverviewPage);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-overview": OverviewPage;
  }
}
