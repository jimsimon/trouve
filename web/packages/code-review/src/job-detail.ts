import { nothing, type TemplateResult } from "lit";
import { keyed } from "lit/directives/keyed.js";
import { createRef, ref } from "lit/directives/ref.js";
import { repeat } from "lit/directives/repeat.js";

import { fontAwesomeIcon } from "@trouve-ai/ui-foundation/font-awesome-icon";

import type { ReviewApi } from "./api";
import "./copy-button";
import { Clock, errorMessage, ReviewElement } from "./element";
import "./output-block";
import {
  AUTOMATIC_RETRY_MS,
  duration,
  formatDate,
  isReviewTaskProgress,
  liveElapsed,
  pickPreferredTask,
  reviewJobAttentionState,
  routingModeLabel,
  taskAttemptLabel,
  taskLifecycleLabel,
} from "./presentation";
import {
  LIVE_OUTPUT_BATCH_MS,
  appendBoundedReviewOutput,
  boundReviewTaskOutput,
  reviewTaskSummary,
  type ReviewOutputField,
} from "./review-output";
import { liveModelElapsed, mergeReviewTaskSnapshot } from "./review-progress";
import { dispatchNavigate, type Section } from "./route";
import { routingReasonLabel } from "./routing-labels";
import {
  attentionPill,
  externalLink,
  modelOptionFacts,
  panelTitle,
  progressBar,
  statusPill,
} from "./shared-views";
import { html } from "./template";
import type {
  Finding,
  JobDetail,
  PersonaResult,
  ReviewJob,
  ReviewTask,
  RoutingReason,
} from "./types";

interface ActivityGroup {
  id: string;
  name: string;
  status: string;
  subtitle: string;
  tasks: ReviewTask[];
  persona?: PersonaResult;
}

const PUBLISHING_NOTICE =
  "This review is still publishing, so it was reconciled instead of retried. Retry again once it finishes.";

/** The sticky detail pane beside the job list: live task activity, findings, and actions. */
export class JobDetailPane extends ReviewElement {
  static override properties = {
    api: { attribute: false },
    jobId: { attribute: false },
    finalEditorRetryable: { attribute: false },
    onClose: { attribute: false },
    onChanged: { attribute: false },
    detail: { state: true },
    error: { state: true },
    busy: { state: true },
    retryStatus: { state: true },
    actionNotice: { state: true },
    selectedTaskId: { state: true },
    taskDetails: { state: true },
    taskLoading: { state: true },
    taskErrors: { state: true },
    eventCursor: { state: true },
    routingOpen: { state: true },
    navigationStatus: { state: true },
  };

  api!: ReviewApi;
  jobId = "";
  finalEditorRetryable = false;
  onClose: () => void = () => {};
  onChanged: () => void = () => {};

  private detail: JobDetail | null = null;
  private error = "";
  private busy = "";
  private retryStatus = "";
  private actionNotice = "";
  private selectedTaskId = "";
  private taskDetails: Record<string, ReviewTask> = {};
  private taskLoading = "";
  private taskErrors: Record<string, string> = {};
  private eventCursor: number | null = null;
  private routingOpen = false;
  private navigationStatus = "";

  private readonly clock = new Clock(this);
  /** The job this pane currently shows; `null` once it moved on or unmounted. */
  private aliveJobId: string | null = null;
  private focusReplacementJobId = "";
  private readonly taskRequests = new Set<string>();
  private readonly jobHeading = createRef<HTMLHeadingElement>();
  private activityGroupButtons: Record<string, HTMLButtonElement | undefined> = {};

  private readonly jobEffect = this.effect();
  private readonly eventsEffect = this.effect();
  private readonly selectTaskEffect = this.effect();
  private readonly loadTaskEffect = this.effect();
  private readonly focusEffect = this.effect();
  private readonly retryTaskEffect = this.effect();
  private readonly retainTaskEffect = this.effect();

  private readonly navigate = (section: Section, id = ""): void =>
    dispatchNavigate(this, section, id);

  private async load(): Promise<JobDetail | undefined> {
    const requestedJobId = this.jobId;
    try {
      const response = await this.api.getJob(requestedJobId);
      const receivedAt = Date.now();
      const currentTaskList = this.detail?.tasks ?? [];
      const currentTasks = new Map<string, ReviewTask>(
        currentTaskList.map((task): [string, ReviewTask] => [task.id, task]),
      );
      const responseTaskIds = new Set(response.tasks.map((task) => task.id));
      const next = {
        ...response,
        tasks: [
          ...response.tasks.map((task) =>
            mergeReviewTaskSnapshot(currentTasks.get(task.id), task, receivedAt),
          ),
          ...currentTaskList.filter((task) => !responseTaskIds.has(task.id)),
        ],
      };
      if (this.aliveJobId === requestedJobId) {
        this.detail = next;
        this.eventCursor = this.eventCursor ?? next.event_cursor ?? 0;
        this.error = "";
        return next;
      }
    } catch (cause) {
      if (this.aliveJobId === requestedJobId) {
        this.error = errorMessage(cause);
      }
    }
    return undefined;
  }

  private async loadTask(taskId: string, jobId = this.jobId): Promise<void> {
    if (this.taskRequests.has(taskId)) return;
    this.taskRequests.add(taskId);
    this.taskLoading = taskId;
    if (taskId in this.taskErrors) {
      const next = { ...this.taskErrors };
      delete next[taskId];
      this.taskErrors = next;
    }
    try {
      const response = await this.api.getTask(jobId, taskId);
      const receivedAt = Date.now();
      const currentTask =
        this.taskDetails[taskId] ?? this.detail?.tasks.find((task) => task.id === taskId);
      const next = boundReviewTaskOutput(
        mergeReviewTaskSnapshot(currentTask, response, receivedAt),
      );
      if (
        this.aliveJobId === jobId &&
        next.job_id === jobId &&
        this.selectedTaskId === taskId
      ) {
        this.taskDetails = { [taskId]: next };
      }
    } catch (cause) {
      if (this.aliveJobId === jobId) {
        this.taskErrors = { ...this.taskErrors, [taskId]: errorMessage(cause) };
      }
    } finally {
      this.taskRequests.delete(taskId);
      if (this.taskLoading === taskId) this.taskLoading = "";
    }
  }

