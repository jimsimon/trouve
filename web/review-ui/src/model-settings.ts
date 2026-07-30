export interface ModelWithOptions {
  id: string;
  options_schema?: unknown;
}

export interface ThinkingOptions {
  values: string[];
  defaultValue?: string;
  budget?: {
    minimum: number;
    maximum?: number;
  };
}

const THINKING_KEYS = [
  "thinking_level",
  "reasoning_effort",
  "effort",
  "reasoning",
] as const;

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

export function thinkingOptions(model?: ModelWithOptions): ThinkingOptions {
  const schema = object(model?.options_schema);
  const properties = object(schema?.properties);
  if (!properties) return { values: [] };

  for (const key of THINKING_KEYS) {
    const property = object(properties[key]);
    if (!property || !Array.isArray(property.enum)) continue;
    const values = property.enum.filter((value): value is string => typeof value === "string");
    if (values.length < 2) continue;
    return {
      values,
      defaultValue: typeof property.default === "string" ? property.default : undefined,
    };
  }
  const budget = object(properties.thinking_budget_tokens);
  if (budget?.type === "integer" || budget?.type === "number") {
    const minimum = typeof budget.minimum === "number" ? budget.minimum : 1;
    const maximum = typeof budget.maximum === "number" ? budget.maximum : undefined;
    return {
      values: [],
      ...(typeof budget.default === "number"
        ? { defaultValue: String(budget.default) }
        : {}),
      budget: { minimum, maximum },
    };
  }
  return { values: [] };
}

export function thinkingSelectionIsValid(
  model: ModelWithOptions | undefined,
  configured?: string,
): boolean {
  if (!configured) return false;
  const options = thinkingOptions(model);
  if (options.values.includes(configured)) return true;
  if (!options.budget) return false;
  const value = Number(configured);
  return Number.isInteger(value)
    && value >= options.budget.minimum
    && (options.budget.maximum === undefined || value <= options.budget.maximum);
}

export function defaultThinkingSelection(
  model: ModelWithOptions | undefined,
  configured?: string,
): string {
  const options = thinkingOptions(model);
  if (thinkingSelectionIsValid(model, configured)) return configured ?? "";
  if (options.budget && options.defaultValue) {
    return options.defaultValue;
  }
  if (options.defaultValue && options.values.includes(options.defaultValue)) {
    return options.defaultValue;
  }
  if (options.budget) return String(options.budget.minimum);
  return options.values[0] ?? "";
}

export function thinkingLevelLabel(value: string): string {
  if (/^\d+$/.test(value)) return `${Number(value).toLocaleString()} tokens`;
  const labels: Record<string, string> = {
    off: "Off",
    on: "On",
    none: "None",
    minimal: "Minimal",
    low: "Low",
    default: "Default",
    medium: "Medium",
    high: "High",
    xhigh: "Extra High",
    max: "Max",
  };
  return labels[value] ?? value;
}
