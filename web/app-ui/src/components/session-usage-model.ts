import type { ProtocolSubscriptionHealth } from "../services/protocol-client.js";
import { type ModelHealthTone, modelHealthPresentation } from "./model-health.js";

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
