import { thinkingOption } from "../app/new-session-model.js";
import type { ProtocolModelInfo } from "../services/protocol-client.js";

export interface EnumModelOptionControl {
  readonly key: string;
  readonly values: readonly string[];
  readonly selected: string;
}

export interface BooleanModelOptionControl {
  readonly key: "fast";
  readonly selected: boolean;
}

export interface ModelOptionControls {
  readonly thinking?: EnumModelOptionControl;
  readonly context?: EnumModelOptionControl;
  readonly fast?: BooleanModelOptionControl;
}

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;

const enumProperty = (
  model: ProtocolModelInfo | undefined,
  key: string,
): { readonly values: readonly string[]; readonly defaultValue?: string } | undefined => {
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  const property = asRecord(properties?.[key]);
  const values = property?.["enum"];
  if (
    !Array.isArray(values)
    || values.length <= 1
    || !values.every(
      (value): value is string =>
        typeof value === "string" && value !== "" && value.trim() === value,
    )
    || new Set(values).size !== values.length
  ) return undefined;
  const defaultValue = property?.["default"];
  return {
    values: [...values],
    ...(typeof defaultValue === "string" && values.includes(defaultValue)
      ? { defaultValue }
      : {}),
  };
};

const selectedEnum = (
  options: Readonly<Record<string, unknown>>,
  key: string,
  values: readonly string[],
  defaultValue?: string,
  legacyKey?: string,
): string => {
  const selected = options[key];
  if (typeof selected === "string" && values.includes(selected)) return selected;
  const legacy = legacyKey === undefined ? undefined : options[legacyKey];
  if (typeof legacy === "string" && values.includes(legacy)) return legacy;
  return defaultValue !== undefined && values.includes(defaultValue) ? defaultValue : "";
};

/** Derive only the option controls already supported by the Slint composer.
 * Model schemas are untrusted catalog data, so malformed enums are ignored. */
export const modelOptionControls = (
  model: ProtocolModelInfo | undefined,
  options: Readonly<Record<string, unknown>> | undefined,
): ModelOptionControls => {
  const current = options ?? {};
  const thinking = thinkingOption(model);
  const validThinking = thinking !== undefined && thinking.values.length > 1
    ? {
        key: thinking.key,
        values: thinking.values,
        selected: selectedEnum(
          current,
          thinking.key,
          thinking.values,
          thinking.defaultValue,
          thinking.key === "thinking_level" ? undefined : "thinking_level",
        ),
      }
    : undefined;

  const context = enumProperty(model, "context");
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  const fastProperty = asRecord(properties?.["fast"]);
  const fastDefault = fastProperty?.["default"];
  const fastCurrent = current["fast"];

  return {
    ...(validThinking === undefined ? {} : { thinking: validThinking }),
    ...(context === undefined
      ? {}
      : {
          context: {
            key: "context",
            values: context.values,
            selected: selectedEnum(
              current,
              "context",
              context.values,
              context.defaultValue,
            ),
          },
        }),
    ...(fastProperty === undefined
      ? {}
      : {
          fast: {
            key: "fast" as const,
            selected: typeof fastCurrent === "boolean"
              ? fastCurrent
              : typeof fastDefault === "boolean" && fastDefault,
          },
        }),
  };
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

/** Model selectors use the provider-qualified protocol id as their visible
 * label so identically named models from different providers stay distinct. */
export const modelSelectorLabel = (
  model: Pick<ProtocolModelInfo, "id">,
): string => model.id;
