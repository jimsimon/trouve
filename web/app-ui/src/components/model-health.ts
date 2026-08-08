import type {
  ProtocolModelInfo,
  ProtocolSubscriptionHealth,
} from "../services/protocol-client.js";

export type ModelHealthTone = "neutral" | "ok" | "warning" | "error";

export interface ModelHealthPresentation {
  readonly summary: string;
  readonly detail: string;
  readonly tone: ModelHealthTone;
}

export const boundedSubscriptionUsage = (usedPercent: number): number =>
  Math.max(0, Math.min(100, usedPercent));

const displayedSubscriptionUsage = (usedPercent: number): number =>
  Math.round(boundedSubscriptionUsage(usedPercent));

export const subscriptionUsageTone = (usedPercent: number): Exclude<
  ModelHealthTone,
  "neutral"
> => {
  const percent = boundedSubscriptionUsage(usedPercent);
  return percent >= 90 ? "error" : percent >= 70 ? "warning" : "ok";
};

const displayPlan = (plan: string): string =>
  plan === "" ? "" : `${plan[0]?.toLocaleUpperCase() ?? ""}${plan.slice(1)}`;

export const modelHealthPresentation = (
  health: ProtocolSubscriptionHealth,
): ModelHealthPresentation => {
  const plan = displayPlan(health.plan);
  const constrained = health.windows.reduce<(typeof health.windows)[number] | undefined>(
    (current, window) => current === undefined || window.used_percent > current.used_percent
      ? window
      : current,
    undefined,
  );
  const note = health.note.toLocaleLowerCase();
  let summary = "usage unavailable";
  let tone: ModelHealthTone = "neutral";

  if (health.status === "ok") {
    if (constrained !== undefined) {
      const percent = displayedSubscriptionUsage(constrained.used_percent);
      summary = `${plan === "" ? "" : `${plan} · `}${percent}% used`;
      tone = subscriptionUsageTone(percent);
    } else if (plan !== "") {
      summary = plan;
      tone = "ok";
    } else if (health.credits !== "") {
      summary = health.credits;
      tone = "ok";
    } else {
      summary = "usage available";
      tone = "ok";
    }
  } else if (health.status === "unavailable") {
    summary = note.includes("login") || note.includes("logged in")
      ? "login required"
      : "usage unavailable";
    tone = "error";
  } else if (health.status === "unsupported") {
    summary = note.includes("api key") || note.includes("usage-billed")
      ? "API billed"
      : "usage unavailable";
  }

  const detail = [
    plan === "" ? health.provider_id : `${health.provider_id} · ${plan}`,
    ...(health.windows.length === 0
      ? []
      : [
          "",
          ...health.windows.map((window) => {
            const percent = displayedSubscriptionUsage(window.used_percent);
            return `${window.label}: ${percent}% used${window.resets === "" ? "" : ` · ${window.resets}`}`;
          }),
        ]),
    ...(health.credits === "" ? [] : [health.credits]),
    ...(health.note === "" ? [] : ["", health.note]),
    ...(health.status === "ok"
      ? [
          "",
          "Highest reported usage is shown in the picker. Provider limits may change before the next refresh.",
        ]
      : []),
  ].join("\n");

  return { summary, detail, tone };
};

export const modelHealthPresentations = (
  models: readonly ProtocolModelInfo[],
  subscriptions: readonly ProtocolSubscriptionHealth[],
): readonly (ModelHealthPresentation | undefined)[] => {
  const byProvider = new Map(
    subscriptions.map((health) => [health.provider_id, health] as const),
  );
  return models.map((model) => {
    const separator = model.id.indexOf("/");
    if (separator <= 0) return undefined;
    const health = byProvider.get(model.id.slice(0, separator));
    return health === undefined ? undefined : modelHealthPresentation(health);
  });
};

const subsequenceScore = (value: string, query: string): number | undefined => {
  if (query === "") return 0;
  if (value.startsWith(query)) return 0;
  const contained = value.indexOf(query);
  if (contained >= 0) return 100 + contained;
  let cursor = 0;
  let spread = 0;
  for (const character of query) {
    const next = value.indexOf(character, cursor);
    if (next < 0) return undefined;
    spread += next - cursor;
    cursor = next + 1;
  }
  return 1_000 + spread;
};

export const filteredModelIndices = (
  models: readonly ProtocolModelInfo[],
  query: string,
  limit = 100,
): readonly number[] => {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  return models
    .map((model, index) => {
      const value = `${model.display_name} ${model.id}`.toLocaleLowerCase();
      return { index, score: subsequenceScore(value, normalizedQuery) };
    })
    .filter((entry): entry is { readonly index: number; readonly score: number } =>
      entry.score !== undefined)
    .sort((left, right) => left.score - right.score || left.index - right.index)
    .slice(0, Math.max(0, limit))
    .map((entry) => entry.index);
};
