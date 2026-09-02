import type {
  ProtocolCodeReviewSettings,
  ProtocolSetCodeReviewSettingsRequest,
} from "../services/protocol-client.js";

export const CODE_REVIEW_STATUS_FILTERS = [
  "all",
  "queued",
  "running",
  "succeeded",
  "failed",
  "cancelled",
  "stale",
] as const;

export type CodeReviewStatusFilter = (typeof CODE_REVIEW_STATUS_FILTERS)[number];
export type CodeReviewJobAction = "cancel" | "retry" | "final-editor";

export interface ReviewJobSummary {
  readonly id: string;
  readonly repository: string;
  readonly status: string;
  readonly created_at: string;
  readonly open_issue_count?: number | null;
  readonly advisory_open_issue_count?: number | null;
  readonly legacy_coverage_pending?: boolean;
  readonly legacy_coverage_exhausted?: boolean;
}

export interface ReviewJobGroup<T extends ReviewJobSummary = ReviewJobSummary> {
  readonly repository: string;
  readonly jobs: readonly T[];
  readonly activeCount: number;
}

export interface ReconciledReviewGroupOrder {
  readonly order: readonly string[];
  readonly changed: boolean;
}

export interface CodeReviewSettingsDraft {
  readonly maxParallel: string;
  readonly totalMinutes: string;
  readonly reviewerMinutes: string;
  readonly coordinatorMinutes: string;
}

export const MAX_PARALLEL_REVIEWS = 32;
export const TIMEOUT_MINUTES_INPUT_MIN = String(1 / 60);
export const TIMEOUT_MINUTES_INPUT_STEP = "any";

const isActiveStatus = (status: string): boolean =>
  status === "queued" || status === "running";

const statusPriority = (status: string): number => {
  if (status === "running") return 0;
  if (status === "queued") return 1;
  return 2;
};

const createdEpoch = (value: string): number => {
  const parsed = Date.parse(value);
  return Number.isFinite(parsed) ? parsed : 0;
};

/** Groups compact dashboard rows by repository while keeping active work first. */
export function groupCodeReviewJobs<T extends ReviewJobSummary>(
  jobs: readonly T[],
  filter: CodeReviewStatusFilter,
): readonly ReviewJobGroup<T>[] {
  const repositories = new Map<string, T[]>();
  for (const job of jobs) {
    if (filter !== "all" && job.status !== filter) continue;
    const repository = job.repository.trim() || "Unknown repository";
    const existing = repositories.get(repository);
    if (existing === undefined) repositories.set(repository, [job]);
    else existing.push(job);
  }

  const groups = [...repositories.entries()].map(([repository, repositoryJobs]) => {
    const sorted = [...repositoryJobs].sort((left: T, right: T) => {
      const priority = statusPriority(left.status) - statusPriority(right.status);
      return priority !== 0
        ? priority
        : createdEpoch(right.created_at) - createdEpoch(left.created_at);
    });
    return {
      repository,
      jobs: sorted,
      activeCount: sorted.filter((job) => isActiveStatus(job.status)).length,
    };
  });

  return [...groups].sort(
    (left: ReviewJobGroup<T>, right: ReviewJobGroup<T>) =>
      right.activeCount - left.activeCount || left.repository.localeCompare(right.repository),
  );
}

const sameOrder = (left: readonly string[], right: readonly string[]): boolean =>
  left.length === right.length && left.every((entry, index) => entry === right[index]);

/** Mirror the desktop client's reconciliation rule for dynamic repository
 * groups: retain known saved keys once, drop stale keys, then append newly
 * observed groups in the current attention-first order. */
export const reconcileReviewGroupOrder = (
  savedOrder: readonly string[],
  currentRepositories: readonly string[],
): ReconciledReviewGroupOrder => {
  const available = new Set(currentRepositories);
  const seen = new Set<string>();
  const order: string[] = [];
  for (const repository of savedOrder) {
    if (available.has(repository) && !seen.has(repository)) {
      seen.add(repository);
      order.push(repository);
    }
  }
  for (const repository of currentRepositories) {
    if (!seen.has(repository)) {
      seen.add(repository);
      order.push(repository);
    }
  }
  return { order, changed: !sameOrder(savedOrder, order) };
};

/** Apply a reconciled preference without changing job order inside a group.
 * Groups absent from the preference keep their existing attention-first
 * relative order at the end. */
export function orderReviewJobGroups<T extends ReviewJobSummary>(
  groups: readonly ReviewJobGroup<T>[],
  order: readonly string[],
): readonly ReviewJobGroup<T>[] {
  const byRepository = new Map(
    groups.map((group) => [group.repository, group] as const),
  );
  const ordered: ReviewJobGroup<T>[] = [];
  const seen = new Set<string>();
  for (const repository of order) {
    const group = byRepository.get(repository);
    if (group !== undefined && !seen.has(repository)) {
      seen.add(repository);
      ordered.push(group);
    }
  }
  for (const group of groups) {
    if (!seen.has(group.repository)) ordered.push(group);
  }
  return ordered;
}

/** Stable reconciliation universe: visible job groups retain their current
 * attention-first order, while configured repositories without recent jobs
 * are kept alphabetically so a temporary empty group does not erase its
 * persisted position. */
