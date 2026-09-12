import { Chart, registerables } from "chart.js";
import { nothing, type TemplateResult } from "lit";
import { createRef, ref } from "lit/directives/ref.js";
import { repeat } from "lit/directives/repeat.js";

import type { ReviewApi } from "./api";
import { errorMessage, ReviewElement, targetValue } from "./element";
import { duration } from "./presentation";
import { selectValue } from "./select-value";
import { durationCard, metric, pageHeader, panelTitle } from "./shared-views";
import { html } from "./template";
import type { Repository, ReviewStats, StatsRange } from "./types";

Chart.register(...registerables);

const RANGES: StatsRange[] = ["hour", "day", "week", "month", "year", "all"];

const rangeLabel = (value: StatsRange): string =>
  value === "hour"
    ? "1H"
    : value === "day"
      ? "1D"
      : value === "week"
        ? "1W"
        : value === "month"
          ? "1M"
          : value === "year"
            ? "1Y"
            : "All";

export interface ChartDataset {
  label: string;
  data: number[];
  /** A `--trouve-*` color token; resolved against the active theme when drawn. */
  color: `--trouve-${string}`;
}

/** Chart.js paints on a canvas, so theme tokens are resolved to concrete colors here. */
/** Used when a token is missing, e.g. before themes.css has loaded. */
const SERIES_FALLBACK = "#9d9d9d";
const CHART_TOKENS = ["--trouve-text-mid", "--trouve-text-dim", "--trouve-rule"] as const;

const resolveToken = (style: CSSStyleDeclaration, token: string, fallback: string): string =>
  style.getPropertyValue(token).trim() || fallback;

/** Translucent fill under each series; hex tokens get an alpha byte, anything else a color-mix. */
export const chartFill = (color: string): string => {
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/iu.exec(color)?.[1];
  if (!hex) return `color-mix(in srgb, ${color} 13%, transparent)`;
  const full = hex.length === 3 ? [...hex].map((digit) => digit + digit).join("") : hex;
  return `#${full}22`;
};

/** The element carrying `data-theme` for this node: `<html>` on the review site, or a host element when the dashboard is mounted inside another shell. */
const themeHost = (start: Node): Element => {
  let current: Node | null = start;
  while (current) {
    if (current instanceof Element && current.hasAttribute("data-theme")) return current;
    current = current instanceof ShadowRoot ? current.host : current.parentNode;
  }
  return document.documentElement;
};

