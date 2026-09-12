import type {
  ProtocolCursorSnapshot,
  ProtocolThreadToolDetails,
  ProtocolThreadViewSnapshot,
} from "@trouve-ai/protocol/client";
import type { components } from "@trouve-ai/protocol/types";
import type {
  TranscriptClient,
  TranscriptEventStream,
} from "@trouve-ai/transcript/transcript-view";

import type { ReviewTask } from "./types";

type ThreadViewItem = components["schemas"]["ThreadViewItem"];
type ThreadTurnState = components["schemas"]["ThreadTurnState"];

/** Task fields the server retains after the review session is deleted. */
export type RetainedTask = Pick<
  ReviewTask,
  | "id"
  | "status"
  | "model"
  | "prompt"
  | "output"
  | "thinking"
  | "tool_output"
  | "error"
  | "started_at"
  | "created_at"
  | "elapsed_ms"
  | "input_tokens"
  | "cached_input_tokens"
  | "output_tokens"
>;

/** Tool name for the retained, concatenated tool log; the renderer titles it "Tool Output". */
export const RETAINED_TOOL_OUTPUT_CALL_ID = "retained-tool-output";

/** Review tasks whose thread is still live on the server. */
export const isLiveTaskStatus = (status: string): boolean =>
  status === "running" || status === "queued";

const turnState = (task: RetainedTask): ThreadTurnState => {
  switch (task.status) {
    case "queued":
      return { state: "waiting_for_capacity" };
    case "running":
      return { state: "running" };
    case "succeeded":
      return {
        state: "completed",
        usage: {
          input_tokens: task.input_tokens,
          output_tokens: task.output_tokens,
          cached_input_tokens: task.cached_input_tokens,
        },
      };
    default:
      return {
        state: "failed",
        error: task.error || `Task ${task.status}`,
      };
  }
};

/**
 * Fold a task's retained prompt/thinking/tool/output columns into the same
 * thread snapshot shape the protocol serves for live threads, so completed
 * reviews render through the desktop transcript instead of raw text blocks.
 */
export const retainedTaskSnapshot = (task: RetainedTask): ProtocolThreadViewSnapshot => {
  const complete = !isLiveTaskStatus(task.status);
  const items: ThreadViewItem[] = [];
  const prompt = task.prompt ?? "";
  items.push({ kind: "user", turn: 1, content: prompt, attachments: [] });
  if (task.thinking) {
    items.push({ kind: "thinking", turn: 1, content: task.thinking, complete: true });
  }
  if (task.tool_output) {
    items.push({
      kind: "tool_call",
      call_id: RETAINED_TOOL_OUTPUT_CALL_ID,
      tool: "tool_output",
      args: {},
      status: "ok",
      result: task.tool_output,
    });
  }
  if (task.output || complete) {
    items.push({ kind: "assistant", turn: 1, content: task.output ?? "", complete });
  }
  items.push({ kind: "turn_status", turn: 1, state: turnState(task) });
  const startedAt = task.started_at ?? task.created_at;
  return {
    items,
    turn_running: !complete,
    ...(task.model ? { turn_models: { "1": task.model } } : {}),
    ...(startedAt ? { turn_started_at: { "1": startedAt } } : {}),
    ...(complete ? { turn_duration_ms: { "1": task.elapsed_ms } } : {}),
  };
};

const idleStream: TranscriptEventStream = { start: () => {}, close: () => {} };

/**
 * A `TranscriptClient` that serves one static snapshot and never streams.
 * Used for finished tasks, whose sessions the server has already deleted.
 */
export class RetainedTranscriptClient implements TranscriptClient {
  readonly #snapshot: ProtocolThreadViewSnapshot;

  constructor(task: RetainedTask) {
    this.#snapshot = retainedTaskSnapshot(task);
  }

  threadView(): Promise<ProtocolCursorSnapshot<ProtocolThreadViewSnapshot>> {
    return Promise.resolve({ cursor: 0, value: this.#snapshot });
  }

  threadEvents(
    _threadId: string,
    options: { readonly onOpen?: () => void },
  ): Promise<TranscriptEventStream> {
    options.onOpen?.();
    return Promise.resolve(idleStream);
  }

  threadToolDetails(): Promise<ProtocolThreadToolDetails> {
    return Promise.reject(new Error("Retained review output has no deferred tool details."));
  }
}

/** Thread id used for the synthesized retained transcript. */
export const retainedThreadId = (taskId: string): string => `retained:${taskId}`;
