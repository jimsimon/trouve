export interface ModelWithOptions {
  id: string;
  options_schema?: unknown;
  routes?: ReadonlyArray<{
    provider_id: string;
    provider_model: string;
  }>;
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

/** Resolve either a routed model id or one of its provider-qualified pins. */
export function modelForSelection<T extends ModelWithOptions>(
  models: readonly T[],
  selection?: string,
): T | undefined {
  if (!selection) return undefined;
  const exact = models.find((model) => model.id === selection);
  if (exact) return exact;
  if (!selection.includes("/")) {
    const automatic = models.find((model) => model.id === `auto/${selection}`);
    if (automatic) return automatic;
  }
  return models.find((model) =>
    model.routes?.some(
      (route) => `${route.provider_id}/${route.provider_model}` === selection,
    ),
  );
}

/** Map legacy bare automatic ids to their catalog row; preserve hard pins. */
export function modelSelectionValue(
  models: readonly ModelWithOptions[],
  selection?: string,
): string {
  if (!selection) return "";
  if (
    !selection.includes("/")
    && models.some((model) => model.id === `auto/${selection}`)
  ) {
    return `auto/${selection}`;
  }
  return selection;
}

/** Extra picker row needed to display a persisted pin or unavailable id. */
export function supplementalModelSelection(
  models: readonly ModelWithOptions[],
  selection?: string,
): { value: string; kind: "pinned" | "unavailable" } | undefined {
  if (
    !selection
    || models.some((model) => model.id === modelSelectionValue(models, selection))
  ) {
    return undefined;
  }
  return {
    value: selection,
    kind: modelForSelection(models, selection) ? "pinned" : "unavailable",
  };
}

export function modelCatalogStatusMessage(
  loaded: boolean,
  error: string,
): string | undefined {
  if (error) {
    return loaded ? `Model choices may be stale: ${error}` : error;
  }
  return loaded
    ? undefined
    : "Model choices are still loading. Model settings remain disabled.";
}

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

export type ModelOptionValue = string | number | boolean;

interface ModelOptionControlBase {
  key: string;
  label: string;
  description: string;
}

export interface BooleanModelOptionControl extends ModelOptionControlBase {
  kind: "boolean";
  /** Explicitly configured value; undefined means "model default". */
  selected?: boolean;
  defaultValue?: boolean;
}

export interface ChoiceModelOptionControl extends ModelOptionControlBase {
  kind: "choice";
  choices: Array<{ label: string; value: ModelOptionValue }>;
  /** Index into `choices`, or -1 for "model default". */
  selectedIndex: number;
  defaultIndex: number;
}

export interface TextModelOptionControl extends ModelOptionControlBase {
  kind: "text";
  scalarType: "string" | "number" | "integer";
  text: string;
  hint: string;
  minimum?: number;
  maximum?: number;
}

export type ModelOptionControl =
  | BooleanModelOptionControl
  | ChoiceModelOptionControl
  | TextModelOptionControl;

const NON_THINKING_EXCLUDED_KEYS = new Set<string>([
  ...THINKING_KEYS,
  "thinking_budget_tokens",
]);

export const isThinkingModelOption = (key: string): boolean =>
  NON_THINKING_EXCLUDED_KEYS.has(key);

const isScalar = (value: unknown): value is ModelOptionValue =>
  typeof value === "string"
  || typeof value === "boolean"
  || (typeof value === "number" && Number.isFinite(value));

const humanize = (token: string): string => {
  const words = token.replaceAll("_", " ").replaceAll("-", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
};

export const modelOptionValueLabel = (value: ModelOptionValue): string => {
  if (value === true) return "On";
  if (value === false) return "Off";
  if (typeof value === "number") return String(value);
  const known = thinkingLevelLabel(value);
  return known === value ? humanize(value) : known;
};

const scalarType = (
  property: Record<string, unknown>,
): "string" | "number" | "integer" | "boolean" | undefined => {
  const advertised = property.type;
  const names = typeof advertised === "string"
    ? [advertised]
    : Array.isArray(advertised)
      ? advertised.filter((name): name is string => typeof name === "string")
      : [];
  const nonNull = names.filter((name) => name !== "null");
  if (nonNull.length !== 1) return undefined;
  const name = nonNull[0];
  return name === "string" || name === "number" || name === "integer" || name === "boolean"
    ? name
    : undefined;
};

const matchesType = (
  type: "string" | "number" | "integer" | "boolean",
  value: unknown,
  minimum?: number,
  maximum?: number,
): value is ModelOptionValue => {
  if (type === "string") return typeof value === "string";
  if (type === "boolean") return typeof value === "boolean";
  if (typeof value !== "number" || !Number.isFinite(value)) return false;
  if (type === "integer" && !Number.isInteger(value)) return false;
  return (minimum === undefined || value >= minimum)
    && (maximum === undefined || value <= maximum);
};

/** Derive the editable non-thinking scalar controls advertised by a model's
 * `options_schema`. Thinking is excluded because it has a dedicated
 * `*_thinking_level` setting. Catalog data is treated as untrusted: malformed,
 * read-only, constant, object, and array properties are ignored. */
export function modelOptionControls(
  model: ModelWithOptions | undefined,
  options?: Record<string, unknown>,
): ModelOptionControl[] {
  const schema = object(model?.options_schema);
  const properties = object(schema?.properties);
  if (!properties) return [];
  const current = options ?? {};
  const controls: ModelOptionControl[] = [];
  for (const [key, advertised] of Object.entries(properties)) {
    if (isThinkingModelOption(key)) continue;
    const property = object(advertised);
    if (!property || property.readOnly === true || Object.hasOwn(property, "const")) continue;
    const type = scalarType(property);
    if (!type) continue;
    const label = typeof property.title === "string" ? property.title : humanize(key);
    const description = typeof property.description === "string" ? property.description : "";
    const stored = current[key];
    const minimum = typeof property.minimum === "number" ? property.minimum : undefined;
    const maximum = typeof property.maximum === "number" ? property.maximum : undefined;
    if (minimum !== undefined && maximum !== undefined && minimum > maximum) continue;

    if (Array.isArray(property.enum)) {
      const values = property.enum.filter(
        (value): value is ModelOptionValue =>
          isScalar(value) && matchesType(type, value, minimum, maximum),
      );
      if (values.length < 2 || new Set(values.map(String)).size !== values.length) continue;
      const choices = values.map((value) => ({ value, label: modelOptionValueLabel(value) }));
      controls.push({
        kind: "choice",
        key,
        label,
        description,
        choices,
        selectedIndex: values.findIndex((value) => Object.is(value, stored)),
        defaultIndex: values.findIndex((value) => Object.is(value, property.default)),
      });
      continue;
    }
    if (type === "boolean") {
      controls.push({
        kind: "boolean",
        key,
        label,
        description,
        ...(typeof stored === "boolean" ? { selected: stored } : {}),
        ...(typeof property.default === "boolean" ? { defaultValue: property.default } : {}),
      });
      continue;
    }
    const hint = minimum !== undefined && maximum !== undefined
      ? `${minimum} – ${maximum}`
      : minimum !== undefined
        ? `at least ${minimum}`
        : maximum !== undefined
          ? `at most ${maximum}`
          : "model default";
    controls.push({
      kind: "text",
      key,
      label,
      description,
      scalarType: type,
      text: matchesType(type, stored, minimum, maximum) ? String(stored) : "",
      hint,
      ...(minimum === undefined ? {} : { minimum }),
      ...(maximum === undefined ? {} : { maximum }),
    });
  }
  return controls;
}

export function modelOptionValueIsValid(
  control: ModelOptionControl,
  value: unknown,
): value is ModelOptionValue {
  if (control.kind === "choice") {
    return control.choices.some((choice) => Object.is(choice.value, value));
  }
  if (control.kind === "boolean") return typeof value === "boolean";
  return matchesType(control.scalarType, value, control.minimum, control.maximum);
}

/** Parse a text control's raw input; `undefined` clears the option and `null`
 * marks an invalid entry that should be ignored. */
export function modelOptionTextValue(
  control: TextModelOptionControl,
  raw: string,
): ModelOptionValue | undefined | null {
  const trimmed = raw.trim();
  if (trimmed === "") return undefined;
  if (control.scalarType === "string") return raw;
  const parsed = Number(trimmed);
  return modelOptionValueIsValid(control, parsed) ? parsed : null;
}

/** Keep only values the selected model advertises as editable non-thinking
 * scalar controls, so stale options are dropped when the model changes. */
export function sanitizeModelOptions(
  model: ModelWithOptions | undefined,
  options?: Record<string, unknown>,
): Record<string, ModelOptionValue> {
  if (!options) return {};
  const sanitized: Record<string, ModelOptionValue> = {};
  for (const control of modelOptionControls(model)) {
    const value = options[control.key];
    if (modelOptionValueIsValid(control, value)) sanitized[control.key] = value;
  }
  return sanitized;
}

/** Set or clear (`undefined`) one option, returning a new map. */
export function changeModelOption(
  options: Record<string, ModelOptionValue> | undefined,
  key: string,
  value: ModelOptionValue | undefined,
): Record<string, ModelOptionValue> {
  const next = { ...(options ?? {}) };
  if (value === undefined) delete next[key];
  else next[key] = value;
  return next;
}

/** Human-readable `Label: value` pairs for snapshotted options. */
export function modelOptionSummaries(
  options?: Record<string, unknown>,
): Array<{ key: string; label: string; value: string }> {
  return Object.entries(options ?? {})
    .filter(([key, value]) => !isThinkingModelOption(key) && isScalar(value))
    .map(([key, value]) => ({
      key,
      label: humanize(key),
      value: modelOptionValueLabel(value as ModelOptionValue),
    }));
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
    ultra: "Ultra",
  };
  return labels[value] ?? value;
}
