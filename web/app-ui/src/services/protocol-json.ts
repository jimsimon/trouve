const LOSSY_NUMBER = Symbol("trouve-lossy-json-number");
const EXACT_OPTION_SCHEMA_NODES = new WeakSet<object>();
const EXACT_MODEL_OPTION_NUMBER_TOKENS = new WeakMap<object, ReadonlyMap<string, string>>();

interface ParsedNumber {
  readonly [LOSSY_NUMBER]: true;
  readonly rounded: number;
  readonly source?: string;
}

interface JsonReviverContext {
  readonly source?: string;
}

/** Persisted option numbers must never be rounded or silently removed. */
export class UnsupportedModelOptionNumberError extends TypeError {
  constructor() {
    super("model options contain a number this JavaScript runtime cannot preserve exactly");
    this.name = "UnsupportedModelOptionNumberError";
  }
}

/** True only for option-schema nodes whose numeric metadata was checked
 * against its original JSON token by parseProtocolJson. */
export const protocolOptionSchemaNumbersAreExact = (value: object): boolean =>
  EXACT_OPTION_SCHEMA_NODES.has(value);

const normalizedNumberToken = (raw: string): string => {
  const sign = raw.startsWith("-") ? "-" : "";
  const [coefficient, exponent = "0"] = raw.split(/[eE]/u);
  const [integer, fraction = ""] = coefficient!.split(".");
  const digits = `${integer}${fraction}`.replace(/^[+-]?0*/u, "");
  if (digits === "") return "0";
  const trimmed = digits.replace(/0+$/u, "");
  return `${sign}${trimmed}e${
    Number(exponent) - fraction.length + digits.length - trimmed.length
  }`;
};

/** Whether parsing a JSON number token produced exactly the value that would
 * be serialized back onto the wire. Equivalent spellings such as `1e3` and
 * `1000` compare equal; rounded integers and high-precision decimals do not. */
export const jsonNumberTokenIsExact = (raw: string, value: number): boolean => {
  if (!Number.isFinite(value)) return false;
  const serialized = JSON.stringify(value);
  return serialized !== undefined
    && normalizedNumberToken(raw) === normalizedNumberToken(serialized);
};

/** Whether a model-option number still has a verified source token. Safe
 * integers do not require provenance; every other Number does. */
export const protocolModelOptionNumberIsExact = (
  options: object,
  key: string,
  value: unknown,
): boolean => {
  if (typeof value !== "number" || Number.isSafeInteger(value)) return true;
  const source = EXACT_MODEL_OPTION_NUMBER_TOKENS.get(options)?.get(key);
  return source !== undefined && jsonNumberTokenIsExact(source, value);
};

/** Clone an option map without discarding verified number-token provenance. */
export const copyProtocolModelOptions = <Value>(
  options: Readonly<Record<string, Value>>,
): Record<string, Value> => {
  const copy = { ...options };
  const tokens = EXACT_MODEL_OPTION_NUMBER_TOKENS.get(options);
  if (tokens !== undefined) {
    EXACT_MODEL_OPTION_NUMBER_TOKENS.set(copy, new Map(tokens));
  }
  return copy;
};

/** Apply one control-validated option change while retaining numeric source
 * proof for the resulting map. */
export const updateProtocolModelOption = <Existing, Value>(
  options: Readonly<Record<string, Existing>>,
  key: string,
  value: Value | undefined,
  remove: readonly string[] = [],
  numberSource?: {
    readonly options: object;
    readonly key: string;
  },
): Record<string, Existing | Value> => {
  const next: Record<string, Existing | Value> = copyProtocolModelOptions(options);
  const tokens = new Map(EXACT_MODEL_OPTION_NUMBER_TOKENS.get(next) ?? []);
  for (const removed of remove) {
    delete next[removed];
    tokens.delete(removed);
  }
  if (value === undefined) {
    delete next[key];
    tokens.delete(key);
  } else {
    next[key] = value;
    if (typeof value === "number") {
      const retainedSource = numberSource === undefined
        ? undefined
        : EXACT_MODEL_OPTION_NUMBER_TOKENS.get(numberSource.options)?.get(numberSource.key);
      const source = retainedSource !== undefined
          && jsonNumberTokenIsExact(retainedSource, value)
        ? retainedSource
        : JSON.stringify(value);
      if (source !== undefined && jsonNumberTokenIsExact(source, value)) {
        tokens.set(key, source);
      }
    } else {
      tokens.delete(key);
    }
  }
  if (tokens.size > 0) EXACT_MODEL_OPTION_NUMBER_TOKENS.set(next, tokens);
  return next;
};

const isParsedNumber = (value: unknown): value is ParsedNumber =>
  typeof value === "object"
  && value !== null
  && (value as Partial<ParsedNumber>)[LOSSY_NUMBER] === true;

const containsUnverifiedNumber = (value: unknown): boolean =>
  isParsedNumber(value) && value.source === undefined
  || Array.isArray(value) && value.some(containsUnverifiedNumber)
  || typeof value === "object" && value !== null
    && Object.values(value).some(containsUnverifiedNumber);

