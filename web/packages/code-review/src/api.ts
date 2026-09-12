import type {
  CodeReviewSettings,
  Dashboard,
  EventEnvelope,
  JobDetail,
  KnownProvider,
  LoginStarted,
  LoginStatus,
  Model,
  PersonaInfo,
  Provider,
  ProvidersResponse,
  Repository,
  ReviewJob,
  ReviewStats,
  ReviewTask,
  ReviewerProfile,
  StatsRange,
} from "./types";
import type { CliInfo, CliInstallStatus } from "./cli";

const EVENT_CURSOR_HEADER = "x-trouve-event-cursor";

export interface ReviewApiOptions {
  /**
   * Origin (and optional path prefix) of the trouve server, without a
   * trailing slash. The empty string means same-origin requests to `/v1/...`,
   * which is how the self-hosted review site is deployed behind its nginx
   * proxy.
   */
  baseUrl?: string;
  /**
   * Transport for JSON requests. Defaults to the global `fetch`; a host that
   * needs extra headers or a different origin supplies its own. Server-sent
   * event streams always use the platform `EventSource` and are unaffected.
   */
  fetch?: typeof globalThis.fetch;
}

export interface DashboardSnapshot {
  dashboard: Dashboard;
  cursor: number;
}

async function decodeApiResponse<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const body = await response.json().catch(() => ({ message: response.statusText }));
    throw new Error(body.message ?? `Request failed (${response.status})`);
  }
  const body = await response.text();
  return body ? (JSON.parse(body) as T) : (undefined as T);
}

const personaId = (reviewer: Pick<ReviewerProfile, "id" | "name">): string =>
  reviewer.id || reviewer.name.trim().toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

/**
 * Build the review dashboard client. Every request is issued relative to
 * `baseUrl`; the default (empty) base keeps the historical same-origin
 * behaviour of the review site.
 */
