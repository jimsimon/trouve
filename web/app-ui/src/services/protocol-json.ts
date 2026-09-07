const LOSSY_NUMBER = Symbol("trouve-lossy-json-number");
const EXACT_OPTION_SCHEMA_NODES = new WeakSet<object>();
const EXACT_MODEL_OPTION_NUMBER_TOKENS = new WeakMap<object, ReadonlyMap<string, string>>();
const JSON_NUMBER_TOKEN = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$/u;
const JSON_NUMBER_PREFIX = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/u;

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
  if (digits === "") return `${sign}0`;
  const trimmed = digits.replace(/0+$/u, "");
  return `${sign}${trimmed}e${
    Number(exponent) - fraction.length + digits.length - trimmed.length
  }`;
};

/** The JSON token this runtime emits for a number. Preserve signed zero,
 * which native JSON.stringify otherwise changes to positive zero. */
export const jsonNumberValueToken = (value: number): string =>
  Object.is(value, -0) ? "-0" : JSON.stringify(value);

/** Every decimal, unsafe integer, and signed zero requires its verified JSON
 * source token before it can be used as a model option value. Keeping the
 * decimal branch explicit prevents a rounded Number from being mistaken for
 * the exact decimal advertised by a provider. */
export const modelOptionNumberNeedsSourceProof = (value: number): boolean =>
  !Number.isInteger(value)
  || !Number.isSafeInteger(value)
  || Object.is(value, -0);

/** Whether parsing a JSON number token produced exactly the value that would
 * be serialized back onto the wire. Equivalent spellings such as `1e3` and
 * `1000` compare equal; rounded integers and high-precision decimals do not. */
export const jsonNumberTokenIsExact = (raw: string, value: number): boolean => {
  if (!Number.isFinite(value) || !JSON_NUMBER_TOKEN.test(raw)) return false;
  return normalizedNumberToken(raw) === normalizedNumberToken(jsonNumberValueToken(value));
};

/** Return a still-valid source token for one model-option number. */
export const protocolModelOptionNumberSource = (
  options: object,
  key: string,
  value: unknown,
): string | undefined => {
  if (typeof value !== "number") return undefined;
  const source = EXACT_MODEL_OPTION_NUMBER_TOKENS.get(options)?.get(key);
  return source !== undefined && jsonNumberTokenIsExact(source, value) ? source : undefined;
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
  numberSource?: string,
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
    tokens.delete(key);
    if (typeof value === "number") {
      if (numberSource !== undefined && jsonNumberTokenIsExact(numberSource, value)) {
        tokens.set(key, numberSource);
      }
    }
  }
  if (tokens.size > 0) {
    EXACT_MODEL_OPTION_NUMBER_TOKENS.set(next, tokens);
  } else {
    EXACT_MODEL_OPTION_NUMBER_TOKENS.delete(next);
  }
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

const containsString = (value: unknown, candidate: string): boolean => {
  if (value === candidate) return true;
  if (Array.isArray(value)) return value.some((child) => containsString(child, candidate));
  if (typeof value !== "object" || value === null) return false;
  return Object.entries(value).some(([key, child]) =>
    key === candidate || containsString(child, candidate)
  );
};

/** Replace number tokens outside JSON strings with uniquely tagged objects.
 * The second native parse then exposes each original token to runtimes whose
 * reviver does not implement the ES2023 `context.source` argument. */
const parseWithNumberTokenFallback = (text: string, firstParse: unknown): unknown => {
  let marker = "\0trouve-json-number";
  while (containsString(firstParse, marker)) marker += "\0";
  const markerJson = JSON.stringify(marker);
  let transformed = "";
  let inString = false;
  let escaped = false;
  for (let index = 0; index < text.length;) {
    const character = text[index]!;
    if (inString) {
      transformed += character;
      index += 1;
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === "\"") inString = false;
      continue;
    }
    if (character === "\"") {
      inString = true;
      transformed += character;
      index += 1;
      continue;
    }
    if (character === "-" || character >= "0" && character <= "9") {
      const source = JSON_NUMBER_PREFIX.exec(text.slice(index))?.[0];
      if (source !== undefined) {
        transformed += `{${markerJson}:${JSON.stringify(source)}}`;
        index += source.length;
        continue;
      }
    }
    transformed += character;
    index += 1;
  }
  return JSON.parse(transformed, (_key: string, value: unknown): unknown => {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return value;
    const record = value as Record<string, unknown>;
    const keys = Object.keys(record);
    const source = record[marker];
    if (keys.length !== 1 || keys[0] !== marker || typeof source !== "string") {
      return value;
    }
    const rounded = Number(source);
    return {
      [LOSSY_NUMBER]: true,
      rounded,
      ...(jsonNumberTokenIsExact(source, rounded) ? { source } : {}),
    } satisfies ParsedNumber;
  });
};

