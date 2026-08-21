export type ReviewMode = "off" | "manual" | "automatic";
export type ReviewScope = "incremental" | "full";
export type RoutingMode = "manual" | "additive" | "automatic";
export type RoutingSource =
  | "core"
  | "baseline"
  | "deterministic"
  | "semantic"
  | "included"
  | "thorough";
export type ReviewStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled"
  | "stale";
export type StatsRange = "hour" | "day" | "week" | "month" | "year" | "all";

export interface GithubAppStatus {
  configured: boolean;
  app_id?: number;
  slug: string;
  bot_login: string;
  webhook_configured: boolean;
  checks_write_configured: boolean;
  check_run_webhook_configured: boolean;
  installation_count: number;
  last_poll_at?: string;
  last_error: string;
  rate_limit_remaining?: number;
  rate_limit_reset_at?: string;
}

export interface ReviewerOverride {
  reviewer_id: string;
  model?: string;
  thinking_level?: string;
  prompt_mode: "inherit" | "append" | "replace";
  prompt: string;
}

export interface ReviewerProfile {
  id: string;
  name: string;
  prompt: string;
  model?: string;
  default_thinking_level?: string;
  built_in: boolean;
}

export interface Repository {
  installation_id: number;
  repository: string;
  private: boolean;
  mode: ReviewMode;
  model?: string;
  coordinator_thinking_level?: string;
  router_model?: string;
  router_thinking_level?: string;
  prompt: string;
  reviewer_ids: string[];
  routing_mode: RoutingMode;
  semantic_routing: boolean;
  included_reviewer_ids?: string[];
  excluded_reviewer_ids?: string[];
  reviewer_overrides?: ReviewerOverride[];
}

export interface Progress {
  completed_reviewers: number;
  total_reviewers: number;
  percent: number;
}

export interface ReviewJob {
  id: string;
  installation_id: number;
  repository: string;
  pull_number: number;
  pull_title: string;
  pull_url: string;
  head_sha: string;
  review_base_sha?: string;
  review_watermark_sha?: string;
  base_ref: string;
  head_ref: string;
  scope: ReviewScope;
  trigger: string;
  status: ReviewStatus;
  retry_of?: string;
  retried_by?: string;
  model?: string;
  coordinator_thinking_level?: string;
  router_model?: string;
  router_thinking_level?: string;
  reviewer_ids: string[];
  routing_mode: RoutingMode;
  semantic_routing: boolean;
  included_reviewer_ids?: string[];
  excluded_reviewer_ids?: string[];
  session_id?: string;
  thread_id?: string;
  review_url: string;
  lifecycle_comment_url: string;
  check_run_id?: number;
  check_run_url: string;
  check_sync_error: string;
  cancel_requested: boolean;
  progress: Progress;
  candidate_issue_count: number;
  issue_count: number;
  fixed_issue_count: number;
  error: string;
  created_at: string;
  started_at?: string;
  completed_at?: string;
  pending_elapsed_ms: number;
  running_elapsed_ms: number;
  preparation_elapsed_ms: number;
  reviewer_elapsed_ms: number;
  coordinator_elapsed_ms: number;
  publication_elapsed_ms: number;
}

export interface ReviewTask {
  id: string;
  job_id: string;
  role: "router" | "reviewer" | "coordinator";
  reviewer_id?: string;
  reviewer_name: string;
  batch_index: number;
  batch_count: number;
  status: string;
  lifecycle_stage:
    | "queued"
    | "waiting_for_capacity"
    | "starting_model"
    | "running_model"
    | "running_tool"
    | "repairing_output"
    | "completed";
  model?: string;
  session_id?: string;
  thread_id?: string;
  prompt?: string;
  output?: string;
  thinking?: string;
  tool_output?: string;
  candidate_issue_count: number;
  confirmed_issue_count: number;
  provider_wait_ms: number;
  model_elapsed_ms: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  tool_call_count: number;
  error?: string;
  created_at: string;
  started_at?: string;
  model_started_at?: string | null;
  last_progress_at?: string;
  /** Browser clock time when the current model_elapsed_ms snapshot arrived. */
  model_elapsed_snapshot_at?: number;
  completed_at?: string;
  elapsed_ms: number;
}

export interface ReviewTaskProgress {
  lifecycle_stage: ReviewTask["lifecycle_stage"];
  provider_wait_ms: number;
  model_elapsed_ms: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  tool_call_count: number;
  model_started_at: string | null;
  last_progress_at: string;
}

export interface CodeReviewSettings {
  max_parallel_reviews: number;
  total_timeout_seconds: number;
  reviewer_timeout_seconds: number;
  coordinator_timeout_seconds: number;
}

export interface PersonaResult {
  reviewer_id: string;
  reviewer_name: string;
  status: string;
  models: string[];
  completed_batches: number;
  total_batches: number;
  candidate_issue_count: number;
  confirmed_issue_count: number;
  provider_wait_ms: number;
  model_elapsed_ms: number;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  tool_call_count: number;
  started_at?: string;
  completed_at?: string;
  elapsed_ms: number;
}

export interface FindingSource {
  reviewer_id: string;
  reviewer_name: string;
  candidate_id: string;
  task_id: string;
}

export interface Finding {
  id: string;
  job_id: string;
  path: string;
  line: number;
  side: string;
  outside_diff?: boolean;
  severity: string;
  confidence?: string;
  title: string;
  body: string;
  prompt_for_agents: string;
  status: string;
  sources: FindingSource[];
  github_comment_id?: number;
  github_comment_url: string;
  github_publication_status:
    | "pending"
    | "published"
    | "not_eligible"
    | "suppressed_by_policy"
    | "grouped_by_theme"
    | "failed";
  evidence?: {
    preconditions?: string;
    execution_path?: string;
    consequence?: string;
    introduction?: string;
    regression_test?: string;
  };
  origin?: "new_change" | "recurrence" | "fix_regression" | "previously_missed";
  theme_ids?: string[];
  github_thread_id?: string;
  resolved_at?: string;
  observed_head?: string;
  resolved_head?: string;
  resolved_by_job_id?: string;
}

