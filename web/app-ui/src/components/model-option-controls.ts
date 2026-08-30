import type { ProtocolModelInfo } from "../services/protocol-client.js";
import {
  jsonNumberTokenIsExact,
  jsonNumberValueToken,
  modelOptionNumberNeedsSourceProof,
  protocolModelOptionNumberSource,
  protocolOptionSchemaNumbersAreExact,
  updateProtocolModelOption,
} from "../services/protocol-json.js";

export type ModelOptionValue = string | number | boolean;
export type ModelOptionScalarType = "string" | "number" | "integer";

interface ModelOptionControlBase {
  readonly key: string;
  readonly label: string;
  readonly description: string;
  readonly overridden: boolean;
}

export interface ChoiceModelOptionControl extends ModelOptionControlBase {
  readonly kind: "choice";
  readonly choices: readonly {
    readonly label: string;
    readonly value: ModelOptionValue;
    readonly numberSource?: string;
  }[];
  readonly selectedIndex: number;
}

export interface BooleanModelOptionControl extends ModelOptionControlBase {
  readonly kind: "boolean";
  readonly selected: boolean | undefined;
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
  /** Original verified JSON token for a numeric value. */
  readonly numberSource?: string;
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

type AdvertisedScalarType = ModelOptionScalarType | "boolean";

const scalarType = (
  property: Readonly<Record<string, unknown>>,
): AdvertisedScalarType | null | undefined => {
  const advertised = property["type"];
  if (advertised === undefined) return undefined;
  const names = typeof advertised === "string"
    ? [advertised]
    : Array.isArray(advertised)
      && advertised.every((name): name is string => typeof name === "string")
      ? advertised
      : undefined;
  if (names === undefined) return null;
  const nonNull = names.filter((candidate) => candidate !== "null");
  if (nonNull.length !== 1) return null;
  const name = nonNull[0];
  return name === "string" || name === "number" || name === "integer" || name === "boolean"
    ? name
    : null;
};

const UNSUPPORTED_SCHEMA_KEYWORDS = new Set([
  "multipleOf",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "minLength",
  "maxLength",
  "pattern",
  "format",
  "allOf",
  "anyOf",
  "not",
  "if",
  "then",
  "else",
]);

const hasUnsupportedConstraints = (
  property: Readonly<Record<string, unknown>>,
): boolean => [...UNSUPPORTED_SCHEMA_KEYWORDS].some((key) =>
  Object.hasOwn(property, key)
) || ["minimum", "maximum"].some((key) =>
  Object.hasOwn(property, key)
  && (typeof property[key] !== "number" || !Number.isFinite(property[key]))
);

/** Decimal values always require verified source provenance. Integers may
 * proceed without it only inside JavaScript's safe-integer range; signed zero
 * also requires provenance so its sign cannot be erased. */
const optionNumberIsSafe = (value: number, sourceVerified: boolean): boolean => {
  if (!Number.isFinite(value)) return false;
  if (!Number.isInteger(value)) return sourceVerified;
  return !modelOptionNumberNeedsSourceProof(value) || sourceVerified;
};

/** Require parser provenance for every advertised number: lossy input can
 * round to an apparently safe integer before this control layer sees it. */
const containsNumericMetadata = (value: unknown): boolean =>
  typeof value === "number"
    ? true
    : Array.isArray(value)
      ? value.some(containsNumericMetadata)
      : typeof value === "object" && value !== null
        && Object.values(value).some(containsNumericMetadata);

/** Parsed number and integer schemas are editable only when every advertised
 * numeric token survived protocol parsing without IEEE-754 rounding. */
const advertisedNumbersAreExact = (property: Readonly<Record<string, unknown>>): boolean =>
  !containsNumericMetadata(property) || protocolOptionSchemaNumbersAreExact(property);

const matchesScalarType = (type: AdvertisedScalarType, value: unknown): boolean =>
  type === "string"
    ? typeof value === "string"
    : type === "boolean"
      ? typeof value === "boolean"
      : typeof value === "number"
        && optionNumberIsSafe(value, true)
        && (type !== "integer" || Number.isInteger(value));

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
  if (typeof value === "number") return jsonNumberValueToken(value);
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
  if (advertised !== undefined && property["oneOf"] !== undefined) return undefined;
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
      ...(typeof value === "number"
          && modelOptionNumberNeedsSourceProof(value)
          && protocolOptionSchemaNumbersAreExact(property)
        ? { numberSource: jsonNumberValueToken(value) }
        : {}),
    }));
  }

  const oneOf = property["oneOf"];
  if (!Array.isArray(oneOf) || oneOf.length <= 1) return undefined;
  const choices: { label: string; value: ModelOptionValue }[] = [];
  for (const candidate of oneOf) {
    const entry = asRecord(candidate);
    const value = entry?.["const"];
    const entryType = entry === undefined ? null : scalarType(entry);
    if (
      entry === undefined
      || !scalar(value)
      || entryType === null
      || entryType !== undefined && !matchesScalarType(entryType, value)
      || hasUnsupportedConstraints(entry)
      || Object.hasOwn(entry, "enum")
      || Object.hasOwn(entry, "oneOf")
      || Object.hasOwn(entry, "minimum")
      || Object.hasOwn(entry, "maximum")
    ) return undefined;
    choices.push({
      value,
      label: typeof entry["title"] === "string"
        ? entry["title"]
        : valueLabel(value),
      ...(typeof value === "number"
          && modelOptionNumberNeedsSourceProof(value)
          && protocolOptionSchemaNumbersAreExact(property)
        ? { numberSource: jsonNumberValueToken(value) }
        : {}),
    });
  }
  return new Set(choices.map(({ value }) => `${typeof value}:${String(value)}`)).size
    === choices.length
    ? choices
    : undefined;
};