/** The `#/stats` screen. Rendered inside the app shell's `<section>`. */
export class StatsPage extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    repositories: { attribute: false },
    range: { state: true },
    repository: { state: true },
    stats: { state: true },
    error: { state: true },
  };

  api!: ReviewApi;
  repositories: Repository[] = [];

  private range: StatsRange = "day";
  private repository = "";
  private stats: ReviewStats | null = null;
  private error = "";
  private readonly loadEffect = this.effect();

  protected override updated(): void {
    const range = this.range;
    const repository = this.repository;
    this.loadEffect.run([range, repository], () => {
      let alive = true;
      this.api
        .getStats(range, repository)
        .then((next) => {
          if (alive) {
            this.stats = next;
            this.error = "";
          }
        })
        .catch((cause: unknown) => {
          if (alive) this.error = errorMessage(cause);
        });
      return () => {
        alive = false;
      };
    });
  }

  protected override render(): TemplateResult {
    const stats = this.stats;
    const churn = stats?.churn ?? {
      recurrence_issue_count: 0,
      fix_regression_issue_count: 0,
      previously_missed_issue_count: 0,
      grouped_issue_count: 0,
      external_duplicate_count: 0,
      insufficient_evidence_rejection_count: 0,
      pull_request_count: 0,
      clean_pull_request_count: 0,
      average_rounds_to_clean: 0,
      max_rounds_to_clean: 0,
    };
    return html`${pageHeader({
        eyebrow: "Analytics",
        title: "Review statistics",
        description:
          "Global or per-repository outcomes, queue/run latency, persona/model timing, and issue attribution.",
      })}
      <div class="filters stats-filters">
        <div class="segmented" role="group" aria-label="Statistics range">
          ${RANGES.map(
            (value) => html`<button
              type="button"
              class=${this.range === value ? "active" : ""}
              @click=${() => {
                this.range = value;
              }}
            >
              ${rangeLabel(value)}
            </button>`,
          )}
        </div>
        <label>
          Repository
          <select
            @change=${(event: Event) => {
              this.repository = targetValue(event);
            }}
          >
            <option value="">All repositories</option>
            ${this.repositories.map(
              (repo) => html`<option value=${repo.repository}>${repo.repository}</option>`,
            )}
            ${selectValue(this.repository)}
          </select>
        </label>
      </div>
      ${this.error ? html`<div class="banner error">${this.error}</div>` : nothing}
      ${stats
        ? html`<div class="metric-grid">
              ${metric("Succeeded", stats.status.succeeded, "green")}
              ${metric("Running", stats.status.running, "blue")}
              ${metric("Failed", stats.status.failed, "red")}
              ${metric("Issues found", stats.issue_count, "amber")}
            </div>
            <section class="panel">
              ${panelTitle(
                "Review churn",
                `${churn.clean_pull_request_count} of ${churn.pull_request_count} pull requests reached a clean review in this range.`,
              )}
              <div class="metric-grid">
                ${metric("Recurrences", churn.recurrence_issue_count, "red")}
                ${metric("Fix regressions", churn.fix_regression_issue_count, "red")}
                ${metric("Previously missed", churn.previously_missed_issue_count, "amber")}
                ${metric("Grouped symptoms", churn.grouped_issue_count, "blue")}
                ${metric("External duplicates", churn.external_duplicate_count, "green")}
                ${metric(
                  "Weak evidence rejected",
                  churn.insufficient_evidence_rejection_count,
                  "green",
                )}
                ${metric(
                  "Avg rounds to clean",
                  Math.round(churn.average_rounds_to_clean * 10) / 10,
                  "blue",
                )}
                ${(stats.thread_collapse_backlog?.pending ?? 0) > 0
                  ? metric(
                      `Thread resolve backlog${
                        stats.thread_collapse_backlog?.oldest_pending_minutes != null
                          ? ` (oldest ${stats.thread_collapse_backlog.oldest_pending_minutes}m)`
                          : ""
                      }${
                        (stats.thread_collapse_backlog?.failing ?? 0) > 0
                          ? `, ${stats.thread_collapse_backlog?.failing} failing`
                          : ""
                      }`,
                      stats.thread_collapse_backlog?.pending ?? 0,
                      "amber",
                    )
                  : nothing}
                ${(stats.thread_collapse_backlog?.abandoned ?? 0) > 0
                  ? metric(
                      "Thread resolves abandoned",
                      stats.thread_collapse_backlog?.abandoned ?? 0,
                      "red",
                    )
                  : nothing}
                ${metric("Max rounds to clean", churn.max_rounds_to_clean, "amber")}
              </div>
              ${stats.thread_collapse_backlog?.last_error
                ? html`<p class="warning">Latest thread resolve failure: ${stats.thread_collapse_backlog.last_error}</p>`
                : nothing}
            </section>
            <div class="chart-grid">
              <trouve-code-review-stats-chart
                .heading=${"Review outcomes"}
                .labels=${stats.buckets.map((bucket) =>
                  new Date(bucket.started_at).toLocaleString(),
                )}
                .datasets=${[
                  {
                    label: "Succeeded",
                    data: stats.buckets.map((bucket) => bucket.status.succeeded),
                    color: "--trouve-ok",
                  },
                  {
                    label: "Failed",
                    data: stats.buckets.map((bucket) => bucket.status.failed),
                    color: "--trouve-err",
                  },
                  {
                    label: "Cancelled/stale",
                    data: stats.buckets.map(
                      (bucket) => bucket.status.cancelled + bucket.status.stale,
                    ),
                    color: "--trouve-text-dim",
                  },
                ]}
              ></trouve-code-review-stats-chart>
              <trouve-code-review-stats-chart
                .heading=${"Average latency"}
                .labels=${stats.buckets.map((bucket) =>
                  new Date(bucket.started_at).toLocaleString(),
                )}
                .datasets=${[
                  {
                    label: "Pending",
                    data: stats.buckets.map((bucket) =>
                      Math.round(bucket.pending_average_ms / 1_000),
                    ),
                    color: "--trouve-warn",
                  },
                  {
                    label: "Running",
                    data: stats.buckets.map((bucket) =>
                      Math.round(bucket.running_average_ms / 1_000),
                    ),
                    color: "--trouve-accent",
                  },
                ]}
                .suffix=${"s"}
              ></trouve-code-review-stats-chart>
            </div>
            <div class="stats-summary-grid">
              ${durationCard("Pending duration", stats.pending_duration)}
              ${durationCard("Running duration", stats.running_duration)}
              ${durationCard("Preparation phase", stats.preparation_duration)}
              ${durationCard("Reviewer phase", stats.reviewer_duration)}
              ${durationCard("Coordinator phase", stats.coordinator_duration)}
              ${durationCard("Publication phase", stats.publication_duration)}
            </div>
            <section class="panel table-panel">
              ${panelTitle(
                "Persona and model performance",
                "Batches are per-batch model tasks; outcomes and average durations are rolled up once per persona run. Confirmed issue credits are many-to-many.",
              )}
              <div class="table-scroll">
                <table>
                  <thead>
                    <tr>
                      <th>Persona</th>
                      <th>Actual model</th>
                      <th>Batches</th>
                      <th>Successful runs</th>
                      <th>Failed runs</th>
                      <th>Cancelled runs</th>
                      <th>N/A runs</th>
                      <th>Avg run duration</th>
                      <th>Avg run capacity wait</th>
                      <th>Avg run model/tools</th>
                      <th>Cached input</th>
                      <th>Tool calls</th>
                      <th>Candidates</th>
                      <th>Confirmed issues</th>
                    </tr>
                  </thead>
                  <tbody>
                    ${repeat(
                      stats.personas,
                      (persona) => `${persona.reviewer_id}:${persona.model}`,
                      (persona) => html`<tr>
                        <td>${persona.reviewer_name}</td>
                        <td><code>${persona.model}</code></td>
                        <td>${persona.task_count}</td>
                        <td>${persona.succeeded}</td>
                        <td>${persona.failed}</td>
                        <td>${persona.cancelled}</td>
                        <td>${persona.not_applicable}</td>
                        <td>${duration(persona.duration.average_ms)}</td>
                        <td>${duration(persona.provider_wait_duration.average_ms)}</td>
                        <td>${duration(persona.model_duration.average_ms)}</td>
                        <td>${persona.cached_input_tokens.toLocaleString()}</td>
                        <td>${persona.tool_call_count.toLocaleString()}</td>
                        <td>${persona.candidate_issue_count}</td>
                        <td>${persona.confirmed_issue_count}</td>
                      </tr>`,
                    )}
                  </tbody>
                </table>
              </div>
            </section>`
        : nothing}`;
  }
}

