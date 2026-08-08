import type { ProtocolUsageSummary } from "../services/protocol-client.js";

export interface TurnUsageLike {
  readonly input_tokens: number;
  readonly cached_input_tokens?: number;
  readonly context_input_tokens?: number | null;
  readonly context_window?: number | null;
}

export interface ComposerContextUsage {
  readonly fill: number;
  readonly percent: number;
  readonly usedTokens: number;
  readonly windowTokens: number | undefined;
  readonly unavailable: boolean;
  readonly compacting: boolean;
  readonly label: string;
}

const safeTokenCount = (value: number | null | undefined): number =>
  Number.isFinite(value) && (value ?? 0) > 0 ? Math.floor(value ?? 0) : 0;

export const composerContextUsage = (
  usage: TurnUsageLike | undefined,
  catalogWindow: number | null | undefined,
  compacting: boolean,
  legacyInputIncludesCached = false,
): ComposerContextUsage => {
  const explicitContextReported = usage?.context_input_tokens !== undefined
    && usage.context_input_tokens !== null;
  const usedTokens = explicitContextReported
    ? safeTokenCount(usage?.context_input_tokens)
    : safeTokenCount(usage?.input_tokens)
      + (legacyInputIncludesCached ? 0 : safeTokenCount(usage?.cached_input_tokens));
  const configuredWindow = safeTokenCount(catalogWindow);
  const liveWindowReported = usage?.context_window !== undefined
    && usage.context_window !== null;
  const liveWindow = safeTokenCount(usage?.context_window);
  const windowTokens = liveWindowReported
    ? liveWindow > 0 ? liveWindow : undefined
    : configuredWindow > 0 ? configuredWindow : undefined;

  // A compaction boundary invalidates the previous request's context size.
  // Keep the raw values available to callers, but do not present that stale
  // measurement as a determinate fill while the provider is replacing it.
  if (compacting) {
    return {
      fill: 0,
      percent: 0,
      usedTokens,
      windowTokens,
      unavailable: windowTokens === undefined,
      compacting: true,
      label: "Context compaction in progress",
    };
  }

  if (windowTokens === undefined) {
    const prefix = usedTokens === 0 ? "" : `Context: ${usedTokens} tokens. `;
    return {
      fill: 0,
      percent: 0,
      usedTokens,
      windowTokens: undefined,
      unavailable: true,
      compacting,
      label: `${prefix}Automatic compaction is disabled because this provider did not report the model's context-window size.`,
    };
  }

  if (usedTokens === 0) {
    return {
      fill: 0,
      percent: 0,
      usedTokens,
      windowTokens,
      unavailable: false,
      compacting,
      label: "Context: no usage yet",
    };
  }

  const fill = Math.min(1, usedTokens / windowTokens);
  const percent = Math.round(fill * 100);
  return {
    fill,
    percent,
    usedTokens,
    windowTokens,
    unavailable: false,
    compacting,
    label: `Context: ${usedTokens} / ${windowTokens} tokens (${percent}%)`,
  };
};

export const formatTokenCount = (value: number): string => {
  const count = safeTokenCount(value);
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}k`;
  return String(count);
};

export const formatSessionUsage = (
  usage: ProtocolUsageSummary | undefined,
): string => {
  if (usage === undefined) return "";
  const cost = usage.cost_usd > 0 ? ` · $${usage.cost_usd.toFixed(4)}` : "";
  return `${formatTokenCount(usage.input_tokens)} in / ${formatTokenCount(usage.output_tokens)} out${cost}`;
};
