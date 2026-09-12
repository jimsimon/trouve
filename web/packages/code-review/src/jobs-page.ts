import { nothing, type TemplateResult } from "lit";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import { Clock, errorMessage, LatestRequest, ReviewElement, targetValue } from "./element";
import "./job-detail";
import { dispatchNavigate, type Section } from "./route";
import { selectValue } from "./select-value";
import { jobRow, pageHeader } from "./shared-views";
import { html } from "./template";
import type { Dashboard, ReviewJob } from "./types";

const STATUS_FILTERS = ["running", "queued", "succeeded", "failed", "cancelled", "stale"];

/** The `#/jobs` screen: filterable history plus the optional detail pane. */
export class JobsPage extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    dashboard: { attribute: false },
    selectedId: { attribute: false },
    onChanged: { attribute: false },
    status: { state: true },
    repository: { state: true },
    jobs: { state: true },
    error: { state: true },
  };

  api!: ReviewApi;
  dashboard!: Dashboard;
  selectedId = "";
  onChanged: () => void = () => {};

  private status = "";
  private repository = "";
  private jobs: ReviewJob[] = [];
  private error = "";

  private readonly clock = new Clock(this);
  private readonly loadEffect = this.effect();
  /** Filter changes and detail-pane refreshes overlap; only the newest load publishes. */
  private readonly jobsRequest = new LatestRequest();

  private readonly navigate = (section: Section, id = ""): void =>
    dispatchNavigate(this, section, id);

  protected override willUpdate(): void {
    if (!this.hasUpdated) this.jobs = this.dashboard.jobs;
  }

  protected override updated(): void {
    this.clock.sync(this.jobs.some((job) => job.status === "running"));
    const { status, repository } = this;
    this.loadEffect.run([status, repository, this.dashboard.jobs], () => {
      void this.load(status, repository);
    });
  }

  private async load(status: string, repository: string): Promise<void> {
    const isCurrent = this.jobsRequest.begin();
    try {
      const { jobs } = await this.api.getJobs(status, repository);
      if (!isCurrent()) return;
      this.jobs = jobs;
      this.error = "";
    } catch (cause) {
      if (!isCurrent()) return;
      this.error = errorMessage(cause);
    }
  }

  protected override render(): TemplateResult {
    const dashboard = this.dashboard;
    const selectedId = this.selectedId;
    const { status, repository } = this;
    const now = this.clock.now;
    return html`${pageHeader({
        eyebrow: "History",
        title: "Review jobs",
        description:
          "Running jobs first, then pending in execution order, then recent terminal jobs.",
      })}
      <div class=${selectedId ? "jobs-layout detail-open" : "jobs-layout"}>
        <section class="panel jobs-index">
          <div class="filters">
            <label>
              Status
              <select
                @change=${(event: Event) => {
                  this.status = targetValue(event);
                }}
              >
                <option value="">All statuses</option>
                ${STATUS_FILTERS.map((value) => html`<option value=${value}>${value}</option>`)}
                ${selectValue(this.status)}
              </select>
            </label>
            <label>
              Repository
              <select
                @change=${(event: Event) => {
                  this.repository = targetValue(event);
                }}
              >
                <option value="">All repositories</option>
                ${dashboard.repositories.map(
                  (repo) => html`<option value=${repo.repository}>${repo.repository}</option>`,
                )}
                ${selectValue(this.repository)}
              </select>
            </label>
          </div>
          ${this.error ? html`<p class="error-text">${this.error}</p>` : nothing}
          <div class="job-list">
            ${repeat(this.jobs, (job) => job.id, (job) => jobRow(job, now, this.navigate))}
          </div>
        </section>
        ${selectedId
          ? html`<trouve-code-review-job-detail
              .api=${this.api}
              .jobId=${selectedId}
              .finalEditorRetryable=${(dashboard.final_editor_retryable_job_ids ?? []).includes(selectedId)}
              .onClose=${() => this.navigate("jobs")}
              .onChanged=${() => {
                void this.load(status, repository);
                this.onChanged();
              }}
            ></trouve-code-review-job-detail>`
          : nothing}
      </div>`;
  }
}

customElements.define("trouve-code-review-jobs", JobsPage);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-jobs": JobsPage;
  }
}