export function createReviewApi({
  baseUrl = "",
  fetch: fetchImpl,
}: ReviewApiOptions = {}) {
  const root = `${baseUrl.replace(/\/+$/u, "")}/v1`;
  // Resolved per call so test doubles installed on `globalThis` after the
  // client is built are still honoured, as they were for the module-level
  // functions this factory replaced.
  const send: typeof globalThis.fetch = (input, init) =>
    (fetchImpl ?? globalThis.fetch)(input, init);
  async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
    const response = await send(`${root}${path}`, {
      ...init,
      headers: {
        "Content-Type": "application/json",
        ...(init.headers ?? {}),
      },
    });
    return decodeApiResponse<T>(response);
  }

  async function getDashboard(): Promise<DashboardSnapshot> {
    const response = await send(`${root}/code-review`, {
      headers: { "Content-Type": "application/json" },
    });
    const dashboard = await decodeApiResponse<Dashboard>(response);
    const cursorValue = response.headers.get(EVENT_CURSOR_HEADER);
    const cursor = cursorValue === null ? Number.NaN : Number(cursorValue);
    if (!Number.isSafeInteger(cursor) || cursor < 0) {
      throw new Error("Review dashboard response is missing a valid event cursor");
    }
    return { dashboard, cursor };
  }

  const getReviewSettings = (): Promise<CodeReviewSettings> =>
    api("/config/code-review");
  const saveReviewSettings = (
    settings: CodeReviewSettings,
  ): Promise<CodeReviewSettings> =>
    api("/config/code-review", {
      method: "PUT",
      body: JSON.stringify(settings),
    });
  const getJob = (id: string): Promise<JobDetail> =>
    api(`/code-review/jobs/${encodeURIComponent(id)}?include_task_content=false`);
  const getTask = (jobId: string, taskId: string): Promise<ReviewTask> =>
    api(
      `/code-review/jobs/${encodeURIComponent(jobId)}/tasks/${encodeURIComponent(taskId)}`,
    );
  const getJobs = (
    status: string,
    repository: string,
  ): Promise<{ jobs: ReviewJob[] }> => {
    const query = new URLSearchParams({ limit: "250" });
    if (status) query.set("status", status);
    if (repository) query.set("repository", repository);
    return api(`/code-review/jobs?${query}`);
  };
  const getStats = (range: StatsRange, repository: string): Promise<ReviewStats> => {
    const query = new URLSearchParams({ range });
    if (repository) query.set("repository", repository);
    return api(`/code-review/stats?${query}`);
  };
  const cancelJob = (id: string): Promise<ReviewJob> =>
    api(`/code-review/jobs/${encodeURIComponent(id)}/cancel`, {
      method: "POST",
      body: "{}",
    });
  const retryJob = (id: string): Promise<ReviewJob> =>
    api(`/code-review/jobs/${encodeURIComponent(id)}/retry`, {
      method: "POST",
      body: "{}",
    });
  const retryPersona = (id: string, reviewerId: string): Promise<ReviewJob> =>
    api(
      `/code-review/jobs/${encodeURIComponent(id)}/reviewers/${encodeURIComponent(reviewerId)}/retry`,
      {
        method: "POST",
        body: "{}",
      },
    );
  const retryFinalEditor = (id: string): Promise<ReviewJob> =>
    api(`/code-review/jobs/${encodeURIComponent(id)}/final-editor/retry`, {
      method: "POST",
      body: "{}",
    });
  const requestReview = (job: ReviewJob): Promise<ReviewJob> =>
    api("/code-review/requests", {
      method: "POST",
      body: JSON.stringify({
        installation_id: job.installation_id,
        repository: job.repository,
        pull_number: job.pull_number,
      }),
    });
  const saveRepository = (repository: Repository): Promise<Repository> =>
    api("/code-review/repository", {
      method: "PUT",
      body: JSON.stringify({
        installation_id: repository.installation_id,
        repository: repository.repository,
        mode: repository.mode,
        model: repository.model || null,
        coordinator_thinking_level: repository.coordinator_thinking_level || null,
        router_model: repository.router_model || null,
        router_thinking_level: repository.router_thinking_level || null,
        analyst_model: repository.analyst_model || null,
        analyst_thinking_level: repository.analyst_thinking_level || null,
        // Always sent as maps: an empty map clears stale options server-side.
        coordinator_model_options: repository.coordinator_model_options ?? {},
        router_model_options: repository.router_model_options ?? {},
        analyst_model_options: repository.analyst_model_options ?? {},
        prompt: repository.prompt,
        reviewer_ids: repository.reviewer_ids,
        routing_mode: repository.routing_mode,
        semantic_routing:
          repository.routing_mode === "automatic" ? true : repository.semantic_routing,
        included_reviewer_ids:
          repository.routing_mode === "additive" ? (repository.included_reviewer_ids ?? []) : [],
        excluded_reviewer_ids: [],
        reviewer_overrides: repository.reviewer_overrides ?? [],
      }),
    });
  const saveReviewer = async (
    reviewer: Omit<ReviewerProfile, "built_in"> & { built_in?: boolean },
  ): Promise<void> => {
    const id = personaId(reviewer);
    if (!id) {
      throw new Error("Persona name must include at least one ASCII letter or digit.");
    }
    const personas = await api<PersonaInfo[]>("/persona-infos");
    const existing = personas.find((info) => info.persona.id === id)?.persona;
    if (reviewer.id === "" && existing !== undefined) {
      throw new Error(`A persona with the ID "${id}" already exists.`);
    }
    const policy = existing ?? personas.find((info) => info.persona.id === "review")?.persona;
    if (policy === undefined) {
      throw new Error("The built-in Reviewer persona is unavailable.");
    }
    await api(`/personas/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify({
        display_name: reviewer.name,
        group: "reviewer",
        system_prompt: reviewer.prompt,
        allowed_tools: policy.allowed_tools,
        read_only: policy.read_only,
        default_permission_mode: policy.default_permission_mode ?? null,
        default_model: reviewer.model || null,
        default_thinking_level: reviewer.default_thinking_level || null,
      }),
    });
  };
  const deleteReviewer = (id: string): Promise<void> =>
    api(`/personas/${encodeURIComponent(id)}`, { method: "DELETE" });
  const configureApp = (body: {
    app_id: number;
    private_key_pem: string;
    webhook_secret: string;
  }): Promise<Dashboard["app"]> =>
    api("/code-review/github-app", { method: "PUT", body: JSON.stringify(body) });
  const getProviders = (): Promise<ProvidersResponse> => api("/providers");
  const getModels = (): Promise<Model[]> => api("/models");
  const getPersonaInfos = (): Promise<PersonaInfo[]> => api("/persona-infos");
  const savePersona = (persona: PersonaInfo["persona"]): Promise<void> =>
    api(`/personas/${encodeURIComponent(persona.id)}`, {
      method: "PUT",
      body: JSON.stringify({
        display_name: persona.display_name,
        group: persona.group ?? "general",
        system_prompt: persona.system_prompt,
        allowed_tools: persona.allowed_tools,
        read_only: persona.read_only,
        default_permission_mode: persona.default_permission_mode ?? null,
        default_model: persona.default_model ?? null,
        default_thinking_level: persona.default_thinking_level ?? null,
      }),
    });
  const resetPersona = (id: string): Promise<void> =>
    api(`/personas/${encodeURIComponent(id)}`, { method: "DELETE" });
  const saveDefaultModel = (
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
  const getKnownProviders = (): Promise<KnownProvider[]> => api("/providers/known");
  const saveProvider = (
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
        ...(apiKey === undefined ? {} : { api_key: apiKey || null }),
      }),
    });
  const getClis = async (): Promise<CliInfo[]> =>
    (await api<{ clis: CliInfo[] }>("/clis")).clis;
  const getCliInstallStatus = (id: string): Promise<CliInstallStatus> =>
    api(`/clis/${encodeURIComponent(id)}/install`);
  const installCli = (id: string): Promise<void> =>
    api(`/clis/${encodeURIComponent(id)}/install`, {
      method: "POST",
      body: "{}",
    });
  const cancelCliInstall = (id: string): Promise<void> =>
    api(`/clis/${encodeURIComponent(id)}/install`, { method: "DELETE" });
  const uninstallCli = (id: string): Promise<void> =>
    api(`/clis/${encodeURIComponent(id)}`, { method: "DELETE" });
  const startLogin = (provider: Provider): Promise<LoginStarted> =>
    api(`/providers/${encodeURIComponent(provider.id)}/login`, {
      method: "POST",
      body: "{}",
    });
  const loginStatus = (providerId: string): Promise<LoginStatus> =>
    api(`/providers/${encodeURIComponent(providerId)}/login`);
  const submitLoginCode = (
    providerId: string,
    code: string,
  ): Promise<LoginStatus> =>
    api(`/providers/${encodeURIComponent(providerId)}/login/callback`, {
      method: "POST",
      body: JSON.stringify({ callback_url: code }),
    });

  function openServerEvents(
    after: number,
    onReviewUpdate: (event: EventEnvelope) => void,
  ): () => void {
    const source = new EventSource(`${root}/events?after=${encodeURIComponent(after)}`);
    source.onmessage = (message) => {
      try {
        const event = JSON.parse(message.data) as EventEnvelope;
        if (event.type === "code_review.updated") onReviewUpdate(event);
      } catch {
        // Reconnect/replay will deliver the next complete event.
      }
    };
    return () => source.close();
  }

  function openJobEvents(
    jobId: string,
    after: number,
    onEvent: (event: EventEnvelope) => void,
  ): () => void {
    const source = new EventSource(
      `${root}/code-review/jobs/${encodeURIComponent(jobId)}/events?after=${encodeURIComponent(after)}`,
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

  return {
    baseUrl,
    api,
    getDashboard,
    getReviewSettings,
    saveReviewSettings,
    getJob,
    getTask,
    getJobs,
    getStats,
    cancelJob,
    retryJob,
    retryPersona,
    retryFinalEditor,
    requestReview,
    saveRepository,
    saveReviewer,
    deleteReviewer,
    configureApp,
    getProviders,
    getModels,
    getPersonaInfos,
    savePersona,
    resetPersona,
    saveDefaultModel,
    getKnownProviders,
    saveProvider,
    getClis,
    getCliInstallStatus,
    installCli,
    cancelCliInstall,
    uninstallCli,
    startLogin,
    loginStatus,
    submitLoginCode,
    openServerEvents,
    openJobEvents,
  };
}

export type ReviewApi = ReturnType<typeof createReviewApi>;