  protected override updated(): void {
    // State as this render saw it. Effects below may schedule newer values,
    // which the following update observes, exactly like the Preact hook
    // closures; functional updaters read `this.*` for the pending value.
    const { jobId, detail, selectedTaskId, taskDetails, taskErrors, eventCursor } = this;
    this.clock.sync(detail?.job.status === "running");

    this.jobEffect.run([jobId], () => {
      this.aliveJobId = jobId;
      this.detail = null;
      this.selectedTaskId = "";
      this.taskDetails = {};
      this.taskLoading = "";
      this.taskErrors = {};
      this.eventCursor = null;
      this.routingOpen = false;
      this.busy = "";
      this.retryStatus = "";
      this.actionNotice = "";
      this.activityGroupButtons = {};
      this.taskRequests.clear();
      void this.load();
      return () => {
        if (this.aliveJobId === jobId) this.aliveJobId = null;
      };
    });

    this.eventsEffect.run([jobId, eventCursor], () =>
      this.subscribeToJobEvents(jobId, eventCursor),
    );

    this.selectTaskEffect.run([detail?.tasks], () => {
      if (!detail?.tasks.length) {
        this.selectedTaskId = "";
        return;
      }
      const current = this.selectedTaskId;
      if (!detail.tasks.some((task) => task.id === current)) {
        this.selectedTaskId = pickPreferredTask(detail.tasks)?.id ?? "";
      }
    });

    this.loadTaskEffect.run([selectedTaskId, taskDetails, taskErrors], () => {
      if (!selectedTaskId || taskDetails[selectedTaskId] || taskErrors[selectedTaskId]) return;
      void this.loadTask(selectedTaskId, jobId);
    });

    this.focusEffect.run([detail?.job.id], () => {
      if (detail?.job.id !== this.focusReplacementJobId) return;
      this.jobHeading.value?.focus();
      this.focusReplacementJobId = "";
    });

    const selectedTaskError = selectedTaskId ? taskErrors[selectedTaskId] : undefined;
    this.retryTaskEffect.run([selectedTaskError, selectedTaskId], () => {
      if (!selectedTaskId || !selectedTaskError) return;
      const timer = window.setInterval(() => {
        if (document.visibilityState === "visible") void this.loadTask(selectedTaskId, jobId);
      }, AUTOMATIC_RETRY_MS);
      return () => window.clearInterval(timer);
    });

    this.retainTaskEffect.run([selectedTaskId], () => {
      const current = this.taskDetails;
      const retained = selectedTaskId ? current[selectedTaskId] : undefined;
      this.taskDetails = retained ? { [selectedTaskId]: retained } : {};
    });
  }

  private subscribeToJobEvents(
    jobId: string,
    eventCursor: number | null,
  ): (() => void) | undefined {
    if (eventCursor === null) return undefined;
    type PendingOutput = Partial<Record<ReviewOutputField, string>>;
    const pendingOutput = new Map<string, PendingOutput>();
    let outputTimer: number | undefined;
    let detailReloadTimer: number | undefined;
    let missedHiddenOutput = false;

    const scheduleDetailReload = (): void => {
      if (detailReloadTimer !== undefined) window.clearTimeout(detailReloadTimer);
      detailReloadTimer = window.setTimeout(() => {
        detailReloadTimer = undefined;
        void this.load();
      }, 150);
    };

    const flushOutput = (): void => {
      outputTimer = undefined;
      if (document.visibilityState !== "visible") {
        missedHiddenOutput ||= pendingOutput.size > 0;
        pendingOutput.clear();
        return;
      }
      const patches = new Map(pendingOutput);
      pendingOutput.clear();
      if (!patches.size) return;
      const current = this.taskDetails;
      let next = current;
      for (const [taskId, streams] of patches) {
        const task = next[taskId];
        if (!task) continue;
        let updated = task;
        for (const [field, text] of Object.entries(streams) as Array<
          [ReviewOutputField, string]
        >) {
          updated = {
            ...updated,
            [field]: appendBoundedReviewOutput(updated[field], text),
          };
        }
        if (updated !== task) {
          if (next === current) next = { ...current };
          next[taskId] = updated;
        }
      }
      this.taskDetails = next;
    };

    const scheduleOutputFlush = (): void => {
      if (outputTimer !== undefined || document.visibilityState !== "visible") return;
      outputTimer = window.setTimeout(flushOutput, LIVE_OUTPUT_BATCH_MS);
    };

    const syncVisibility = (): void => {
      if (document.visibilityState !== "visible") {
        if (outputTimer !== undefined) window.clearTimeout(outputTimer);
        outputTimer = undefined;
        missedHiddenOutput ||= pendingOutput.size > 0;
        pendingOutput.clear();
        return;
      }
      if (!missedHiddenOutput) return;
      missedHiddenOutput = false;
      const taskId = this.selectedTaskId;
      this.taskDetails = {};
      void this.load();
      if (taskId) void this.loadTask(taskId, jobId);
    };

    document.addEventListener("visibilitychange", syncVisibility);
    const close = this.api.openJobEvents(jobId, eventCursor, (event) => {
      if (this.aliveJobId !== jobId) return;
      if (event.type === "code_review.output_delta" && event.task_id && event.text) {
        if (event.task_id !== this.selectedTaskId) return;
        if (document.visibilityState !== "visible") {
          missedHiddenOutput = true;
          return;
        }
        if (!this.taskDetails[event.task_id]) {
          void this.loadTask(event.task_id, jobId);
          return;
        }
        const field: ReviewOutputField =
          event.stream === "thinking"
            ? "thinking"
            : event.stream === "tool"
              ? "tool_output"
              : "output";
        const streams = pendingOutput.get(event.task_id) ?? {};
        streams[field] = appendBoundedReviewOutput(streams[field] ?? "", event.text);
        pendingOutput.set(event.task_id, streams);
        scheduleOutputFlush();
      } else if (
        event.type === "code_review.routing_updated" &&
        event.routing_decisions
      ) {
        const routingDecisions = event.routing_decisions;
        const current = this.detail;
        this.detail = current ? { ...current, routing_decisions: routingDecisions } : current;
      } else if (
        event.type === "code_review.task_progress_updated" &&
        event.task_id &&
        isReviewTaskProgress(event.progress)
      ) {
        const taskId = event.task_id;
        const progress = event.progress;
        const receivedAt = Date.now();
        const mergeProgress = (task: ReviewTask): ReviewTask =>
          mergeReviewTaskSnapshot(task, { ...task, ...progress }, receivedAt);
        const current = this.detail;
        this.detail = current
          ? {
              ...current,
              tasks: current.tasks.map((task) =>
                task.id === taskId ? mergeProgress(task) : task,
              ),
            }
          : current;
        const task = this.taskDetails[taskId];
        if (task) {
          this.taskDetails = { ...this.taskDetails, [taskId]: mergeProgress(task) };
        }
      } else if (event.type === "code_review.task_updated" && event.task) {
        const receivedAt = Date.now();
        const incomingTask = event.task;
        pendingOutput.delete(incomingTask.id);
        if (!pendingOutput.size && outputTimer !== undefined) {
          window.clearTimeout(outputTimer);
          outputTimer = undefined;
        }
        const current = this.detail;
        if (current) {
          const currentTask = current.tasks.find(
            (task) => task.id === incomingTask.id,
          );
          const task = mergeReviewTaskSnapshot(
            currentTask,
            incomingTask,
            receivedAt,
          );
          const summary = reviewTaskSummary(task);
          const exists = current.tasks.some((currentTask) => currentTask.id === task.id);
          this.detail = {
            ...current,
            tasks: exists
              ? current.tasks.map((currentTask) =>
                  currentTask.id === task.id ? summary : currentTask,
                )
              : [...current.tasks, summary],
          };
        }
        if (incomingTask.id === this.selectedTaskId) {
          if (document.visibilityState === "visible") {
            const currentTask =
              this.taskDetails[incomingTask.id] ??
              this.detail?.tasks.find((task) => task.id === incomingTask.id);
            const task = boundReviewTaskOutput(
              mergeReviewTaskSnapshot(currentTask, incomingTask, receivedAt),
            );
            this.taskDetails = { [task.id]: task };
          } else {
            missedHiddenOutput = true;
          }
        }
        scheduleDetailReload();
      } else {
        scheduleDetailReload();
      }
    });
    return () => {
      document.removeEventListener("visibilitychange", syncVisibility);
      if (outputTimer !== undefined) window.clearTimeout(outputTimer);
      if (detailReloadTimer !== undefined) window.clearTimeout(detailReloadTimer);
      close();
    };
  }