export const reviewGroupRepositoryKeys = <T extends ReviewJobSummary>(
  groups: readonly ReviewJobGroup<T>[],
  configuredRepositories: readonly string[],
): readonly string[] => {
  const keys = groups.map((group) => group.repository);
  const seen = new Set(keys);
  const configured = configuredRepositories
    .map((repository) => repository.trim())
    .filter((repository) => repository !== "" && !seen.has(repository))
    .sort((left, right) => left.localeCompare(right));
  for (const repository of configured) {
    if (!seen.has(repository)) {
      seen.add(repository);
      keys.push(repository);
    }
  }
  return keys;
};

/** Move one repository immediately before or after another, matching the
 * desktop drag/drop semantics. Invalid or no-op requests preserve identity. */
export const reorderReviewGroup = (
  order: readonly string[],
  repository: string,
  targetRepository: string,
  after: boolean,
): readonly string[] => {
  if (repository === targetRepository) return order;
  const source = order.indexOf(repository);
  if (source < 0 || !order.includes(targetRepository)) return order;
  const next = [...order];
  const [moved] = next.splice(source, 1);
  if (moved === undefined) return order;
  const target = next.indexOf(targetRepository);
  next.splice(target + (after ? 1 : 0), 0, moved);
  return sameOrder(order, next) ? order : next;
};

/** Move against the currently visible group sequence. This keeps controls
 * responsive under a status filter while retaining hidden repositories in
 * the full persisted order. */
export const moveReviewGroup = (
  order: readonly string[],
  visibleRepositories: readonly string[],
  repository: string,
  offset: number,
): readonly string[] => {
  const source = visibleRepositories.indexOf(repository);
  if (source < 0 || visibleRepositories.length === 0 || offset === 0) return order;
  const target = Math.max(
    0,
    Math.min(visibleRepositories.length - 1, source + Math.trunc(offset)),
  );
  if (target === source) return order;
  const targetRepository = visibleRepositories[target];
  return targetRepository === undefined
    ? order
    : reorderReviewGroup(order, repository, targetRepository, target > source);
};

export const codeReviewStatusClass = (status: string): string => {
  switch (status) {
    case "queued":
    case "running":
    case "succeeded":
    case "failed":
    case "cancelled":
    case "stale":
      return status;
    default:
      return "unknown";
  }
};

export const codeReviewStatusLabel = (status: string): string => {
  const normalized = status.trim().replaceAll("_", " ");
  return normalized === ""
    ? "Unknown"
    : `${normalized[0]?.toUpperCase() ?? ""}${normalized.slice(1)}`;
};

export const codeReviewNeedsAttention = (
  job: Pick<
    ReviewJobSummary,
    "status" | "open_issue_count" | "legacy_coverage_pending" | "legacy_coverage_exhausted"
  >,
): boolean =>
  job.status === "succeeded"
  && (
    job.legacy_coverage_pending === true
    || job.legacy_coverage_exhausted === true
    || job.open_issue_count !== 0
  );

/** Only absolute, credential-free HTTPS links may cross the native boundary. */
export const safeCodeReviewHref = (value: string | null | undefined): string | undefined => {
  if (value === undefined || value === null || value.trim() === "") return undefined;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" || url.username !== "" || url.password !== "") {
      return undefined;
    }
    return url.href;
  } catch {
    return undefined;
  }
};

export const canCancelCodeReviewJob = (status: string): boolean =>
  status === "queued" || status === "running";

export const canRetryCodeReviewJob = (status: string): boolean =>
  !canCancelCodeReviewJob(status);

export const codeReviewSettingsDraft = (
  settings: ProtocolCodeReviewSettings,
): CodeReviewSettingsDraft => ({
  maxParallel: String(settings.max_parallel_reviews),
  totalMinutes: String(settings.total_timeout_seconds / 60),
  reviewerMinutes: String(settings.reviewer_timeout_seconds / 60),
  coordinatorMinutes: String(settings.coordinator_timeout_seconds / 60),
});

const timeoutSeconds = (value: string, label: string): number => {
  const minutes = Number(value);
  const seconds = minutes * 60;
  if (!Number.isFinite(minutes) || minutes <= 0 || !Number.isSafeInteger(seconds)) {
    throw new Error(`${label} must be a positive number of whole seconds`);
  }
  return seconds;
};

export function codeReviewSettingsRequest(
  draft: CodeReviewSettingsDraft,
): ProtocolSetCodeReviewSettingsRequest {
  const maxParallelReviews = Number(draft.maxParallel);
  if (
    !Number.isSafeInteger(maxParallelReviews) ||
    maxParallelReviews < 1 ||
    maxParallelReviews > MAX_PARALLEL_REVIEWS
  ) {
    throw new Error(
      `Max parallel reviews must be a whole number from 1 to ${MAX_PARALLEL_REVIEWS}`,
    );
  }

  const request = {
    max_parallel_reviews: maxParallelReviews,
    total_timeout_seconds: timeoutSeconds(draft.totalMinutes, "Total review timeout"),
    reviewer_timeout_seconds: timeoutSeconds(draft.reviewerMinutes, "Reviewer timeout"),
    coordinator_timeout_seconds: timeoutSeconds(
      draft.coordinatorMinutes,
      "Final editor timeout",
    ),
  } satisfies ProtocolSetCodeReviewSettingsRequest;

  if (request.reviewer_timeout_seconds > request.total_timeout_seconds) {
    throw new Error("Reviewer timeout cannot exceed the total review timeout");
  }
  if (request.coordinator_timeout_seconds > request.total_timeout_seconds) {
    throw new Error("Final editor timeout cannot exceed the total review timeout");
  }
  return request;
}
