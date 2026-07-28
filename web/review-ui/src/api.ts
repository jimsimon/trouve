import type {
  Dashboard,
  EventEnvelope,
  JobDetail,
  KnownProvider,
  LoginStarted,
  LoginStatus,
  Model,
  Provider,
  ProvidersResponse,
  Repository,
  ReviewJob,
  ReviewScope,
  ReviewStats,
  ReviewTask,
  ReviewerProfile,
  StatsRange,
} from "./types";
import type { CliInfo, CliInstallStatus } from "./cli";

export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`/v1${path}`, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init.headers ?? {}),
    },
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({ message: response.statusText }));
    throw new Error(body.message ?? `Request failed (${response.status})`);
  }
  const body = await response.text();
  return body ? (JSON.parse(body) as T) : (undefined as T);
}

export const getDashboard = (): Promise<Dashboard> => api("/code-review");
export const getJob = (id: string): Promise<JobDetail> =>
  api(`/code-review/jobs/${encodeURIComponent(id)}?include_task_content=false`);
export const getTask = (jobId: string, taskId: string): Promise<ReviewTask> =>
  api(
    `/code-review/jobs/${encodeURIComponent(jobId)}/tasks/${encodeURIComponent(taskId)}`,
  );
export const getJobs = (
  status: string,
  repository: string,
): Promise<{ jobs: ReviewJob[] }> => {
  const query = new URLSearchParams({ limit: "250" });
  if (status) query.set("status", status);
  if (repository) query.set("repository", repository);
  return api(`/code-review/jobs?${query}`);
};
export const getStats = (range: StatsRange, repository: string): Promise<ReviewStats> => {
  const query = new URLSearchParams({ range });
  if (repository) query.set("repository", repository);
  return api(`/code-review/stats?${query}`);
};
export const cancelJob = (id: string): Promise<ReviewJob> =>
  api(`/code-review/jobs/${encodeURIComponent(id)}/cancel`, {
    method: "POST",
    body: "{}",
  });
export const retryJob = (id: string): Promise<ReviewJob> =>
  api(`/code-review/jobs/${encodeURIComponent(id)}/retry`, {
    method: "POST",
    body: "{}",
  });
export const retryPersona = (id: string, reviewerId: string): Promise<ReviewJob> =>
  api(
    `/code-review/jobs/${encodeURIComponent(id)}/reviewers/${encodeURIComponent(reviewerId)}/retry`,
    {
      method: "POST",
      body: "{}",
    },
  );
export const requestReview = (
  job: ReviewJob,
  scope: ReviewScope,
): Promise<ReviewJob> =>
  api("/code-review/requests", {
    method: "POST",
    body: JSON.stringify({
      installation_id: job.installation_id,
      repository: job.repository,
      pull_number: job.pull_number,
      scope,
    }),
  });
export const saveRepository = (repository: Repository): Promise<Repository> =>
  api("/code-review/repository", {
    method: "PUT",
    body: JSON.stringify({
      installation_id: repository.installation_id,
      repository: repository.repository,
      mode: repository.mode,
      model: repository.model || null,
      router_model: repository.router_model || null,
      router_thinking_level: repository.router_thinking_level || null,
      prompt: repository.prompt,
      reviewer_ids: repository.reviewer_ids,
      routing_mode: repository.routing_mode,
      semantic_routing: repository.semantic_routing,
      included_reviewer_ids: repository.included_reviewer_ids ?? [],
      excluded_reviewer_ids: repository.excluded_reviewer_ids ?? [],
      reviewer_overrides: repository.reviewer_overrides ?? [],
    }),
  });
export const saveReviewer = (
  reviewer: Omit<ReviewerProfile, "built_in"> & { built_in?: boolean },
): Promise<ReviewerProfile> =>
  api("/code-review/reviewer", {
    method: "PUT",
    body: JSON.stringify({
      id: reviewer.id || null,
      name: reviewer.name,
      prompt: reviewer.prompt,
      model: reviewer.model || null,
      default_thinking_level: reviewer.default_thinking_level || null,
    }),
  });
export const deleteReviewer = (id: string): Promise<void> =>
  api(`/code-review/reviewer/${encodeURIComponent(id)}`, { method: "DELETE" });
export const configureApp = (body: {
  app_id: number;
  private_key_pem: string;
  webhook_secret: string;
}): Promise<Dashboard["app"]> =>
  api("/code-review/github-app", { method: "PUT", body: JSON.stringify(body) });
export const refreshReviews = (): Promise<void> =>
  api("/code-review/refresh", { method: "POST", body: "{}" });
export const getProviders = (): Promise<ProvidersResponse> => api("/providers");
export const getModels = (): Promise<Model[]> => api("/models");
export const saveDefaultModel = (
  model: string,
  defaultThinkingLevel?: string,
): Promise<void> =>
  api("/config/default-model", {
    method: "PUT",
    body: JSON.stringify({
      model,
      ...(defaultThinkingLevel !== undefined
        ? { default_thinking_level: defaultThinkingLevel || null }
        : {}),
    }),
  });
export const getKnownProviders = (): Promise<KnownProvider[]> => api("/providers/known");
export const saveProvider = (
  id: string,
  kind: string,
  baseUrl?: string,
  apiKey?: string,
): Promise<Provider> =>
  api(`/providers/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify({
      kind,
      base_url: baseUrl || null,
      api_key: apiKey || null,
    }),
  });
export const getClis = async (): Promise<CliInfo[]> =>
  (await api<{ clis: CliInfo[] }>("/clis")).clis;
export const getCliInstallStatus = (id: string): Promise<CliInstallStatus> =>
  api(`/clis/${encodeURIComponent(id)}/install`);
export const installCli = (id: string): Promise<void> =>
  api(`/clis/${encodeURIComponent(id)}/install`, {
    method: "POST",
    body: "{}",
  });
export const cancelCliInstall = (id: string): Promise<void> =>
  api(`/clis/${encodeURIComponent(id)}/install`, { method: "DELETE" });
export const uninstallCli = (id: string): Promise<void> =>
  api(`/clis/${encodeURIComponent(id)}`, { method: "DELETE" });
export const startLogin = (provider: Provider): Promise<LoginStarted> =>
  api(`/providers/${encodeURIComponent(provider.id)}/login`, {
    method: "POST",
    body: "{}",
  });
export const loginStatus = (providerId: string): Promise<LoginStatus> =>
  api(`/providers/${encodeURIComponent(providerId)}/login`);
export const submitLoginCode = (
  providerId: string,
  code: string,
): Promise<LoginStatus> =>
  api(`/providers/${encodeURIComponent(providerId)}/login/callback`, {
    method: "POST",
    body: JSON.stringify({ callback_url: code }),
  });

export function openServerEvents(onReviewUpdate: () => void): () => void {
  const source = new EventSource("/v1/events");
  source.onmessage = (message) => {
    try {
      const event = JSON.parse(message.data) as EventEnvelope;
      if (event.type === "code_review.updated") onReviewUpdate();
    } catch {
      // Reconnect/replay will deliver the next complete event.
    }
  };
  return () => source.close();
}

export function openJobEvents(
  jobId: string,
  after: number,
  onEvent: (event: EventEnvelope) => void,
): () => void {
  const source = new EventSource(
    `/v1/code-review/jobs/${encodeURIComponent(jobId)}/events?after=${encodeURIComponent(after)}`,
  );
  source.onmessage = (message) => {
    try {
      onEvent(JSON.parse(message.data) as EventEnvelope);
    } catch {
      // Ignore malformed/unknown forward-compatible events.
    }
  };
  return () => source.close();
}