const optionText = (
  value: unknown,
  type?: ModelOptionScalarType,
  numberSource?: string,
): string => typeof value === "number" && Number.isFinite(value)
  ? numberSource ?? jsonNumberValueToken(value)
  : type === "number" && Number.isFinite(value as number)
    || scalar(value) ? String(value) : "";

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

interface StoredOptionValue {
  readonly value: unknown;
  readonly numberSourceVerified: boolean;
  readonly numberSource?: string;
}

const storedValues = (
  options: Readonly<Record<string, unknown>>,
  key: string,
): readonly StoredOptionValue[] => {
  const keys = !THINKING_KEYS.has(key)
    ? [key]
    : key === "thinking_level"
      ? [...THINKING_KEYS]
      : [
          key,
          ...[...THINKING_KEYS].filter((candidate) =>
            candidate !== key && candidate !== "thinking_level"
          ),
          "thinking_level",
        ];
  return keys
    .filter((candidate) => Object.hasOwn(options, candidate))
    .map((candidate) => {
      const stored = options[candidate];
      const value = key === "thinking_budget_tokens"
          && typeof stored === "string"
          && stored.trim() !== ""
        ? Number(stored)
        : stored;
      const numberSource = typeof value !== "number"
        ? undefined
        : typeof stored === "string" && jsonNumberTokenIsExact(stored, value)
          ? stored
          : protocolModelOptionNumberSource(options, candidate, stored);
      return {
        value,
        ...(numberSource === undefined ? {} : { numberSource }),
        numberSourceVerified: typeof value !== "number"
          || !modelOptionNumberNeedsSourceProof(value)
          || numberSource !== undefined,
      };
    });
};

const validTextValue = (
  type: ModelOptionScalarType,
  value: unknown,
  minimum: number | undefined,
  maximum: number | undefined,
  numberSourceVerified: boolean,
): boolean => {
  if (type === "string") return typeof value === "string";
  if (typeof value !== "number" || !optionNumberIsSafe(value, numberSourceVerified)) return false;
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
  inheritedOptions: Readonly<Record<string, unknown>> = {},
): readonly ModelOptionControl[] => {
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  if (properties === undefined) return [];
  const current = options ?? {};
  const controls: ModelOptionControl[] = [];

  for (const [key, advertised] of Object.entries(properties)) {
    const property = asRecord(advertised);
    const advertisedType = property === undefined ? null : scalarType(property);
    if (
      property === undefined
      || property["readOnly"] === true
      || Object.hasOwn(property, "const")
      || (Array.isArray(property["enum"]) && property["enum"].length <= 1)
      || advertisedType === null
      || hasUnsupportedConstraints(property)
      || !advertisedNumbersAreExact(property)
    ) continue;
    const label = typeof property["title"] === "string"
      ? property["title"]
      : optionLabel(key);
    const description = typeof property["description"] === "string"
      ? property["description"]
      : "";
    const explicit = storedValues(current, key);
    const inherited = storedValues(inheritedOptions, key);
    const defaultValue = property["default"];
    const minimum = typeof property["minimum"] === "number"
      && Number.isFinite(property["minimum"])
      ? property["minimum"]
      : undefined;
    const maximum = typeof property["maximum"] === "number"
      && Number.isFinite(property["maximum"])
      ? property["maximum"]
      : undefined;
    if (
      (minimum !== undefined || maximum !== undefined)
        && advertisedType !== "number" && advertisedType !== "integer"
      || minimum !== undefined && maximum !== undefined && minimum > maximum
    ) continue;
    const choices = choiceValues(property);
    if (
      choices === undefined
      && (Object.hasOwn(property, "enum") || Object.hasOwn(property, "oneOf"))
    ) continue;
    if (choices !== undefined) {
      const editableChoices = choices.filter(({ value }) =>
        (advertisedType === undefined || matchesScalarType(advertisedType, value))
        && (typeof value !== "number"
          || (minimum === undefined || value >= minimum)
            && (maximum === undefined || value <= maximum))
      );
      if (editableChoices.length <= 1) continue;
      const selected = explicit.find(({ value, numberSourceVerified }) =>
        (typeof value !== "number" || optionNumberIsSafe(value, numberSourceVerified))
        && editableChoices.some((choice) => Object.is(choice.value, value))
      )?.value;
      const inheritedValue = inherited.find(({ value, numberSourceVerified }) =>
        (typeof value !== "number" || optionNumberIsSafe(value, numberSourceVerified))
        && editableChoices.some((choice) => Object.is(choice.value, value))
      )?.value;
      const selectedIndex = editableChoices.findIndex(({ value }) => Object.is(value, selected));
      const inheritedIndex = editableChoices.findIndex(({ value }) =>
        Object.is(value, inheritedValue)
      );
      const defaultIndex = editableChoices.findIndex(({ value }) => Object.is(value, defaultValue));
      controls.push({
        kind: "choice",
        key,
        label,
        description,
        overridden: selectedIndex >= 0,
        choices: editableChoices,
        selectedIndex: selectedIndex >= 0
          ? selectedIndex
          : inheritedIndex >= 0 ? inheritedIndex : defaultIndex,
      });
      continue;
    }
    if (advertisedType === "boolean") {
      const selected = explicit.find(({ value }) => typeof value === "boolean")?.value
        ?? inherited.find(({ value }) => typeof value === "boolean")?.value;
      controls.push({
        kind: "boolean",
        key,
        label,
        description,
        overridden: explicit.some(({ value }) => typeof value === "boolean"),
        selected: typeof selected === "boolean"
          ? selected
          : typeof defaultValue === "boolean" ? defaultValue : undefined,
      });
      continue;
    }
    const type = advertisedType;
    if (type === undefined) continue;
    const explicitValue = explicit.find(({ value, numberSourceVerified }) =>
      validTextValue(type, value, minimum, maximum, numberSourceVerified)
    );
    const inheritedValue = inherited.find(({ value, numberSourceVerified }) =>
      validTextValue(type, value, minimum, maximum, numberSourceVerified)
    );
    const selected = explicitValue ?? inheritedValue;
    const value = selected !== undefined
      ? selected.value
      : validTextValue(type, defaultValue, minimum, maximum, true)
        ? defaultValue
        : undefined;
    controls.push({
      kind: "text",
      key,
      label,
      description,
      overridden: explicitValue !== undefined,
      scalarType: type,
      text: optionText(value, type, selected?.numberSource),
      hint: optionHint(property),
      ...(minimum === undefined ? {} : { minimum }),
      ...(maximum === undefined ? {} : { maximum }),
    });
  }
  const thinkingKey = [...THINKING_KEYS].find((key) =>
    controls.some((control) => control.key === key)
  );
  return controls.filter((control) =>
    !THINKING_KEYS.has(control.key) || control.key === thinkingKey
  );
};

