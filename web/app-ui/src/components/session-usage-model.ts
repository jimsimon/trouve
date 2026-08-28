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
