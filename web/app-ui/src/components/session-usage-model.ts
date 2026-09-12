import type { ProtocolSubscriptionHealth } from "../services/protocol-client.js";
import {
  boundedSubscriptionUsage,
  type ModelHealthTone,
  modelHealthPresentation,
} from "./model-health.js";

export type SessionUsagePanelKind =
  | "placeholder"
  | "local"
  | "subscription"
  | "api";

export const sessionUsagePanelKind = (input: {
  readonly placeholder: boolean;
  readonly sessionId: string;
  readonly threadId: string;
  readonly model: string;
  readonly hasSubscriptionHealth: boolean;
}): SessionUsagePanelKind => {
  if (
    input.placeholder
    || input.sessionId === ""
    || input.threadId === ""
    || input.model === ""
  ) return "placeholder";
  if (input.model.startsWith("local/")) return "local";
  return input.hasSubscriptionHealth ? "subscription" : "api";
};

export const isAutomaticModel = (model: string): boolean => model.startsWith("auto/");

export interface UsageRouteCandidate {
  readonly provider_id: string;
  readonly provider_model: string;
}

/** The concrete provider route whose usage the panel attributes a thread to. */
export interface UsageRoute {
  readonly providerId: string;
  readonly providerModel: string;
  /** Where the attribution came from: a pinned `provider/model` selection,
   * the route a turn in this thread actually ran on, the thread's durable
   * sticky route, or the healthiest catalog candidate before any turn ran. */
  readonly source: "pinned" | "turn" | "thread" | "candidate";
}

const splitRoute = (
  id: string,
): { readonly providerId: string; readonly providerModel: string } | undefined => {
  const separator = id.indexOf("/");
  if (separator <= 0 || separator === id.length - 1) return undefined;
  return { providerId: id.slice(0, separator), providerModel: id.slice(separator + 1) };
};

const candidateHealthRank = (
  health: ProtocolSubscriptionHealth | undefined,
): readonly [number, number] => {
  if (health === undefined || health.status === "unsupported") return [1, 0];
  if (health.status !== "ok") return [2, 0];
  const highestUsage = health.windows.reduce(
    (highest, window) => Math.max(highest, boundedSubscriptionUsage(window.used_percent)),
    0,
  );
  return [0, highestUsage];
};

/**
 * Resolve which provider's usage the panel should show. Pinned models name
 * their provider directly. Automatic models are attributed to the most
 * recent turn that was routed (a running turn's failover included), then to
 * the thread's sticky route from the server, and before either exists to the
 * candidate route with the healthiest subscription so a fresh thread still
 * shows a meaningful meter.
 */
export const usagePanelRoute = (input: {
  readonly model: string;
  readonly turnModels: ReadonlyMap<number, string>;
  readonly threadRoute: UsageRouteCandidate | null | undefined;
  readonly candidates: readonly UsageRouteCandidate[];
  readonly subscriptions: readonly ProtocolSubscriptionHealth[];
}): UsageRoute | undefined => {
  if (!isAutomaticModel(input.model)) {
    const pinned = splitRoute(input.model);
    return pinned === undefined ? undefined : { ...pinned, source: "pinned" };
  }
  let latestTurn: number | undefined;
  let latestRoute: ReturnType<typeof splitRoute>;
  for (const [turn, id] of input.turnModels) {
    // turn.started seeds the automatic id; the concrete route replaces it.
    if (isAutomaticModel(id)) continue;
    if (latestTurn !== undefined && turn <= latestTurn) continue;
    const route = splitRoute(id);
    if (route === undefined) continue;
    latestTurn = turn;
    latestRoute = route;
  }
  if (latestRoute !== undefined) return { ...latestRoute, source: "turn" };
  if (input.threadRoute) {
    return {
      providerId: input.threadRoute.provider_id,
      providerModel: input.threadRoute.provider_model,
      source: "thread",
    };
  }
  const byProvider = new Map(
    input.subscriptions.map((health) => [health.provider_id, health] as const),
  );
  const ranked = input.candidates
    .map((candidate, index) => ({
      candidate,
      index,
      rank: candidateHealthRank(byProvider.get(candidate.provider_id)),
    }))
    .sort((left, right) =>
      left.rank[0] - right.rank[0]
      || left.rank[1] - right.rank[1]
      || left.index - right.index);
  const best = ranked[0]?.candidate;
  return best === undefined
    ? undefined
    : {
        providerId: best.provider_id,
        providerModel: best.provider_model,
        source: "candidate",
      };
};

export interface CollapsedUsageSummary {
  readonly text: string;
  readonly tone: ModelHealthTone;
}

/**
 * One-line summary for the collapsed sidebar footer: the same "most
 * constrained window" line the model picker shows for subscriptions, the
 * running cost for API-billed models, and the server state for local ones.
 */
export const collapsedUsageSummary = (input: {
  readonly kind: SessionUsagePanelKind;
  readonly loading: boolean;
  readonly error: string;
  readonly health: ProtocolSubscriptionHealth | undefined;
  readonly sessionCostUsd: number | undefined;
  readonly localServerStatus: string | undefined;
}): CollapsedUsageSummary => {
  if (input.kind === "placeholder") return { text: "No active session", tone: "neutral" };
  if (input.kind === "subscription" && input.health !== undefined) {
    const presentation = modelHealthPresentation(input.health);
    return { text: presentation.summary, tone: presentation.tone };
  }
  if (input.kind === "local") {
    return {
      text: input.localServerStatus === undefined
        ? "Local model"
        : `Local · ${input.localServerStatus || "stopped"}`,
      tone: "neutral",
    };
  }
  if (input.sessionCostUsd !== undefined) {
    return { text: `Session · $${input.sessionCostUsd.toFixed(2)}`, tone: "neutral" };
  }
  if (input.loading) return { text: "Loading usage…", tone: "neutral" };
  return { text: input.error || "Usage unavailable", tone: input.error === "" ? "neutral" : "warning" };
};

export const usageThroughput = (
  outputTokens: number,
  durationMs: number | undefined,
): number | undefined =>
  durationMs === undefined || durationMs <= 0
    ? undefined
    : outputTokens / (durationMs / 1_000);

export const latestCompletedTurnDuration = (
  turnDurationMs: ReadonlyMap<number, number>,
): number | undefined => {
  let latestTurn: number | undefined;
  let latestDuration: number | undefined;
  for (const [turn, duration] of turnDurationMs) {
    if (latestTurn === undefined || turn > latestTurn) {
      latestTurn = turn;
      latestDuration = duration;
    }
  }
  return latestDuration;
};

export interface UsageTotals {
  readonly turns: number;
  readonly input_tokens: number;
  readonly output_tokens: number;
  readonly cached_input_tokens: number;
  readonly cost_usd: number;
}

export interface ModelUsageTotals extends UsageTotals {
  readonly model: string;
}

export interface UsageBreakdownRow extends UsageTotals {
  readonly label: string;
  readonly total: boolean;
}

export const usageBreakdownRows = (
  summary: UsageTotals & { readonly models?: readonly ModelUsageTotals[] },
): readonly UsageBreakdownRow[] => {
  const models = summary.models ?? [];
  const rows: UsageBreakdownRow[] = models.map((usage) => ({
    ...usage,
    label: usage.model || "Unknown model",
    total: false,
  }));
  if (models.length > 1) rows.push({ ...summary, label: "Total", total: true });
  return rows;
};

export const localMemoryUtilization = (
  modelBytes: number,
  capacityBytes: number,
): number => capacityBytes <= 0
  ? 0
  : Math.round(Math.min(100, Math.max(0, modelBytes / capacityBytes * 100)));