export const modelOptionValueIsValid = (
  control: ModelOptionControl,
  value: unknown,
  numberSourceVerified = false,
): value is ModelOptionValue => {
  if (control.kind === "choice") {
    return (typeof value !== "number" || optionNumberIsSafe(value, numberSourceVerified))
      && control.choices.some((choice) => Object.is(choice.value, value));
  }
  if (control.kind === "boolean") return typeof value === "boolean";
  return validTextValue(
    control.scalarType,
    value,
    control.minimum,
    control.maximum,
    numberSourceVerified,
  );
};

export const modelOptionTextValue = (
  control: TextModelOptionControl,
  raw: string,
): ModelOptionValue | undefined | null => {
  if (control.scalarType === "string") return raw;
  if (raw === "") return undefined;
  const value = Number(raw);
  return jsonNumberTokenIsExact(raw, value) && modelOptionValueIsValid(control, value, true)
    ? value
    : null;
};

/** Keep only values represented by the selected model's supported scalar
 * controls. Defaults remain implicit instead of being copied into requests. */
export const sanitizeModelOptions = (
  model: ProtocolModelInfo | null | undefined,
  options: Readonly<Record<string, unknown>> | undefined,
): Readonly<Record<string, ModelOptionValue>> => {
  if (options === undefined) return {};
  const controls = modelOptionControls(model, {});
  let sanitized: Readonly<Record<string, ModelOptionValue>> = {};
  for (const control of controls) {
    const candidate = storedValues(options, control.key).find(({ value, numberSourceVerified }) =>
      modelOptionValueIsValid(control, value, numberSourceVerified)
    );
    if (
      candidate !== undefined
      && modelOptionValueIsValid(
        control,
        candidate.value,
        candidate.numberSourceVerified,
      )
    ) {
      sanitized = updateProtocolModelOption(
        sanitized,
        control.key,
        candidate.value,
        [],
        candidate.numberSource,
      );
    }
  }
  return sanitized;
};

export const changeModelOption = <T>(
  options: Readonly<Record<string, T>>,
  change: ModelOptionChangeDetail,
): Readonly<Record<string, T | ModelOptionValue>> => {
  const remove = THINKING_KEYS.has(change.key)
    ? [...THINKING_KEYS]
    : [change.key];
  return updateProtocolModelOption(
    options,
    change.key,
    change.value,
    remove,
    change.numberSource,
  );
};

/** Model selectors use the provider-qualified protocol id as their visible
 * label so identically named models from different providers stay distinct. */
export const modelSelectorLabel = (
  model: Pick<ProtocolModelInfo, "id">,
): string => model.id;
