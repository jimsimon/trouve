import type { ReviewTask } from "./types";

function progressTimestamp(value: string | undefined): number | undefined {
  if (!value) return undefined;
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? undefined : parsed;
}

function statusRank(status: string): number {
  if (status === "queued") return 0;
  if (status === "running") return 1;
  return 2;
}

function progressRegressed(current: ReviewTask, incoming: ReviewTask): boolean {
  const currentProgressAt = progressTimestamp(current.last_progress_at);
  const incomingProgressAt = progressTimestamp(incoming.last_progress_at);
  return (
    (currentProgressAt !== undefined && incomingProgressAt === undefined) ||
    (currentProgressAt !== undefined &&
      incomingProgressAt !== undefined &&
      incomingProgressAt < currentProgressAt) ||
    statusRank(incoming.status) < statusRank(current.status) ||
    incoming.provider_wait_ms < current.provider_wait_ms ||
    incoming.model_elapsed_ms < current.model_elapsed_ms ||
    incoming.input_tokens < current.input_tokens ||
    incoming.cached_input_tokens < current.cached_input_tokens ||
    incoming.output_tokens < current.output_tokens ||
    incoming.tool_call_count < current.tool_call_count ||
    incoming.candidate_issue_count < current.candidate_issue_count ||
    incoming.confirmed_issue_count < current.confirmed_issue_count
  );
}

function sameProgressSnapshot(current: ReviewTask, incoming: ReviewTask): boolean {
  return (
    incoming.status === current.status &&
    incoming.lifecycle_stage === current.lifecycle_stage &&
    incoming.provider_wait_ms === current.provider_wait_ms &&
    incoming.model_elapsed_ms === current.model_elapsed_ms &&
    incoming.input_tokens === current.input_tokens &&
    incoming.cached_input_tokens === current.cached_input_tokens &&
    incoming.output_tokens === current.output_tokens &&
    incoming.tool_call_count === current.tool_call_count &&
    incoming.model_started_at === current.model_started_at &&
    incoming.last_progress_at === current.last_progress_at
  );
}

function preferDetailedValue(
  current: string | undefined,
  incoming: string | undefined,
): string | undefined {
  if (!current) return incoming;
  if (!incoming || current.length >= incoming.length) return current;
  return incoming;
}

function preserveDetailedValues(
  task: ReviewTask,
  current: ReviewTask,
): ReviewTask {
  return {
    ...task,
    prompt: preferDetailedValue(current.prompt, task.prompt),
    output: preferDetailedValue(current.output, task.output),
    thinking: preferDetailedValue(current.thinking, task.thinking),
    tool_output: preferDetailedValue(current.tool_output, task.tool_output),
  };
}

export function mergeReviewTaskSnapshot(
  current: ReviewTask | undefined,
  incoming: ReviewTask,
  receivedAt: number,
): ReviewTask {
  if (!current) {
    return { ...incoming, model_elapsed_snapshot_at: receivedAt };
  }
  if (progressRegressed(current, incoming)) {
    return {
      ...preserveDetailedValues(incoming, current),
      ...current,
      prompt: preferDetailedValue(current.prompt, incoming.prompt),
      output: preferDetailedValue(current.output, incoming.output),
      thinking: preferDetailedValue(current.thinking, incoming.thinking),
      tool_output: preferDetailedValue(current.tool_output, incoming.tool_output),
      model_elapsed_snapshot_at:
        current.model_elapsed_snapshot_at ?? receivedAt,
    };
  }
  if (
    current.status === "running" &&
    sameProgressSnapshot(current, incoming)
  ) {
    return {
      ...preserveDetailedValues(incoming, current),
      model_elapsed_snapshot_at:
        current.model_elapsed_snapshot_at ?? receivedAt,
    };
  }
  return { ...incoming, model_elapsed_snapshot_at: receivedAt };
}

export function liveModelElapsed(task: ReviewTask, now: number): number {
  if (
    task.status !== "running" ||
    !task.model_started_at ||
    task.model_elapsed_snapshot_at === undefined
  ) {
    return task.model_elapsed_ms;
  }
  return (
    task.model_elapsed_ms + Math.max(0, now - task.model_elapsed_snapshot_at)
  );
}
