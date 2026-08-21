import type {
  ProtocolAgentPersona,
  ProtocolCreateThreadRequest,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
} from "../services/protocol-client.js";

export const NEW_SESSION_TITLE_MAX_LENGTH = 48;
export const NEW_SESSION_TITLE_FALLBACK = "New session";
export const NEW_THREAD_TITLE_FALLBACK = "New thread";
export const NEW_SESSION_OPTIONS_TIMEOUT_MS = 10_000;

type ThinkingOptionKey =
  | "thinking_level"
  | "reasoning_effort"
  | "effort"
  | "reasoning"
  | "thinking_budget_tokens";

export interface ThinkingBudget {
  readonly minimum: number;
  readonly maximum?: number;
}

export interface ThinkingOption {
  readonly key: ThinkingOptionKey;
  readonly values: readonly string[];
  readonly defaultValue?: string;
  readonly budget?: ThinkingBudget;
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

export type NewSessionOptionsStatus =
  | "unloaded"
  | "loading"
  | "refreshing"
  | "ready"
  | "failed"
  | "timed-out";

export interface NewSessionOptionsLifecycle {
  readonly status: NewSessionOptionsStatus;
  /** Workspace targeted by the current or most recently settled request. */
  readonly workspaceId: string;
  /** Workspace whose successfully loaded catalog remains authoritative. */
  readonly catalogWorkspaceId: string;
}

export interface NewSessionOptionLoadState {
  readonly lifecycle: NewSessionOptionsLifecycle;
  readonly edits: NewThreadOptionEdits;
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: string | undefined;
}

export interface NewSessionSubmissionSnapshotInput {
  readonly selections: NewThreadOptionSelections;
  readonly edits: NewThreadOptionEdits;
  readonly modes: readonly ProtocolAgentPersona[];
  readonly providers: ProtocolProvidersResponse | null | undefined;
  readonly selectableModels: readonly ProtocolModelInfo[];
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: string | undefined;
  readonly optionsAuthoritative: boolean;
}

export interface NewSessionSubmissionSnapshot extends NewThreadOptionSelections {
  readonly edits: NewThreadOptionEdits;
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: string | undefined;
  readonly modelInfo: ProtocolModelInfo | undefined;
  readonly optionsAuthoritative: boolean;
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

