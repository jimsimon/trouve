import type { ThreadChatItem, TurnState } from "../state/thread-view-model.js";
import { effectiveToolCall, toolDisplayName } from "./tool-presentation.js";

export interface AgentActivityPresentation {
  readonly label: string;
  readonly detail: string;
  readonly announcementLabel: string;
}

export interface RunningAgentActivityInput {
  readonly items: readonly ThreadChatItem[];
  readonly turnRunning: boolean;
  readonly thinking: boolean;
  readonly compacting: boolean;
  readonly turnModels: ReadonlyMap<number, string>;
  readonly turnStartedAt: ReadonlyMap<number, string>;
  readonly nowMs: number;
}

type ActiveTurnState = Extract<TurnState, {
  readonly kind: "waiting-for-capacity" | "running";
}>;

const normalizedToolIdentifier = (tool: string): string =>
  tool.replaceAll(/[^a-z0-9]/giu, "").toLowerCase();

const activity = (
  label: string,
  detail = "",
  announcementLabel = label,
): AgentActivityPresentation => ({ label, detail, announcementLabel });

const runningModelName = (
  models: ReadonlyMap<number, string>,
  turn: number | undefined,
): string => {
  const model = turn === undefined ? undefined : models.get(turn)?.trim();
  if (model === undefined || model === "") return "model";
  const separator = model.indexOf("/");
  return separator < 0 ? model : model.slice(separator + 1);
};

export const runningToolActivityLabel = (tool: string, args: unknown): string => {
  const effective = effectiveToolCall(tool, args);
  let effectiveTool = effective.tool;
  if (effectiveTool.startsWith("mcp__")) {
    const [server, ...name] = effectiveTool.slice(5).split("__");
    if (server !== "trouve") return `Using ${server ?? "tool"}…`;
    effectiveTool = name.join("__");
  }

  const title = typeof effective.args["title"] === "string"
    ? effective.args["title"].toLowerCase()
    : "";
  if (title.includes("web search")) return "Searching the web…";
  if (title.includes("code search") || title.includes("find related")) {
    return "Searching through code…";
  }

  const normalized = normalizedToolIdentifier(effectiveTool);
  if (/^(edit|multiedit|notebookedit|write|editfile|writefile|createfile|applypatch|delete|deletefile|filechange)$/u.test(normalized)) {
    return "Editing files…";
  }
  if (/^(read|readfile|listdir)$/u.test(normalized)) return "Reading files…";
  if (/^(shell|bash|execute|commandexecution|shelloutput|shellkill)$/u.test(normalized)) {
    return "Running commands…";
  }
  if (/^(search|findrelated|grep|glob)$/u.test(normalized)) {
    return "Searching through code…";
  }
  if (normalized === "websearch") return "Searching the web…";
  if (normalized === "webfetch") return "Fetching web content…";
  if (/^(todowrite|createplan|updateplan)$/u.test(normalized)) return "Updating the plan…";
  if (/^(task|agent|spawnagent|collabagenttoolcall)$/u.test(normalized)) {
    return "Delegating work…";
  }
  return `Using ${toolDisplayName(effectiveTool)}…`;
};

export const compactRunningElapsed = (elapsedMs: number): string => {
  const totalSeconds = Number.isFinite(elapsedMs)
    ? Math.max(0, Math.floor(elapsedMs / 1_000))
    : 0;
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m ${seconds}s`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
};

/** Describe the current running phase without relying on a new durable event.
 * Items before the newest active-turn marker are deliberately ignored so a
 * stale tool or question from an earlier turn cannot own the live status. */
export const runningAgentActivity = (
  input: RunningAgentActivityInput,
): AgentActivityPresentation | undefined => {
  if (!input.turnRunning) return undefined;

  let turn: number | undefined;
  let activeState: ActiveTurnState | undefined;
  let start = 0;
  for (let index = input.items.length - 1; index >= 0; index -= 1) {
    const item = input.items[index];
    if (
      item?.kind !== "turn-status"
      || (item.state.kind !== "waiting-for-capacity" && item.state.kind !== "running")
    ) continue;
    turn = item.turn;
    activeState = item.state;
    start = index + 1;
    break;
  }
  if (turn === undefined) {
    for (const metadata of [input.turnModels, input.turnStartedAt]) {
      for (const candidate of metadata.keys()) {
        if (turn === undefined || candidate > turn) turn = candidate;
      }
    }
  }
  if (activeState === undefined && turn !== undefined) {
    const turnBoundary = input.items.findIndex((item) =>
      "turn" in item && item.turn === turn
    );
    if (turnBoundary >= 0) start = turnBoundary;
  }
  const current = input.items.slice(start);
  const model = runningModelName(input.turnModels, turn);

  if (input.compacting) {
    return activity(
      "Compacting context…",
      "Preparing a shorter conversation history before contacting the model.",
    );
  }
  if (current.some((item) => item.kind === "questions" && item.answers === undefined)) {
    return activity(
      "Waiting for your answer…",
      "The agent will continue after you answer or skip its questions.",
    );
  }
  if (current.some(
    (item) => item.kind === "tool" && item.status === "awaiting-approval",
  )) {
    return activity(
      "Waiting for approval…",
      "The agent will continue after the pending tool request is resolved.",
    );
  }
  if (activeState?.kind === "waiting-for-capacity") {
    return activity("Waiting for provider admission…");
  }
  if (
    input.thinking
    || current.some((item) => item.kind === "thinking" && !item.complete)
  ) {
    return activity("Thinking…", `${model} is streaming its reasoning.`);
  }

  for (let index = current.length - 1; index >= 0; index -= 1) {
    const item = current[index];
    if (item?.kind !== "tool" || item.status !== "running") continue;
    return activity(runningToolActivityLabel(item.tool, item.args));
  }

  let latestWork: ThreadChatItem | undefined;
  for (let index = current.length - 1; index >= 0; index -= 1) {
    const item = current[index];
    if (
      item === undefined
      || ["user", "steered", "turn-status", "compaction", "todo"].includes(item.kind)
    ) continue;
    latestWork = item;
    break;
  }
  if (latestWork?.kind === "tool") {
    return activity(
      "Agent is working…",
      "The agent is processing tool activity.",
    );
  }

  const modelHasResponded = current.some((item) =>
    !["user", "steered", "turn-status", "compaction"].includes(item.kind)
  );
  if (modelHasResponded) {
    return activity(
      `Waiting for ${model}…`,
      "The model is between visible response or tool events.",
    );
  }

  const startedAt = turn === undefined
    ? activeState?.startedAt
    : input.turnStartedAt.get(turn) ?? activeState?.startedAt;
  const parsedStartedAt = startedAt === undefined ? Number.NaN : Date.parse(startedAt);
  const elapsedMs = Number.isFinite(parsedStartedAt)
    ? Math.max(0, input.nowMs - parsedStartedAt)
    : undefined;
  if (elapsedMs === undefined || elapsedMs < 2_000) {
    return activity(`Starting ${model}…`, "Preparing the model request.");
  }
  if (elapsedMs < 120_000) {
    return activity(
      `Waiting for first response from ${model} · ${compactRunningElapsed(elapsedMs)}`,
      "The turn is running, but no model output has arrived yet.",
      `Waiting for first response from ${model}…`,
    );
  }
  return activity(
    `Still waiting for ${model} · ${compactRunningElapsed(elapsedMs)}`,
    "No model output has arrived yet. You can keep waiting or cancel and retry.",
    `Still waiting for ${model}…`,
  );
};
