export interface QueueItemIdentity {
  readonly id: string;
}

/** Compact rows show the first meaningful line while editors and the title
 * retain the complete prompt. This avoids multiline overflow and large text
 * layout work in the queue list. */
export const queuePreview = (prompt: string): string =>
  prompt.split(/\r?\n/u).map((line) => line.trim()).find((line) => line !== "") ?? "";

export interface QueueControlStateInput {
  readonly threadAvailable: boolean;
  readonly queueLength: number;
  readonly turnRunning: boolean;
  readonly busy: boolean;
  readonly connectivityBlocked: boolean;
}

export interface QueueControlState {
  readonly mutationsDisabled: boolean;
  readonly dispatchDisabled: boolean;
}

export const queueControlState = (
  input: QueueControlStateInput,
): QueueControlState => {
  const mutationsDisabled =
    !input.threadAvailable || input.busy || input.connectivityBlocked;
  return {
    mutationsDisabled,
    dispatchDisabled:
      mutationsDisabled || input.turnRunning || input.queueLength === 0,
  };
};

/** Return a complete queue order for the protocol, or undefined when the
 * requested move is no longer valid against the rendered queue snapshot. */
export const reorderedQueueIds = (
  queue: readonly QueueItemIdentity[],
  index: number,
  delta: -1 | 1,
): readonly string[] | undefined => {
  const destination = index + delta;
  if (
    !Number.isInteger(index)
    || index < 0
    || index >= queue.length
    || destination < 0
    || destination >= queue.length
  ) return undefined;
  const ids = queue.map(({ id }) => id);
  [ids[index], ids[destination]] = [ids[destination]!, ids[index]!];
  return ids;
};

export type QueueDropPlacement = "before" | "after";

/** Build the complete protocol order produced by dropping one queue row on
 * either edge of another row. IDs are used instead of stale render indexes so
 * an SSE queue replacement during a drag fails closed. */
export const droppedQueueIds = (
  queue: readonly QueueItemIdentity[],
  sourceId: string,
  targetId: string,
  placement: QueueDropPlacement,
): readonly string[] | undefined => {
  const ids = queue.map(({ id }) => id);
  const sourceIndex = ids.indexOf(sourceId);
  const targetIndex = ids.indexOf(targetId);
  if (sourceIndex < 0 || targetIndex < 0 || sourceId === targetId) return undefined;

  ids.splice(sourceIndex, 1);
  const remainingTargetIndex = ids.indexOf(targetId);
  ids.splice(remainingTargetIndex + (placement === "after" ? 1 : 0), 0, sourceId);
  return ids.every((id, index) => id === queue[index]?.id) ? undefined : ids;
};

export const prioritizedQueueIds = (
  queue: readonly QueueItemIdentity[],
  index: number,
): readonly string[] | undefined => {
  if (!Number.isInteger(index) || index < 0 || index >= queue.length) return undefined;
  return [queue[index]!.id, ...queue.filter((_, candidate) => candidate !== index).map(({ id }) => id)];
};

export type QueueFocusAfterDelete =
  | { readonly kind: "prompt"; readonly promptId: string }
  | { readonly kind: "composer" };

export const queueFocusAfterDelete = (
  queue: readonly QueueItemIdentity[],
  promptId: string,
): QueueFocusAfterDelete => {
  const index = queue.findIndex(({ id }) => id === promptId);
  if (index < 0) return { kind: "composer" };
  const remaining = queue.filter(({ id }) => id !== promptId);
  const next = remaining[Math.min(index, remaining.length - 1)];
  return next === undefined
    ? { kind: "composer" }
    : { kind: "prompt", promptId: next.id };
};
