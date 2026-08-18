import type {
  ProtocolAgentPersona,
  ProtocolCreateThreadRequest,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";

export const NEW_SESSION_TITLE_MAX_LENGTH = 48;
export const NEW_SESSION_TITLE_FALLBACK = "New session";
export const NEW_THREAD_TITLE_FALLBACK = "New thread";

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

export type ResolvedPermissionMode = "ask" | "allow_list" | "yolo";

export interface ResolvedNewThreadDefaults {
  readonly modeId: string;
  readonly modelId: string;
  readonly thinking: string;
  readonly permissionMode: ResolvedPermissionMode;
  /** Authoritative persona/global values that may remain server-inherited. */
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: ResolvedPermissionMode | undefined;
}

export interface NewThreadInheritance {
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: ResolvedPermissionMode | undefined;
}

export interface NewThreadOptionEdits {
  readonly mode: boolean;
  readonly model: boolean;
  readonly thinking: boolean;
  readonly permission: boolean;
}

export interface NewThreadOptionSelections {
  readonly modeId: string;
  readonly modelId: string;
  readonly thinking: string;
  readonly permissionMode: string;
}

export interface NewSessionThreadRequestInput {
  readonly sessionId: string;
  readonly title?: string | null;
  readonly mode?: string | null;
  readonly model?: string | null;
  /** Raw form value; only protocol-advertised permission modes are emitted. */
  readonly permissionMode?: string | null;
  readonly thinking?: string | null;
  /** Effective inherited values shown by the form. Matching selections stay server-inherited. */
  readonly inheritedPermissionMode?: string | null;
  readonly inheritedThinking?: string | null;
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
const promptTitleFallback = (prompt: string, fallback: string): string => {
  const sanitized = prompt
    .replace(/[\u0000-\u0009\u000b\u000c\u000e-\u001f\u007f-\u009f\u200b\u200e\u200f\u202a-\u202e\u2066-\u2069]+/gu, " ")
    .trim();
  const normalized = (sanitized.split(/\r\n?|\n/u)[0] ?? "")
    .replace(/\s+/gu, " ")
    .trim();
  if (normalized === "") return fallback;

  const title = Array.from(normalized)
    .slice(0, NEW_SESSION_TITLE_MAX_LENGTH)
    .join("")
    .trimEnd();
  return title === "" ? fallback : title;
};

export const sessionTitleFallback = (prompt: string): string =>
  promptTitleFallback(prompt, NEW_SESSION_TITLE_FALLBACK);

export const threadTitleFallback = (prompt: string): string =>
  promptTitleFallback(prompt, NEW_THREAD_TITLE_FALLBACK);

/** Returns the first valid thinking option in the established precedence. */
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
  selectedMode: ProtocolAgentPersona | null | undefined,
  providers: ProtocolProvidersResponse | null | undefined,
): string | undefined =>
  nonEmpty(explicitModel)
  ?? nonEmpty(selectedMode?.default_model)
  ?? nonEmpty(providers?.default_model);

const validPermissionMode = (value: unknown): ResolvedPermissionMode | undefined =>
  value === "ask" || value === "allow_list" || value === "yolo" ? value : undefined;

/** Resolve displayed defaults and identify values backed by persona/global metadata. */
export const resolveNewThreadDefaults = (
  modes: readonly ProtocolAgentPersona[],
  models: readonly ProtocolModelInfo[],
  providers: ProtocolProvidersResponse | null | undefined,
  overrides: { readonly modeId?: string; readonly modelId?: string } = {},
): ResolvedNewThreadDefaults => {
  const requestedModeId = nonEmpty(overrides.modeId);
  const mode = modes.find((candidate) => candidate.id === requestedModeId)
    ?? modes.find((candidate) => candidate.id === "code")
    ?? modes[0];
  const resolvedModelId = resolveNewSessionModel(overrides.modelId, mode, providers);
  const modelId = resolvedModelId !== undefined
      && models.some((candidate) => candidate.id === resolvedModelId)
    ? resolvedModelId
    : models[0]?.id ?? "";
  const model = models.find((candidate) => candidate.id === modelId);
  const option = thinkingOption(model);
  const inheritedThinking = [
    nonEmpty(mode?.default_thinking_level),
    nonEmpty(providers?.default_thinking_level),
  ].find((candidate): candidate is string =>
    candidate !== undefined && option?.values.includes(candidate) === true);
  const thinking = inheritedThinking
    ?? option?.defaultValue
    ?? option?.values[0]
    ?? "";
  const inheritedPermissionMode = validPermissionMode(mode?.default_permission_mode)
    ?? validPermissionMode(providers?.default_permission_mode);
  const permissionMode = inheritedPermissionMode ?? "ask";
  return {
    modeId: mode?.id ?? "code",
    modelId,
    thinking,
    permissionMode,
    inheritedThinking,
    inheritedPermissionMode,
  };
};

export const createNewThreadOptionEdits = (): NewThreadOptionEdits => ({
  mode: false,
  model: false,
  thinking: false,
  permission: false,
});

/** Merge a refreshed catalog into fields the user has not edited. */
export const reconcileNewThreadDefaults = (
  selections: NewThreadOptionSelections,
  modes: readonly ProtocolAgentPersona[],
  models: readonly ProtocolModelInfo[],
  providers: ProtocolProvidersResponse | null | undefined,
  edits: NewThreadOptionEdits,
): ResolvedNewThreadDefaults => {
  const initial = resolveNewThreadDefaults(modes, models, providers);
  const modeId = edits.mode && modes.some((mode) => mode.id === selections.modeId)
    ? selections.modeId
    : initial.modeId;
  const modeDefaults = resolveNewThreadDefaults(modes, models, providers, { modeId });
  const modelId = edits.model && models.some((model) => model.id === selections.modelId)
    ? selections.modelId
    : modeDefaults.modelId;
  const refreshed = resolveNewThreadDefaults(modes, models, providers, { modeId, modelId });
  const option = thinkingOption(models.find((model) => model.id === refreshed.modelId));
  const keepThinking = edits.thinking
    && option?.values.includes(selections.thinking) === true;
  const permissionMode = validPermissionMode(selections.permissionMode);
  const keepPermission = edits.permission && permissionMode !== undefined;

  return {
    ...refreshed,
    thinking: keepThinking ? selections.thinking : refreshed.thinking,
    inheritedThinking: keepThinking ? undefined : refreshed.inheritedThinking,
    permissionMode: keepPermission ? permissionMode : refreshed.permissionMode,
    inheritedPermissionMode: keepPermission
      ? undefined
      : refreshed.inheritedPermissionMode,
  };
};

/** Only metadata loaded successfully for the selected workspace is authoritative. */
export const newThreadInheritanceForWorkspace = (
  defaults: ResolvedNewThreadDefaults,
  catalogWorkspaceId: string,
  selectedWorkspaceId: string,
): NewThreadInheritance =>
  catalogWorkspaceId !== "" && catalogWorkspaceId === selectedWorkspaceId
    ? {
        inheritedThinking: defaults.inheritedThinking,
        inheritedPermissionMode: defaults.inheritedPermissionMode,
      }
    : {
        inheritedThinking: undefined,
        inheritedPermissionMode: undefined,
      };

/** Prefer an explicit base, then the repository HEAD/default, then conventional trunks. */
export const resolveNewSessionBaseRef = (
  branches: readonly string[],
  preferredBaseRef = "",
  repositoryHead = "",
): string => {
  const preferred = nonEmpty(preferredBaseRef);
  if (preferred !== undefined && branches.includes(preferred)) return preferred;
  const head = nonEmpty(repositoryHead);
  if (head !== undefined) return branches.includes(head) ? head : "HEAD";
  if (branches.includes("main")) return "main";
  if (branches.includes("master")) return "master";
  return "HEAD";
};

/**
 * Builds the initial thread request while preserving authoritative inheritance.
 * Values without a captured persona/global source are emitted so the request
 * always matches what the form displayed.
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
  const title = nonEmpty(input.title);
  const permissionMode = input.permissionMode;
  const validPermission = permissionMode === "ask"
    || permissionMode === "allow_list"
    || permissionMode === "yolo";
  const inheritedPermission = validPermissionMode(input.inheritedPermissionMode);
  const permissionOverride = validPermission && permissionMode !== inheritedPermission;

  const advertisedThinking = model === undefined || input.modelInfo?.id === model
    ? thinkingOption(input.modelInfo)
    : undefined;
  const thinking = nonEmpty(input.thinking);
  const inheritedThinking = nonEmpty(input.inheritedThinking);
  const modelOptions = thinking !== undefined
    && thinking !== inheritedThinking
    && advertisedThinking?.values.includes(thinking) === true
    ? { [advertisedThinking.key]: thinking }
    : undefined;

  return {
    session_id: sessionId,
    ...(title === undefined ? {} : { title }),
    ...(mode === undefined ? {} : { mode }),
    ...(model === undefined ? {} : { model }),
    ...(permissionOverride ? { permission_mode: permissionMode } : {}),
    ...(modelOptions === undefined ? {} : { model_options: modelOptions }),
  };
};