const restoreOrdinaryNumbers = (value: unknown): unknown => {
  if (isParsedNumber(value)) return value.rounded;
  if (Array.isArray(value)) return value.map(restoreProtocolValue);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(Object.entries(value).map(([key, child]) => [
    key,
    restoreProtocolValue(child),
  ]));
};

const sanitizeOptionsSchema = (value: unknown): unknown => {
  if (isParsedNumber(value)) {
    return value.source === undefined ? null : value.rounded;
  }
  if (Array.isArray(value)) return value.map(sanitizeOptionsSchema);
  if (typeof value !== "object" || value === null) return value;
  const sanitized: Record<string, unknown> = {};
  for (const [key, child] of Object.entries(value)) {
    if (key !== "properties" || typeof child !== "object" || child === null) {
      sanitized[key] = sanitizeOptionsSchema(child);
      continue;
    }
    sanitized[key] = Object.fromEntries(
      Object.entries(child).flatMap(([property, schema]) =>
        containsUnverifiedNumber(schema)
          ? []
          : [[property, sanitizeOptionsSchema(schema)] as const]
      ),
    );
  }
  EXACT_OPTION_SCHEMA_NODES.add(sanitized);
  return sanitized;
};

const sanitizeModelOptions = (value: unknown): unknown => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return restoreOrdinaryNumbers(value);
  }
  const restored: Record<string, unknown> = {};
  const tokens = new Map<string, string>();
  for (const [key, child] of Object.entries(value)) {
    if (containsUnverifiedNumber(child)) throw new UnsupportedModelOptionNumberError();
    if (isParsedNumber(child) && child.source !== undefined) tokens.set(key, child.source);
    restored[key] = restoreOrdinaryNumbers(child);
  }
  if (tokens.size > 0) EXACT_MODEL_OPTION_NUMBER_TOKENS.set(restored, tokens);
  return restored;
};

const restoreProtocolValue = (value: unknown): unknown => {
  if (isParsedNumber(value)) return value.rounded;
  if (Array.isArray(value)) return value.map(restoreProtocolValue);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(Object.entries(value).map(([key, child]) => [
    key,
    key === "options_schema"
      ? sanitizeOptionsSchema(child)
      : key === "model_options"
        ? sanitizeModelOptions(child)
        : restoreProtocolValue(child),
  ]));
};

/** Parse protocol JSON without allowing JavaScript number rounding to mutate
 * model-specific controls or persisted option values. Lossy schema properties
 * are hidden; a lossy persisted option rejects the response so a later
 * replacement update cannot erase it. Runtimes without reviver source tokens
 * therefore reject responses that contain numeric model options. */
export const parseProtocolJson = (text: string): unknown => {
  const parsed = JSON.parse(text, ((
    _key: string,
    value: unknown,
    context?: JsonReviverContext,
  ): unknown => {
    if (typeof value !== "number") return value;
    const source = context?.source;
    return {
      [LOSSY_NUMBER]: true,
      rounded: value,
      ...(source !== undefined && jsonNumberTokenIsExact(source, value) ? { source } : {}),
    } satisfies ParsedNumber;
  }) as (this: unknown, key: string, value: unknown) => unknown);
  return restoreProtocolValue(parsed);
};

const serializeProtocolValue = (
  value: unknown,
  parent: object | undefined,
  key: string,
  ancestors: Set<object>,
): string | undefined => {
  if (value === null) return "null";
  if (typeof value === "number") {
    const source = parent === undefined
      ? undefined
      : EXACT_MODEL_OPTION_NUMBER_TOKENS.get(parent)?.get(key);
    return source !== undefined && jsonNumberTokenIsExact(source, value)
      ? source
      : JSON.stringify(value);
  }
  if (typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value !== "object") return undefined;
  if (ancestors.has(value)) throw new TypeError("protocol request body contains a cycle");
  ancestors.add(value);
  let encoded: string;
  if (Array.isArray(value)) {
    encoded = `[${value.map((child, index) =>
      serializeProtocolValue(child, value, String(index), ancestors) ?? "null"
    ).join(",")}]`;
  } else {
    encoded = `{${Object.entries(value).flatMap(([childKey, child]) => {
      const serialized = serializeProtocolValue(child, value, childKey, ancestors);
      return serialized === undefined
        ? []
        : [`${JSON.stringify(childKey)}:${serialized}`];
    }).join(",")}}`;
  }
  ancestors.delete(value);
  return encoded;
};

/** Serialize protocol requests while emitting verified model-option number
 * tokens verbatim. Protocol request bodies are plain JSON values. */
export const stringifyProtocolJson = (value: unknown): string => {
  const serialized = serializeProtocolValue(value, undefined, "", new Set());
  if (serialized === undefined) throw new TypeError("protocol request body is not JSON serializable");
  return serialized;
};