/**
 * One line chart. The chart is rebuilt whenever its data changes and destroyed
 * when the element leaves the document, exactly like the original effect.
 */
export class StatsChart extends ReviewElement {
  static override properties = {
    heading: { attribute: false },
    labels: { attribute: false },
    datasets: { attribute: false },
    suffix: { attribute: false },
  };

  heading = "";
  labels: string[] = [];
  datasets: ChartDataset[] = [];
  suffix = "";

  private readonly canvas = createRef<HTMLCanvasElement>();
  private readonly chartEffect = this.effect();
  private themeObserver: MutationObserver | undefined;

  override connectedCallback(): void {
    super.connectedCallback();
    // Token values change with the host's `data-theme`; the canvas must repaint.
    this.themeObserver = new MutationObserver(() => this.requestUpdate());
    this.themeObserver.observe(themeHost(this), {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
  }

  override disconnectedCallback(): void {
    this.themeObserver?.disconnect();
    this.themeObserver = undefined;
    super.disconnectedCallback();
  }

  protected override updated(): void {
    const { labels, datasets, suffix } = this;
    const style = getComputedStyle(this);
    const palette = {
      legend: resolveToken(style, CHART_TOKENS[0], "#c8c8c8"),
      ticks: resolveToken(style, CHART_TOKENS[1], "#9d9d9d"),
      grid: resolveToken(style, CHART_TOKENS[2], "#33363b"),
      series: datasets.map((dataset) => resolveToken(style, dataset.color, SERIES_FALLBACK)),
    };
    this.chartEffect.run([labels.join("|"), JSON.stringify(datasets), JSON.stringify(palette), suffix], () => {
      const canvas = this.canvas.value;
      if (!canvas) return;
      const chart = new Chart(canvas, {
        type: "line",
        data: {
          labels,
          datasets: datasets.map((dataset, index) => ({
            label: dataset.label,
            data: dataset.data,
            borderColor: palette.series[index],
            backgroundColor: chartFill(palette.series[index] ?? SERIES_FALLBACK),
            fill: true,
            tension: 0.25,
            pointRadius: labels.length > 40 ? 0 : 2,
          })),
        },
        options: {
          // Stats refresh by replacing the chart. Chart.js's default entrance
          // animation repaints the entire canvas for decorative motion only.
          animation: false,
          responsive: true,
          maintainAspectRatio: false,
          interaction: { mode: "index", intersect: false },
          plugins: {
            legend: { labels: { color: palette.legend } },
            tooltip: {
              callbacks: {
                label: (context) =>
                  `${context.dataset.label}: ${context.formattedValue}${suffix}`,
              },
            },
          },
          scales: {
            x: { ticks: { color: palette.ticks, maxTicksLimit: 8 }, grid: { color: palette.grid } },
            y: { beginAtZero: true, ticks: { color: palette.ticks }, grid: { color: palette.grid } },
          },
        },
      });
      return () => chart.destroy();
    });
  }

  protected override render(): TemplateResult {
    const { labels, datasets, suffix } = this;
    return html`<section class="panel chart-card">
      ${panelTitle(this.heading)}
      <div class="canvas-wrap"><canvas ${ref(this.canvas)}></canvas></div>
      <details class="chart-data">
        <summary>View chart data</summary>
        <div class="table-scroll">
          <table>
            <thead><tr><th>Time</th>${datasets.map((set) => html`<th>${set.label}</th>`)}</tr></thead>
            <tbody>
              ${labels.map(
                (label, index) => html`<tr>
                  <td>${label}</td>
                  ${datasets.map((set) => html`<td>${set.data[index]}${suffix}</td>`)}
                </tr>`,
              )}
            </tbody>
          </table>
        </div>
      </details>
    </section>`;
  }
}

customElements.define("trouve-code-review-stats", StatsPage);
customElements.define("trouve-code-review-stats-chart", StatsChart);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-stats": StatsPage;
    "trouve-code-review-stats-chart": StatsChart;
  }
}
