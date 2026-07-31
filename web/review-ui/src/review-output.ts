import type { ReviewTask } from "./types";

export type ReviewOutputField = "output" | "thinking" | "tool_output";

export const LIVE_OUTPUT_BATCH_MS = 250;
export const LIVE_OUTPUT_MAX_CHARS = 256 * 1024;
export const LIVE_OUTPUT_OMITTED_MARKER =
  "[Showing the latest output; earlier content was omitted to keep this page responsive.]\n\n";

export function boundReviewOutput(
  value: string,
  maximum = LIVE_OUTPUT_MAX_CHARS,
): string {
  if (value.length <= maximum) return value;
  if (maximum <= LIVE_OUTPUT_OMITTED_MARKER.length) {
    return LIVE_OUTPUT_OMITTED_MARKER.slice(0, maximum);
  }
  return (
    LIVE_OUTPUT_OMITTED_MARKER +
    value.slice(-(maximum - LIVE_OUTPUT_OMITTED_MARKER.length))
  );
}

export function appendBoundedReviewOutput(
  current: string,
  addition: string,
  maximum = LIVE_OUTPUT_MAX_CHARS,
): string {
  const wasTruncated = current.startsWith(LIVE_OUTPUT_OMITTED_MARKER);
  const retained = wasTruncated
    ? current.slice(LIVE_OUTPUT_OMITTED_MARKER.length)
    : current;
  const combined = `${retained}${addition}`;
  if (!wasTruncated) return boundReviewOutput(combined, maximum);
  if (maximum <= LIVE_OUTPUT_OMITTED_MARKER.length) {
    return LIVE_OUTPUT_OMITTED_MARKER.slice(0, maximum);
  }
  return (
    LIVE_OUTPUT_OMITTED_MARKER +
    combined.slice(-(maximum - LIVE_OUTPUT_OMITTED_MARKER.length))
  );
}

export function boundReviewTaskOutput(task: ReviewTask): ReviewTask {
  return {
    ...task,
    output: boundReviewOutput(task.output),
    thinking: boundReviewOutput(task.thinking),
    tool_output: boundReviewOutput(task.tool_output),
  };
}

export function reviewTaskSummary(task: ReviewTask): ReviewTask {
  return {
    ...task,
    prompt: "",
    output: "",
    thinking: "",
    tool_output: "",
  };
}
