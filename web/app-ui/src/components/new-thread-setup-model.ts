import {
  createNewSessionThreadRequest,
  resolveNewSessionModel,
  resolveNewThreadDefaults,
  thinkingOption,
  threadTitleFallback,
} from "../app/new-session-model.js";
import {
  modelOptionControls,
  sanitizeModelOptions,
  type ModelOptionControl,
} from "./model-option-controls.js";
import {
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "../services/attachments.js";
import type {
  ProtocolAgentPersona,
  ProtocolCreateThreadRequest,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
  ProtocolSendMessageRequest,
} from "../services/protocol-client.js";

export type NewThreadPermissionSelection = "" | "ask" | "allow_list" | "yolo";

export interface NewThreadSetupCatalog {
  readonly modes: readonly ProtocolAgentPersona[];
  readonly models: readonly ProtocolModelInfo[];
  readonly providers: ProtocolProvidersResponse | undefined;
}

export interface NewThreadSetupDraft {
  readonly modeId: string;
  readonly modelId: string;
  readonly modelOptions: Readonly<Record<string, unknown>>;
  readonly permissionMode: NewThreadPermissionSelection;
  /** Captured authoritative sources for the values currently displayed. */
  readonly inheritedThinking: string | undefined;
  readonly inheritedPermissionMode: Exclude<NewThreadPermissionSelection, ""> | undefined;
  readonly prompt: string;
  readonly attachments: readonly PendingAttachment[];
}

export interface NewThreadSetupEdits {
  readonly mode: boolean;
  readonly model: boolean;
  readonly thinking: boolean;
  readonly permission: boolean;
}

export interface NewThreadSetupSubmitDetail {
  readonly workspaceId: string;
  readonly sessionId: string;
  readonly request: ProtocolCreateThreadRequest;
  /** Omitted when the user starts an empty thread without attachments. */
  readonly initialMessage?: ProtocolSendMessageRequest;
}

export interface NewThreadSetupCancelDetail {
  readonly workspaceId: string;
  readonly sessionId: string;
}

export type NewThreadAttachmentLimit =
  | "item-too-large"
  | "too-many"
  | "total-too-large";

export interface NewThreadAttachmentAppendResult {
  readonly attachments: readonly PendingAttachment[];
  readonly accepted: boolean;
  readonly limit?: NewThreadAttachmentLimit;
}

export interface NewThreadSetupControls {
  readonly formDisabled: boolean;
  readonly optionControlsDisabled: boolean;
  readonly canSubmit: boolean;
  readonly canCancel: boolean;
  readonly submitLabel: "Start thread" | "Starting…";
}

const nonEmpty = (value: string): string | undefined => {
  const trimmed = value.trim();
  return trimmed === "" ? undefined : trimmed;
};

const knownMode = (
  modes: readonly ProtocolAgentPersona[],
  modeId: string,
): ProtocolAgentPersona | undefined => modes.find((mode) => mode.id === modeId);

const knownModel = (
  models: readonly ProtocolModelInfo[],
  modelId: string | undefined,
): ProtocolModelInfo | undefined =>
  modelId === undefined ? undefined : models.find((model) => model.id === modelId);

export const effectiveNewThreadModel = (
  draft: Pick<NewThreadSetupDraft, "modeId" | "modelId">,
  catalog: NewThreadSetupCatalog,
): ProtocolModelInfo | undefined => {
  const mode = knownMode(catalog.modes, draft.modeId);
  const modelId = resolveNewSessionModel(draft.modelId, mode, catalog.providers);
  return knownModel(catalog.models, modelId);
};

export const newThreadModelOptionControls = (
  draft: Pick<NewThreadSetupDraft, "modeId" | "modelId" | "modelOptions">,
  catalog: NewThreadSetupCatalog,
): readonly ModelOptionControl[] => {
  const model = effectiveNewThreadModel(draft, catalog);
  const thinking = thinkingOption(model);
  const defaults = resolveNewThreadDefaults(
    catalog.modes,
    catalog.models,
    catalog.providers,
    { modeId: draft.modeId, modelId: draft.modelId },
  );
  const options = {
    ...(thinking === undefined || defaults.thinking === ""
      ? {}
      : { [thinking.key]: defaults.thinking }),
    ...draft.modelOptions,
  };
  return modelOptionControls(model, options);
};

/** Show the concrete server defaults instead of inheritance placeholders. */
export const createInitialNewThreadDraft = (
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const defaults = resolveNewThreadDefaults(
    catalog.modes,
    catalog.models,
    catalog.providers,
  );
  return {
    modeId: defaults.modeId,
    modelId: defaults.modelId,
    modelOptions: {},
    permissionMode: defaults.permissionMode,
    inheritedThinking: defaults.inheritedThinking,
    inheritedPermissionMode: defaults.inheritedPermissionMode,
    prompt: "",
    attachments: [],
  };
};

/** Selecting a mode adopts all of its effective defaults, matching the product form. */
export const selectNewThreadMode = (
  draft: NewThreadSetupDraft,
  modeId: string,
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const previousEffectiveModelId = effectiveNewThreadModel(draft, catalog)?.id;
  const defaults = resolveNewThreadDefaults(
    catalog.modes,
    catalog.models,
    catalog.providers,
    { modeId },
  );
  const next = {
    ...draft,
    modeId: defaults.modeId,
    modelId: defaults.modelId,
    permissionMode: defaults.permissionMode,
    inheritedThinking: defaults.inheritedThinking,
    inheritedPermissionMode: defaults.inheritedPermissionMode,
  };
  return {
    ...next,
    ...(effectiveNewThreadModel(next, catalog)?.id === previousEffectiveModelId
      ? {}
      : { modelOptions: {} }),
  };
};

export const selectNewThreadModel = (
  draft: NewThreadSetupDraft,
  modelId: string,
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const previousEffectiveModelId = effectiveNewThreadModel(draft, catalog)?.id;
  const defaults = resolveNewThreadDefaults(
    catalog.modes,
    catalog.models,
    catalog.providers,
    { modeId: draft.modeId, modelId },
  );
  const next = {
    ...draft,
    modelId: defaults.modelId,
    inheritedThinking: defaults.inheritedThinking,
  };
  return {
    ...next,
    ...(effectiveNewThreadModel(next, catalog)?.id === previousEffectiveModelId
      ? {}
      : { modelOptions: {} }),
  };
};

export const createNewThreadSetupEdits = (): NewThreadSetupEdits => ({
  mode: false,
  model: false,
  thinking: false,
  permission: false,
});

/**
 * Reconcile a draft with a refreshed catalog. Explicit, still-valid fields
 * remain request overrides; untouched fields adopt refreshed defaults and
 * their authoritative inheritance markers together.
 */
export const reconcileNewThreadDraft = (
  draft: NewThreadSetupDraft,
  catalog: NewThreadSetupCatalog,
  edits: NewThreadSetupEdits,
): NewThreadSetupDraft => {
  const initial = createInitialNewThreadDraft(catalog);
  const modeId = edits.mode && knownMode(catalog.modes, draft.modeId) !== undefined
    ? draft.modeId
    : initial.modeId;
  const modeDefaults = selectNewThreadMode(initial, modeId, catalog);
  const modelId = edits.model && knownModel(catalog.models, draft.modelId) !== undefined
    ? draft.modelId
    : modeDefaults.modelId;
  const refreshed = selectNewThreadModel(modeDefaults, modelId, catalog);
  const keepPermission = edits.permission
    && (draft.permissionMode === "ask"
      || draft.permissionMode === "allow_list"
      || draft.permissionMode === "yolo");
  const modelOptions = edits.thinking
    ? sanitizeModelOptions(effectiveNewThreadModel(refreshed, catalog), draft.modelOptions)
    : {};
  const thinking = thinkingOption(effectiveNewThreadModel(refreshed, catalog));
  const hasThinkingOverride = thinking !== undefined
    && modelOptions[thinking.key] !== undefined;

  return {
    ...draft,
    modeId: refreshed.modeId,
    modelId: refreshed.modelId,
    modelOptions,
    inheritedThinking: hasThinkingOverride ? undefined : refreshed.inheritedThinking,
    permissionMode: keepPermission ? draft.permissionMode : refreshed.permissionMode,
    inheritedPermissionMode: keepPermission
      ? undefined
      : refreshed.inheritedPermissionMode,
  };
};

export const appendNewThreadAttachment = (
  attachments: readonly PendingAttachment[],
  attachment: PendingAttachment,
): NewThreadAttachmentAppendResult => {
  if (attachment.size > MAX_ATTACHMENT_BYTES) {
    return { attachments, accepted: false, limit: "item-too-large" };
  }
  if (attachments.length >= MAX_PENDING_ATTACHMENTS) {
    return { attachments, accepted: false, limit: "too-many" };
  }
  const total = attachments.reduce((bytes, pending) => bytes + pending.size, attachment.size);
  if (total > MAX_PENDING_ATTACHMENT_BYTES) {
    return { attachments, accepted: false, limit: "total-too-large" };
  }
  return { attachments: [...attachments, attachment], accepted: true };
};

export const newThreadAttachmentLimitMessage = (
  limit: NewThreadAttachmentLimit,
  name = "Attachment",
): string => {
  const megabytes = (bytes: number): string =>
    String(bytes / (1024 * 1024));
  if (limit === "item-too-large") {
    return `${name} is larger than the ${megabytes(MAX_ATTACHMENT_BYTES)} MB limit.`;
  }
  if (limit === "too-many") return `Attach at most ${MAX_PENDING_ATTACHMENTS} files at once.`;
  return `Pending attachments exceed the ${megabytes(MAX_PENDING_ATTACHMENT_BYTES)} MB mobile memory budget.`;
};

export const formatNewThreadAttachmentBytes = (bytes: number): string => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

export const newThreadSetupControls = (input: {
  readonly sessionId: string;
  readonly workspaceId: string;
  readonly disabled: boolean;
  readonly busy: boolean;
  readonly attachmentLoading: boolean;
}): NewThreadSetupControls => {
  const validScope = nonEmpty(input.sessionId) !== undefined
    && nonEmpty(input.workspaceId) !== undefined;
  const formDisabled = input.disabled || input.busy;
  return {
    formDisabled,
    // Catalog refreshes only enrich these controls. Server defaults are a
    // valid immediate choice, so waiting for static mode/model metadata must
    // never make the setup form inert.
    optionControlsDisabled: formDisabled,
    canSubmit:
      validScope
      && !formDisabled
      && !input.attachmentLoading,
    canCancel: !input.busy,
    submitLabel: input.busy ? "Starting…" : "Start thread",
  };
};

const attachmentLimit = (
  attachments: readonly PendingAttachment[],
): NewThreadAttachmentLimit | undefined => {
  if (attachments.some((attachment) => attachment.size > MAX_ATTACHMENT_BYTES)) {
    return "item-too-large";
  }
  if (attachments.length > MAX_PENDING_ATTACHMENTS) return "too-many";
  const total = attachments.reduce((bytes, attachment) => bytes + attachment.size, 0);
  return total > MAX_PENDING_ATTACHMENT_BYTES ? "total-too-large" : undefined;
};

export const createNewThreadSetupSubmission = (input: {
  readonly workspaceId: string;
  readonly sessionId: string;
  readonly draft: NewThreadSetupDraft;
  readonly catalog: NewThreadSetupCatalog;
}): NewThreadSetupSubmitDetail => {
  const workspaceId = nonEmpty(input.workspaceId);
  if (workspaceId === undefined) {
    throw new TypeError("A nonempty workspace id is required for new-thread setup.");
  }
  const limit = attachmentLimit(input.draft.attachments);
  if (limit !== undefined) {
    throw new RangeError(newThreadAttachmentLimitMessage(limit));
  }

  const mode = knownMode(input.catalog.modes, input.draft.modeId);
  const model = knownModel(input.catalog.models, input.draft.modelId);
  const effectiveModel = effectiveNewThreadModel(
    {
      modeId: mode?.id ?? "",
      modelId: model?.id ?? "",
    },
    input.catalog,
  );
  const prompt = input.draft.prompt.trim();
  const request = createNewSessionThreadRequest({
    sessionId: input.sessionId,
    title: threadTitleFallback(prompt),
    ...(mode === undefined ? {} : { mode: mode.id }),
    ...(model === undefined ? {} : { model: model.id }),
    permissionMode: input.draft.permissionMode,
    modelOptions: input.draft.modelOptions,
    ...(input.draft.inheritedPermissionMode === undefined
      ? {}
      : { inheritedPermissionMode: input.draft.inheritedPermissionMode }),
    ...(input.draft.inheritedThinking === undefined
      ? {}
      : { inheritedThinking: input.draft.inheritedThinking }),
    ...(effectiveModel === undefined ? {} : { modelInfo: effectiveModel }),
  });
  const initialMessage = prompt === "" && input.draft.attachments.length === 0
    ? undefined
    : {
        content: prompt,
        ...(input.draft.attachments.length === 0
          ? {}
          : {
              attachments: input.draft.attachments.map(({ upload }) => upload),
            }),
      };
  return {
    workspaceId,
    sessionId: request.session_id,
    request,
    ...(initialMessage === undefined ? {} : { initialMessage }),
  };
};
