import type { ProtocolModelInfo } from "../services/protocol-client.js";

export type ModelOptionValue = string | number | boolean;
export type ModelOptionScalarType = "string" | "number" | "integer";

interface ModelOptionControlBase {
  readonly key: string;
  readonly label: string;
  readonly description: string;
}

export interface ChoiceModelOptionControl extends ModelOptionControlBase {
  readonly kind: "choice";
  readonly choices: readonly {
    readonly label: string;
    readonly value: ModelOptionValue;
  }[];
  readonly selectedIndex: number;
}

export interface BooleanModelOptionControl extends ModelOptionControlBase {
  readonly kind: "boolean";
  readonly selected: boolean;
}

export interface TextModelOptionControl extends ModelOptionControlBase {
  readonly kind: "text";
  readonly scalarType: ModelOptionScalarType;
  readonly text: string;
  readonly hint: string;
  readonly minimum?: number;
  readonly maximum?: number;
}

export type ModelOptionControl =
  | ChoiceModelOptionControl
  | BooleanModelOptionControl
  | TextModelOptionControl;

export interface ModelOptionChangeDetail {
  readonly key: string;
  /** Undefined removes an explicit override and restores the model default. */
  readonly value: ModelOptionValue | undefined;
}

const THINKING_KEYS = new Set([
  "thinking_level",
  "reasoning_effort",
  "effort",
  "reasoning",
  "thinking_budget_tokens",
]);

export const isThinkingModelOption = (key: string): boolean => THINKING_KEYS.has(key);

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;

const scalar = (value: unknown): value is ModelOptionValue =>
  typeof value === "string"
  || typeof value === "number" && Number.isFinite(value)
  || typeof value === "boolean";

const scalarType = (
  property: Readonly<Record<string, unknown>>,
): ModelOptionScalarType | undefined => {
  const advertised = property["type"];
  const names = typeof advertised === "string"
    ? [advertised]
    : Array.isArray(advertised)
      ? advertised.filter((name): name is string => typeof name === "string")
      : [];
  const name = names.find((candidate) => candidate !== "null");
  return name === "string" || name === "number" || name === "integer"
    ? name
    : undefined;
};

const hasType = (
  property: Readonly<Record<string, unknown>>,
  expected: string,
): boolean => {
  const advertised = property["type"];
  return advertised === expected
    || Array.isArray(advertised) && advertised.includes(expected);
};

const humanize = (token: string): string => {
  const words = token.replaceAll("_", " ").replaceAll("-", " ");
  const first = words[0];
  return first === undefined ? "" : first.toUpperCase() + words.slice(1);
};

