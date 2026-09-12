// Render helpers for the stateless pieces of the dashboard. They return
// exactly the markup the original components produced so the global
// stylesheet's descendant and child selectors keep matching.
import { nothing, type TemplateResult } from "lit";
import { live } from "lit/directives/live.js";

import { fontAwesomeIcon } from "@trouve-ai/ui-foundation/font-awesome-icon";
import { inlineStyle } from "@trouve-ai/ui-foundation/inline-style";

import { targetValue } from "./element";
import {
  changeModelOption,
  modelOptionControls,
  modelOptionSummaries,
  modelOptionTextValue,
  thinkingLevelLabel,
  type ThinkingOptions,
} from "./model-settings";
import {
  duration,
  liveElapsed,
  reviewJobAttentionState,
  type ReviewJobAttentionState,
} from "./presentation";
import type { Section } from "./route";
import { jobStatusClass, safeExternalUrl } from "./security";
import { selectValue } from "./select-value";
import { html } from "./template";
import type { DurationStats, Model, ModelOptions, ReviewJob } from "./types";

export type Navigate = (section: Section, id?: string) => void;

export const statusPill = (status: string): TemplateResult =>
  html`<span class="status ${jobStatusClass(status)}">${status}</span>`;

/** The attention pill replaces the plain status pill for succeeded jobs that still need work. */
export const attentionPill = (
  attentionState: ReviewJobAttentionState,
  status: string,
): TemplateResult =>
  attentionState === "coverage_exhausted"
    ? html`<span class="status warning">full review required</span>`
    : attentionState === "coverage_pending"
      ? html`<span class="status warning">full review pending</span>`
      : attentionState === "open"
        ? html`<span class="status warning">needs attention</span>`
        : attentionState === "unknown"
          ? html`<span class="status warning">status unknown</span>`
          : statusPill(status);

export const progressBar = (job: ReviewJob): TemplateResult => html`<div
  class="progress"
  role="progressbar"
  aria-valuemin="0"
  aria-valuemax="100"
  aria-valuenow=${job.progress.percent}
  aria-label="${job.progress.completed_reviewers} of ${job.progress.total_reviewers} reviewers complete"
><span
    ${inlineStyle(`transform: scaleX(${Math.max(0, Math.min(100, job.progress.percent)) / 100});`)}
  ></span><small>${job.progress.completed_reviewers}/${job.progress.total_reviewers} reviewers ·${" "}${job.progress.percent}%</small></div>`;

export const externalLink = (href: string, children: unknown): TemplateResult | typeof nothing => {
  const safe = safeExternalUrl(href);
  return safe
    ? html`<a href=${safe} target="_blank" rel="noopener noreferrer">${children}${fontAwesomeIcon("arrow-up-right-from-square")}</a>`
    : nothing;
};

export const pageHeader = ({
  eyebrow,
  title,
  description,
  action,
}: {
  eyebrow: string;
  title: string;
  description: string;
  action?: TemplateResult;
}): TemplateResult => html`<header class="page-header">
  <div>
    <p class="eyebrow">${eyebrow}</p>
    <h1>${title}</h1>
    <p>${description}</p>
  </div>
  ${action ? html`<div class="header-actions">${action}</div>` : nothing}
</header>`;

export const panelTitle = (title: string, subtitle?: string): TemplateResult => html`<header
  class="panel-title"
>
  <div>
    <h2>${title}</h2>
    ${subtitle ? html`<p>${subtitle}</p>` : nothing}
  </div>
</header>`;

export const emptyState = (title: string, body: string): TemplateResult => html`<div class="empty">
  <strong>${title}</strong>
  <p>${body}</p>
</div>`;

export function jobRow(job: ReviewJob, now: number, navigate: Navigate): TemplateResult {
  const elapsed = liveElapsed(job.running_elapsed_ms, job.status, job.started_at, now);
  const openIssueCount = job.open_issue_count;
  const attentionState = reviewJobAttentionState(job);
  return html`<button class="job-row" type="button" @click=${() => navigate("jobs", job.id)}>
    ${attentionPill(attentionState, job.status)}
    <span class="job-main">
      <strong>${job.repository} #${job.pull_number}</strong>
      <small>${job.pull_title}</small>
      ${job.status === "running" || job.status === "queued" ? progressBar(job) : nothing}
    </span>
    <span class="job-meta">
      <b>${openIssueCount == null
        ? `Open status unknown · ${job.issue_count} new`
        : `${openIssueCount} blocking · ${job.issue_count} new`}</b>
      <small>${job.status === "queued" ? duration(job.pending_elapsed_ms) : duration(elapsed)}</small>
    </span>
  </button>`;
}

export function thinkingSetting({
  options,
  value,
  onChange,
  inheritLabel,
  disabled = false,
}: {
  options: ThinkingOptions;
  value: string;
  onChange: (value: string) => void;
  inheritLabel?: string;
  disabled?: boolean;
}): TemplateResult {
  if (options.budget) {
    return html`<input
      type="number"
      min=${options.budget.minimum}
      max=${options.budget.maximum ?? ""}
      step="1"
      .value=${live(value)}
      placeholder=${inheritLabel ?? ""}
      ?disabled=${disabled}
      @input=${(event: Event) => onChange(targetValue(event))}
    />`;
  }
  return html`<select
    @change=${(event: Event) => onChange(targetValue(event))}
    ?disabled=${disabled || !options.values.length}
  >
    ${inheritLabel !== undefined ? html`<option value="">${inheritLabel}</option>` : nothing}
    ${options.values.map((level) => html`<option value=${level}>${thinkingLevelLabel(level)}</option>`)}
    ${selectValue(value)}
  </select>`;
}

