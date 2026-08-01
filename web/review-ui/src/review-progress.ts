import type { ReviewTask } from "./types";

export function liveModelElapsed(task: ReviewTask, now: number): number {
  if (task.status !== "running" || !task.model_started_at) {
    return task.model_elapsed_ms;
  }
  const progressAnchor = task.last_progress_at ?? task.model_started_at;
  return (
    task.model_elapsed_ms + Math.max(0, now - new Date(progressAnchor).getTime())
  );
}