/** Parse protocol JSON without allowing JavaScript number rounding to mutate
 * model-specific controls or persisted option values. Lossy schema properties
 * are hidden; a lossy persisted option rejects the response so a later
 * replacement update cannot erase it. Runtimes without reviver source tokens
 * recover the tokens with a lexical fallback before restoring protocol data. */
export const parseProtocolJson = (text: string): unknown => {
  let sourceUnavailable = false;
  const parsed = JSON.parse(text, ((
    _key: string,
    value: unknown,
    context?: JsonReviverContext,
  ): unknown => {
    if (typeof value !== "number") return value;
    const source = context?.source;
    if (source === undefined) sourceUnavailable = true;
    return {
      [LOSSY_NUMBER]: true,
      rounded: value,
      ...(source !== undefined && jsonNumberTokenIsExact(source, value) ? { source } : {}),
    } satisfies ParsedNumber;
  }) as (this: unknown, key: string, value: unknown) => unknown);
  return restoreProtocolValue(
    sourceUnavailable ? parseWithNumberTokenFallback(text, parsed) : parsed,
  );
};

/** A model-option number paired with the exact JSON token that produced it.
 * Keeping these values together prevents editor code from accepting a rounded
 * Number and later attaching unrelated provenance. */
export interface ExactModelOptionNumber {
  readonly value: number;
  readonly source: string;
}

/** Parse one editable model-option number through the same exact-token path as
 * protocol responses. Lossy integers, high-precision decimals, underflow, and
 * overflow are rejected before the value can enter editor state. */
export const parseExactModelOptionNumber = (
  source: string,
): ExactModelOptionNumber | undefined => {
  if (!JSON_NUMBER_TOKEN.test(source)) return undefined;
  try {
    const parsed = parseProtocolJson(
      `{"model_options":{"value":${source}}}`,
    ) as { readonly model_options?: Readonly<Record<string, unknown>> };
    const options = parsed.model_options;
    const value = options?.["value"];
    if (options === undefined || typeof value !== "number") return undefined;
    const preservedSource = protocolModelOptionNumberSource(options, "value", value);
    return preservedSource === source ? { value, source } : undefined;
  } catch {
    return undefined;
  }
};

const serializeProtocolValue = (
  value: unknown,
  parent: object | undefined,
  key: string,
  ancestors: Set<object>,
  modelOptionMap = false,
): string | undefined => {
  if (value === null) return "null";
  if (typeof value === "number") {
    const source = parent === undefined
      ? undefined
      : protocolModelOptionNumberSource(parent, key, value);
    if (modelOptionMap && modelOptionNumberNeedsSourceProof(value) && source === undefined) {
      throw new UnsupportedModelOptionNumberError();
    }
    return source ?? jsonNumberValueToken(value);
  }
  if (typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value !== "object") return undefined;
  if (ancestors.has(value)) throw new TypeError("protocol request body contains a cycle");
  ancestors.add(value);
  let encoded: string;
  if (Array.isArray(value)) {
    encoded = `[${value.map((child, index) =>
      serializeProtocolValue(child, value, String(index), ancestors, modelOptionMap) ?? "null"
    ).join(",")}]`;
  } else {
    encoded = `{${Object.entries(value).flatMap(([childKey, child]) => {
      const serialized = serializeProtocolValue(
        child,
        value,
        childKey,
        ancestors,
        modelOptionMap || childKey === "model_options",
      );
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
