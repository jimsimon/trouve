export interface ModelWithOptions {
  id: string;
  options_schema?: unknown;
}

export interface ThinkingOptions {
  values: string[];
  defaultValue?: string;
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
  return { values: [] };
}

export function defaultThinkingSelection(
  model: ModelWithOptions | undefined,
  configured?: string,
): string {
  const options = thinkingOptions(model);
  if (configured && options.values.includes(configured)) return configured;
  if (options.defaultValue && options.values.includes(options.defaultValue)) {
    return options.defaultValue;
  }
  return options.values[0] ?? "";
}

export function thinkingLevelLabel(value: string): string {
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
