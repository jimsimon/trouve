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

export const localMemoryUtilization = (
  modelBytes: number,
  capacityBytes: number,
): number => capacityBytes <= 0
  ? 0
  : Math.round(Math.min(100, Math.max(0, modelBytes / capacityBytes * 100)));
