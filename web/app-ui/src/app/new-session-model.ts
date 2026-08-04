import type {
  ProtocolAgentMode,
  ProtocolCreateThreadRequest,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";

export const NEW_SESSION_TITLE_MAX_LENGTH = 48;
export const NEW_SESSION_TITLE_FALLBACK = "New session";

type ThinkingOptionKey =
  | "thinking_level"
  | "reasoning_effort"
  | "effort"
  | "reasoning";

export interface ThinkingOption {
  readonly key: ThinkingOptionKey;
  readonly values: readonly string[];
  readonly defaultValue?: string;
}

export interface NewSessionThreadRequestInput {
  readonly sessionId: string;
  readonly mode?: string | null;
  readonly model?: string | null;
  /** Raw form value; only protocol-advertised permission modes are emitted. */
  readonly permissionMode?: string | null;
  readonly thinking?: string | null;
  /** Metadata for the effective model whose thinking option is being overridden. */
  readonly modelInfo?: ProtocolModelInfo | null;
}

const asRecord = (value: unknown): Record<string, unknown> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;

const nonEmpty = (value: string | null | undefined): string | undefined => {
  const trimmed = value?.trim();
  return trimmed === undefined || trimmed === "" ? undefined : trimmed;
};

/**
 * Derives the same bounded first-line fallback as the retained desktop UI,
 * while additionally removing invisible controls and avoiding a split UTF-16
 * surrogate pair at the title boundary.
 */
export const sessionTitleFallback = (prompt: string): string => {
  const sanitized = prompt
    .replace(/[\u0000-\u0009\u000b\u000c\u000e-\u001f\u007f-\u009f\u200b\u200e\u200f\u202a-\u202e\u2066-\u2069]+/gu, " ")
    .trim();
  const normalized = (sanitized.split(/\r\n?|\n/u)[0] ?? "")
    .replace(/\s+/gu, " ")
    .trim();
  if (normalized === "") return NEW_SESSION_TITLE_FALLBACK;

  const title = Array.from(normalized)
    .slice(0, NEW_SESSION_TITLE_MAX_LENGTH)
    .join("")
    .trimEnd();
  return title === "" ? NEW_SESSION_TITLE_FALLBACK : title;
};

/** Returns the first valid thinking option in the same precedence as Slint. */
export const thinkingOption = (
  model: ProtocolModelInfo | null | undefined,
): ThinkingOption | undefined => {
  const schema = asRecord(model?.options_schema);
  const properties = asRecord(schema?.["properties"]);
  if (properties === undefined) return undefined;

  for (const key of [
    "thinking_level",
    "reasoning_effort",
    "effort",
    "reasoning",
  ] as const) {
    const property = asRecord(properties[key]);
    if (property === undefined) continue;
    if (property["type"] !== undefined && property["type"] !== "string") continue;

    const advertised = property["enum"];
    if (
      !Array.isArray(advertised)
      || advertised.length === 0
      || !advertised.every(
        (value): value is string =>
          typeof value === "string" && value !== "" && value.trim() === value,
      )
      || new Set(advertised).size !== advertised.length
    ) {
      continue;
    }

    const hasDefault = Object.prototype.hasOwnProperty.call(property, "default");
    const defaultValue = property["default"];
    if (
      hasDefault
      && (typeof defaultValue !== "string" || !advertised.includes(defaultValue))
    ) {
      continue;
    }

    return {
      key,
      values: [...advertised],
      ...(hasDefault ? { defaultValue: defaultValue as string } : {}),
    };
  }
  return undefined;
};

/** Resolves the model shown for a new session in protocol precedence order. */
export const resolveNewSessionModel = (
  explicitModel: string | null | undefined,
  selectedMode: ProtocolAgentMode | null | undefined,
  providers: ProtocolProvidersResponse | null | undefined,
): string | undefined =>
  nonEmpty(explicitModel)
  ?? nonEmpty(selectedMode?.default_model)
  ?? nonEmpty(providers?.default_model);

/**
 * Builds the initial thread request while preserving server-side inheritance.
 * A thinking override is sent only when the effective model advertises both its
 * option key and the selected enum value.
 */
export const createNewSessionThreadRequest = (
  input: NewSessionThreadRequestInput,
): ProtocolCreateThreadRequest => {
  const sessionId = nonEmpty(input.sessionId);
  if (sessionId === undefined) {
    throw new TypeError("A nonempty session id is required to create a thread.");
  }

  const mode = nonEmpty(input.mode);
  const model = nonEmpty(input.model);
  const permissionMode = input.permissionMode;
  const validPermission = permissionMode === "ask"
    || permissionMode === "allow_list"
    || permissionMode === "yolo";

  const advertisedThinking = model === undefined || input.modelInfo?.id === model
    ? thinkingOption(input.modelInfo)
    : undefined;
  const thinking = nonEmpty(input.thinking);
  const modelOptions = thinking !== undefined
    && advertisedThinking?.values.includes(thinking) === true
    ? { [advertisedThinking.key]: thinking }
    : undefined;

  return {
    session_id: sessionId,
    ...(mode === undefined ? {} : { mode }),
    ...(model === undefined ? {} : { model }),
    ...(validPermission ? { permission_mode: permissionMode } : {}),
    ...(modelOptions === undefined ? {} : { model_options: modelOptions }),
  };
};
