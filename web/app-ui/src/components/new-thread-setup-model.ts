import {
  createNewSessionThreadRequest,
  resolveNewSessionModel,
  threadTitleFallback,
  thinkingOption,
  type ThinkingOption,
} from "../app/new-session-model.js";
import {
  MAX_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENT_BYTES,
  MAX_PENDING_ATTACHMENTS,
  type PendingAttachment,
} from "../services/attachments.js";
import type {
  ProtocolAgentMode,
  ProtocolCreateThreadRequest,
  ProtocolModelInfo,
  ProtocolProvidersResponse,
  ProtocolSendMessageRequest,
} from "../services/protocol-client.js";

export type NewThreadPermissionSelection = "" | "ask" | "allow_list" | "yolo";

export interface NewThreadSetupCatalog {
  readonly modes: readonly ProtocolAgentMode[];
  readonly models: readonly ProtocolModelInfo[];
  readonly providers: ProtocolProvidersResponse | undefined;
}

export interface NewThreadSetupDraft {
  readonly modeId: string;
  readonly modelId: string;
  readonly thinking: string;
  readonly permissionMode: NewThreadPermissionSelection;
  readonly prompt: string;
  readonly attachments: readonly PendingAttachment[];
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
  modes: readonly ProtocolAgentMode[],
  modeId: string,
): ProtocolAgentMode | undefined => modes.find((mode) => mode.id === modeId);

const knownModel = (
  models: readonly ProtocolModelInfo[],
  modelId: string | undefined,
): ProtocolModelInfo | undefined =>
  modelId === undefined ? undefined : models.find((model) => model.id === modelId);

const defaultThinking = (model: ProtocolModelInfo | undefined): string => {
  const option = thinkingOption(model);
  return option?.defaultValue ?? option?.values[0] ?? "";
};

export const effectiveNewThreadModel = (
  draft: Pick<NewThreadSetupDraft, "modeId" | "modelId">,
  catalog: NewThreadSetupCatalog,
): ProtocolModelInfo | undefined => {
  const mode = knownMode(catalog.modes, draft.modeId);
  const modelId = resolveNewSessionModel(draft.modelId, mode, catalog.providers);
  return knownModel(catalog.models, modelId);
};

export const newThreadThinkingOption = (
  draft: Pick<NewThreadSetupDraft, "modeId" | "modelId">,
  catalog: NewThreadSetupCatalog,
): ThinkingOption | undefined => thinkingOption(effectiveNewThreadModel(draft, catalog));

/** Use the established initial code mode and first advertised model. */
export const createInitialNewThreadDraft = (
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const modeId = catalog.modes.find((mode) => mode.id === "code")?.id
    ?? catalog.modes[0]?.id
    ?? "";
  const modelId = catalog.models[0]?.id ?? "";
  return {
    modeId,
    modelId,
    thinking: defaultThinking(knownModel(catalog.models, modelId)),
    permissionMode: "",
    prompt: "",
    attachments: [],
  };
};

/** Selecting a mode adopts its known default model, matching the product picker. */
export const selectNewThreadMode = (
  draft: NewThreadSetupDraft,
  modeId: string,
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const previousEffectiveModelId = effectiveNewThreadModel(draft, catalog)?.id;
  const mode = knownMode(catalog.modes, modeId);
  const nextModeId = mode?.id ?? "";
  const advertisedDefault = nonEmpty(mode?.default_model ?? "");
  const nextModelId = knownModel(catalog.models, advertisedDefault)?.id ?? draft.modelId;
  const nextEffectiveModel = effectiveNewThreadModel(
    { modeId: nextModeId, modelId: nextModelId },
    catalog,
  );
  return {
    ...draft,
    modeId: nextModeId,
    modelId: nextModelId,
    ...(nextEffectiveModel?.id === previousEffectiveModelId
      ? {}
      : { thinking: defaultThinking(nextEffectiveModel) }),
  };
};

export const selectNewThreadModel = (
  draft: NewThreadSetupDraft,
  modelId: string,
  catalog: NewThreadSetupCatalog,
): NewThreadSetupDraft => {
  const nextModelId = knownModel(catalog.models, modelId)?.id ?? "";
  const effective = effectiveNewThreadModel(
    { modeId: draft.modeId, modelId: nextModelId },
    catalog,
  );
  return {
    ...draft,
    modelId: nextModelId,
    thinking: defaultThinking(effective),
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
    thinking: input.draft.thinking,
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
