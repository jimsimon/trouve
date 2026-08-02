import type { ReviewTask } from "./types";

export function receiveReviewTaskSnapshot(
  task: ReviewTask,
  receivedAt: number,
): ReviewTask {
  return { ...task, model_elapsed_snapshot_at: receivedAt };
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