  private async act(action: "cancel" | "request" | "retry"): Promise<void> {
    const detail = this.detail;
    if (!detail) return;
    const submittedJobId = detail.job.id;
    this.busy = action;
    this.actionNotice = "";
    try {
      const replacement =
        action === "cancel"
          ? await this.api.cancelJob(submittedJobId)
          : action === "request"
            ? await this.api.requestReview(detail.job)
            : await this.api.retryJob(submittedJobId);
      this.onChanged();
      // The pane may have moved to another job while the request was in
      // flight; its notices belong to that job now.
      if (this.aliveJobId !== submittedJobId) return;
      if (action !== "cancel") {
        if (replacement.id === submittedJobId) {
          // The server refuses to replace a job that is mid-publication; it
          // reconciled the existing review instead, so nothing new opened.
          this.actionNotice = PUBLISHING_NOTICE;
          this.navigationStatus = PUBLISHING_NOTICE;
          await this.load();
        } else {
          this.focusReplacementJobId = replacement.id;
          this.navigationStatus = `Opened replacement review ${replacement.id}.`;
          this.navigate("jobs", replacement.id);
        }
      } else await this.load();
    } catch (cause) {
      if (this.aliveJobId === submittedJobId) {
        this.error = errorMessage(cause);
      }
    } finally {
      if (this.aliveJobId === submittedJobId) this.busy = "";
    }
  }

  private async retryFailedPersona(reviewerId: string): Promise<void> {
    const detail = this.detail;
    if (!detail) return;
    const submittedJobId = detail.job.id;
    const action = `persona:${reviewerId}`;
    const label =
      detail.personas.find((persona) => persona.reviewer_id === reviewerId)?.reviewer_name ||
      "Reviewer persona";
    this.busy = action;
    this.actionNotice = "";
    this.retryStatus = `Retrying full review after ${label}…`;
    let replacement: ReviewJob;
    try {
      replacement = await this.api.retryPersona(submittedJobId, reviewerId);
    } catch (cause) {
      if (this.aliveJobId === submittedJobId) {
        this.error = errorMessage(cause);
        this.retryStatus = `Full review retry after ${label} failed.`;
        this.busy = "";
      }
      return;
    }
    if (this.aliveJobId !== submittedJobId) return;
    this.onChanged();
    if (replacement.id === submittedJobId) {
      // Same server refusal as `act("retry")`: the job is mid-publication.
      this.actionNotice = PUBLISHING_NOTICE;
      this.navigationStatus = PUBLISHING_NOTICE;
      this.retryStatus = `Full review retry after ${label} was reconciled instead.`;
      try {
        await this.load();
      } finally {
        if (this.aliveJobId === submittedJobId) this.busy = "";
      }
    } else {
      this.focusReplacementJobId = replacement.id;
      this.navigationStatus =
        `Opened replacement review ${replacement.id}; all reviewer personas will run again.`;
      this.retryStatus = `Full review retry after ${label} queued.`;
      this.navigate("jobs", replacement.id);
      if (this.aliveJobId === submittedJobId) {
        this.busy = "";
      }
    }
  }

  private async retryFailedFinalEditor(): Promise<void> {
    const detail = this.detail;
    if (!detail) return;
    const submittedJobId = detail.job.id;
    this.busy = "final-editor";
    this.actionNotice = "";
    this.retryStatus = "Retrying Final review editor…";
    try {
      await this.api.retryFinalEditor(submittedJobId);
    } catch (cause) {
      if (this.aliveJobId === submittedJobId) {
        this.error = errorMessage(cause);
        this.retryStatus = "Final review editor retry failed.";
        this.busy = "";
      }
      return;
    }
    if (this.aliveJobId !== submittedJobId) return;
    this.activityGroupButtons["coordinator"]?.focus();
    this.retryStatus = "Final review editor retry queued.";
    this.onChanged();
    try {
      const refreshed = await this.load();
      if (this.aliveJobId === submittedJobId && refreshed) {
        const retriedTask = pickPreferredTask(
          refreshed.tasks.filter((task) => task.role === "coordinator"),
        );
        this.selectedTaskId = retriedTask?.id ?? "";
      }
    } finally {
      if (this.aliveJobId === submittedJobId) this.busy = "";
    }
  }