  const budget = asRecord(properties["thinking_budget_tokens"]);
  if (budget?.["type"] === "integer" || budget?.["type"] === "number") {
    const advertisedMinimum = budget["minimum"];
    const minimum = typeof advertisedMinimum === "number"
        && Number.isSafeInteger(advertisedMinimum)
        && advertisedMinimum >= 0
      ? advertisedMinimum
      : 1;
    const advertisedMaximum = budget["maximum"];
    const maximum = typeof advertisedMaximum === "number"
        && Number.isSafeInteger(advertisedMaximum)
        && advertisedMaximum >= minimum
      ? advertisedMaximum
      : undefined;
    const advertisedDefault = budget["default"];
    const defaultValue = typeof advertisedDefault === "number"
        && Number.isSafeInteger(advertisedDefault)
        && advertisedDefault >= minimum
        && (maximum === undefined || advertisedDefault <= maximum)
      ? String(advertisedDefault)
      : undefined;
    return {
      key: "thinking_budget_tokens",
      values: [],
      ...(defaultValue === undefined ? {} : { defaultValue }),
      budget: {
        minimum,
        ...(maximum === undefined ? {} : { maximum }),
      },
    };
  }
  return undefined;
};

/** Validate a persisted/form thinking token against its model-advertised control. */
export const thinkingSelectionIsValid = (
  option: ThinkingOption | null | undefined,
  value: string | null | undefined,
): boolean => {
  const selected = nonEmpty(value);
  if (option == null || selected === undefined) return false;
  if (option.values.includes(selected)) return true;
  if (option.budget === undefined || !/^\d+$/u.test(selected)) return false;
  const budget = Number(selected);
  return Number.isSafeInteger(budget)
    && budget >= option.budget.minimum
    && (option.budget.maximum === undefined || budget <= option.budget.maximum);
};

/** Resolve a valid configured value, schema default, or first/minimum option. */
export const defaultThinkingSelection = (
  option: ThinkingOption | null | undefined,
  configured?: string | null,
): string => {
  if (thinkingSelectionIsValid(option, configured)) return configured?.trim() ?? "";
  if (thinkingSelectionIsValid(option, option?.defaultValue)) return option?.defaultValue ?? "";
  if (option?.budget !== undefined) return String(option.budget.minimum);
  return option?.values[0] ?? "";
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

/** Use settled live availability without allowing it to replace static metadata. */
export const mergeNewSessionModelCatalogs = (
  staticModels: readonly ProtocolModelInfo[],
  liveModels: readonly ProtocolModelInfo[],
  liveLoaded: boolean,
  preservedModelId?: string,
): readonly ProtocolModelInfo[] => {
  if (!liveLoaded) return staticModels;
  const staticById = new Map(staticModels.map((model) => [model.id, model]));
  const merged = liveModels.map((model) => staticById.get(model.id) ?? model);
  const preserved = nonEmpty(preservedModelId);
  if (preserved !== undefined && !merged.some((model) => model.id === preserved)) {
    const metadata = staticById.get(preserved);
    if (metadata !== undefined) merged.push(metadata);
  }
  return merged
    .sort((left, right) => left.id.localeCompare(right.id));
};

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
  const automaticModelId = models.find((candidate) => candidate.id === "cursor/default")?.id;
  const modelId = resolvedModelId !== undefined
      && models.some((candidate) => candidate.id === resolvedModelId)
    ? resolvedModelId
    : automaticModelId ?? models[0]?.id ?? "";
  const model = models.find((candidate) => candidate.id === modelId);
  const option = thinkingOption(model);
  const inheritedThinking = [
    nonEmpty(mode?.default_thinking_level),
    nonEmpty(providers?.default_thinking_level),
  ].find((candidate): candidate is string =>
    candidate !== undefined && thinkingSelectionIsValid(option, candidate));
  const thinking = inheritedThinking ?? defaultThinkingSelection(option);
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

export const createNewSessionOptionsLifecycle = (): NewSessionOptionsLifecycle => ({
  status: "unloaded",
  workspaceId: "",
  catalogWorkspaceId: "",
});

export const newSessionOptionsAreAuthoritative = (
  lifecycle: NewSessionOptionsLifecycle,
  workspaceId: string,
): boolean =>
  workspaceId !== "" && lifecycle.catalogWorkspaceId === workspaceId;

export const newSessionOptionsAreLoading = (
  lifecycle: NewSessionOptionsLifecycle,
): boolean => lifecycle.status === "loading" || lifecycle.status === "refreshing";

export const newSessionOptionsBlockSubmission = (
  lifecycle: NewSessionOptionsLifecycle,
): boolean => lifecycle.status === "loading";

export const newSessionOptionsCatalogWorkspaceId = (
  lifecycle: NewSessionOptionsLifecycle,
): string => lifecycle.catalogWorkspaceId;

/** Starts a required load or a nonblocking refresh of authoritative metadata. */
export const beginNewSessionOptionLoad = (
  current: NewSessionOptionLoadState,
  workspaceId: string,
  preserveSelections: boolean,
): NewSessionOptionLoadState => {
  const refresh = preserveSelections
    && newSessionOptionsAreAuthoritative(current.lifecycle, workspaceId);
  const lifecycle: NewSessionOptionsLifecycle = {
    status: refresh ? "refreshing" : "loading",
    workspaceId,
    catalogWorkspaceId: refresh ? workspaceId : "",
  };
  return preserveSelections
    ? {
        ...current,
        lifecycle,
        edits: { ...current.edits },
      }
    : {
        lifecycle,
        edits: createNewThreadOptionEdits(),
        inheritedThinking: undefined,
        inheritedPermissionMode: undefined,
      };
};

export const settleNewSessionOptionLoad = (
  current: NewSessionOptionsLifecycle,
  workspaceId: string,
  outcome: "ready" | "failed" | "timed-out",
): NewSessionOptionsLifecycle => {
  if (current.workspaceId !== workspaceId) return current;
  return {
    status: outcome,
    workspaceId,
    catalogWorkspaceId: outcome === "ready"
      ? workspaceId
      : current.catalogWorkspaceId,
  };
};

export const interruptNewSessionOptionLoad = (
  current: NewSessionOptionsLifecycle,
): NewSessionOptionsLifecycle =>
  current.status === "loading" || current.status === "refreshing"
    ? {
        status: current.catalogWorkspaceId === "" ? "failed" : "ready",
        workspaceId: current.workspaceId,
        catalogWorkspaceId: current.catalogWorkspaceId,
      }
    : current;

/** Submission is safe only after every producer of form state has settled. */
export const canSubmitNewSession = (state: {
  readonly sessionPending: boolean;
  readonly optionsBlocking: boolean;
  readonly attachmentPending: boolean;
}): boolean =>
  !state.sessionPending && !state.optionsBlocking && !state.attachmentPending;

/** Capture the option values used after asynchronous title generation completes. */
export const snapshotNewSessionSubmission = (
  input: NewSessionSubmissionSnapshotInput,
): NewSessionSubmissionSnapshot => {
  const selectedMode = input.modes.find(
    (mode) => mode.id === input.selections.modeId,
  );
  const effectiveModel = resolveNewSessionModel(
    input.selections.modelId,
    selectedMode,
    input.providers,
  );
  return Object.freeze({
    ...input.selections,
    edits: Object.freeze({ ...input.edits }),
    inheritedThinking: input.inheritedThinking,
    inheritedPermissionMode: input.inheritedPermissionMode,
    modelInfo: input.selectableModels.find((model) => model.id === effectiveModel),
    optionsAuthoritative: input.optionsAuthoritative,
  });
};

/** Merge a refreshed catalog into fields the user has not edited. */
export const reconcileNewThreadDefaults = (
  selections: NewThreadOptionSelections,
  modes: readonly ProtocolAgentPersona[],
  models: readonly ProtocolModelInfo[],
  providers: ProtocolProvidersResponse | null | undefined,
  edits: NewThreadOptionEdits,
  selectableModels: readonly ProtocolModelInfo[] = models,
): ResolvedNewThreadDefaults => {
  const initial = resolveNewThreadDefaults(modes, selectableModels, providers);
  const modeId = edits.mode && modes.some((mode) => mode.id === selections.modeId)
    ? selections.modeId
    : initial.modeId;
  const modeDefaults = resolveNewThreadDefaults(
    modes,
    selectableModels,
    providers,
    { modeId },
  );
  const keepModel = edits.model
    && selectableModels.some((model) => model.id === selections.modelId);
  const modelId = keepModel
    ? selections.modelId
    : modeDefaults.modelId;
  const refreshed = resolveNewThreadDefaults(
    modes,
    selectableModels,
    providers,
    { modeId, modelId },
  );
  const option = thinkingOption(
    selectableModels.find((model) => model.id === refreshed.modelId),
  );
  const keepThinking = edits.thinking
    && thinkingSelectionIsValid(option, selections.thinking);
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
    && advertisedThinking !== undefined
    && thinkingSelectionIsValid(advertisedThinking, thinking)
    ? {
        [advertisedThinking.key]:
          advertisedThinking.budget === undefined ? thinking : Number(thinking),
      }
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

/**
 * Serialize authoritative selections plus any deliberate edits made while
 * catalog metadata was unavailable. Thinking edits also pin their model
 * because the option schema is model-specific.
 */
export const createNewSessionThreadRequestFromSnapshot = (input: {
  readonly sessionId: string;
  readonly title: string;
  readonly snapshot: NewSessionSubmissionSnapshot;
}): ProtocolCreateThreadRequest => {
  const { snapshot } = input;
  const includeMode = snapshot.optionsAuthoritative || snapshot.edits.mode;
  const includeModel = snapshot.optionsAuthoritative
    || snapshot.edits.model
    || snapshot.edits.thinking;
  // Permission is always serialized: in degraded mode there is no trustworthy
  // inherited value, so omitting the displayed choice could let the server use
  // a different (and potentially more permissive) default.
  const includePermission = true;
  const includeThinking = snapshot.optionsAuthoritative || snapshot.edits.thinking;
  return createNewSessionThreadRequest({
    sessionId: input.sessionId,
    title: input.title,
    ...(includeMode ? { mode: snapshot.modeId } : {}),
    ...(includeModel ? { model: snapshot.modelId } : {}),
    ...(includePermission ? { permissionMode: snapshot.permissionMode } : {}),
    ...(includeThinking ? { thinking: snapshot.thinking } : {}),
    ...(snapshot.optionsAuthoritative
        && snapshot.inheritedPermissionMode !== undefined
      ? { inheritedPermissionMode: snapshot.inheritedPermissionMode }
      : {}),
    ...(snapshot.optionsAuthoritative && snapshot.inheritedThinking !== undefined
      ? { inheritedThinking: snapshot.inheritedThinking }
      : {}),
    ...(snapshot.modelInfo === undefined ? {} : { modelInfo: snapshot.modelInfo }),
  });
};