/** Snapshotted non-thinking model options on a job, one fact per option. */
export function modelOptionFacts(
  label: string,
  options: ModelOptions | undefined,
): TemplateResult[] {
  return modelOptionSummaries(options).map(
    (summary) => html`<div>
      <dt>${label} ${summary.label.toLowerCase()}</dt>
      <dd>${summary.value}</dd>
    </div>`,
  );
}

/** Schema-driven controls for a role's non-thinking model options (for
 * example Codex "fast" mode). Renders nothing when the effective model does
 * not advertise any. Thinking keeps its dedicated `thinkingSetting`. */
export function modelOptionsSetting({
  model,
  options,
  onChange,
  disabled = false,
  scope,
}: {
  model: Model | undefined;
  options: ModelOptions | undefined;
  onChange: (options: ModelOptions) => void;
  disabled?: boolean;
  /** Short prefix for control labels, e.g. "Coordinator". */
  scope?: string;
}): TemplateResult[] {
  const controls = modelOptionControls(model, options);
  const labelFor = (label: string): string =>
    scope ? `${scope} ${label.toLowerCase()}` : label;
  const labelClass = disabled ? "field-disabled" : nothing;
  return controls.map((control) => {
    const description = control.description
      ? html`<small>${control.description}</small>`
      : nothing;
    if (control.kind === "boolean") {
      const value = control.selected === undefined ? "" : String(control.selected);
      const defaultLabel = control.defaultValue === undefined
        ? "Model default"
        : `Model default · ${control.defaultValue ? "On" : "Off"}`;
      return html`<label class=${labelClass}>
        ${labelFor(control.label)}
        <select
          ?disabled=${disabled}
          @change=${(event: Event) => {
            const selected = targetValue(event);
            onChange(changeModelOption(
              options,
              control.key,
              selected === "" ? undefined : selected === "true",
            ));
          }}
        >
          <option value="">${defaultLabel}</option>
          <option value="true">On</option>
          <option value="false">Off</option>
          ${selectValue(value)}
        </select>
        ${description}
      </label>`;
    }
    if (control.kind === "choice") {
      const defaultChoice = control.choices[control.defaultIndex];
      return html`<label class=${labelClass}>
        ${labelFor(control.label)}
        <select
          ?disabled=${disabled}
          @change=${(event: Event) => {
            const index = targetValue(event);
            onChange(changeModelOption(
              options,
              control.key,
              index === "" ? undefined : control.choices[Number(index)]?.value,
            ));
          }}
        >
          <option value="">${defaultChoice
            ? `Model default · ${defaultChoice.label}`
            : "Model default"}</option>
          ${control.choices.map(
            (choice, index) => html`<option value=${String(index)}>${choice.label}</option>`,
          )}
          ${selectValue(control.selectedIndex >= 0 ? String(control.selectedIndex) : "")}
        </select>
        ${description}
      </label>`;
    }
    return html`<label class=${labelClass}>
      ${labelFor(control.label)}
      <input
        type=${control.scalarType === "string" ? "text" : "number"}
        min=${control.minimum ?? nothing}
        max=${control.maximum ?? nothing}
        step=${control.scalarType === "integer" ? "1" : "any"}
        .value=${live(control.text)}
        placeholder=${control.hint}
        ?disabled=${disabled}
        @change=${(event: Event) => {
          const parsed = modelOptionTextValue(control, targetValue(event));
          if (parsed === null) return;
          onChange(changeModelOption(options, control.key, parsed));
        }}
      />
      ${description}
    </label>`;
  });
}

export const metric = (label: string, value: number, color: string): TemplateResult => html`<article
  class="metric ${color}"
>
  <span>${label}</span>
  <strong>${value}</strong>
  <small>selected range</small>
</article>`;

export const durationCard = (title: string, value: DurationStats): TemplateResult => html`<article
  class="panel duration-card"
>
  <span>${title}</span>
  <strong>${duration(value.average_ms)}</strong>
  <dl>
    <div><dt>p50</dt><dd>${duration(value.p50_ms)}</dd></div>
    <div><dt>p95</dt><dd>${duration(value.p95_ms)}</dd></div>
    <div><dt>max</dt><dd>${duration(value.maximum_ms)}</dd></div>
    <div><dt>samples</dt><dd>${value.samples}</dd></div>
  </dl>
</article>`;

export const health = ({
  ok,
  optional,
  label,
  detail,
}: {
  ok: boolean;
  optional?: boolean;
  label: string;
  detail: string;
}): TemplateResult => html`<div>
  <i class=${ok ? "ok" : optional ? "optional" : "bad"}></i>
  <span><strong>${label}</strong><small>${detail}</small></span>
</div>`;