  private renderFinding(finding: Finding): TemplateResult {
    return html`<article class="finding ${finding.severity}">
      <header>
        <strong>${finding.title}${finding.outside_diff ? " · outside diff" : nothing}</strong>
        ${statusPill(finding.status)}
      </header>
      <small>${finding.path}:${finding.line} · Severity: ${finding.severity.toUpperCase()} · Confidence: ${(finding.confidence ?? "medium").toUpperCase()}${finding.origin && finding.origin !== "new_change" ? ` · ${finding.origin.replaceAll("_", " ").toUpperCase()}` : ""}</small>
      <p>${finding.body}</p>
      ${finding.status === "open" && finding.carried_verdict?.reason
        ? html`<small>Still open at ${finding.carried_verdict.head_sha.slice(0, 12)} (review ${finding.carried_verdict.job_id.slice(0, 12)}): ${finding.carried_verdict.reason}</small>`
        : nothing}${finding.resolved_head && finding.status !== "advisory"
        ? html`<small>Fixed at ${finding.resolved_head.slice(0, 12)} by review ${finding.resolved_by_job_id?.slice(0, 12) || "unknown"}</small>`
        : nothing}${finding.resolved_head && finding.status === "advisory"
        ? html`<small>Promoted to a blocking finding at ${finding.resolved_head.slice(0, 12)} by review ${finding.resolved_by_job_id?.slice(0, 12) || "unknown"}</small>`
        : nothing}${finding.github_publication_status === "suppressed_by_policy"
        ? html`<small>Retained in Trouve · Not posted to GitHub by confidence policy</small>`
        : nothing}${finding.github_publication_status === "grouped_by_theme"
        ? html`<small>Retained in Trouve · Represented by the shared root-cause comment on GitHub</small>`
        : nothing}${finding.thread_collapse?.last_error
        ? html`<small class="warning-text">${finding.thread_collapse.pending
              ? `GitHub thread not resolved yet (${finding.thread_collapse.attempts ?? 0} failed attempt(s)${
                  finding.thread_collapse.next_attempt_at
                    ? `, retrying ${new Date(finding.thread_collapse.next_attempt_at).toLocaleString()}`
                    : ""
                })`
              : "GitHub thread left unresolved after repeated failures"}: ${finding.thread_collapse.last_error}</small>`
        : nothing}
      ${finding.evidence?.execution_path
        ? html`<details>
            <summary>Verification evidence</summary>
            <dl>
              <dt>Preconditions</dt><dd>${finding.evidence.preconditions}</dd>
              <dt>Execution path</dt><dd>${finding.evidence.execution_path}</dd>
              <dt>Consequence</dt><dd>${finding.evidence.consequence}</dd>
              <dt>Introduced by</dt><dd>${finding.evidence.introduction}</dd>
              <dt>Regression test</dt><dd>${finding.evidence.regression_test}</dd>
            </dl>
          </details>`
        : nothing}
      <small>Found by ${finding.sources.map((source) => source.reviewer_name).join(", ") || "legacy review"}</small>
      <div class="action-row">
        <trouve-code-review-copy-button .text=${finding.prompt_for_agents}></trouve-code-review-copy-button>
        ${finding.status !== "advisory"
          ? externalLink(
              finding.github_comment_url,
              finding.github_comment_id == null
                ? "Open review comment"
                : finding.origin === "fix_regression"
                  ? "Open thread reply"
                  : "Open inline comment",
            )
          : nothing}
      </div>
    </article>`;
  }

  private renderRoutingReasons(
    reasons: RoutingReason[] | undefined,
    routingMode: ReviewJob["routing_mode"],
    emptyMessage: string,
  ): TemplateResult {
    return (reasons ?? []).length > 0
      ? html`<ul>
          ${(reasons ?? []).map(
            (reason) => html`<li><b>${routingReasonLabel(reason.source, routingMode)}:</b> ${reason.detail}</li>`,
          )}
        </ul>`
      : html`<p>${emptyMessage}</p>`;
  }

