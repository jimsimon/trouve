import type {
  ProtocolCodeReviewRepository,
  ProtocolGithubAppStatus,
  ProtocolReviewerProfile,
  ProtocolUpdateCodeReviewRepositoryRequest,
} from "../services/protocol-client.js";

export type CodeReviewMode = "off" | "manual" | "automatic";
export type CodeReviewRoutingMode = "manual" | "additive" | "automatic";
export type ReviewerPromptMode = "inherit" | "append" | "replace";

export interface ReviewerOverrideDraft {
  readonly reviewerId: string;
  readonly model: string;
  readonly thinkingLevel: string;
  readonly promptMode: ReviewerPromptMode;
  readonly prompt: string;
}

export interface RepositoryDraft {
  readonly mode: CodeReviewMode;
  readonly model: string;
  readonly coordinatorThinkingLevel: string;
  readonly routerModel: string;
  readonly routerThinkingLevel: string;
  readonly prompt: string;
  readonly reviewerIds: readonly string[];
  readonly routingMode: CodeReviewRoutingMode;
  readonly semanticRouting: boolean;
  readonly includedReviewerIds: readonly string[];
  readonly excludedReviewerIds: readonly string[];
  readonly reviewerOverrides: readonly ReviewerOverrideDraft[];
}

export interface ReviewerDraft {
  readonly name: string;
  readonly prompt: string;
  readonly model: string;
  readonly thinkingLevel: string;
}

export interface SanitizedGithubAppStatus {
  readonly status: ProtocolGithubAppStatus;
  readonly needsAttention: boolean;
}

const unique = (values: readonly string[]): readonly string[] => [
  ...new Set(values.filter((value) => value !== "")),
];

const optionalValue = (value: string): string | null => {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
};

/** Keep only the presence of a server diagnostic. Diagnostic text can contain
 * vendor or installation details and must never be retained in UI state. */
export const sanitizeGithubAppStatus = (
  value: ProtocolGithubAppStatus,
): SanitizedGithubAppStatus => ({
  status: { ...value, last_error: "" },
  needsAttention: value.configured && (value.last_error ?? "") !== "",
});

export const repositoryDraft = (
  repository: ProtocolCodeReviewRepository,
): RepositoryDraft => ({
  mode: repository.mode ?? "off",
  model: repository.model ?? "",
  coordinatorThinkingLevel: repository.coordinator_thinking_level ?? "",
  routerModel: repository.router_model ?? "",
  routerThinkingLevel: repository.router_thinking_level ?? "",
  prompt: repository.prompt ?? "",
  reviewerIds: unique(repository.reviewer_ids ?? []),
  routingMode: repository.routing_mode ?? "additive",
  semanticRouting: repository.semantic_routing ?? false,
  includedReviewerIds: unique(repository.included_reviewer_ids ?? []),
  excludedReviewerIds: unique(repository.excluded_reviewer_ids ?? []),
  reviewerOverrides: (repository.reviewer_overrides ?? []).map((override) => ({
    reviewerId: override.reviewer_id,
    model: override.model ?? "",
    thinkingLevel: override.thinking_level ?? "",
    promptMode: override.prompt_mode ?? "inherit",
    prompt: override.prompt ?? "",
  })),
});

/** Build the full repository replacement request. Even fields ignored by the
 * active routing mode are resent so a save cannot accidentally drop settings
 * that were returned by the server. */
export const repositoryUpdateRequest = (
  repository: ProtocolCodeReviewRepository,
  draft: RepositoryDraft,
): ProtocolUpdateCodeReviewRepositoryRequest => {
  if (draft.mode !== "off" && optionalValue(draft.model) === null) {
    throw new Error("coordinator model required");
  }
  if (draft.mode !== "off" && draft.routingMode === "manual" && draft.reviewerIds.length === 0) {
    throw new Error("manual reviewer required");
  }

  return {
    installation_id: repository.installation_id,
    repository: repository.repository,
    mode: draft.mode,
    model: optionalValue(draft.model),
    coordinator_thinking_level: optionalValue(draft.coordinatorThinkingLevel),
    router_model: optionalValue(draft.routerModel),
    router_thinking_level: optionalValue(draft.routerThinkingLevel),
    prompt: draft.prompt,
    reviewer_ids: [...unique(draft.reviewerIds)],
    routing_mode: draft.routingMode,
    semantic_routing: draft.routingMode === "manual" ? false : draft.semanticRouting,
    included_reviewer_ids: [...unique(draft.includedReviewerIds)],
    excluded_reviewer_ids: [...unique(draft.excludedReviewerIds)],
    reviewer_overrides: draft.reviewerOverrides.map((override) => ({
      reviewer_id: override.reviewerId,
      model: optionalValue(override.model),
      thinking_level: optionalValue(override.thinkingLevel),
      prompt_mode: override.promptMode,
      prompt: override.prompt,
    })),
  };
};

export const reviewerDraft = (profile?: ProtocolReviewerProfile): ReviewerDraft => ({
  name: profile?.name ?? "",
  prompt: profile?.prompt ?? "",
  model: profile?.model ?? "",
  thinkingLevel: profile?.default_thinking_level ?? "",
});

export const repositoryKey = (
  repository: Pick<ProtocolCodeReviewRepository, "installation_id" | "repository">,
): string => `${repository.installation_id}:${repository.repository}`;
