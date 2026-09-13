import type { EventEnvelope, Repository, ReviewJob, ReviewTask, ReviewTaskProgress } from "./types";

export const SERVER_EVENT_REFRESH_DEBOUNCE_MS = 100;
export const AUTOMATIC_RETRY_MS = 5_000;
export const DASHBOARD_FALLBACK_REFRESH_MS = 30_000;
export const CLI_IDLE_REFRESH_MS = 5 * 60_000;

export function formatDate(value?: string): string {
  return value ? new Date(value).toLocaleString() : "—";
}

export function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  const remainder = seconds % 60;
  if (hours) return `${hours}h ${minutes}m ${remainder}s`;
  if (minutes) return `${minutes}m ${remainder}s`;
  return `${remainder}s`;
}

export function liveElapsed(
  baseline: number,
  status: string,
  startedAt: string | undefined,
  now: number,
): number {
  if (status !== "running" || !startedAt) return baseline;
  const liveAge = Math.max(0, now - new Date(startedAt).getTime());
  return Math.max(baseline, liveAge);
}

export function taskLifecycleLabel(stage: ReviewTask["lifecycle_stage"]): string {
  switch (stage) {
    case "waiting_for_capacity":
      return "Waiting for capacity";
    case "starting_model":
      return "Starting model";
    case "running_model":
      return "Running model";
    case "running_tool":
      return "Running tool";
    case "repairing_output":
      return "Repairing output";
    case "completed":
      return "Completed";
    default:
      return "Queued";
  }
}

export function isReviewTaskProgress(
  progress: EventEnvelope["progress"],
): progress is ReviewTaskProgress {
  return Boolean(progress && "lifecycle_stage" in progress);
}

export function pickPreferredTask(tasks: ReviewTask[]): ReviewTask | undefined {
  const latestByBatch = new Map<string, ReviewTask>();
  tasks.forEach((task) => {
    const reviewerKey = task.reviewer_id || task.reviewer_name || task.id;
    const key = `${task.role}:${reviewerKey}:${task.batch_index}`;
    const current = latestByBatch.get(key);
    if (
      !current ||
      task.created_at > current.created_at ||
      (task.created_at === current.created_at && task.id > current.id)
    ) {
      latestByBatch.set(key, task);
    }
  });
  const latest = [...latestByBatch.values()];
  return (
    latest.find((task) => task.status === "running") ??
    latest.find((task) => task.status === "queued") ??
    latest.find((task) => task.status === "failed") ??
    latest
      .slice()
      .reverse()
      .find((task) => task.role === "coordinator") ??
    latest[0] ??
    tasks[0]
  );
}

export function taskAttemptLabel(tasks: ReviewTask[], task: ReviewTask): string {
  const attempts = tasks.filter(
    (candidate) =>
      candidate.role === task.role && candidate.batch_index === task.batch_index,
  );
  const base =
    task.role === "coordinator" || task.role === "analyst"
      ? "Attempt"
      : task.role === "router"
        ? `Routing ${task.batch_index + 1}`
        : `Batch ${task.batch_index + 1}`;
  if (attempts.length === 1) return base;
  return `${base} · attempt ${attempts.indexOf(task) + 1}`;
}

export function routingModeLabel(mode: Repository["routing_mode"]): string {
  switch (mode) {
    case "additive":
      return "Additive";
    case "automatic":
      return "Automatic";
    default:
      return "Manual";
  }
}

export type ReviewJobAttentionState =
  | "coverage_exhausted"
  | "coverage_pending"
  | "open"
  | "unknown"
  | null;

export function reviewJobAttentionState(
  job: Pick<
    ReviewJob,
    "status" | "open_issue_count" | "legacy_coverage_pending" | "legacy_coverage_exhausted"
  >,
): ReviewJobAttentionState {
  if (job.status !== "succeeded") return null;
  if (job.legacy_coverage_exhausted) return "coverage_exhausted";
  if (job.legacy_coverage_pending) return "coverage_pending";
  if (job.open_issue_count != null && job.open_issue_count > 0) return "open";
  if (job.open_issue_count == null) return "unknown";
  return null;
}