export interface ReviewTheme {
  id: string;
  repository: string;
  pull_number: number;
  root_cause: string;
  recommendation: string;
  status: string;
  first_seen_head: string;
  last_seen_head: string;
  resolved_head?: string;
  recurrence_count: number;
  affected_paths?: string[];
  finding_ids?: string[];
  observations?: Array<{
    job_id: string;
    head_sha: string;
    kind: "new" | "continuation" | "recurrence";
    finding_ids: string[];
    created_at: string;
  }>;
}

export interface CandidateRejection {
  candidate_id: string;
  task_id: string;
  reviewer_id: string;
  reviewer_name: string;
  path: string;
  line: number;
  side: string;
  severity: string;
  confidence?: string;
  title: string;
  body: string;
  reason: string;
}

export interface RoutingReason {
  source: RoutingSource;
  detail: string;
}

export interface RoutingDecision {
  batch_index: number;
  reviewer_id: string;
  reviewer_name: string;
  selected: boolean;
  reasons?: RoutingReason[];
}

export interface JobDetail {
  job: ReviewJob;
  event_cursor: number;
  tasks: ReviewTask[];
  personas: PersonaResult[];
  findings: Finding[];
  themes?: ReviewTheme[];
  candidate_rejections?: CandidateRejection[];
  routing_decisions?: RoutingDecision[];
  summary: string;
  prompt_for_agents: string;
}

export interface Dashboard {
  app: GithubAppStatus;
  reviewers: ReviewerProfile[];
  repositories: Repository[];
  jobs: ReviewJob[];
}

export interface StatusCounts {
  queued: number;
  running: number;
  succeeded: number;
  failed: number;
  cancelled: number;
  stale: number;
}

export interface DurationStats {
  samples: number;
  average_ms: number;
  p50_ms: number;
  p95_ms: number;
  maximum_ms: number;
}

export interface StatsBucket {
  started_at: string;
  completed_at: string;
  status: StatusCounts;
  issue_count: number;
  pending_average_ms: number;
  running_average_ms: number;
}

export interface PersonaStats {
  reviewer_id: string;
  reviewer_name: string;
  model: string;
  // Per-batch reviewer tasks. Outcome counters below are rolled-up persona
  // runs, one per review job and actual model.
  task_count: number;
  succeeded: number;
  failed: number;
  cancelled: number;
  not_applicable: number;
  candidate_issue_count: number;
  confirmed_issue_count: number;
  duration: DurationStats;
  provider_wait_duration: DurationStats;
  model_duration: DurationStats;
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  tool_call_count: number;
}

export interface RepositoryStats {
  repository: string;
  status: StatusCounts;
  issue_count: number;
  pending_duration: DurationStats;
  running_duration: DurationStats;
  preparation_duration: DurationStats;
  reviewer_duration: DurationStats;
  coordinator_duration: DurationStats;
  publication_duration: DurationStats;
}

export interface ReviewStats {
  range: StatsRange;
  repository?: string;
  generated_at: string;
  status: StatusCounts;
  pending_duration: DurationStats;
  running_duration: DurationStats;
  preparation_duration: DurationStats;
  reviewer_duration: DurationStats;
  coordinator_duration: DurationStats;
  publication_duration: DurationStats;
  issue_count: number;
  churn?: {
    recurrence_issue_count: number;
    fix_regression_issue_count: number;
    previously_missed_issue_count: number;
    grouped_issue_count: number;
    external_duplicate_count: number;
    insufficient_evidence_rejection_count: number;
    pull_request_count: number;
    clean_pull_request_count: number;
    average_rounds_to_clean: number;
    max_rounds_to_clean: number;
  };
  buckets: StatsBucket[];
  personas: PersonaStats[];
  repositories: RepositoryStats[];
}

export interface Provider {
  id: string;
  kind: string;
  base_url?: string;
  has_credentials: boolean;
  auth: string;
  category: string;
  experimental: boolean;
}

export interface ProvidersResponse {
  providers: Provider[];
  default_model: string;
  default_thinking_level?: string;
}

export interface KnownProvider {
  id: string;
  display_name: string;
  kind: string;
  base_url?: string;
  api_key_env?: string;
  auth: string;
  category: string;
  experimental: boolean;
}

export interface Model {
  id: string;
  display_name: string;
  options_schema?: unknown;
}

export interface AgentPersona {
  id: string;
  display_name: string;
  group?: "general" | "reviewer";
  system_prompt: string;
  allowed_tools: string[];
  read_only: boolean;
  default_permission_mode?: string;
  default_model?: string;
  default_thinking_level?: string;
}

export interface PersonaInfo {
  persona: AgentPersona;
  origin: "builtin" | "customized" | "custom" | "workspace";
}

export interface LoginStarted {
  verification_url: string;
  user_code?: string;
}

export interface LoginStatus {
  status: "none" | "pending" | "success" | "failed";
  error?: string;
}

export interface EventEnvelope {
  cursor: number;
  scope: unknown;
  ts: string;
  type: string;
  job_id?: string;
  task_id?: string;
  stream?: "assistant" | "thinking" | "tool";
  text?: string;
  task?: ReviewTask;
  progress?: Progress | ReviewTaskProgress;
  routing_decisions?: RoutingDecision[];
  settings?: CodeReviewSettings;
}