  protected override render(): TemplateResult {
    const detail = this.detail;
    if (!detail) {
      return html`<aside class="panel job-detail">
        <p class="visually-hidden" role="status" aria-live="polite">${this.navigationStatus}</p>
        <button class="icon-button" type="button" @click=${() => this.onClose()} aria-label="Close detail">${fontAwesomeIcon("xmark")}</button>${this.error || "Loading review detail…"}
      </aside>`;
    }
    const now = this.clock.now;
    const busy = this.busy;
    const finalEditorRetryable = this.finalEditorRetryable;
    const job = detail.job;
    const runningElapsed = liveElapsed(
      job.running_elapsed_ms,
      job.status,
      job.started_at,
      now,
    );
    const acceptedCandidateIds = new Set(
      detail.findings.flatMap((finding) =>
        finding.sources.map((source) => source.candidate_id).filter(Boolean),
      ),
    );
    const candidateRejections = detail.candidate_rejections ?? [];
    const unadjudicatedCandidates = detail.unadjudicated_candidates ?? [];
    // Advisory findings are trouve-internal debt: kept out of the main ledger
    // and shown only inside a collapsed section.
    const ledgerFindings = detail.findings.filter((finding) => finding.status !== "advisory");
    const advisoryFindings = detail.findings.filter((finding) => finding.status === "advisory");
    const routingDecisions = detail.routing_decisions ?? [];
    const unrecordedCandidateDecisions = Math.max(
      0,
      job.candidate_issue_count - acceptedCandidateIds.size - candidateRejections.length - unadjudicatedCandidates.length,
    );
    const activityGroups: ActivityGroup[] = [];
    const routerTasks = detail.tasks.filter((task) => task.role === "router");
    if (routerTasks.length) {
      const latestRouterByBatch = new Map<number, ReviewTask>();
      routerTasks.forEach((task) => {
        // Tasks arrive in durable attempt order, so the final entry for a
        // batch is the current attempt even when timestamps collide.
        latestRouterByBatch.set(task.batch_index, task);
      });
      const currentRouterTasks = [...latestRouterByBatch.values()];
      const runningRouter = currentRouterTasks.find((task) => task.status === "running");
      const queuedRouter = currentRouterTasks.find((task) => task.status === "queued");
      const failedRouter = currentRouterTasks.find((task) => task.status === "failed");
      const routerStatus =
        runningRouter?.status ??
        queuedRouter?.status ??
        failedRouter?.status ??
        (currentRouterTasks.every((task) => task.status === "succeeded")
          ? "succeeded"
          : "cancelled");
      const routerElapsed = currentRouterTasks.reduce(
        (sum, task) =>
          sum + liveElapsed(task.elapsed_ms, task.status, task.started_at, now),
        0,
      );
      activityGroups.push({
        id: "router",
        name: "Persona router",
        status: routerStatus,
        subtitle: `${currentRouterTasks.filter((task) => !["queued", "running"].includes(task.status)).length}/${currentRouterTasks.length} batches · ${duration(routerElapsed)}`,
        tasks: routerTasks,
      });
    }
    const analystTasks = detail.tasks.filter((task) => task.role === "analyst");
    const analystTask = analystTasks.at(-1);
    if (analystTask) {
      activityGroups.push({
        id: "analyst",
        name: "Change analyst",
        status: analystTask.status,
        subtitle: `Full-branch analysis · ${duration(
          liveElapsed(analystTask.elapsed_ms, analystTask.status, analystTask.started_at, now),
        )}`,
        tasks: analystTasks,
      });
    }
    activityGroups.push(
      ...detail.personas.map((persona) => ({
        id: `persona:${persona.reviewer_id}`,
        name: persona.reviewer_name,
        status: persona.status,
        subtitle: `${persona.completed_batches}/${persona.total_batches} batches · ${duration(
          liveElapsed(persona.elapsed_ms, persona.status, persona.started_at, now),
        )}`,
        tasks: detail.tasks.filter((task) => task.reviewer_id === persona.reviewer_id),
        persona,
      })),
    );
    const personaReviewerIds = new Set(detail.personas.map((persona) => persona.reviewer_id));
    const unmatchedReviewerTasks = new Map<string, ReviewTask[]>();
    detail.tasks
      .filter(
        (task) =>
          task.role === "reviewer" &&
          (!task.reviewer_id || !personaReviewerIds.has(task.reviewer_id)),
      )
      .forEach((task) => {
        const key = task.reviewer_id || task.reviewer_name || task.id;
        unmatchedReviewerTasks.set(key, [...(unmatchedReviewerTasks.get(key) ?? []), task]);
      });
    unmatchedReviewerTasks.forEach((tasks, reviewerId) => {
      const latestTask = tasks.at(-1);
      if (!latestTask) return;
      const status =
        tasks.find((task) => task.status === "running")?.status ??
        tasks.find((task) => task.status === "failed")?.status ??
        latestTask.status;
      const completed = tasks.filter(
        (task) => !["queued", "running"].includes(task.status),
      ).length;
      const total = Math.max(tasks.length, ...tasks.map((task) => task.batch_count));
      const elapsed = tasks.reduce((sum, task) => {
        if (task.status === "queued") return sum;
        return (
          sum +
          (task.status === "running"
            ? liveElapsed(task.elapsed_ms, task.status, task.started_at, now)
            : task.elapsed_ms)
        );
      }, 0);
      activityGroups.push({
        id: `task-reviewer:${reviewerId}`,
        name: latestTask.reviewer_name || latestTask.reviewer_id || "Reviewer",
        status,
        subtitle: `${completed}/${total} batches · ${duration(elapsed)}`,
        tasks,
      });
    });
    const coordinatorTasks = detail.tasks.filter((task) => task.role === "coordinator");
    const coordinatorTask = coordinatorTasks.at(-1);
    if (coordinatorTask) {
      activityGroups.push({
        id: "coordinator",
        name: "Final review editor",
        status: coordinatorTask.status,
        subtitle: `Final selection · ${duration(
          liveElapsed(
            coordinatorTask.elapsed_ms,
            coordinatorTask.status,
            coordinatorTask.started_at,
            now,
          ),
        )}`,
        tasks: coordinatorTasks,
      });
    }
    const selectedTaskSummary =
      detail.tasks.find((task) => task.id === this.selectedTaskId) ?? detail.tasks[0];
    const retainedTask = selectedTaskSummary ? this.taskDetails[selectedTaskSummary.id] : undefined;
    const selectedTask =
      selectedTaskSummary && retainedTask
        ? {
            ...selectedTaskSummary,
            prompt: retainedTask.prompt,
            output: retainedTask.output,
            thinking: retainedTask.thinking,
            tool_output: retainedTask.tool_output,
          }
        : selectedTaskSummary;
    const selectedGroup = activityGroups.find((group) =>
      group.tasks.some((task) => task.id === selectedTask?.id),
    );
    const selectedRoutingDecision =
      selectedTask?.role === "reviewer"
        ? routingDecisions.find(
            (decision) =>
              decision.reviewer_id === selectedTask.reviewer_id &&
              decision.batch_index === selectedTask.batch_index,
          )
        : undefined;
    const openIssueCount = job.open_issue_count;
    const attentionState = reviewJobAttentionState(job);
    const hasOpenIssues =
      job.status === "succeeded" && openIssueCount != null && openIssueCount > 0;
    const openIssueStatusUnknown = job.status === "succeeded" && openIssueCount == null;
    const selectPreferredTask = (tasks: ReviewTask[]): void => {
      const preferred = pickPreferredTask(tasks);
      if (preferred) this.selectedTaskId = preferred.id;
    };
    const jobActive = job.status === "running" || job.status === "queued";
    return html`<aside class="panel job-detail">
      <p class="visually-hidden" role="status" aria-live="polite">${this.navigationStatus}</p>
      <header class="detail-header">
        <div>
          ${attentionPill(attentionState, job.status)}
          <h2 ${ref(this.jobHeading)} tabindex="-1">${job.repository} #${job.pull_number}</h2>
          <p>${job.pull_title}</p>
        </div>
        <button class="icon-button" type="button" @click=${() => this.onClose()} aria-label="Close detail">${fontAwesomeIcon("xmark")}</button>
      </header>
      ${progressBar(job)}
      <dl class="facts">
        <div>
          <dt>Scope</dt>
          <dd>${job.scope}</dd>
        </div>
        <div>
          <dt>Persona selection mode</dt>
          <dd>${routingModeLabel(job.routing_mode)}</dd>
        </div>
        <div>
          <dt>Semantic triage</dt>
          <dd>${job.routing_mode === "automatic"
              ? "Required"
              : job.routing_mode === "additive" && job.semantic_routing
                ? "Enabled"
                : "Off"}</dd>
        </div>
        <div>
          <dt>Router model</dt>
          <dd>${job.router_model || job.model || "Missing configuration"}</dd>
        </div>
        <div>
          <dt>Router thinking</dt>
          <dd>${job.router_thinking_level || "Review persona default"}</dd>
        </div>
        ${modelOptionFacts("Router", job.router_model_options)}
        <div>
          <dt>Change analyst model</dt>
          <dd>${job.analyst_model || job.model || "Missing configuration"}</dd>
        </div>
        <div>
          <dt>Change analyst thinking</dt>
          <dd>${job.analyst_thinking_level || "Review persona default"}</dd>
        </div>
        ${modelOptionFacts("Change analyst", job.analyst_model_options)}
        ${modelOptionFacts("Coordinator", job.coordinator_model_options)}
        <div>
          <dt>Pending</dt>
          <dd>${duration(job.pending_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Running</dt>
          <dd>${duration(runningElapsed)}</dd>
        </div>
        <div>
          <dt>Revision</dt>
          <dd>${job.review_base_sha
              ? html`<code>${job.review_base_sha.slice(0, 8)}</code>…<code>${job.head_sha.slice(0, 8)}</code>`
              : html`Preparing merge base for <code>${job.head_sha.slice(0, 8)}</code>`}</dd>
        </div>
        <div>
          <dt>Preparation</dt>
          <dd>${duration(job.preparation_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Reviewers</dt>
          <dd>${duration(job.reviewer_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Coordinator</dt>
          <dd>${duration(job.coordinator_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Publication</dt>
          <dd>${duration(job.publication_elapsed_ms)}</dd>
        </div>
      </dl>
      <div class="action-row">
        ${jobActive
          ? html`<button
                class="danger"
                type="button"
                ?disabled=${Boolean(busy)}
                @click=${() => void this.act("cancel")}
              >
                ${busy === "cancel" ? "Cancelling…" : "Cancel"}
              </button>
              <button type="button" ?disabled=${Boolean(busy)} @click=${() => void this.act("retry")}>
                ${busy === "retry" ? "Retrying…" : "Cancel & retry"}
              </button>`
          : nothing}
        ${!["running", "queued"].includes(job.status)
          ? html`${finalEditorRetryable
                ? html`<button type="button" ?disabled=${Boolean(busy)} @click=${() => void this.retryFailedFinalEditor()}>
                    ${busy === "final-editor" ? "Retrying…" : "Retry final editor"}
                  </button>`
                : nothing}
              ${job.legacy_coverage_exhausted
                ? html`<button type="button" ?disabled=${Boolean(busy)} @click=${() => void this.act("request")}>
                    ${busy === "request" ? "Queueing…" : "Run whole review"}
                  </button>`
                : html`<button type="button" ?disabled=${Boolean(busy)} @click=${() => void this.act("retry")}>
                    ${busy === "retry" ? "Retrying…" : unadjudicatedCandidates.length > 0 ? "Rerun all reviewers" : "Retry"}
                  </button>`}`
          : nothing}
      </div>
      ${this.error ? html`<div class="banner error">${this.error}</div>` : nothing}
      ${this.actionNotice
        ? html`<div class="banner warning" role="status">${this.actionNotice}</div>`
        : nothing}
      ${job.error ? html`<div class="banner error">${job.error}</div>` : nothing}
      <div class="link-row">
        ${externalLink(job.pull_url, "Open pull request")}
        ${externalLink(job.review_url, "Open published review")}
        ${externalLink(job.check_run_url, "Open Check Run")}
      </div>
      ${job.check_sync_error ? html`<p class="warning-text">Check sync: ${job.check_sync_error}</p>` : nothing}
      ${job.legacy_coverage_exhausted
        ? html`<div class="banner warning stacked" role="alert">
            <strong>Automatic full-branch compatibility attempts exhausted</strong>
            <p>This pre-8.0 partial result cannot establish branch coverage. Use Run whole review above to request the current head with every selected reviewer.</p>
          </div>`
        : nothing}
      ${job.legacy_coverage_pending
        ? html`<div class="banner warning stacked" role="status" aria-live="polite">
            <strong>Full-branch compatibility review pending</strong>
            <p>This successful result came from a pre-8.0 partial review. A full-branch compatibility result is still required before the revision can pass; the server schedules at most two automatic attempts.</p>
          </div>`
        : nothing}
      ${hasOpenIssues
        ? html`<div class="banner warning stacked" role="alert">
            <strong>${openIssueCount} blocking issue${openIssueCount === 1 ? " remains" : "s remain"} open across this pull request</strong>
            <p>This round found ${job.issue_count} new issue${job.issue_count === 1 ? "" : "s"}. A clean full-branch result does not resolve findings from earlier rounds unless the final editor verifies their fixes.</p>
          </div>`
        : nothing}
      ${openIssueStatusUnknown
        ? html`<div class="banner warning stacked" role="alert">
            <strong>PR-wide open issue status is unknown</strong>
            <p>This legacy review predates PR-wide finding snapshots. It cannot establish that older findings are resolved, even when this round found no new issues.</p>
          </div>`
        : nothing}
      ${routingDecisions.length > 0
        ? html`<details
            class="routing-decisions"
            @toggle=${(event: Event) => {
              this.routingOpen = (event.currentTarget as HTMLDetailsElement).open;
            }}
          >
            <summary>
              <strong>Persona selection</strong>
              <span>${routingDecisions.filter((decision) => decision.selected).length} of${" "}${routingDecisions.length} persona-batch candidates selected</span>
            </summary>
            ${this.routingOpen
              ? html`<div class="routing-batches">
                  ${[...new Set(routingDecisions.map((decision) => decision.batch_index))].map(
                    (batchIndex) => html`<section>
                      <h3>Batch ${batchIndex + 1}</h3>
                      <div>
                        ${repeat(
                          routingDecisions.filter((decision) => decision.batch_index === batchIndex),
                          (decision) => decision.reviewer_id,
                          (decision) => html`<article
                            class="routing-decision ${decision.selected ? "selected" : "skipped"}"
                          >
                            <header>
                              <strong>${decision.reviewer_name}</strong>
                              <span>${decision.selected ? "Selected" : "Skipped"}</span>
                            </header>
                            ${this.renderRoutingReasons(
                              decision.reasons,
                              job.routing_mode,
                              "No applicable routing signal.",
                            )}
                          </article>`,
                        )}
                      </div>
                    </section>`,
                  )}
                </div>`
              : nothing}
          </details>`
        : nothing}
      <section class="detail-section">
        <div class="panel-title inline">
          <div>
            <h2>${jobActive ? "Review overview" : "Completed overview"}</h2>
            <p>${job.issue_count} new confirmed findings${openIssueCount != null ? ` · ${openIssueCount} blocking open across pull request` : nothing}${` · ${acceptedCandidateIds.size} selected candidates`} · ${candidateRejections.length} rejected · ${unadjudicatedCandidates.length} unresolved · ${job.fixed_issue_count} fixed</p>
          </div>
          ${detail.prompt_for_agents
            ? html`<trouve-code-review-copy-button
                .text=${detail.prompt_for_agents}
                .label=${"Copy fix-all prompt"}
              ></trouve-code-review-copy-button>`
            : nothing}
        </div>
        ${detail.summary ? html`<p class="summary">${detail.summary}</p>` : nothing}
        ${unadjudicatedCandidates.length > 0
          ? html`<div class="banner warning stacked" role="alert">
              ${jobActive
                ? html`<strong>Final-editor retry in progress</strong>
                    <p>The prior unresolved candidates remain visible until the replacement final-editor decision completes.</p>`
                : html`<strong>Review incomplete</strong>
                    <p>The final editor did not decide ${unadjudicatedCandidates.length} candidate issue${unadjudicatedCandidates.length === 1 ? "" : "s"}. No clean verdict was published, and these candidates will not become rejection precedent.</p>`}
            </div>`
          : nothing}
        ${(detail.themes ?? []).length > 0
          ? html`<div class="theme-list">
              ${repeat(
                detail.themes ?? [],
                (theme) => theme.id,
                (theme) => html`<article class="finding medium">
                  <header>
                    <strong>Shared root cause</strong>
                    ${statusPill(theme.status)}
                  </header>
                  <p>${theme.root_cause}</p>
                  <p><b>Structural fix:</b> ${theme.recommendation}</p>
                  <small>${theme.finding_ids?.length ?? 0} manifestations across ${theme.observations?.length ?? 0} review rounds${theme.recurrence_count > 0 ? ` · Recurred ${theme.recurrence_count} time${theme.recurrence_count === 1 ? "" : "s"}` : ""}</small>
                </article>`,
              )}
            </div>`
          : nothing}
        ${repeat(ledgerFindings, (finding) => finding.id, (finding) => this.renderFinding(finding))}
        ${advisoryFindings.length > 0
          ? html`<details class="candidate-decisions">
              <summary>
                <strong>Advisory ledger (${advisoryFindings.length})</strong>
                <span>Below the blocking bar · not posted to GitHub · does not gate</span>
              </summary>
              <div class="finding-list">${repeat(advisoryFindings, (finding) => finding.id, (finding) => this.renderFinding(finding))}</div>
            </details>`
          : nothing}
        ${candidateRejections.length > 0
          ? html`<details class="candidate-decisions">
              <summary>
                <strong>Why ${candidateRejections.length} candidates were not selected</strong>
                <span>Final-editor decisions</span>
              </summary>
              <div class="rejection-list">
                ${repeat(
                  candidateRejections,
                  (rejection) => rejection.candidate_id,
                  (rejection) => html`<article class="candidate-rejection">
                    <header>
                      <strong>${rejection.title}</strong>
                      <span>${rejection.reviewer_name}</span>
                    </header>
                    <small>${rejection.path}:${rejection.line} · Severity: ${rejection.severity.toUpperCase()} · Confidence: ${(rejection.confidence ?? "medium").toUpperCase()}</small>
                    <p>${rejection.body}</p>
                    <div><b>Not selected:</b> ${rejection.reason}</div>
                  </article>`,
                )}
              </div>
            </details>`
          : nothing}
        ${unadjudicatedCandidates.length > 0
          ? html`<details class="candidate-decisions unresolved" open>
              <summary>
                <strong>${unadjudicatedCandidates.length} unresolved final-editor decision${unadjudicatedCandidates.length === 1 ? "" : "s"}</strong>
                <span>Reviewer evidence awaiting adjudication</span>
              </summary>
              <div class="rejection-list">
                ${repeat(
                  unadjudicatedCandidates,
                  (candidate) => candidate.candidate_id,
                  (candidate) => html`<article class="candidate-rejection candidate-unadjudicated">
                    <header>
                      <strong>${candidate.title}</strong>
                      <span>${candidate.reviewer_name}</span>
                    </header>
                    <small>${candidate.path}:${candidate.line} · Severity: ${candidate.severity.toUpperCase()} · Confidence: ${(candidate.confidence ?? "medium").toUpperCase()}</small>
                    <p>${candidate.body}</p>
                    <div><b>Status:</b> Awaiting a final-editor decision</div>
                  </article>`,
                )}
              </div>
            </details>`
          : nothing}
        ${unrecordedCandidateDecisions > 0 && !["running", "queued"].includes(job.status)
          ? html`<p class="decision-note">${job.status === "succeeded"
                ? `${unrecordedCandidateDecisions} candidate decision${
                    unrecordedCandidateDecisions === 1 ? " was" : "s were"
                  } not recorded by this older review run.`
                : `Candidate selection did not complete, so ${unrecordedCandidateDecisions} decision${
                    unrecordedCandidateDecisions === 1 ? " is" : "s are"
                  } unavailable.`}</p>`
          : nothing}
      </section>
      <section class="detail-section">
        ${panelTitle(
          "Review activity",
          "Select a persona and batch to inspect its metrics, retained output, and prompt",
        )}
        <p class="visually-hidden" role="status" aria-live="polite">${this.retryStatus}</p>
        ${detail.tasks.length
          ? html`<div class="persona-layout">
              <nav class="persona-groups" aria-label="Review personas and batches">
                ${repeat(
                  activityGroups,
                  (group) => group.id,
                  (group) => {
                    const active = group.id === selectedGroup?.id;
                    const coordinatorRetryBlocked =
                      group.id === "coordinator" && !finalEditorRetryable;
                    const retryable =
                      group.persona
                        ? job.status === "failed" &&
                          ["failed", "cancelled", "queued", "running"].includes(group.status)
                        : group.id === "coordinator" &&
                          ["failed", "cancelled"].includes(job.status) &&
                          ["failed", "cancelled"].includes(group.status) &&
                          !coordinatorRetryBlocked;
                    const retrying = group.persona
                      ? busy === `persona:${group.persona.reviewer_id}`
                      : group.id === "coordinator" && busy === "final-editor";
                    const persona = group.persona;
                    return html`<div class="persona-group${active ? " active" : ""}">
                      <div class="persona-group-summary">
                        <button
                          type="button"
                          ${ref((element) => {
                            this.activityGroupButtons[group.id] = element as
                              | HTMLButtonElement
                              | undefined;
                          })}
                          @click=${() => selectPreferredTask(group.tasks)}
                        >
                          <span>
                            <strong>${group.name}</strong>
                            <small>${group.subtitle}</small>
                          </span>
                          ${statusPill(group.status)}
                        </button>
                        ${retryable
                          ? html`<button
                              class="compact ghost retry-activity"
                              type="button"
                              ?disabled=${Boolean(busy)}
                              @click=${() =>
                                void (persona
                                  ? this.retryFailedPersona(persona.reviewer_id)
                                  : this.retryFailedFinalEditor())}
                              aria-label=${persona
                                ? `Retry full review after ${group.name} ${group.status}`
                                : `Retry ${group.name}`}
                              title=${persona
                                ? "Starts a new review and reruns every selected persona using current settings"
                                : "Retries only the final review editor and retains successful reviewer output"}
                            >
                              ${retrying ? "Retrying…" : persona ? "Retry all" : "Retry"}
                            </button>`
                          : nothing}
                        ${["failed", "cancelled"].includes(job.status) &&
                        group.id === "coordinator" &&
                        ["failed", "cancelled"].includes(group.status) &&
                        coordinatorRetryBlocked
                          ? html`<small class="retry-blocked">Retry personas first</small>`
                          : nothing}
                      </div>
                      ${active && group.tasks.length > 1
                        ? html`<div class="batch-tabs">
                            ${repeat(
                              group.tasks,
                              (task) => task.id,
                              (task) => html`<button
                                class=${task.id === selectedTask?.id ? "active" : ""}
                                type="button"
                                @click=${() => {
                                  this.selectedTaskId = task.id;
                                }}
                              >
                                ${taskAttemptLabel(group.tasks, task)}
                                ${statusPill(task.status)}
                              </button>`,
                            )}
                          </div>`
                        : nothing}
                    </div>`;
                  },
                )}
              </nav>
              ${selectedTask
                ? keyed(
                    selectedTask.id,
                    html`<article class="persona-detail">
                      <header>
                        <div>
                          ${statusPill(selectedTask.status)}
                          <h3>${selectedTask.reviewer_name || "Final review editor"}</h3>
                          <p>${selectedTask.model || "Model not recorded"}${selectedTask.batch_count > 1
                              ? ` · batch ${selectedTask.batch_index + 1}/${selectedTask.batch_count}`
                              : ""}</p>
                        </div>
                        <time>${duration(
                            liveElapsed(
                              selectedTask.elapsed_ms,
                              selectedTask.status,
                              selectedTask.started_at,
                              now,
                            ),
                          )}</time>
                      </header>
                      ${selectedGroup?.persona
                        ? html`<p class="persona-rollup">Persona total: ${selectedGroup.persona.candidate_issue_count} candidates ·${" "}${selectedGroup.persona.confirmed_issue_count} confirmed ·${" "}${duration(selectedGroup.persona.provider_wait_ms)} capacity wait ·${" "}${duration(selectedGroup.persona.model_elapsed_ms)} model/tools</p>`
                        : nothing}
                      ${selectedRoutingDecision
                        ? html`<div
                            class="selected-routing ${selectedRoutingDecision.selected ? "selected" : "skipped"}"
                          >
                            <strong>${selectedRoutingDecision.selected
                                ? "Why this persona ran"
                                : "Why this persona was skipped"}</strong>
                            ${this.renderRoutingReasons(
                              selectedRoutingDecision.reasons,
                              job.routing_mode,
                              "No baseline, semantic, or repository inclusion matched.",
                            )}
                          </div>`
                        : nothing}
                      <dl class="task-facts">
                        <div>
                          <dt>Lifecycle</dt>
                          <dd aria-live="polite">${taskLifecycleLabel(selectedTask.lifecycle_stage)}</dd>
                        </div>
                        <div>
                          <dt>Capacity wait</dt>
                          <dd>${duration(selectedTask.provider_wait_ms)}</dd>
                        </div>
                        <div>
                          <dt>Model/tools</dt>
                          <dd>${duration(liveModelElapsed(selectedTask, now))}</dd>
                        </div>
                        <div>
                          <dt>Tokens</dt>
                          <dd>${selectedTask.input_tokens.toLocaleString()} in ·${" "}${selectedTask.output_tokens.toLocaleString()} out</dd>
                        </div>
                        <div>
                          <dt>Cached input</dt>
                          <dd>${selectedTask.cached_input_tokens.toLocaleString()}</dd>
                        </div>
                        <div>
                          <dt>Tool calls</dt>
                          <dd>${selectedTask.tool_call_count}</dd>
                        </div>
                        <div>
                          <dt>Last progress</dt>
                          <dd>${formatDate(selectedTask.last_progress_at)}</dd>
                        </div>
                        <div>
                          <dt>Candidates / confirmed</dt>
                          <dd>${selectedTask.candidate_issue_count} / ${selectedTask.confirmed_issue_count}</dd>
                        </div>
                      </dl>
                      ${this.taskLoading === selectedTask.id
                        ? html`<p class="decision-note">Loading retained task output…</p>`
                        : nothing}
                      ${this.taskErrors[selectedTask.id]
                        ? html`<div class="banner error">
                            ${this.taskErrors[selectedTask.id]}
                            <span>Retrying automatically.</span>
                          </div>`
                        : nothing}
                      <trouve-code-review-output-block
                        .heading=${"Assistant output"}
                        .value=${selectedTask.output ?? ""}
                        .followTail=${selectedTask.status === "running"}
                      ></trouve-code-review-output-block>
                      <trouve-code-review-output-block
                        .heading=${"Reasoning"}
                        .value=${selectedTask.thinking ?? ""}
                        .followTail=${selectedTask.status === "running"}
                      ></trouve-code-review-output-block>
                      <trouve-code-review-output-block
                        .heading=${"Tool output"}
                        .value=${selectedTask.tool_output ?? ""}
                        .followTail=${selectedTask.status === "running"}
                      ></trouve-code-review-output-block>
                      ${selectedTask.prompt
                        ? html`<details class="nested">
                            <summary>Prompt</summary>
                            <pre>${selectedTask.prompt}</pre>
                          </details>`
                        : nothing}
                      ${selectedTask.error ? html`<p class="error-text">${selectedTask.error}</p>` : nothing}
                    </article>`,
                  )
                : nothing}
            </div>`
          : html`<p class="decision-note">Review tasks have not started yet.</p>`}
      </section>
    </aside>`;
  }
}

customElements.define("trouve-code-review-job-detail", JobDetailPane);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-code-review-job-detail": JobDetailPane;
  }
}
