const LOSSY_NUMBER = Symbol("trouve-lossy-json-number");

interface LossyNumber {
  readonly [LOSSY_NUMBER]: true;
  readonly rounded: number;
}

interface JsonReviverContext {
  readonly source?: string;
}

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

const isLossyNumber = (value: unknown): value is LossyNumber =>
  typeof value === "object"
  && value !== null
  && (value as Partial<LossyNumber>)[LOSSY_NUMBER] === true;

const containsLossyNumber = (value: unknown): boolean =>
  isLossyNumber(value)
  || Array.isArray(value) && value.some(containsLossyNumber)
  || typeof value === "object" && value !== null
    && Object.values(value).some(containsLossyNumber);

const restoreOrdinaryNumbers = (value: unknown): unknown => {
  if (isLossyNumber(value)) return value.rounded;
  if (Array.isArray(value)) return value.map(restoreProtocolValue);
  if (typeof value !== "object" || value === null) return value;
  return Object.fromEntries(Object.entries(value).map(([key, child]) => [
    key,
    restoreProtocolValue(child),
  ]));
};

const sanitizeOptionsSchema = (value: unknown): unknown => {
  if (isLossyNumber(value)) return null;
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
        containsLossyNumber(schema)
          ? []
          : [[property, sanitizeOptionsSchema(schema)] as const]
      ),
    );
  }
  return sanitized;
};

const sanitizeModelOptions = (value: unknown): unknown => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return restoreOrdinaryNumbers(value);
  }
  return Object.fromEntries(Object.entries(value).flatMap(([key, child]) =>
    containsLossyNumber(child)
      ? []
      : [[key, restoreOrdinaryNumbers(child)] as const]
  ));
};

const restoreProtocolValue = (value: unknown): unknown => {
  if (isLossyNumber(value)) return value.rounded;
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
 * model-specific controls or persisted option values. Modern engines expose
 * each primitive's source token to the reviver. When they do not, every
 * numeric option token is treated conservatively as potentially lossy. */
export const parseProtocolJson = (text: string): unknown => {
  const parsed = JSON.parse(text, ((
    _key: string,
    value: unknown,
    context?: JsonReviverContext,
  ): unknown => {
    if (typeof value !== "number") return value;
    return context?.source !== undefined
        && jsonNumberTokenIsExact(context.source, value)
      ? value
      : { [LOSSY_NUMBER]: true, rounded: value } satisfies LossyNumber;
  }) as (this: unknown, key: string, value: unknown) => unknown);
  return restoreProtocolValue(parsed);
};