export const modelOptionLabel = (value: string): string => {
  const labels: Readonly<Record<string, string>> = {
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
};

const optionLabel = (key: string): string => {
  if (key === "thinking_level") return "Thinking";
  if (key === "reasoning_effort") return "Reasoning effort";
  return humanize(key);
};

const valueLabel = (value: ModelOptionValue): string => {
  if (value === true) return "On";
  if (value === false) return "Off";
  if (typeof value === "number") return String(value);
  if (/^[+-]?(?:\d+(?:\.\d+)?|\.\d+)[km]$/iu.test(value)) {
    return value.toUpperCase();
  }
  const known = modelOptionLabel(value);
  return known === value ? humanize(value) : known;
};

const choiceValues = (
  property: Readonly<Record<string, unknown>>,
): readonly { readonly label: string; readonly value: ModelOptionValue }[] | undefined => {
  const advertised = property["enum"];
  if (
    Array.isArray(advertised)
    && advertised.length > 1
    && advertised.every(scalar)
    && new Set(advertised.map((value) => `${typeof value}:${String(value)}`)).size
      === advertised.length
  ) {
    const enumNames = property["x-enumNames"] ?? property["enumNames"];
    const labels = Array.isArray(enumNames)
      && enumNames.length === advertised.length
      && enumNames.every((label): label is string => typeof label === "string")
      ? enumNames
      : undefined;
    return advertised.map((value, index) => ({
      value,
      label: labels?.[index] ?? valueLabel(value),
    }));
  }

  const oneOf = property["oneOf"];
  if (!Array.isArray(oneOf) || oneOf.length <= 1) return undefined;
  const choices: { label: string; value: ModelOptionValue }[] = [];
  for (const candidate of oneOf) {
    const entry = asRecord(candidate);
    const value = entry?.["const"];
    if (entry === undefined || !scalar(value)) return undefined;
    choices.push({
      value,
      label: typeof entry["title"] === "string"
        ? entry["title"]
        : valueLabel(value),
    });
  }
  return new Set(choices.map(({ value }) => `${typeof value}:${String(value)}`)).size
    === choices.length
    ? choices
    : undefined;
};

const optionText = (value: unknown): string =>
  typeof value === "string" ? value : scalar(value) ? String(value) : "";

const optionHint = (property: Readonly<Record<string, unknown>>): string => {
  const examples = property["examples"];
  if (Array.isArray(examples) && scalar(examples[0])) return optionText(examples[0]);
  const minimum = scalar(property["minimum"]) ? optionText(property["minimum"]) : undefined;
  const maximum = scalar(property["maximum"]) ? optionText(property["maximum"]) : undefined;
  if (minimum !== undefined && maximum !== undefined) return `${minimum} – ${maximum}`;
  if (minimum !== undefined) return `at least ${minimum}`;
  if (maximum !== undefined) return `at most ${maximum}`;
  return "value";
};

const storedOption = (
  options: Readonly<Record<string, unknown>>,
  key: string,
): unknown => {
  if (Object.prototype.hasOwnProperty.call(options, key)) return options[key];
  const thinkingKeys = [...THINKING_KEYS].filter((candidate) =>
    candidate !== key && (key === "thinking_level" || candidate !== "thinking_level")
  );
  if (thinkingKeys.some((candidate) => Object.prototype.hasOwnProperty.call(options, candidate))) {
    return undefined;
  }
  return key !== "thinking_level" && THINKING_KEYS.has(key)
    ? options["thinking_level"]
    : undefined;
};

const validTextValue = (
  type: ModelOptionScalarType,
  value: unknown,
  minimum: number | undefined,
  maximum: number | undefined,
): boolean => {
  if (type === "string") return typeof value === "string";
  if (typeof value !== "number" || !Number.isFinite(value)) return false;
  if (type === "integer" && !Number.isInteger(value)) return false;
  return (minimum === undefined || value >= minimum)
    && (maximum === undefined || value <= maximum);
};

/** Derive the editable scalar subset of a model's JSON Schema. Catalog data
 * is treated as untrusted: malformed, read-only, constant, object, and array
 * properties are ignored. */
export const modelOptionControls = (
  model: ProtocolModelInfo | null | undefined,
  options: Readonly<Record<string, unknown>> | undefined,
): readonly ModelOptionControl[] => {
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  if (properties === undefined) return [];
  const current = options ?? {};
  const controls: ModelOptionControl[] = [];

  for (const [key, advertised] of Object.entries(properties)) {
    const property = asRecord(advertised);
    if (
      property === undefined
      || property["readOnly"] === true
      || Object.prototype.hasOwnProperty.call(property, "const")
      || (Array.isArray(property["enum"]) && property["enum"].length <= 1)
    ) continue;
    const label = typeof property["title"] === "string"
      ? property["title"]
      : optionLabel(key);
    const description = typeof property["description"] === "string"
      ? property["description"]
      : "";
    const stored = storedOption(current, key);
    const selected = key === "thinking_budget_tokens"
      && typeof stored === "string"
      && stored.trim() !== ""
      ? Number(stored)
      : stored;
    const defaultValue = property["default"];
    const choices = choiceValues(property);
    if (
      choices === undefined
      && (Object.prototype.hasOwnProperty.call(property, "enum")
        || Object.prototype.hasOwnProperty.call(property, "oneOf"))
    ) continue;
    if (choices !== undefined) {
      const selectedIndex = choices.findIndex(({ value }) => Object.is(value, selected));
      const defaultIndex = choices.findIndex(({ value }) => Object.is(value, defaultValue));
      controls.push({
        kind: "choice",
        key,
        label,
        description,
        choices,
        selectedIndex: selectedIndex >= 0 ? selectedIndex : defaultIndex,
      });
      continue;
    }
    if (hasType(property, "boolean")) {
      controls.push({
        kind: "boolean",
        key,
        label,
        description,
        selected: typeof selected === "boolean"
          ? selected
          : defaultValue === true,
      });
      continue;
    }
    const type = scalarType(property);
    if (type === undefined) continue;
    const minimum = typeof property["minimum"] === "number"
      && Number.isFinite(property["minimum"])
      ? property["minimum"]
      : undefined;
    const maximum = typeof property["maximum"] === "number"
      && Number.isFinite(property["maximum"])
      ? property["maximum"]
      : undefined;
    const value = validTextValue(type, selected, minimum, maximum)
      ? selected
      : validTextValue(type, defaultValue, minimum, maximum)
        ? defaultValue
        : undefined;
    controls.push({
      kind: "text",
      key,
      label,
      description,
      scalarType: type,
      text: optionText(value),
      hint: optionHint(property),
      ...(minimum === undefined ? {} : { minimum }),
      ...(maximum === undefined ? {} : { maximum }),
    });
  }
  return controls;
};

export const modelOptionValueIsValid = (
  control: ModelOptionControl,
  value: unknown,
): value is ModelOptionValue => {
  if (control.kind === "choice") {
    return control.choices.some((choice) => Object.is(choice.value, value));
  }
  if (control.kind === "boolean") return typeof value === "boolean";
  return validTextValue(
    control.scalarType,
    value,
    control.minimum,
    control.maximum,
  );
};

/** Keep only values represented by the selected model's supported scalar
 * controls. Defaults remain implicit instead of being copied into requests. */
export const sanitizeModelOptions = (
  model: ProtocolModelInfo | null | undefined,
  options: Readonly<Record<string, unknown>> | undefined,
): Readonly<Record<string, ModelOptionValue>> => {
  if (options === undefined) return {};
  const controls = modelOptionControls(model, {});
  const sanitized: Record<string, ModelOptionValue> = {};
  for (const control of controls) {
    const direct = options[control.key];
    const canonical = options["thinking_level"];
    const legacy = control.key === "thinking_budget_tokens"
      && typeof canonical === "string"
      && canonical.trim() !== ""
      ? Number(canonical)
      : canonical;
    const value = modelOptionValueIsValid(control, direct)
      ? direct
      : control.key !== "thinking_level" && THINKING_KEYS.has(control.key)
        ? legacy
        : undefined;
    if (modelOptionValueIsValid(control, value)) sanitized[control.key] = value;
  }
  return sanitized;
};

export const changeModelOption = (
  options: Readonly<Record<string, unknown>>,
  change: ModelOptionChangeDetail,
): Readonly<Record<string, unknown>> => {
  const next = { ...options };
  if (change.value === undefined) {
    delete next[change.key];
  } else {
    next[change.key] = change.value;
  }
  if (change.key !== "thinking_level" && THINKING_KEYS.has(change.key)) {
    delete next["thinking_level"];
  }
  return next;
};

/** Model selectors use the provider-qualified protocol id as their visible
 * label so identically named models from different providers stay distinct. */
export const modelSelectorLabel = (
  model: Pick<ProtocolModelInfo, "id">,
): string => model.id;
