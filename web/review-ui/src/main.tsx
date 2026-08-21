import { Chart, registerables } from "chart.js";
import { render } from "preact";
import type { ComponentChildren } from "preact";
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import "./styles.css";
import {
  api,
  cancelCliInstall,
  cancelJob,
  configureApp,
  deleteReviewer,
  getCliInstallStatus,
  getClis,
  getDashboard,
  getJob,
  getJobs,
  getKnownProviders,
  getPersonaInfos,
  getModels,
  getProviders,
  getReviewSettings,
  getStats,
  getTask,
  installCli,
  loginStatus,
  openJobEvents,
  openServerEvents,
  requestReview,
  resetPersona,
  retryJob,
  retryPersona,
  saveDefaultModel,
  savePersona,
  saveProvider,
  saveRepository,
  saveReviewSettings,
  saveReviewer,
  startLogin,
  submitLoginCode,
  uninstallCli,
} from "./api";
import {
  cliIsInstalled,
  cliProgressLabel,
  cliVersionLabel,
  idleCliInstallStatus,
} from "./cli";
import type { CliInfo, CliInstallStatus } from "./cli";
import {
  defaultThinkingSelection,
  thinkingLevelLabel,
  thinkingOptions,
  thinkingSelectionIsValid,
} from "./model-settings";
import {
  LIVE_OUTPUT_BATCH_MS,
  appendBoundedReviewOutput,
  boundReviewOutput,
  boundReviewTaskOutput,
  reviewTaskSummary,
  type ReviewOutputField,
} from "./review-output";
import { liveModelElapsed, mergeReviewTaskSnapshot } from "./review-progress";
import {
  MAX_PARALLEL_REVIEWS,
  TIMEOUT_MINUTES_INPUT_MIN,
  TIMEOUT_MINUTES_INPUT_STEP,
  reviewSettingsFromMinutes,
  timeoutMinutes,
} from "./review-settings";
import { routingReasonLabel } from "./routing-labels";
import { jobStatusClass, safeExternalUrl } from "./security";
import type {
  CodeReviewSettings,
  Dashboard,
  DurationStats,
  EventEnvelope,
  GithubAppStatus,
  JobDetail,
  KnownProvider,
  LoginStarted,
  Model,
  PersonaInfo,
  PersonaResult,
  Provider,
  ProvidersResponse,
  Repository,
  ReviewJob,
  RoutingDecision,
  ReviewTask,
  ReviewTaskProgress,
  ReviewStats,
  ReviewerOverride,
  ReviewerProfile,
  StatsRange,
} from "./types";

Chart.register(...registerables);

type Section = "overview" | "jobs" | "repositories" | "reviewers" | "stats" | "settings";

const SERVER_EVENT_REFRESH_DEBOUNCE_MS = 100;
const AUTOMATIC_RETRY_MS = 5_000;
const DASHBOARD_FALLBACK_REFRESH_MS = 30_000;
const CLI_IDLE_REFRESH_MS = 5 * 60_000;

const sections: Array<{ id: Section; label: string; icon: string }> = [
  { id: "overview", label: "Overview", icon: "◫" },
  { id: "jobs", label: "Review jobs", icon: "◉" },
  { id: "repositories", label: "Repositories", icon: "⌘" },
  { id: "reviewers", label: "Reviewer personas", icon: "◎" },
  { id: "stats", label: "Statistics", icon: "↗" },
  { id: "settings", label: "Settings", icon: "⚙" },
];

function routeFromHash(): { section: Section; jobId: string } {
  const [rawSection, rawId = ""] = window.location.hash.replace(/^#\/?/, "").split("/");
  const section = sections.some(({ id }) => id === rawSection)
    ? (rawSection as Section)
    : "overview";
  return { section, jobId: section === "jobs" ? rawId : "" };
}

function navigate(section: Section, id = ""): void {
  window.location.hash = `/${section}${id ? `/${id}` : ""}`;
}

function formatDate(value?: string): string {
  return value ? new Date(value).toLocaleString() : "—";
}

function duration(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
  const hours = Math.floor(seconds / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  const remainder = seconds % 60;
  if (hours) return `${hours}h ${minutes}m ${remainder}s`;
  if (minutes) return `${minutes}m ${remainder}s`;
  return `${remainder}s`;
}

function liveElapsed(
  baseline: number,
  status: string,
  startedAt: string | undefined,
  now: number,
): number {
  if (status !== "running" || !startedAt) return baseline;
  const liveAge = Math.max(0, now - new Date(startedAt).getTime());
  return Math.max(baseline, liveAge);
}

function taskLifecycleLabel(stage: ReviewTask["lifecycle_stage"]): string {
  switch (stage) {
    case "waiting_for_capacity":
      return "Waiting for capacity";
    case "starting_model":
      return "Starting model";
    case "running_model":
      return "Running model";
    case "running_tool":
      return "Running tool";
    case "repairing_output":
      return "Repairing output";
    case "completed":
      return "Completed";
    default:
      return "Queued";
  }
}

function isReviewTaskProgress(
  progress: EventEnvelope["progress"],
): progress is ReviewTaskProgress {
  return Boolean(progress && "lifecycle_stage" in progress);
}

function pickPreferredTask(tasks: ReviewTask[]): ReviewTask | undefined {
  const latestByBatch = new Map<string, ReviewTask>();
  tasks.forEach((task) => {
    const reviewerKey = task.reviewer_id || task.reviewer_name || task.id;
    const key = `${task.role}:${reviewerKey}:${task.batch_index}`;
    const current = latestByBatch.get(key);
    if (
      !current ||
      task.created_at > current.created_at ||
      (task.created_at === current.created_at && task.id > current.id)
    ) {
      latestByBatch.set(key, task);
    }
  });
  const latest = [...latestByBatch.values()];
  return (
    latest.find((task) => task.status === "running") ??
    latest.find((task) => task.status === "queued") ??
    latest.find((task) => task.status === "failed") ??
    latest
      .slice()
      .reverse()
      .find((task) => task.role === "coordinator") ??
    latest[0] ??
    tasks[0]
  );
}

function taskAttemptLabel(tasks: ReviewTask[], task: ReviewTask): string {
  const attempts = tasks.filter(
    (candidate) =>
      candidate.role === task.role && candidate.batch_index === task.batch_index,
  );
  const base =
    task.role === "coordinator"
      ? "Attempt"
      : task.role === "router"
        ? `Routing ${task.batch_index + 1}`
        : `Batch ${task.batch_index + 1}`;
  if (attempts.length === 1) return base;
  return `${base} · attempt ${attempts.indexOf(task) + 1}`;
}

function routingModeLabel(mode: Repository["routing_mode"]): string {
  switch (mode) {
    case "additive":
      return "Additive";
    case "automatic":
      return "Automatic";
    default:
      return "Manual";
  }
}

function useClock(active: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    let timer: number | undefined;
    const syncVisibility = (): void => {
      if (timer !== undefined) window.clearInterval(timer);
      timer = undefined;
      if (document.visibilityState === "visible") {
        setNow(Date.now());
        timer = window.setInterval(() => setNow(Date.now()), 1_000);
      }
    };
    document.addEventListener("visibilitychange", syncVisibility);
    syncVisibility();
    return () => {
      document.removeEventListener("visibilitychange", syncVisibility);
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [active]);
  return now;
}

function useFlash(): [string, (message: string) => void] {
  const [message, setMessage] = useState("");
  const flash = useCallback((next: string): void => {
    setMessage(next);
    window.setTimeout(() => setMessage(""), 2_500);
  }, []);
  return [message, flash];
}

function StatusPill({ status }: { status: string }) {
  return <span class={`status ${jobStatusClass(status)}`}>{status}</span>;
}

function ProgressBar({ job }: { job: ReviewJob }) {
  return (
    <div
      class="progress"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={job.progress.percent}
      aria-label={`${job.progress.completed_reviewers} of ${job.progress.total_reviewers} reviewers complete`}
    >
      <span
        style={{
          transform: `scaleX(${Math.max(0, Math.min(100, job.progress.percent)) / 100})`,
        }}
      />
      <small>
        {job.progress.completed_reviewers}/{job.progress.total_reviewers} reviewers ·{" "}
        {job.progress.percent}%
      </small>
    </div>
  );
}

function CopyButton({ text, label = "Copy prompt" }: { text: string; label?: string }) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">("idle");
  const copy = async (): Promise<void> => {
    try {
      if (!navigator.clipboard?.writeText) throw new Error("Clipboard access is unavailable");
      await navigator.clipboard.writeText(text);
      setCopyState("copied");
    } catch {
      setCopyState("failed");
    }
    window.setTimeout(() => setCopyState("idle"), 1_500);
  };
  return (
    <button class="ghost compact" type="button" onClick={() => void copy()} disabled={!text}>
      {copyState === "copied" ? "Copied" : copyState === "failed" ? "Copy failed" : label}
    </button>
  );
}

function ExternalLink({ href, children }: { href: string; children: ComponentChildren }) {
  const safe = safeExternalUrl(href);
  return safe ? (
    <a href={safe} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  ) : null;
}

function App() {
  const [route, setRoute] = useState(routeFromHash);
  const [dashboard, setDashboard] = useState<Dashboard | null>(null);
  const [providers, setProviders] = useState<ProvidersResponse | null>(null);
  const [reviewSettings, setReviewSettings] = useState<CodeReviewSettings | null>(null);
  const [models, setModels] = useState<Model[]>([]);
  const [modeInfos, setPersonaInfos] = useState<PersonaInfo[]>([]);
  const [dashboardError, setDashboardError] = useState("");
  const [configurationError, setConfigurationError] = useState("");
  const [loading, setLoading] = useState(true);
  const [serverEventAfter, setServerEventAfter] = useState<number | null>(null);
  const serverEventCursorRef = useRef(0);
  const dashboardLoadRef = useRef<{
    promise: Promise<void> | null;
    reloadRequested: boolean;
  }>({ promise: null, reloadRequested: false });
  const configurationLoadRef = useRef<Promise<void> | null>(null);

  const loadDashboard = useCallback((quiet = false): Promise<void> => {
    const state = dashboardLoadRef.current;
    if (state.promise) {
      state.reloadRequested = true;
      return state.promise;
    }
    const request = (async (): Promise<void> => {
      if (!quiet) setLoading(true);
      try {
        do {
          state.reloadRequested = false;
          try {
            const snapshot = await getDashboard();
            serverEventCursorRef.current = Math.max(
              serverEventCursorRef.current,
              snapshot.cursor,
            );
            setServerEventAfter((current) => current ?? snapshot.cursor);
            setDashboard(snapshot.dashboard);
            setDashboardError("");
          } catch (cause) {
            setDashboardError(cause instanceof Error ? cause.message : String(cause));
          }
        } while (state.reloadRequested);
      } finally {
        if (!quiet) setLoading(false);
      }
    })();
    state.promise = request;
    void request.finally(() => {
      if (state.promise === request) state.promise = null;
    });
    return request;
  }, []);

  const loadConfiguration = useCallback((): Promise<void> => {
    if (configurationLoadRef.current) return configurationLoadRef.current;
    const request = (async (): Promise<void> => {
      const results = await Promise.allSettled([
        getProviders().then(setProviders),
        getReviewSettings().then(setReviewSettings),
        getModels().then(setModels),
        getPersonaInfos().then(setPersonaInfos),
      ]);
      const errors = results
        .filter((result): result is PromiseRejectedResult => result.status === "rejected")
        .map(({ reason }) => (reason instanceof Error ? reason.message : String(reason)));
      setConfigurationError(errors.join("; "));
    })();
    configurationLoadRef.current = request;
    void request.finally(() => {
      if (configurationLoadRef.current === request) configurationLoadRef.current = null;
    });
    return request;
  }, []);

  useEffect(() => {
    const onHash = (): void => setRoute(routeFromHash());
    window.addEventListener("hashchange", onHash);
    if (!window.location.hash) navigate("overview");
    void loadDashboard();
    return () => window.removeEventListener("hashchange", onHash);
  }, [loadDashboard]);

  const needsConfiguration =
    route.section === "repositories" ||
    route.section === "reviewers" ||
    route.section === "settings";
  useEffect(() => {
    if (needsConfiguration) void loadConfiguration();
  }, [needsConfiguration, loadConfiguration]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadDashboard(true);
    }, DASHBOARD_FALLBACK_REFRESH_MS);
    return () => window.clearInterval(timer);
  }, [loadDashboard]);

  useEffect(() => {
    if (!dashboardError) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadDashboard(true);
    }, AUTOMATIC_RETRY_MS);
    return () => window.clearInterval(timer);
  }, [dashboardError, loadDashboard]);

  useEffect(() => {
    if (!needsConfiguration || !configurationError) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadConfiguration();
    }, AUTOMATIC_RETRY_MS);
    return () => window.clearInterval(timer);
  }, [configurationError, loadConfiguration, needsConfiguration]);

  useEffect(() => {
    if (serverEventAfter === null) return;
    let refreshTimer: number | undefined;
    const close = openServerEvents(serverEventAfter, (event) => {
      if (event.cursor <= serverEventCursorRef.current) return;
      serverEventCursorRef.current = event.cursor;
      if (refreshTimer !== undefined) window.clearTimeout(refreshTimer);
      refreshTimer = window.setTimeout(() => {
        refreshTimer = undefined;
        void loadDashboard(true);
      }, SERVER_EVENT_REFRESH_DEBOUNCE_MS);
    });
    return () => {
      if (refreshTimer !== undefined) window.clearTimeout(refreshTimer);
      close();
    };
  }, [serverEventAfter, loadDashboard]);

  const error = dashboardError || (needsConfiguration ? configurationError : "");
  const content = dashboard ? (
    <>
      {route.section === "overview" && <Overview dashboard={dashboard} />}
      {route.section === "jobs" && (
        <JobsPage
          dashboard={dashboard}
          selectedId={route.jobId}
          onChanged={() => void loadDashboard(true)}
        />
      )}
      {route.section === "repositories" && (
        <RepositoriesPage
          dashboard={dashboard}
          models={models}
          onChanged={() => void loadDashboard(true)}
        />
      )}
      {route.section === "reviewers" && (
        <ReviewersPage
          reviewers={dashboard.reviewers}
          models={models}
          defaultModel={providers?.default_model}
          onChanged={() => void loadDashboard(true)}
        />
      )}
      {route.section === "stats" && (
        <StatsPage repositories={dashboard.repositories} />
      )}
      {route.section === "settings" && (
        <SettingsPage
          app={dashboard.app}
          providers={providers}
          reviewSettings={reviewSettings}
          models={models}
          reviewPersonaInfo={modeInfos.find(({ persona }) => persona.id === "review")}
          onChanged={() => {
            void loadDashboard(true);
            void loadConfiguration();
          }}
        />
      )}
    </>
  ) : null;

  return (
    <div class="shell">
      <aside class="sidebar">
        <a class="brand" href="#/overview" aria-label="trouve review dashboard">
          <span class="brand-mark">t</span>
          <span>
            <strong>trouve</strong>
            <small>code reviews</small>
          </span>
        </a>
        <nav aria-label="Dashboard sections">
          {sections.map(({ id, label, icon }) => (
            <a
              key={id}
              href={`#/${id}`}
              class={route.section === id ? "active" : ""}
              aria-current={route.section === id ? "page" : undefined}
            >
              <span aria-hidden="true">{icon}</span>
              {label}
              {id === "jobs" && dashboard && (
                <b>{dashboard.jobs.filter((job) => job.status === "running").length}</b>
              )}
            </a>
          ))}
        </nav>
        <div class="sidebar-health">
          <i class={dashboard?.app.configured ? "online" : ""} />
          <span>
            {dashboard?.app.configured ? "GitHub App online" : "GitHub App not configured"}
          </span>
        </div>
      </aside>
      <main class="content">
        {error && (
          <div class="banner error" role="alert">
            {error}
            <span>Retrying automatically.</span>
          </div>
        )}
        {loading && !dashboard ? <div class="loading">Loading review operations…</div> : content}
      </main>
    </div>
  );
}

function Overview({ dashboard }: { dashboard: Dashboard }) {
  const counts = dashboard.jobs.reduce<Record<string, number>>((result, job) => {
    result[job.status] = (result[job.status] ?? 0) + 1;
    return result;
  }, {});
  const active = dashboard.jobs.filter(
    (job) => job.status === "running" || job.status === "queued",
  );
  const now = useClock(active.some((job) => job.status === "running"));
  const recent = dashboard.jobs.filter((job) => !active.includes(job)).slice(0, 8);
  return (
    <section>
      <PageHeader
        eyebrow="Operations"
        title="Review overview"
        description="Live review activity, queue health, and recent outcomes."
      />
      <div class="metric-grid">
        {[
          ["Running", counts.running ?? 0, "blue"],
          ["Pending", counts.queued ?? 0, "amber"],
          ["Succeeded", counts.succeeded ?? 0, "green"],
          ["Failed", counts.failed ?? 0, "red"],
        ].map(([label, value, color]) => (
          <article class={`metric ${color}`} key={String(label)}>
            <span>{label}</span>
            <strong>{value}</strong>
            <small>in current history</small>
          </article>
        ))}
      </div>
      <div class="overview-grid">
        <section class="panel">
          <PanelTitle title="Active reviews" subtitle={`${active.length} jobs in flight`} />
          {active.length ? (
            <div class="job-list compact-list">
              {active.map((job) => (
                <JobRow job={job} now={now} key={job.id} />
              ))}
            </div>
          ) : (
            <EmptyState title="Queue is clear" body="New review jobs will appear here immediately." />
          )}
        </section>
        <section class="panel">
          <PanelTitle title="Recent outcomes" subtitle="Most recently completed" />
          <div class="job-list compact-list">
            {recent.map((job) => (
              <JobRow job={job} now={now} key={job.id} />
            ))}
          </div>
        </section>
      </div>
      {dashboard.app.last_error && (
        <div class="banner error" role="status">
          <strong>Last reconciliation error:</strong> {dashboard.app.last_error}
        </div>
      )}
    </section>
  );
}

function PageHeader({
  eyebrow,
  title,
  description,
  action,
}: {
  eyebrow: string;
  title: string;
  description: string;
  action?: ComponentChildren;
}) {
  return (
    <header class="page-header">
      <div>
        <p class="eyebrow">{eyebrow}</p>
        <h1>{title}</h1>
        <p>{description}</p>
      </div>
      {action && <div class="header-actions">{action}</div>}
    </header>
  );
}

function PanelTitle({ title, subtitle }: { title: string; subtitle?: string }) {
  return (
    <header class="panel-title">
      <div>
        <h2>{title}</h2>
        {subtitle && <p>{subtitle}</p>}
      </div>
    </header>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div class="empty">
      <strong>{title}</strong>
      <p>{body}</p>
    </div>
  );
}

function JobRow({ job, now }: { job: ReviewJob; now: number }) {
  const elapsed = liveElapsed(job.running_elapsed_ms, job.status, job.started_at, now);
  return (
    <button class="job-row" type="button" onClick={() => navigate("jobs", job.id)}>
      <StatusPill status={job.status} />
      <span class="job-main">
        <strong>
          {job.repository} #{job.pull_number}
        </strong>
        <small>{job.pull_title}</small>
        {(job.status === "running" || job.status === "queued") && <ProgressBar job={job} />}
      </span>
      <span class="job-meta">
        <b>{job.issue_count} issues</b>
        <small>{job.status === "queued" ? duration(job.pending_elapsed_ms) : duration(elapsed)}</small>
      </span>
    </button>
  );
}

function JobsPage({
  dashboard,
  selectedId,
  onChanged,
}: {
  dashboard: Dashboard;
  selectedId: string;
  onChanged: () => void;
}) {
  const [status, setStatus] = useState("");
  const [repository, setRepository] = useState("");
  const [jobs, setJobs] = useState(dashboard.jobs);
  const [error, setError] = useState("");
  const now = useClock(jobs.some((job) => job.status === "running"));

  const load = async (): Promise<void> => {
    try {
      setJobs((await getJobs(status, repository)).jobs);
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  };
  useEffect(() => void load(), [status, repository, dashboard.jobs]);

  return (
    <section>
      <PageHeader
        eyebrow="History"
        title="Review jobs"
        description="Running jobs first, then pending in execution order, then recent terminal jobs."
      />
      <div class={selectedId ? "jobs-layout detail-open" : "jobs-layout"}>
        <section class="panel jobs-index">
          <div class="filters">
            <label>
              Status
              <select value={status} onChange={(event) => setStatus(event.currentTarget.value)}>
                <option value="">All statuses</option>
                {["running", "queued", "succeeded", "failed", "cancelled", "stale"].map(
                  (value) => (
                    <option value={value} key={value}>
                      {value}
                    </option>
                  ),
                )}
              </select>
            </label>
            <label>
              Repository
              <select
                value={repository}
                onChange={(event) => setRepository(event.currentTarget.value)}
              >
                <option value="">All repositories</option>
                {dashboard.repositories.map((repo) => (
                  <option value={repo.repository} key={repo.repository}>
                    {repo.repository}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {error && <p class="error-text">{error}</p>}
          <div class="job-list">
            {jobs.map((job) => (
              <JobRow job={job} now={now} key={job.id} />
            ))}
          </div>
        </section>
        {selectedId && (
          <JobDetailPane
            jobId={selectedId}
            onClose={() => navigate("jobs")}
            onChanged={() => {
              void load();
              onChanged();
            }}
          />
        )}
      </div>
    </section>
  );
}

function JobDetailPane({
  jobId,
  onClose,
  onChanged,
}: {
  jobId: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [detail, setDetail] = useState<JobDetail | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [selectedTaskId, setSelectedTaskId] = useState("");
  const [taskDetails, setTaskDetails] = useState<Record<string, ReviewTask>>({});
  const [taskLoading, setTaskLoading] = useState("");
  const [taskErrors, setTaskErrors] = useState<Record<string, string>>({});
  const [eventCursor, setEventCursor] = useState<number | null>(null);
  const [routingOpen, setRoutingOpen] = useState(false);
  const [navigationStatus, setNavigationStatus] = useState("");
  const now = useClock(detail?.job.status === "running");
  const aliveRef = useRef<string | null>(jobId);
  const detailRef = useRef(detail);
  const jobHeadingRef = useRef<HTMLHeadingElement>(null);
  const focusReplacementJobIdRef = useRef("");
  const taskRequestsRef = useRef(new Set<string>());
  const selectedTaskIdRef = useRef(selectedTaskId);
  const taskDetailsRef = useRef(taskDetails);
  detailRef.current = detail;
  selectedTaskIdRef.current = selectedTaskId;
  taskDetailsRef.current = taskDetails;
  const load = useCallback(async (): Promise<JobDetail | undefined> => {
    const requestedJobId = jobId;
    try {
      const response = await getJob(requestedJobId);
      const receivedAt = Date.now();
      const currentTaskList = detailRef.current?.tasks ?? [];
      const currentTasks = new Map<string, ReviewTask>(
        currentTaskList.map(
          (task): [string, ReviewTask] => [task.id, task],
        ),
      );
      const responseTaskIds = new Set(response.tasks.map((task) => task.id));
      const next = {
        ...response,
        tasks: [
          ...response.tasks.map((task) =>
            mergeReviewTaskSnapshot(currentTasks.get(task.id), task, receivedAt),
          ),
          ...currentTaskList.filter((task) => !responseTaskIds.has(task.id)),
        ],
      };
      if (aliveRef.current === requestedJobId) {
        detailRef.current = next;
        setDetail(next);
        setEventCursor((current) => current ?? next.event_cursor ?? 0);
        setError("");
        return next;
      }
    } catch (cause) {
      if (aliveRef.current === requestedJobId) {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    }
    return undefined;
  }, [jobId]);
  const loadTask = useCallback(
    async (taskId: string): Promise<void> => {
      if (taskRequestsRef.current.has(taskId)) return;
      taskRequestsRef.current.add(taskId);
      setTaskLoading(taskId);
      setTaskErrors((current) => {
        if (!(taskId in current)) return current;
        const next = { ...current };
        delete next[taskId];
        return next;
      });
      try {
        const response = await getTask(jobId, taskId);
        const receivedAt = Date.now();
        const currentTask =
          taskDetailsRef.current[taskId] ??
          detailRef.current?.tasks.find((task) => task.id === taskId);
        const next = boundReviewTaskOutput(
          mergeReviewTaskSnapshot(currentTask, response, receivedAt),
        );
        if (
          aliveRef.current === jobId &&
          next.job_id === jobId &&
          selectedTaskIdRef.current === taskId
        ) {
          const details = { [taskId]: next };
          taskDetailsRef.current = details;
          setTaskDetails(details);
        }
      } catch (cause) {
        if (aliveRef.current === jobId) {
          const message = cause instanceof Error ? cause.message : String(cause);
          setTaskErrors((current) => ({ ...current, [taskId]: message }));
        }
      } finally {
        taskRequestsRef.current.delete(taskId);
        setTaskLoading((current) => (current === taskId ? "" : current));
      }
    },
    [jobId],
  );
  useEffect(() => {
    aliveRef.current = jobId;
    detailRef.current = null;
    setDetail(null);
    setSelectedTaskId("");
    selectedTaskIdRef.current = "";
    taskDetailsRef.current = {};
    setTaskDetails({});
    setTaskLoading("");
    setTaskErrors({});
    setEventCursor(null);
    setRoutingOpen(false);
    taskRequestsRef.current.clear();
    void load();
    return () => {
      if (aliveRef.current === jobId) aliveRef.current = null;
    };
  }, [jobId, load]);

  useEffect(() => {
    if (eventCursor === null) return;
    type PendingOutput = Partial<Record<ReviewOutputField, string>>;
    const pendingOutput = new Map<string, PendingOutput>();
    let outputTimer: number | undefined;
    let detailReloadTimer: number | undefined;
    let missedHiddenOutput = false;

    const scheduleDetailReload = (): void => {
      if (detailReloadTimer !== undefined) window.clearTimeout(detailReloadTimer);
      detailReloadTimer = window.setTimeout(() => {
        detailReloadTimer = undefined;
        void load();
      }, 150);
    };

    const flushOutput = (): void => {
      outputTimer = undefined;
      if (document.visibilityState !== "visible") {
        missedHiddenOutput ||= pendingOutput.size > 0;
        pendingOutput.clear();
        return;
      }
      const patches = new Map(pendingOutput);
      pendingOutput.clear();
      if (!patches.size) return;
      setTaskDetails((current) => {
        let next = current;
        for (const [taskId, streams] of patches) {
          const task = next[taskId];
          if (!task) continue;
          let updated = task;
          for (const [field, text] of Object.entries(streams) as Array<
            [ReviewOutputField, string]
          >) {
            updated = {
              ...updated,
              [field]: appendBoundedReviewOutput(updated[field], text),
            };
          }
          if (updated !== task) {
            if (next === current) next = { ...current };
            next[taskId] = updated;
          }
        }
        taskDetailsRef.current = next;
        return next;
      });
    };

    const scheduleOutputFlush = (): void => {
      if (outputTimer !== undefined || document.visibilityState !== "visible") return;
      outputTimer = window.setTimeout(flushOutput, LIVE_OUTPUT_BATCH_MS);
    };

    const syncVisibility = (): void => {
      if (document.visibilityState !== "visible") {
        if (outputTimer !== undefined) window.clearTimeout(outputTimer);
        outputTimer = undefined;
        missedHiddenOutput ||= pendingOutput.size > 0;
        pendingOutput.clear();
        return;
      }
      if (!missedHiddenOutput) return;
      missedHiddenOutput = false;
      const taskId = selectedTaskIdRef.current;
      taskDetailsRef.current = {};
      setTaskDetails({});
      void load();
      if (taskId) void loadTask(taskId);
    };

    document.addEventListener("visibilitychange", syncVisibility);
    const close = openJobEvents(jobId, eventCursor, (event) => {
      if (aliveRef.current !== jobId) return;
      if (event.type === "code_review.output_delta" && event.task_id && event.text) {
        if (event.task_id !== selectedTaskIdRef.current) return;
        if (document.visibilityState !== "visible") {
          missedHiddenOutput = true;
          return;
        }
        if (!taskDetailsRef.current[event.task_id]) {
          void loadTask(event.task_id);
          return;
        }
        const field: ReviewOutputField =
          event.stream === "thinking"
            ? "thinking"
            : event.stream === "tool"
              ? "tool_output"
              : "output";
        const streams = pendingOutput.get(event.task_id) ?? {};
        streams[field] = appendBoundedReviewOutput(streams[field] ?? "", event.text);
        pendingOutput.set(event.task_id, streams);
        scheduleOutputFlush();
      } else if (
        event.type === "code_review.routing_updated" &&
        event.routing_decisions
      ) {
        setDetail((current) => {
          const next = current
            ? { ...current, routing_decisions: event.routing_decisions }
            : current;
          detailRef.current = next;
          return next;
        });
      } else if (
        event.type === "code_review.task_progress_updated" &&
        event.task_id &&
        isReviewTaskProgress(event.progress)
      ) {
        const taskId = event.task_id;
        const progress = event.progress;
        const receivedAt = Date.now();
        const mergeProgress = (task: ReviewTask): ReviewTask =>
          mergeReviewTaskSnapshot(task, { ...task, ...progress }, receivedAt);
        setDetail((current) => {
          const next = current
            ? {
                ...current,
                tasks: current.tasks.map((task) =>
                  task.id === taskId ? mergeProgress(task) : task,
                ),
              }
            : current;
          detailRef.current = next;
          return next;
        });
        setTaskDetails((current) => {
          const task = current[taskId];
          if (!task) return current;
          const next = { ...current, [taskId]: mergeProgress(task) };
          taskDetailsRef.current = next;
          return next;
        });
      } else if (event.type === "code_review.task_updated" && event.task) {
        const receivedAt = Date.now();
        const incomingTask = event.task;
        pendingOutput.delete(incomingTask.id);
        if (!pendingOutput.size && outputTimer !== undefined) {
          window.clearTimeout(outputTimer);
          outputTimer = undefined;
        }
        setDetail((current) => {
          if (!current) return current;
          const currentTask = current.tasks.find(
            (task) => task.id === incomingTask.id,
          );
          const task = mergeReviewTaskSnapshot(
            currentTask,
            incomingTask,
            receivedAt,
          );
          const summary = reviewTaskSummary(task);
          const exists = current.tasks.some((currentTask) => currentTask.id === task.id);
          const next = {
            ...current,
            tasks: exists
              ? current.tasks.map((currentTask) =>
                  currentTask.id === task.id ? summary : currentTask,
                )
              : [...current.tasks, summary],
          };
          detailRef.current = next;
          return next;
        });
        if (incomingTask.id === selectedTaskIdRef.current) {
          if (document.visibilityState === "visible") {
            setTaskDetails((current) => {
              const currentTask =
                current[incomingTask.id] ??
                detailRef.current?.tasks.find(
                  (task) => task.id === incomingTask.id,
                );
              const task = boundReviewTaskOutput(
                mergeReviewTaskSnapshot(currentTask, incomingTask, receivedAt),
              );
              const details = { [task.id]: task };
              taskDetailsRef.current = details;
              return details;
            });
          } else {
            missedHiddenOutput = true;
          }
        }
        scheduleDetailReload();
      } else {
        scheduleDetailReload();
      }
    });
    return () => {
      document.removeEventListener("visibilitychange", syncVisibility);
      if (outputTimer !== undefined) window.clearTimeout(outputTimer);
      if (detailReloadTimer !== undefined) window.clearTimeout(detailReloadTimer);
      close();
    };
  }, [jobId, eventCursor, load, loadTask]);

  useEffect(() => {
    if (!detail?.tasks.length) {
      setSelectedTaskId("");
      return;
    }
    setSelectedTaskId((current) => {
      if (detail.tasks.some((task) => task.id === current)) return current;
      return pickPreferredTask(detail.tasks)?.id ?? "";
    });
  }, [detail?.tasks]);

  useEffect(() => {
    if (!selectedTaskId || taskDetails[selectedTaskId] || taskErrors[selectedTaskId]) return;
    void loadTask(selectedTaskId);
  }, [selectedTaskId, taskDetails, taskErrors, loadTask]);

  useEffect(() => {
    if (detail?.job.id !== focusReplacementJobIdRef.current) return;
    jobHeadingRef.current?.focus();
    focusReplacementJobIdRef.current = "";
  }, [detail?.job.id]);

  const selectedTaskError = selectedTaskId ? taskErrors[selectedTaskId] : undefined;
  useEffect(() => {
    if (!selectedTaskId || !selectedTaskError) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") void loadTask(selectedTaskId);
    }, AUTOMATIC_RETRY_MS);
    return () => window.clearInterval(timer);
  }, [loadTask, selectedTaskError, selectedTaskId]);

  useEffect(() => {
    setTaskDetails((current) => {
      const retained = selectedTaskId ? current[selectedTaskId] : undefined;
      const next = retained ? { [selectedTaskId]: retained } : {};
      taskDetailsRef.current = next;
      return next;
    });
  }, [selectedTaskId]);

  const act = async (action: "cancel" | "retry" | "full"): Promise<void> => {
    if (!detail) return;
    setBusy(action);
    try {
      const replacement =
        action === "cancel"
          ? await cancelJob(detail.job.id)
          : action === "retry"
            ? await retryJob(detail.job.id)
            : await requestReview(detail.job, "full");
      onChanged();
      if (action !== "cancel") {
        if (replacement.id === detail.job.id) {
          setNavigationStatus(
            "Review publication had already started; the existing review was reconciled instead of retried.",
          );
          await load();
        } else {
          focusReplacementJobIdRef.current = replacement.id;
          setNavigationStatus(`Opened replacement review ${replacement.id}.`);
          navigate("jobs", replacement.id);
        }
      } else await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy("");
    }
  };

  const retryFailedPersona = async (reviewerId: string): Promise<void> => {
    if (!detail) return;
    const action = `persona:${reviewerId}`;
    setBusy(action);
    try {
      const replacement = await retryPersona(detail.job.id, reviewerId);
      onChanged();
      if (replacement.id === detail.job.id) {
        setNavigationStatus(
          "Review publication had already started; the existing review was reconciled instead of retried.",
        );
        await load();
      } else {
        focusReplacementJobIdRef.current = replacement.id;
        setNavigationStatus(
          `Opened replacement review ${replacement.id}; all reviewer personas will run again.`,
        );
        navigate("jobs", replacement.id);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy("");
    }
  };

  if (!detail) {
    return (
      <aside class="panel job-detail">
        <p class="sr-only" role="status" aria-live="polite">
          {navigationStatus}
        </p>
        <button class="icon-button" type="button" onClick={onClose} aria-label="Close detail">
          ×
        </button>
        {error || "Loading review detail…"}
      </aside>
    );
  }
  const job = detail.job;
  const runningElapsed = liveElapsed(
    job.running_elapsed_ms,
    job.status,
    job.started_at,
    now,
  );
  const acceptedCandidateIds = new Set(
    detail.findings.flatMap((finding) =>
      finding.sources.map((source) => source.candidate_id).filter(Boolean),
    ),
  );
  const candidateRejections = detail.candidate_rejections ?? [];
  const routingDecisions = detail.routing_decisions ?? [];
  const unrecordedCandidateDecisions = Math.max(
    0,
    job.candidate_issue_count - acceptedCandidateIds.size - candidateRejections.length,
  );
  const activityGroups: Array<{
    id: string;
    name: string;
    status: string;
    subtitle: string;
    tasks: ReviewTask[];
    persona?: PersonaResult;
  }> = [];
  const routerTasks = detail.tasks.filter((task) => task.role === "router");
  if (routerTasks.length) {
    const latestRouterByBatch = new Map<number, ReviewTask>();
    routerTasks.forEach((task) => {
      // Tasks arrive in durable attempt order, so the final entry for a
      // batch is the current attempt even when timestamps collide.
      latestRouterByBatch.set(task.batch_index, task);
    });
    const currentRouterTasks = [...latestRouterByBatch.values()];
    const runningRouter = currentRouterTasks.find((task) => task.status === "running");
    const queuedRouter = currentRouterTasks.find((task) => task.status === "queued");
    const failedRouter = currentRouterTasks.find((task) => task.status === "failed");
    const routerStatus =
      runningRouter?.status ??
      queuedRouter?.status ??
      failedRouter?.status ??
      (currentRouterTasks.every((task) => task.status === "succeeded")
        ? "succeeded"
        : "cancelled");
    const routerElapsed = currentRouterTasks.reduce(
      (sum, task) =>
        sum + liveElapsed(task.elapsed_ms, task.status, task.started_at, now),
      0,
    );
    activityGroups.push({
      id: "router",
      name: "Persona router",
      status: routerStatus,
      subtitle: `${currentRouterTasks.filter((task) => !["queued", "running"].includes(task.status)).length}/${currentRouterTasks.length} batches · ${duration(routerElapsed)}`,
      tasks: routerTasks,
    });
  }
  activityGroups.push(
    ...detail.personas.map((persona) => ({
      id: `persona:${persona.reviewer_id}`,
      name: persona.reviewer_name,
      status: persona.status,
      subtitle: `${persona.completed_batches}/${persona.total_batches} batches · ${duration(
        liveElapsed(persona.elapsed_ms, persona.status, persona.started_at, now),
      )}`,
      tasks: detail.tasks.filter((task) => task.reviewer_id === persona.reviewer_id),
      persona,
    })),
  );
  const personaReviewerIds = new Set(detail.personas.map((persona) => persona.reviewer_id));
  const unmatchedReviewerTasks = new Map<string, ReviewTask[]>();
  detail.tasks
    .filter(
      (task) =>
        task.role === "reviewer" &&
        (!task.reviewer_id || !personaReviewerIds.has(task.reviewer_id)),
    )
    .forEach((task) => {
      const key = task.reviewer_id || task.reviewer_name || task.id;
      unmatchedReviewerTasks.set(key, [...(unmatchedReviewerTasks.get(key) ?? []), task]);
    });
  unmatchedReviewerTasks.forEach((tasks, reviewerId) => {
    const latestTask = tasks[tasks.length - 1];
    const status =
      tasks.find((task) => task.status === "running")?.status ??
      tasks.find((task) => task.status === "failed")?.status ??
      latestTask.status;
    const completed = tasks.filter(
      (task) => !["queued", "running"].includes(task.status),
    ).length;
    const total = Math.max(tasks.length, ...tasks.map((task) => task.batch_count));
    const elapsed = tasks.reduce((sum, task) => {
      if (task.status === "queued") return sum;
      return (
        sum +
        (task.status === "running"
          ? liveElapsed(task.elapsed_ms, task.status, task.started_at, now)
          : task.elapsed_ms)
      );
    }, 0);
    activityGroups.push({
      id: `task-reviewer:${reviewerId}`,
      name: latestTask.reviewer_name || latestTask.reviewer_id || "Reviewer",
      status,
      subtitle: `${completed}/${total} batches · ${duration(elapsed)}`,
      tasks,
    });
  });
  const coordinatorTasks = detail.tasks.filter((task) => task.role === "coordinator");
  if (coordinatorTasks.length) {
    const coordinatorTask = coordinatorTasks[coordinatorTasks.length - 1];
    activityGroups.push({
      id: "coordinator",
      name: "Final review editor",
      status: coordinatorTask.status,
      subtitle: `Final selection · ${duration(
        liveElapsed(
          coordinatorTask.elapsed_ms,
          coordinatorTask.status,
          coordinatorTask.started_at,
          now,
        ),
      )}`,
      tasks: coordinatorTasks,
    });
  }
  const selectedTaskSummary =
    detail.tasks.find((task) => task.id === selectedTaskId) ?? detail.tasks[0];
  const retainedTask = selectedTaskSummary ? taskDetails[selectedTaskSummary.id] : undefined;
  const selectedTask =
    selectedTaskSummary && retainedTask
      ? {
          ...selectedTaskSummary,
          prompt: retainedTask.prompt,
          output: retainedTask.output,
          thinking: retainedTask.thinking,
          tool_output: retainedTask.tool_output,
        }
      : selectedTaskSummary;
  const selectedGroup = activityGroups.find((group) =>
    group.tasks.some((task) => task.id === selectedTask?.id),
  );
  const selectedRoutingDecision =
    selectedTask?.role === "reviewer"
      ? routingDecisions.find(
          (decision) =>
            decision.reviewer_id === selectedTask.reviewer_id &&
            decision.batch_index === selectedTask.batch_index,
        )
      : undefined;
  const selectPreferredTask = (tasks: ReviewTask[]): void => {
    const preferred = pickPreferredTask(tasks);
    if (preferred) setSelectedTaskId(preferred.id);
  };
  return (
    <aside class="panel job-detail">
      <p class="sr-only" role="status" aria-live="polite">
        {navigationStatus}
      </p>
      <header class="detail-header">
        <div>
          <StatusPill status={job.status} />
          <h2 ref={jobHeadingRef} tabIndex={-1}>
            {job.repository} #{job.pull_number}
          </h2>
          <p>{job.pull_title}</p>
        </div>
        <button class="icon-button" type="button" onClick={onClose} aria-label="Close detail">
          ×
        </button>
      </header>
      <ProgressBar job={job} />
      <dl class="facts">
        <div>
          <dt>Scope</dt>
          <dd>{job.scope}</dd>
        </div>
        <div>
          <dt>Persona selection mode</dt>
          <dd>{routingModeLabel(job.routing_mode)}</dd>
        </div>
        <div>
          <dt>Semantic triage</dt>
          <dd>
            {job.routing_mode === "automatic"
              ? "Required"
              : job.routing_mode === "additive" && job.semantic_routing
                ? "Enabled"
                : "Off"}
          </dd>
        </div>
        <div>
          <dt>Router model</dt>
          <dd>{job.router_model || job.model || "Missing configuration"}</dd>
        </div>
        <div>
          <dt>Router thinking</dt>
          <dd>{job.router_thinking_level || "Review persona default"}</dd>
        </div>
        <div>
          <dt>Pending</dt>
          <dd>{duration(job.pending_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Running</dt>
          <dd>{duration(runningElapsed)}</dd>
        </div>
        <div>
          <dt>Revision</dt>
          <dd>
            <code>{(job.review_base_sha || job.base_ref).slice(0, 8)}</code>…<code>{job.head_sha.slice(0, 8)}</code>
          </dd>
        </div>
        <div>
          <dt>Preparation</dt>
          <dd>{duration(job.preparation_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Reviewers</dt>
          <dd>{duration(job.reviewer_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Coordinator</dt>
          <dd>{duration(job.coordinator_elapsed_ms)}</dd>
        </div>
        <div>
          <dt>Publication</dt>
          <dd>{duration(job.publication_elapsed_ms)}</dd>
        </div>
      </dl>
      <div class="action-row">
        {(job.status === "running" || job.status === "queued") && (
          <>
            <button
              class="danger"
              type="button"
              disabled={Boolean(busy)}
              onClick={() => void act("cancel")}
            >
              {busy === "cancel" ? "Cancelling…" : "Cancel"}
            </button>
            <button
              type="button"
              disabled={Boolean(busy)}
              onClick={() => void act("retry")}
            >
              {busy === "retry" ? "Retrying…" : "Cancel & retry"}
            </button>
          </>
        )}
        {!["running", "queued"].includes(job.status) && (
          <button type="button" disabled={Boolean(busy)} onClick={() => void act("retry")}>
            {busy === "retry" ? "Retrying…" : "Retry"}
          </button>
        )}
        <button class="ghost" type="button" disabled={Boolean(busy)} onClick={() => void act("full")}>
          {busy === "full" ? "Requesting…" : "Full branch review"}
        </button>
      </div>
      {error && <div class="banner error">{error}</div>}
      {job.error && <div class="banner error">{job.error}</div>}
      <div class="link-row">
        <ExternalLink href={job.pull_url}>Open pull request ↗</ExternalLink>
        <ExternalLink href={job.review_url}>Open published review ↗</ExternalLink>
        <ExternalLink href={job.check_run_url}>Open Check Run ↗</ExternalLink>
      </div>
      {job.check_sync_error && <p class="warning">Check sync: {job.check_sync_error}</p>}
      {routingDecisions.length > 0 && (
        <details
          class="routing-decisions"
          onToggle={(event) => setRoutingOpen(event.currentTarget.open)}
        >
          <summary>
            <strong>Persona selection</strong>
            <span>
              {routingDecisions.filter((decision) => decision.selected).length} of{" "}
              {routingDecisions.length} persona-batch candidates selected
            </span>
          </summary>
          {routingOpen && (
            <div class="routing-batches">
              {[...new Set(routingDecisions.map((decision) => decision.batch_index))].map(
                (batchIndex) => (
                  <section key={batchIndex}>
                    <h3>Batch {batchIndex + 1}</h3>
                    <div>
                      {routingDecisions
                        .filter((decision) => decision.batch_index === batchIndex)
                        .map((decision) => (
                          <article
                            class={`routing-decision ${decision.selected ? "selected" : "skipped"}`}
                            key={decision.reviewer_id}
                          >
                            <header>
                              <strong>{decision.reviewer_name}</strong>
                              <span>{decision.selected ? "Selected" : "Skipped"}</span>
                            </header>
                            {(decision.reasons ?? []).length > 0 ? (
                              <ul>
                                {(decision.reasons ?? []).map((reason, index) => (
                                  <li key={`${reason.source}:${index}`}>
                                    <b>{routingReasonLabel(reason.source, job.routing_mode)}:</b>{" "}
                                    {reason.detail}
                                  </li>
                                ))}
                              </ul>
                            ) : (
                              <p>No applicable routing signal.</p>
                            )}
                          </article>
                        ))}
                    </div>
                  </section>
                ),
              )}
            </div>
          )}
        </details>
      )}
      <section class="detail-section">
        <div class="panel-title inline">
          <div>
            <h2>{job.status === "running" || job.status === "queued" ? "Review overview" : "Completed overview"}</h2>
            <p>
              {job.issue_count} confirmed findings · {acceptedCandidateIds.size} selected candidates
              {" · "}
              {candidateRejections.length} rejected · {job.fixed_issue_count} fixed
            </p>
          </div>
          {detail.prompt_for_agents && (
            <CopyButton text={detail.prompt_for_agents} label="Copy fix-all prompt" />
          )}
        </div>
        {detail.summary && <p class="summary">{detail.summary}</p>}
        {(detail.themes ?? []).length > 0 && (
          <div class="theme-list">
            {(detail.themes ?? []).map((theme) => (
              <article class="finding medium" key={theme.id}>
                <header>
                  <strong>Shared root cause</strong>
                  <StatusPill status={theme.status} />
                </header>
                <p>{theme.root_cause}</p>
                <p><b>Structural fix:</b> {theme.recommendation}</p>
                <small>
                  {theme.finding_ids?.length ?? 0} manifestations across {theme.observations?.length ?? 0} review rounds
                  {theme.recurrence_count > 0 ? ` · Recurred ${theme.recurrence_count} time${theme.recurrence_count === 1 ? "" : "s"}` : ""}
                </small>
              </article>
            ))}
          </div>
        )}
        {detail.findings.map((finding) => (
          <article class={`finding ${finding.severity}`} key={finding.id}>
            <header>
              <strong>{finding.title}</strong>
              <StatusPill status={finding.status} />
            </header>
            <small>
              {finding.path}:{finding.line} · Severity: {finding.severity.toUpperCase()} · Confidence: {(finding.confidence ?? "medium").toUpperCase()}
              {finding.origin && finding.origin !== "new_change" ? ` · ${finding.origin.replaceAll("_", " ").toUpperCase()}` : ""}
            </small>
            <p>{finding.body}</p>
            {finding.resolved_head && (
              <small>
                Fixed at {finding.resolved_head.slice(0, 12)} by review {finding.resolved_by_job_id?.slice(0, 12) || "unknown"}
              </small>
            )}
            {finding.github_publication_status === "suppressed_by_policy" && (
              <small>Retained in Trouve · Not posted to GitHub by confidence policy</small>
            )}
            {finding.github_publication_status === "grouped_by_theme" && (
              <small>Retained in Trouve · Represented by the shared root-cause comment on GitHub</small>
            )}
            {finding.evidence?.execution_path && (
              <details>
                <summary>Verification evidence</summary>
                <dl>
                  <dt>Preconditions</dt><dd>{finding.evidence.preconditions}</dd>
                  <dt>Execution path</dt><dd>{finding.evidence.execution_path}</dd>
                  <dt>Consequence</dt><dd>{finding.evidence.consequence}</dd>
                  <dt>Introduced by</dt><dd>{finding.evidence.introduction}</dd>
                  <dt>Regression test</dt><dd>{finding.evidence.regression_test}</dd>
                </dl>
              </details>
            )}
            <small>
              Found by {finding.sources.map((source) => source.reviewer_name).join(", ") || "legacy review"}
            </small>
            <div class="action-row">
              <CopyButton text={finding.prompt_for_agents} />
              <ExternalLink href={finding.github_comment_url}>Open inline comment ↗</ExternalLink>
            </div>
          </article>
        ))}
        {candidateRejections.length > 0 && (
          <details class="candidate-decisions">
            <summary>
              <strong>Why {candidateRejections.length} candidates were not selected</strong>
              <span>Final-editor decisions</span>
            </summary>
            <div class="rejection-list">
              {candidateRejections.map((rejection) => (
                <article class="candidate-rejection" key={rejection.candidate_id}>
                  <header>
                    <strong>{rejection.title}</strong>
                    <span>{rejection.reviewer_name}</span>
                  </header>
                  <small>
                    {rejection.path}:{rejection.line} · Severity: {rejection.severity.toUpperCase()} · Confidence: {(rejection.confidence ?? "medium").toUpperCase()}
                  </small>
                  <p>{rejection.body}</p>
                  <div>
                    <b>Not selected:</b> {rejection.reason}
                  </div>
                </article>
              ))}
            </div>
          </details>
        )}
        {unrecordedCandidateDecisions > 0 &&
          !["running", "queued"].includes(job.status) && (
            <p class="decision-note">
              {job.status === "succeeded"
                ? `${unrecordedCandidateDecisions} candidate decision${
                    unrecordedCandidateDecisions === 1 ? " was" : "s were"
                  } not recorded by this older review run.`
                : `Candidate selection did not complete, so ${unrecordedCandidateDecisions} decision${
                    unrecordedCandidateDecisions === 1 ? " is" : "s are"
                  } unavailable.`}
            </p>
          )}
      </section>
      <section class="detail-section">
        <PanelTitle
          title="Review activity"
          subtitle="Select a persona and batch to inspect its metrics, retained output, and prompt"
        />
        {detail.tasks.length ? (
          <div class="activity-layout">
            <nav class="activity-groups" aria-label="Review personas and batches">
              {activityGroups.map((group) => {
                const active = group.id === selectedGroup?.id;
                return (
                  <div class={`activity-group${active ? " active" : ""}`} key={group.id}>
                    <div class="activity-group-summary">
                      <button type="button" onClick={() => selectPreferredTask(group.tasks)}>
                        <span>
                          <strong>{group.name}</strong>
                          <small>{group.subtitle}</small>
                        </span>
                        <StatusPill status={group.status} />
                      </button>
                      {job.status === "failed" &&
                        (group.status === "failed" || group.status === "cancelled") &&
                        group.persona && (
                          <button
                            class="compact ghost retry-persona"
                            type="button"
                            disabled={Boolean(busy)}
                            onClick={() => void retryFailedPersona(group.persona!.reviewer_id)}
                            aria-label={`Retry full review after ${group.name} ${group.status}`}
                            title="Starts a new review and reruns every selected persona using current settings"
                          >
                            {busy === `persona:${group.persona.reviewer_id}`
                              ? "Retrying…"
                              : "Retry all"}
                          </button>
                        )}
                    </div>
                    {active && group.tasks.length > 1 && (
                      <div class="batch-tabs">
                        {group.tasks.map((task) => (
                          <button
                            class={task.id === selectedTask?.id ? "active" : ""}
                            type="button"
                            onClick={() => setSelectedTaskId(task.id)}
                            key={task.id}
                          >
                            {taskAttemptLabel(group.tasks, task)}
                            <StatusPill status={task.status} />
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
            </nav>
            {selectedTask && (
              <article class="activity-detail" key={selectedTask.id}>
                <header>
                  <div>
                    <StatusPill status={selectedTask.status} />
                    <h3>{selectedTask.reviewer_name || "Final review editor"}</h3>
                    <p>
                      {selectedTask.model || "Model not recorded"}
                      {selectedTask.batch_count > 1
                        ? ` · batch ${selectedTask.batch_index + 1}/${selectedTask.batch_count}`
                        : ""}
                    </p>
                  </div>
                  <time>
                    {duration(
                      liveElapsed(
                        selectedTask.elapsed_ms,
                        selectedTask.status,
                        selectedTask.started_at,
                        now,
                      ),
                    )}
                  </time>
                </header>
                {selectedGroup?.persona && (
                  <p class="persona-rollup">
                    Persona total: {selectedGroup.persona.candidate_issue_count} candidates ·{" "}
                    {selectedGroup.persona.confirmed_issue_count} confirmed ·{" "}
                    {duration(selectedGroup.persona.provider_wait_ms)} capacity wait ·{" "}
                    {duration(selectedGroup.persona.model_elapsed_ms)} model/tools
                  </p>
                )}
                {selectedRoutingDecision && (
                  <div
                    class={`selected-routing ${selectedRoutingDecision.selected ? "selected" : "skipped"}`}
                  >
                    <strong>
                      {selectedRoutingDecision.selected
                        ? "Why this persona ran"
                        : "Why this persona was skipped"}
                    </strong>
                    {(selectedRoutingDecision.reasons ?? []).length > 0 ? (
                      <ul>
                        {(selectedRoutingDecision.reasons ?? []).map((reason, index) => (
                          <li key={`${reason.source}:${index}`}>
                            <b>{routingReasonLabel(reason.source, job.routing_mode)}:</b>{" "}
                            {reason.detail}
                          </li>
                        ))}
                      </ul>
                    ) : (
                      <p>No baseline, semantic, or repository inclusion matched.</p>
                    )}
                  </div>
                )}
                <dl class="task-facts">
                  <div>
                    <dt>Lifecycle</dt>
                    <dd aria-live="polite">
                      {taskLifecycleLabel(selectedTask.lifecycle_stage)}
                    </dd>
                  </div>
                  <div>
                    <dt>Capacity wait</dt>
                    <dd>{duration(selectedTask.provider_wait_ms)}</dd>
                  </div>
                  <div>
                    <dt>Model/tools</dt>
                    <dd>{duration(liveModelElapsed(selectedTask, now))}</dd>
                  </div>
                  <div>
                    <dt>Tokens</dt>
                    <dd>
                      {selectedTask.input_tokens.toLocaleString()} in ·{" "}
                      {selectedTask.output_tokens.toLocaleString()} out
                    </dd>
                  </div>
                  <div>
                    <dt>Cached input</dt>
                    <dd>{selectedTask.cached_input_tokens.toLocaleString()}</dd>
                  </div>
                  <div>
                    <dt>Tool calls</dt>
                    <dd>{selectedTask.tool_call_count}</dd>
                  </div>
                  <div>
                    <dt>Last progress</dt>
                    <dd>{formatDate(selectedTask.last_progress_at)}</dd>
                  </div>
                  <div>
                    <dt>Candidates / confirmed</dt>
                    <dd>
                      {selectedTask.candidate_issue_count} / {selectedTask.confirmed_issue_count}
                    </dd>
                  </div>
                </dl>
                {taskLoading === selectedTask.id && (
                  <p class="decision-note">Loading retained task output…</p>
                )}
                {taskErrors[selectedTask.id] && (
                  <div class="banner error">
                    {taskErrors[selectedTask.id]}
                    <span>Retrying automatically.</span>
                  </div>
                )}
                <OutputBlock
                  title="Assistant output"
                  value={selectedTask.output ?? ""}
                  followTail={selectedTask.status === "running"}
                />
                <OutputBlock
                  title="Reasoning"
                  value={selectedTask.thinking ?? ""}
                  followTail={selectedTask.status === "running"}
                />
                <OutputBlock
                  title="Tool output"
                  value={selectedTask.tool_output ?? ""}
                  followTail={selectedTask.status === "running"}
                />
                {selectedTask.prompt && (
                  <details class="nested">
                    <summary>Prompt</summary>
                    <pre>{selectedTask.prompt}</pre>
                  </details>
                )}
                {selectedTask.error && <p class="error-text">{selectedTask.error}</p>}
              </article>
            )}
          </div>
        ) : (
          <p class="decision-note">Review tasks have not started yet.</p>
        )}
      </section>
    </aside>
  );
}

function OutputBlock({
  title,
  value,
  followTail = false,
}: {
  title: string;
  value: string;
  followTail?: boolean;
}) {
  const preRef = useRef<HTMLPreElement>(null);
  const pinnedRef = useRef(true);
  const boundedValue = boundReviewOutput(value);
  useEffect(() => {
    if (
      !followTail ||
      !pinnedRef.current ||
      document.visibilityState !== "visible"
    ) {
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      const element = preRef.current;
      if (element && pinnedRef.current) element.scrollTop = element.scrollHeight;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [followTail, boundedValue]);
  if (!boundedValue) return null;
  return (
    <section class="output-block">
      <h3>{title}</h3>
      <pre
        ref={preRef}
        tabIndex={0}
        aria-busy={followTail}
        onScroll={(event) => {
          const element = event.currentTarget;
          pinnedRef.current =
            element.scrollHeight - element.scrollTop - element.clientHeight <= 16;
        }}
      >
        {boundedValue}
      </pre>
    </section>
  );
}

function RepositoriesPage({
  dashboard,
  models,
  onChanged,
}: {
  dashboard: Dashboard;
  models: Model[];
  onChanged: () => void;
}) {
  const [showAll, setShowAll] = useState(false);
  const [query, setQuery] = useState("");
  const repositories = dashboard.repositories.filter(
    (repository) =>
      (showAll || repository.mode !== "off") &&
      repository.repository.toLowerCase().includes(query.toLowerCase()),
  );
  return (
    <section>
      <PageHeader
        eyebrow="Configuration"
        title="Repositories"
        description="Configured repositories are shown by default. Discovery remains available without cluttering the working set."
      />
      <section class="panel">
        <div class="filters">
          <label class="grow">
            Search
            <input
              type="search"
              value={query}
              placeholder="owner/repository"
              onInput={(event) => setQuery(event.currentTarget.value)}
            />
          </label>
          <label class="checkbox">
            <input
              type="checkbox"
              checked={showAll}
              onChange={(event) => setShowAll(event.currentTarget.checked)}
            />
            Show all discovered repositories
          </label>
        </div>
        <p class="muted">
          Showing {repositories.length} of {dashboard.repositories.length} repositories.
        </p>
        <div class="repository-list">
          {repositories.map((repository) => (
            <RepositoryEditor
              repository={repository}
              reviewers={dashboard.reviewers}
              models={models}
              onSaved={onChanged}
              key={repository.repository}
            />
          ))}
        </div>
      </section>
    </section>
  );
}

function ThinkingSetting({
  options,
  value,
  onChange,
  inheritLabel,
  disabled = false,
}: {
  options: ReturnType<typeof thinkingOptions>;
  value: string;
  onChange: (value: string) => void;
  inheritLabel?: string;
  disabled?: boolean;
}) {
  if (options.budget) {
    return (
      <input
        type="number"
        min={options.budget.minimum}
        max={options.budget.maximum}
        step={1}
        value={value}
        placeholder={inheritLabel}
        disabled={disabled}
        onInput={(event) => onChange(event.currentTarget.value)}
      />
    );
  }
  return (
    <select
      value={value}
      onChange={(event) => onChange(event.currentTarget.value)}
      disabled={disabled || !options.values.length}
    >
      {inheritLabel !== undefined && <option value="">{inheritLabel}</option>}
      {options.values.map((level) => (
        <option value={level} key={level}>
          {thinkingLevelLabel(level)}
        </option>
      ))}
    </select>
  );
}

function RepositoryEditor({
  repository,
  reviewers,
  models,
  onSaved,
}: {
  repository: Repository;
  reviewers: ReviewerProfile[];
  models: Model[];
  onSaved: () => void;
}) {
  const [draft, setDraft] = useState(repository);
  const [busy, setBusy] = useState(false);
  const [message, flash] = useFlash();
  const persistedRepository = JSON.stringify(repository);
  useEffect(() => setDraft(repository), [persistedRepository]);
  const persistRepository = async (
    next: Repository,
    successMessage = "Saved",
  ): Promise<void> => {
    setBusy(true);
    try {
      await saveRepository(next);
      flash(successMessage);
      onSaved();
    } catch (cause) {
      flash(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };
  const togglePersona = (id: string): void => {
    setDraft((current) => {
      if (current.routing_mode === "automatic") return current;
      if (current.routing_mode === "manual") {
        return {
          ...current,
          reviewer_ids: current.reviewer_ids.includes(id)
            ? current.reviewer_ids.filter((reviewer) => reviewer !== id)
            : [...current.reviewer_ids, id],
        };
      }
      const included = current.included_reviewer_ids ?? [];
      return {
        ...current,
        included_reviewer_ids: included.includes(id)
          ? included.filter((reviewer) => reviewer !== id)
          : [...included, id],
      };
    });
  };
  const updateReviewerOverride = (
    id: string,
    patch: Partial<ReviewerOverride>,
  ): void => {
    setDraft((current) => {
      const overrides = current.reviewer_overrides ?? [];
      const existing = overrides.find((item) => item.reviewer_id === id) ?? {
        reviewer_id: id,
        prompt_mode: "inherit" as const,
        prompt: "",
      };
      const updated = { ...existing, ...patch };
      const retained = overrides.filter((item) => item.reviewer_id !== id);
      if (
        updated.model ||
        updated.thinking_level ||
        updated.prompt_mode !== "inherit" ||
        updated.prompt
      ) {
        retained.push(updated);
      }
      return { ...current, reviewer_overrides: retained };
    });
  };
  const effectiveCoordinatorModel = models.find((model) => model.id === draft.model);
  const coordinatorThinking = thinkingOptions(effectiveCoordinatorModel);
  const effectiveRouterModel = models.find(
    (model) => model.id === (draft.router_model || draft.model),
  );
  const routerThinking = thinkingOptions(effectiveRouterModel);
  const compatibleThinking = (
    configured: string | undefined,
    model: Model | undefined,
  ): string | undefined => {
    if (!configured || !model) return configured;
    return thinkingSelectionIsValid(model, configured) ? configured : undefined;
  };
  const reviewerPolicyInvalid =
    draft.mode !== "off" &&
    (draft.routing_mode === "manual"
      ? draft.reviewer_ids.length === 0
      : reviewers.length === 0);
  const reviewModelInvalid = draft.mode !== "off" && !draft.model;
  const semanticRouterConfigEnabled = draft.routing_mode !== "manual";
  const semanticRouterRequirement =
    "Choose Additive or Automatic persona selection to configure it.";
  return (
    <details class="repository-editor">
      <summary>
        <span>
          <strong>{repository.repository}</strong>
          <small>
            {repository.private ? "private" : "public"} · installation {repository.installation_id}
            {" · "}
            {routingModeLabel(repository.routing_mode)} persona selection
          </small>
        </span>
        <StatusPill status={repository.mode === "off" ? "disabled" : repository.mode} />
      </summary>
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          await persistRepository(draft);
        }}
      >
        <div class="form-grid">
          <label>
            Review mode
            <select
              value={draft.mode}
              onChange={(event) =>
                setDraft({ ...draft, mode: event.currentTarget.value as Repository["mode"] })
              }
            >
              <option value="off">Off</option>
              <option value="manual">Manual requests</option>
              <option value="automatic">Automatic</option>
            </select>
            <small>
              Off prevents reviews. Manual runs only when requested; Automatic reviews eligible
              pull request updates.
            </small>
          </label>
          <label>
            Persona selection mode
            <select
              value={draft.routing_mode}
              onChange={(event) => {
                const routingMode = event.currentTarget.value as Repository["routing_mode"];
                setDraft({
                  ...draft,
                  routing_mode: routingMode,
                  semantic_routing: routingMode !== "manual",
                  included_reviewer_ids:
                    routingMode === "automatic" ? [] : draft.included_reviewer_ids,
                  excluded_reviewer_ids: [],
                });
              }}
            >
              <option value="manual">Manual</option>
              <option value="additive">Additive</option>
              <option value="automatic">Automatic</option>
            </select>
            <small>
              {draft.routing_mode === "manual"
                ? "Runs exactly the personas enabled below; semantic routing is disabled."
                : draft.routing_mode === "additive"
                  ? "Always runs the baseline and enabled core personas, then optionally adds personas using semantic triage."
                  : "Lets semantic triage select personas from the complete catalog."}
            </small>
          </label>
          <label>
            Coordinator and fallback model
            <select
              value={draft.model ?? ""}
              onChange={(event) => {
                const model = event.currentTarget.value || undefined;
                const selectedCoordinatorModel = models.find(
                  (candidate) => candidate.id === model,
                );
                const selectedRouterModel = models.find(
                  (candidate) => candidate.id === (draft.router_model || model),
                );
                setDraft({
                  ...draft,
                  model,
                  coordinator_thinking_level: compatibleThinking(
                    draft.coordinator_thinking_level,
                    selectedCoordinatorModel,
                  ),
                  router_thinking_level: compatibleThinking(
                    draft.router_thinking_level,
                    selectedRouterModel,
                  ),
                  reviewer_overrides: (draft.reviewer_overrides ?? []).map((override) => {
                    const profile = reviewers.find(
                      (reviewer) => reviewer.id === override.reviewer_id,
                    );
                    const selectedReviewerModel = models.find(
                      (candidate) =>
                        candidate.id === (override.model || profile?.model || model),
                    );
                    return {
                      ...override,
                      thinking_level: compatibleThinking(
                        override.thinking_level,
                        selectedReviewerModel,
                      ),
                    };
                  }),
                });
              }}
            >
              <option value="">Select a model</option>
              {models.map((model) => (
                <option value={model.id} key={model.id}>
                  {model.display_name} · {model.id}
                </option>
              ))}
            </select>
            <small>
              {models.length
                ? "Runs the final coordinator that validates and combines findings. It is also the fallback for personas without their own model."
                : "No models are currently available. Configure or sign in to a model provider first."}
            </small>
          </label>
          <label>
            {coordinatorThinking.budget
              ? "Coordinator thinking budget (tokens)"
              : "Coordinator thinking"}
            <ThinkingSetting
              options={coordinatorThinking}
              value={draft.coordinator_thinking_level ?? ""}
              inheritLabel="Inherit review persona"
              onChange={(value) =>
                setDraft({
                  ...draft,
                  coordinator_thinking_level: value || undefined,
                })
              }
            />
            <small>
              Controls reasoning for the final coordinator. Inherit review persona uses the default
              configured in Review persona settings.
            </small>
          </label>
          <label class={semanticRouterConfigEnabled ? undefined : "field-disabled"}>
            Semantic router model
            <select
              value={draft.router_model ?? ""}
              disabled={!semanticRouterConfigEnabled}
              onChange={(event) => {
                const routerModel = event.currentTarget.value || undefined;
                const selectedRouterModel = models.find(
                  (candidate) => candidate.id === (routerModel || draft.model),
                );
                setDraft({
                  ...draft,
                  router_model: routerModel,
                  router_thinking_level: compatibleThinking(
                    draft.router_thinking_level,
                    selectedRouterModel,
                  ),
                });
              }}
            >
              <option value="">Inherit coordinator/fallback model</option>
              {models.map((model) => (
                <option value={model.id} key={model.id}>
                  {model.display_name} · {model.id}
                </option>
              ))}
            </select>
            <small>
              Runs the lightweight, read-only triage pass that may add relevant personas.
              {!semanticRouterConfigEnabled && ` ${semanticRouterRequirement}`}
            </small>
          </label>
          <label class={semanticRouterConfigEnabled ? undefined : "field-disabled"}>
            {routerThinking.budget
              ? "Semantic router thinking budget (tokens)"
              : "Semantic router thinking"}
            <ThinkingSetting
              options={routerThinking}
              value={draft.router_thinking_level ?? ""}
              inheritLabel="Inherit review default"
              disabled={!semanticRouterConfigEnabled}
              onChange={(value) =>
                setDraft({
                  ...draft,
                  router_thinking_level: value || undefined,
                })
              }
            />
            <small>
              Controls reasoning for semantic triage. Inherit review default follows the Review
              mode setting.
              {!semanticRouterConfigEnabled && ` ${semanticRouterRequirement}`}
            </small>
          </label>
        </div>
        <label>
          Repository instructions
          <textarea
            rows={4}
            value={draft.prompt}
            onInput={(event) => setDraft({ ...draft, prompt: event.currentTarget.value })}
          />
          <small>
            Adds repository-specific guidance to the instructions used for this repository's
            reviews.
          </small>
        </label>
        <fieldset>
          <legend>Persona selection</legend>
          <label
            class={`checkbox semantic-routing ${
              draft.routing_mode !== "additive" ? "field-disabled" : ""
            }`}
          >
            <input
              type="checkbox"
              checked={
                draft.routing_mode === "automatic" ||
                (draft.routing_mode === "additive" && draft.semantic_routing)
              }
              disabled={draft.routing_mode !== "additive"}
              onChange={(event) =>
                setDraft({ ...draft, semantic_routing: event.currentTarget.checked })
              }
            />
            <span>
              <strong>Semantic triage</strong>
              <small>
                {draft.routing_mode === "automatic"
                  ? "Required in Automatic mode and solely decides which personas run for each batch."
                  : draft.routing_mode === "additive"
                    ? "Run one lightweight, read-only routing pass per batch. It may add relevant personas but cannot remove baseline or enabled core personas."
                    : "Semantic triage is off in Manual mode; only the checked personas run."}
              </small>
            </span>
          </label>
          <div class="routing-personas">
            {reviewers.map((reviewer) => {
              const checked =
                draft.routing_mode === "manual"
                  ? draft.reviewer_ids.includes(reviewer.id)
                  : draft.routing_mode === "additive"
                    ? (draft.included_reviewer_ids ?? []).includes(reviewer.id)
                    : false;
              const disabled = draft.routing_mode === "automatic";
              return (
                <label
                  class={`routing-persona checkbox ${disabled ? "field-disabled" : ""}`}
                  key={reviewer.id}
                >
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={disabled}
                    onChange={() => togglePersona(reviewer.id)}
                  />
                  <span>
                    <strong>{reviewer.name}</strong>
                    <small>
                      {draft.routing_mode === "manual"
                        ? "Runs exactly when enabled"
                        : draft.routing_mode === "additive"
                          ? "Always runs when enabled"
                          : "Selected automatically"}
                      {" · "}
                      {reviewer.model || "inherits review model"}
                    </small>
                  </span>
                </label>
              );
            })}
          </div>
        </fieldset>
        <fieldset>
          <legend>Persona models and thinking</legend>
          <p class="field-help">
            Tune a persona for this repository without changing its reusable defaults. Model
            overrides take precedence over the persona and coordinator fallback; thinking
            overrides take precedence over the persona and Review persona default.
          </p>
          <div class="persona-execution-grid">
            {reviewers.map((reviewer) => {
              const override = (draft.reviewer_overrides ?? []).find(
                (item) => item.reviewer_id === reviewer.id,
              );
              const effectiveModelId = override?.model || reviewer.model || draft.model;
              const reviewerThinking = thinkingOptions(
                models.find((model) => model.id === effectiveModelId),
              );
              return (
                <div class="persona-execution" key={reviewer.id}>
                  <header>
                    <strong>{reviewer.name}</strong>
                    <small>{effectiveModelId || "No model selected"}</small>
                  </header>
                  <label>
                    Model
                    <select
                      value={override?.model ?? ""}
                      onChange={(event) => {
                        const model = event.currentTarget.value || undefined;
                        const selectedModel = models.find(
                          (candidate) =>
                            candidate.id === (model || reviewer.model || draft.model),
                        );
                        updateReviewerOverride(reviewer.id, {
                          model,
                          thinking_level: compatibleThinking(
                            override?.thinking_level,
                            selectedModel,
                          ),
                        });
                      }}
                    >
                      <option value="">
                        Inherit · {reviewer.model || draft.model || "no model"}
                      </option>
                      {models.map((model) => (
                        <option value={model.id} key={model.id}>
                          {model.display_name} · {model.id}
                        </option>
                      ))}
                    </select>
                    <small>
                      Overrides the model for this persona in this repository only.
                    </small>
                  </label>
                  <label>
                    {reviewerThinking.budget ? "Thinking budget (tokens)" : "Thinking"}
                    <ThinkingSetting
                      options={reviewerThinking}
                      value={override?.thinking_level ?? ""}
                      inheritLabel={
                        reviewer.default_thinking_level
                          ? `Inherit · ${thinkingLevelLabel(reviewer.default_thinking_level)}`
                          : "Inherit persona/review persona"
                      }
                      onChange={(value) =>
                        updateReviewerOverride(reviewer.id, {
                          thinking_level: value || undefined,
                        })
                      }
                    />
                    <small>
                      Overrides this persona's reasoning setting for this repository only.
                    </small>
                  </label>
                </div>
              );
            })}
          </div>
        </fieldset>
        <div class="action-row">
          <button
            type="submit"
            disabled={busy || reviewerPolicyInvalid || reviewModelInvalid}
          >
            {busy ? "Saving…" : "Save repository"}
          </button>
          {reviewModelInvalid && (
            <span class="error-text">Select a review model before enabling reviews.</span>
          )}
          {repository.mode !== "off" && (
            <button
              class="danger ghost"
              type="button"
              disabled={busy}
              onClick={() =>
                void persistRepository(
                  { ...repository, mode: "off" },
                  "Reviews disabled",
                )
              }
            >
              Disable reviews
            </button>
          )}
          {message && <span role="status">{message}</span>}
        </div>
      </form>
    </details>
  );
}

function ReviewersPage({
  reviewers,
  models,
  defaultModel,
  onChanged,
}: {
  reviewers: ReviewerProfile[];
  models: Model[];
  defaultModel?: string;
  onChanged: () => void;
}) {
  return (
    <section>
      <PageHeader
        eyebrow="Review policy"
        title="Reviewer personas"
        description="Focused personas run concurrently and retain separate model, duration, and issue statistics."
      />
      <div class="reviewer-grid">
        {reviewers.map((reviewer) => (
          <ReviewerEditor
            reviewer={reviewer}
            models={models}
            defaultModel={defaultModel}
            onChanged={onChanged}
            key={reviewer.id}
          />
        ))}
        <ReviewerEditor models={models} defaultModel={defaultModel} onChanged={onChanged} />
      </div>
    </section>
  );
}

function ReviewerEditor({
  reviewer,
  models,
  defaultModel,
  onChanged,
}: {
  reviewer?: ReviewerProfile;
  models: Model[];
  defaultModel?: string;
  onChanged: () => void;
}) {
  const empty: ReviewerProfile = {
    id: "",
    name: "",
    prompt: "",
    built_in: false,
  };
  const [draft, setDraft] = useState(reviewer ?? empty);
  const [busy, setBusy] = useState(false);
  const [message, flash] = useFlash();
  const persistedReviewer = JSON.stringify(reviewer ?? null);
  useEffect(() => setDraft(reviewer ?? empty), [persistedReviewer]);
  const reviewerModel = models.find(
    (model) => model.id === (draft.model || defaultModel),
  );
  const reviewerThinking = thinkingOptions(reviewerModel);
  return (
    <form
      class="panel reviewer-editor"
      onSubmit={async (event) => {
        event.preventDefault();
        setBusy(true);
        try {
          await saveReviewer(draft);
          flash("Saved");
          if (!reviewer) setDraft(empty);
          onChanged();
        } catch (cause) {
          flash(cause instanceof Error ? cause.message : String(cause));
        } finally {
          setBusy(false);
        }
      }}
    >
      <header>
        <div>
          <p class="eyebrow">{reviewer?.built_in ? "Built in" : reviewer ? "Custom" : "New persona"}</p>
          <h2>{reviewer?.name || "Create reviewer"}</h2>
        </div>
      </header>
      <label>
        Name
        <input
          value={draft.name}
          onInput={(event) => setDraft({ ...draft, name: event.currentTarget.value })}
          required
        />
      </label>
      <label>
        Focus prompt
        <textarea
          rows={7}
          value={draft.prompt}
          onInput={(event) => setDraft({ ...draft, prompt: event.currentTarget.value })}
          required
        />
      </label>
      <label>
        Default model
        <select
          value={draft.model ?? ""}
          onChange={(event) => {
            const model = event.currentTarget.value || undefined;
            setDraft({
              ...draft,
              model,
              default_thinking_level:
                defaultThinkingSelection(
                  models.find((candidate) => candidate.id === (model || defaultModel)),
                  draft.default_thinking_level,
                ) || undefined,
            });
          }}
        >
          <option value="">Inherit repository/system</option>
          {models.map((model) => (
            <option value={model.id} key={model.id}>
              {model.display_name} · {model.id}
            </option>
          ))}
        </select>
        <small>
          Sets this persona's reusable model. Repository-specific persona overrides take
          precedence; otherwise Inherit uses the repository coordinator/fallback model.
        </small>
      </label>
      <label>
        {reviewerThinking.budget ? "Thinking budget (tokens)" : "Thinking level"}
        <ThinkingSetting
          options={reviewerThinking}
          value={draft.default_thinking_level ?? ""}
          inheritLabel="Inherit default"
          onChange={(value) =>
            setDraft({
              ...draft,
              default_thinking_level: value || undefined,
            })
          }
        />
        <small>
          Sets this persona's reusable reasoning default. Repository-specific overrides take
          precedence; otherwise Inherit follows the Review persona default.
        </small>
      </label>
      <div class="action-row">
        <button type="submit" disabled={busy}>
          {busy ? "Saving…" : reviewer ? "Save persona" : "Create persona"}
        </button>
        {reviewer && !reviewer.built_in && (
          <button
            class="danger ghost"
            type="button"
            onClick={async () => {
              if (!window.confirm(`Delete ${reviewer.name}?`)) return;
              await deleteReviewer(reviewer.id);
              onChanged();
            }}
          >
            Delete
          </button>
        )}
        {message && <span role="status">{message}</span>}
      </div>
    </form>
  );
}

function StatsPage({ repositories }: { repositories: Repository[] }) {
  const [range, setRange] = useState<StatsRange>("day");
  const [repository, setRepository] = useState("");
  const [stats, setStats] = useState<ReviewStats | null>(null);
  const [error, setError] = useState("");
  const churn = stats?.churn ?? {
    recurrence_issue_count: 0,
    fix_regression_issue_count: 0,
    previously_missed_issue_count: 0,
    grouped_issue_count: 0,
    external_duplicate_count: 0,
    insufficient_evidence_rejection_count: 0,
    pull_request_count: 0,
    clean_pull_request_count: 0,
    average_rounds_to_clean: 0,
    max_rounds_to_clean: 0,
  };
  useEffect(() => {
    let alive = true;
    getStats(range, repository)
      .then((next) => {
        if (alive) {
          setStats(next);
          setError("");
        }
      })
      .catch((cause) => alive && setError(cause instanceof Error ? cause.message : String(cause)));
    return () => {
      alive = false;
    };
  }, [range, repository]);
  return (
    <section>
      <PageHeader
        eyebrow="Analytics"
        title="Review statistics"
        description="Global or per-repository outcomes, queue/run latency, persona/model timing, and issue attribution."
      />
      <div class="filters stats-filters">
        <div class="segmented" role="group" aria-label="Statistics range">
          {(["hour", "day", "week", "month", "year", "all"] as StatsRange[]).map((value) => (
            <button
              type="button"
              class={range === value ? "active" : ""}
              onClick={() => setRange(value)}
              key={value}
            >
              {value === "hour" ? "1H" : value === "day" ? "1D" : value === "week" ? "1W" : value === "month" ? "1M" : value === "year" ? "1Y" : "All"}
            </button>
          ))}
        </div>
        <label>
          Repository
          <select value={repository} onChange={(event) => setRepository(event.currentTarget.value)}>
            <option value="">All repositories</option>
            {repositories.map((repo) => (
              <option value={repo.repository} key={repo.repository}>
                {repo.repository}
              </option>
            ))}
          </select>
        </label>
      </div>
      {error && <div class="banner error">{error}</div>}
      {stats && (
        <>
          <div class="metric-grid">
            <Metric label="Succeeded" value={stats.status.succeeded} color="green" />
            <Metric label="Running" value={stats.status.running} color="blue" />
            <Metric label="Failed" value={stats.status.failed} color="red" />
            <Metric label="Issues found" value={stats.issue_count} color="amber" />
          </div>
          <section class="panel">
            <PanelTitle
              title="Review churn"
              subtitle={`${churn.clean_pull_request_count} of ${churn.pull_request_count} pull requests reached a clean review in this range.`}
            />
            <div class="metric-grid">
              <Metric label="Recurrences" value={churn.recurrence_issue_count} color="red" />
              <Metric label="Fix regressions" value={churn.fix_regression_issue_count} color="red" />
              <Metric label="Previously missed" value={churn.previously_missed_issue_count} color="amber" />
              <Metric label="Grouped symptoms" value={churn.grouped_issue_count} color="blue" />
              <Metric label="External duplicates" value={churn.external_duplicate_count} color="green" />
              <Metric label="Weak evidence rejected" value={churn.insufficient_evidence_rejection_count} color="green" />
              <Metric label="Avg rounds to clean" value={Math.round(churn.average_rounds_to_clean * 10) / 10} color="blue" />
              <Metric label="Max rounds to clean" value={churn.max_rounds_to_clean} color="amber" />
            </div>
          </section>
          <div class="chart-grid">
            <StatsChart
              title="Review outcomes"
              labels={stats.buckets.map((bucket) => new Date(bucket.started_at).toLocaleString())}
              datasets={[
                { label: "Succeeded", data: stats.buckets.map((bucket) => bucket.status.succeeded), color: "#55d99a" },
                { label: "Failed", data: stats.buckets.map((bucket) => bucket.status.failed), color: "#ff6b7d" },
                { label: "Cancelled/stale", data: stats.buckets.map((bucket) => bucket.status.cancelled + bucket.status.stale), color: "#8b93a7" },
              ]}
            />
            <StatsChart
              title="Average latency"
              labels={stats.buckets.map((bucket) => new Date(bucket.started_at).toLocaleString())}
              datasets={[
                { label: "Pending", data: stats.buckets.map((bucket) => Math.round(bucket.pending_average_ms / 1_000)), color: "#f5b84b" },
                { label: "Running", data: stats.buckets.map((bucket) => Math.round(bucket.running_average_ms / 1_000)), color: "#62a8ff" },
              ]}
              suffix="s"
            />
          </div>
          <div class="stats-summary-grid">
            <DurationCard title="Pending duration" value={stats.pending_duration} />
            <DurationCard title="Running duration" value={stats.running_duration} />
            <DurationCard title="Preparation phase" value={stats.preparation_duration} />
            <DurationCard title="Reviewer phase" value={stats.reviewer_duration} />
            <DurationCard title="Coordinator phase" value={stats.coordinator_duration} />
            <DurationCard title="Publication phase" value={stats.publication_duration} />
          </div>
          <section class="panel table-panel">
            <PanelTitle
              title="Persona and model performance"
              subtitle="Batches are per-batch model tasks; outcomes and average durations are rolled up once per persona run. Confirmed issue credits are many-to-many."
            />
            <div class="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>Persona</th>
                    <th>Actual model</th>
                    <th>Batches</th>
                    <th>Successful runs</th>
                    <th>Failed runs</th>
                    <th>Cancelled runs</th>
                    <th>N/A runs</th>
                    <th>Avg run duration</th>
                    <th>Avg run capacity wait</th>
                    <th>Avg run model/tools</th>
                    <th>Cached input</th>
                    <th>Tool calls</th>
                    <th>Candidates</th>
                    <th>Confirmed issues</th>
                  </tr>
                </thead>
                <tbody>
                  {stats.personas.map((persona) => (
                    <tr key={`${persona.reviewer_id}:${persona.model}`}>
                      <td>{persona.reviewer_name}</td>
                      <td><code>{persona.model}</code></td>
                      <td>{persona.task_count}</td>
                      <td>{persona.succeeded}</td>
                      <td>{persona.failed}</td>
                      <td>{persona.cancelled}</td>
                      <td>{persona.not_applicable}</td>
                      <td>{duration(persona.duration.average_ms)}</td>
                      <td>{duration(persona.provider_wait_duration.average_ms)}</td>
                      <td>{duration(persona.model_duration.average_ms)}</td>
                      <td>{persona.cached_input_tokens.toLocaleString()}</td>
                      <td>{persona.tool_call_count.toLocaleString()}</td>
                      <td>{persona.candidate_issue_count}</td>
                      <td>{persona.confirmed_issue_count}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>
        </>
      )}
    </section>
  );
}

function Metric({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <article class={`metric ${color}`}>
      <span>{label}</span>
      <strong>{value}</strong>
      <small>selected range</small>
    </article>
  );
}

function DurationCard({ title, value }: { title: string; value: DurationStats }) {
  return (
    <article class="panel duration-card">
      <span>{title}</span>
      <strong>{duration(value.average_ms)}</strong>
      <dl>
        <div><dt>p50</dt><dd>{duration(value.p50_ms)}</dd></div>
        <div><dt>p95</dt><dd>{duration(value.p95_ms)}</dd></div>
        <div><dt>max</dt><dd>{duration(value.maximum_ms)}</dd></div>
        <div><dt>samples</dt><dd>{value.samples}</dd></div>
      </dl>
    </article>
  );
}

function StatsChart({
  title,
  labels,
  datasets,
  suffix = "",
}: {
  title: string;
  labels: string[];
  datasets: Array<{ label: string; data: number[]; color: string }>;
  suffix?: string;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    if (!canvas.current) return;
    const chart = new Chart(canvas.current, {
      type: "line",
      data: {
        labels,
        datasets: datasets.map((dataset) => ({
          label: dataset.label,
          data: dataset.data,
          borderColor: dataset.color,
          backgroundColor: `${dataset.color}22`,
          fill: true,
          tension: 0.25,
          pointRadius: labels.length > 40 ? 0 : 2,
        })),
      },
      options: {
        // Stats refresh by replacing the chart. Chart.js's default entrance
        // animation repaints the entire canvas for decorative motion only.
        animation: false,
        responsive: true,
        maintainAspectRatio: false,
        interaction: { mode: "index", intersect: false },
        plugins: {
          legend: { labels: { color: "#aeb6c8" } },
          tooltip: {
            callbacks: {
              label: (context) => `${context.dataset.label}: ${context.formattedValue}${suffix}`,
            },
          },
        },
        scales: {
          x: { ticks: { color: "#7f899e", maxTicksLimit: 8 }, grid: { color: "#273044" } },
          y: { beginAtZero: true, ticks: { color: "#7f899e" }, grid: { color: "#273044" } },
        },
      },
    });
    return () => chart.destroy();
  }, [labels.join("|"), JSON.stringify(datasets), suffix]);
  return (
    <section class="panel chart-card">
      <PanelTitle title={title} />
      <div class="canvas-wrap"><canvas ref={canvas} /></div>
      <details class="chart-data">
        <summary>View chart data</summary>
        <div class="table-scroll">
          <table>
            <thead><tr><th>Time</th>{datasets.map((set) => <th key={set.label}>{set.label}</th>)}</tr></thead>
            <tbody>
              {labels.map((label, index) => (
                <tr key={`${label}:${index}`}>
                  <td>{label}</td>
                  {datasets.map((set) => <td key={set.label}>{set.data[index]}{suffix}</td>)}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </details>
    </section>
  );
}

function SettingsPage({
  app,
  providers,
  reviewSettings,
  models,
  reviewPersonaInfo,
  onChanged,
}: {
  app: GithubAppStatus;
  providers: ProvidersResponse | null;
  reviewSettings: CodeReviewSettings | null;
  models: Model[];
  reviewPersonaInfo?: PersonaInfo;
  onChanged: () => void;
}) {
  return (
    <section>
      <PageHeader
        eyebrow="Administration"
        title="Settings"
        description="Review execution defaults, GitHub App health, and model-provider authentication."
      />
      <ReviewPersonaSettings
        personaInfo={reviewPersonaInfo}
        models={models}
        globalModel={providers?.default_model}
        globalThinking={providers?.default_thinking_level}
        onChanged={onChanged}
      />
      <ReviewExecutionSettings settings={reviewSettings} onChanged={onChanged} />
      <div class="settings-grid">
        <GithubAppSettings app={app} onChanged={onChanged} />
        <ProviderSettings providers={providers} models={models} onChanged={onChanged} />
      </div>
    </section>
  );
}

function ReviewExecutionSettings({
  settings,
  onChanged,
}: {
  settings: CodeReviewSettings | null;
  onChanged: () => void;
}) {
  const [maxParallel, setMaxParallel] = useState(
    settings ? String(settings.max_parallel_reviews) : "",
  );
  const [total, setTotal] = useState(settings ? timeoutMinutes(settings.total_timeout_seconds) : "");
  const [reviewer, setReviewer] = useState(
    settings ? timeoutMinutes(settings.reviewer_timeout_seconds) : "",
  );
  const [coordinator, setCoordinator] = useState(
    settings ? timeoutMinutes(settings.coordinator_timeout_seconds) : "",
  );
  const [busy, setBusy] = useState(false);
  const [message, flash] = useFlash();
  useEffect(() => {
    if (!settings) return;
    setMaxParallel(String(settings.max_parallel_reviews));
    setTotal(timeoutMinutes(settings.total_timeout_seconds));
    setReviewer(timeoutMinutes(settings.reviewer_timeout_seconds));
    setCoordinator(timeoutMinutes(settings.coordinator_timeout_seconds));
  }, [
    settings?.max_parallel_reviews,
    settings?.total_timeout_seconds,
    settings?.reviewer_timeout_seconds,
    settings?.coordinator_timeout_seconds,
  ]);

  return (
    <section class="panel settings-card review-timeout-settings">
      <PanelTitle
        title="Review execution"
        subtitle="Concurrency and deadlines for unattended review jobs."
      />
      {settings ? (
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            setBusy(true);
            try {
              await saveReviewSettings(
                reviewSettingsFromMinutes(maxParallel, total, reviewer, coordinator),
              );
              flash("Review execution settings saved");
              onChanged();
            } catch (cause) {
              flash(cause instanceof Error ? cause.message : String(cause));
            } finally {
              setBusy(false);
            }
          }}
        >
          <div class="form-grid">
            <label>
              Max parallel reviews
              <input
                type="number"
                min="1"
                max={MAX_PARALLEL_REVIEWS}
                step="1"
                required
                value={maxParallel}
                onInput={(event) => setMaxParallel(event.currentTarget.value)}
              />
              <small>Concurrent pull-request review jobs. Changes apply immediately.</small>
            </label>
            <label>
              Total review timeout (minutes)
              <input
                type="number"
                min={TIMEOUT_MINUTES_INPUT_MIN}
                step={TIMEOUT_MINUTES_INPUT_STEP}
                required
                value={total}
                onInput={(event) => setTotal(event.currentTarget.value)}
              />
              <small>Outer deadline covering preparation through publication.</small>
            </label>
            <label>
              Reviewer timeout (minutes)
              <input
                type="number"
                min={TIMEOUT_MINUTES_INPUT_MIN}
                step={TIMEOUT_MINUTES_INPUT_STEP}
                required
                value={reviewer}
                onInput={(event) => setReviewer(event.currentTarget.value)}
              />
              <small>Maximum time for one persona batch, including JSON repair.</small>
            </label>
            <label>
              Final editor timeout (minutes)
              <input
                type="number"
                min={TIMEOUT_MINUTES_INPUT_MIN}
                step={TIMEOUT_MINUTES_INPUT_STEP}
                required
                value={coordinator}
                onInput={(event) => setCoordinator(event.currentTarget.value)}
              />
              <small>Maximum time for candidate validation and final selection.</small>
            </label>
          </div>
          <p class="field-help">
            Higher concurrency increases provider usage and may encounter provider rate limits. The
            maximum is {MAX_PARALLEL_REVIEWS}. Environment review variables take precedence over
            these persisted values.
          </p>
          <div class="action-row">
            <button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save review execution settings"}
            </button>
            {message && <span role="status">{message}</span>}
          </div>
        </form>
      ) : (
        <p class="muted">Review execution configuration is unavailable.</p>
      )}
    </section>
  );
}

function ReviewPersonaSettings({
  personaInfo,
  models,
  globalModel,
  globalThinking,
  onChanged,
}: {
  personaInfo?: PersonaInfo;
  models: Model[];
  globalModel?: string;
  globalThinking?: string;
  onChanged: () => void;
}) {
  const persona = personaInfo?.persona;
  const [model, setModel] = useState(persona?.default_model ?? "");
  const [thinking, setThinking] = useState(persona?.default_thinking_level ?? "");
  const [busy, setBusy] = useState(false);
  const [message, flash] = useFlash();
  useEffect(() => setModel(persona?.default_model ?? ""), [persona?.default_model]);
  useEffect(
    () => setThinking(persona?.default_thinking_level ?? ""),
    [persona?.default_thinking_level],
  );
  const effectiveModel = model || globalModel || "";
  const selectedModel = models.find((candidate) => candidate.id === effectiveModel);
  useEffect(() => {
    if (!personaInfo || !selectedModel) return;
    setThinking((current) => {
      if (!current || thinkingSelectionIsValid(selectedModel, current)) return current;
      return defaultThinkingSelection(selectedModel);
    });
  }, [effectiveModel, selectedModel, personaInfo, personaInfo?.persona.default_thinking_level]);
  const options = thinkingOptions(selectedModel);
  const inheritedThinking = globalThinking
    ? thinkingLevelLabel(globalThinking)
    : "model default";

  return (
    <section class="panel settings-card review-persona-settings">
      <PanelTitle
        title="Review persona"
        subtitle="Defaults for review-persona threads. Repository models still take precedence for automated reviews."
      />
      {persona ? (
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            setBusy(true);
            try {
              await savePersona({
                ...persona,
                default_model: model || undefined,
                default_thinking_level: thinking || undefined,
              });
              flash("Review persona defaults saved");
              onChanged();
            } catch (cause) {
              flash(cause instanceof Error ? cause.message : String(cause));
            } finally {
              setBusy(false);
            }
          }}
        >
          <div class="form-grid">
            <label>
              Default model
              <select
                value={model}
                onChange={(event) => {
                  const next = event.currentTarget.value;
                  setModel(next);
                  const nextModel = models.find(
                    (candidate) => candidate.id === (next || globalModel),
                  );
                  if (thinking && !thinkingSelectionIsValid(nextModel, thinking)) {
                    setThinking(defaultThinkingSelection(nextModel));
                  }
                }}
              >
                <option value="">
                  Inherit global{globalModel ? ` · ${globalModel}` : ""}
                </option>
                {models.map((candidate) => (
                  <option value={candidate.id} key={candidate.id}>
                    {candidate.display_name} · {candidate.id}
                  </option>
                ))}
              </select>
              <small>
                Manual review threads inherit this model. Automated jobs continue to use
                their repository model.
              </small>
            </label>
            <label>
              {options.budget ? "Default thinking budget (tokens)" : "Default thinking level"}
              <ThinkingSetting
                options={options}
                value={thinking}
                onChange={setThinking}
                inheritLabel={`Inherit global · ${inheritedThinking}`}
              />
              <small>
                Automated coordinators inherit this level; persona-specific settings
                still take precedence.
              </small>
            </label>
          </div>
          <div class="action-row">
            <button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save review persona"}
            </button>
            {personaInfo?.origin === "customized" && (
              <button
                class="ghost"
                type="button"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  try {
                    await resetPersona(persona.id);
                    flash("Review persona reset to built-in defaults");
                    onChanged();
                  } catch (cause) {
                    flash(cause instanceof Error ? cause.message : String(cause));
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                Reset built-in defaults
              </button>
            )}
            {message && <span role="status">{message}</span>}
          </div>
        </form>
      ) : (
        <p class="muted">Review persona configuration is unavailable.</p>
      )}
    </section>
  );
}

function GithubAppSettings({
  app,
  onChanged,
}: {
  app: GithubAppStatus;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [message, flash] = useFlash();
  return (
    <section class="panel settings-card">
      <PanelTitle title="GitHub App" subtitle="Credentials are validated before the saved secret is replaced." />
      <div class="health-list">
        <Health ok={app.configured} label="App credentials" detail={app.bot_login || "Not configured"} />
        <Health ok={app.checks_write_configured} label="Checks permission" detail="Read and write required to show a PR check" />
        <Health ok={app.webhook_configured} label="Webhook secret" detail="Optional with polling; secures webhook delivery" />
        <Health ok={app.check_run_webhook_configured} optional label="check_run webhook" detail="Optional; enables GitHub Re-run actions" />
      </div>
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          const form = event.currentTarget;
          const data = new FormData(form);
          setBusy(true);
          try {
            await configureApp({
              app_id: Number(data.get("app_id")),
              private_key_pem: String(data.get("private_key_pem")),
              webhook_secret: String(data.get("webhook_secret")),
            });
            form.reset();
            flash("GitHub App saved");
            onChanged();
          } catch (cause) {
            flash(cause instanceof Error ? cause.message : String(cause));
          } finally {
            setBusy(false);
          }
        }}
      >
        <label>
          App ID
          <input name="app_id" inputMode="numeric" defaultValue={app.app_id ?? ""} required />
        </label>
        <label>
          Private key PEM
          <textarea name="private_key_pem" rows={8} required placeholder="-----BEGIN RSA PRIVATE KEY-----" />
        </label>
        <label>
          Webhook secret
          <input name="webhook_secret" type="password" autoComplete="new-password" />
          <small>Use the same random secret in GitHub App → Webhook. Leave empty for polling-only operation.</small>
        </label>
        <div class="action-row">
          <button type="submit" disabled={busy}>{busy ? "Validating…" : "Save GitHub App"}</button>
          {message && <span role="status">{message}</span>}
        </div>
      </form>
    </section>
  );
}

function Health({
  ok,
  optional,
  label,
  detail,
}: {
  ok: boolean;
  optional?: boolean;
  label: string;
  detail: string;
}) {
  return (
    <div>
      <i class={ok ? "ok" : optional ? "optional" : "bad"} />
      <span><strong>{label}</strong><small>{detail}</small></span>
    </div>
  );
}

interface LoginView {
  provider: Provider;
  started?: LoginStarted;
  status: "starting" | "pending" | "success" | "failed";
  error: string;
  codeSubmitted: boolean;
}

function ProviderSettings({
  providers,
  models,
  onChanged,
}: {
  providers: ProvidersResponse | null;
  models: Model[];
  onChanged: () => void;
}) {
  const [login, setLogin] = useState<LoginView | null>(null);
  const [defaultModel, setDefaultModel] = useState(providers?.default_model ?? "");
  const [defaultThinking, setDefaultThinking] = useState(
    providers?.default_thinking_level ?? "",
  );
  const [knownProviders, setKnownProviders] = useState<KnownProvider[]>([]);
  const [clis, setClis] = useState<CliInfo[]>([]);
  const [cliStatuses, setCliStatuses] = useState<Record<string, CliInstallStatus>>({});
  const [cliBusy, setCliBusy] = useState("");
  const [subscriptionId, setSubscriptionId] = useState("");
  const [apiPresetId, setApiPresetId] = useState("");
  const [providerId, setProviderId] = useState("");
  const [providerKind, setProviderKind] = useState("openai-compat");
  const [providerBaseUrl, setProviderBaseUrl] = useState("");
  const [providerApiKey, setProviderApiKey] = useState("");
  const [message, flash] = useFlash();
  useEffect(() => setDefaultModel(providers?.default_model ?? ""), [providers?.default_model]);
  useEffect(
    () => setDefaultThinking(providers?.default_thinking_level ?? ""),
    [providers?.default_thinking_level],
  );
  const loadCliData = useCallback(async (): Promise<boolean> => {
    try {
      const [nextKnown, nextClis] = await Promise.all([getKnownProviders(), getClis()]);
      setKnownProviders(nextKnown);
      setClis(nextClis);
      const statusEntries = await Promise.all(
        nextClis.map(async (cli): Promise<[string, CliInstallStatus]> => {
          try {
            return [cli.id, await getCliInstallStatus(cli.id)];
          } catch {
            return [cli.id, idleCliInstallStatus()];
          }
        }),
      );
      setCliStatuses(Object.fromEntries(statusEntries));
      return true;
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      flash(`${message} Retrying automatically.`);
      return false;
    }
  }, [flash]);
  useEffect(() => {
    let disposed = false;
    let timer: number | undefined;
    const refresh = async (): Promise<void> => {
      if (document.visibilityState !== "visible") {
        timer = window.setTimeout(() => void refresh(), AUTOMATIC_RETRY_MS);
        return;
      }
      const loaded = await loadCliData();
      if (!disposed) {
        timer = window.setTimeout(
          () => void refresh(),
          loaded ? CLI_IDLE_REFRESH_MS : AUTOMATIC_RETRY_MS,
        );
      }
    };
    void refresh();
    return () => {
      disposed = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [loadCliData]);
  const cliInstalling = Object.values(cliStatuses).some(
    (status) => status.status === "pending",
  );
  const latestCliStatuses = useRef(cliStatuses);
  useEffect(() => {
    latestCliStatuses.current = cliStatuses;
  }, [cliStatuses]);
  useEffect(() => {
    if (!cliInstalling) return;
    const timer = window.setInterval(async () => {
      const pendingIds = Object.entries(latestCliStatuses.current)
        .filter(([, status]) => status.status === "pending")
        .map(([id]) => id);
      const fetched: Record<string, CliInstallStatus> = {};
      for (const id of pendingIds) {
        try {
          fetched[id] = await getCliInstallStatus(id);
        } catch {
          // A transient status error should not stop the next polling tick.
        }
      }
      setCliStatuses((current) => ({ ...current, ...fetched }));
      if (
        pendingIds.length > 0 &&
        pendingIds.every((id) => fetched[id] && fetched[id].status !== "pending")
      ) {
        setClis(await getClis());
        onChanged();
      }
    }, 1_000);
    return () => window.clearInterval(timer);
  }, [cliInstalling]);
  useEffect(() => {
    if (!login || login.status !== "pending") return;
    const timer = window.setInterval(async () => {
      try {
        const state = await loginStatus(login.provider.id);
        if (state.status === "success") {
          setLogin({ ...login, status: "success", error: "", codeSubmitted: login.codeSubmitted });
          onChanged();
        } else if (state.status === "failed") {
          setLogin({ ...login, status: "failed", error: state.error || "Sign-in failed", codeSubmitted: login.codeSubmitted });
        }
      } catch (cause) {
        setLogin({ ...login, error: cause instanceof Error ? cause.message : String(cause) });
      }
    }, 1_000);
    return () => window.clearInterval(timer);
  }, [login?.provider.id, login?.status]);

  const begin = async (provider: Provider): Promise<void> => {
    setLogin({ provider, status: "starting", error: "", codeSubmitted: false });
    try {
      const started = await startLogin(provider);
      setLogin({ provider, started, status: "pending", error: "", codeSubmitted: false });
      if (started.verification_url) window.open(started.verification_url, "_blank", "noopener,noreferrer");
    } catch (cause) {
      setLogin({
        provider,
        status: "failed",
        error: cause instanceof Error ? cause.message : String(cause),
        codeSubmitted: false,
      });
    }
  };
  const runCliAction = async (
    id: string,
    action: "install" | "cancel" | "uninstall",
  ): Promise<void> => {
    setCliBusy(id);
    try {
      if (action === "install") {
        await installCli(id);
        setCliStatuses((current) => ({
          ...current,
          [id]: { status: "pending", received_bytes: 0, total_bytes: 0 },
        }));
        flash(`Installing ${id}…`);
      } else if (action === "cancel") {
        await cancelCliInstall(id);
        flash(`Cancelling ${id} install…`);
      } else {
        if (!window.confirm(`Remove trouve's managed ${id}?`)) return;
        await uninstallCli(id);
        flash(`Removed managed ${id}`);
        await loadCliData();
      }
    } catch (cause) {
      flash(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCliBusy("");
    }
  };
  const subscriptionProviders = knownProviders.filter(
    (provider) => provider.auth === "cli" || provider.auth === "oauth",
  );
  const apiProviders = knownProviders.filter(
    (provider) => provider.auth !== "cli" && provider.auth !== "oauth",
  );
  const selectedSubscription = subscriptionProviders.find(
    (provider) => provider.id === subscriptionId,
  );
  const requiredCli = selectedSubscription
    ? clis.find((cli) => cli.kinds.includes(selectedSubscription.kind))
    : undefined;
  const selectedModel = models.find((model) => model.id === defaultModel);
  const defaultThinkingOptions = thinkingOptions(selectedModel);
  return (
    <section class="panel settings-card">
      <PanelTitle title="Models and providers" subtitle="Reviewer timings are recorded against the actual provider-qualified model." />
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          try {
            await saveDefaultModel(
              defaultModel,
              defaultThinkingOptions.values.length || defaultThinkingOptions.budget
                ? defaultThinking
                : undefined,
            );
            flash("System model defaults saved");
            onChanged();
          } catch (cause) {
            flash(cause instanceof Error ? cause.message : String(cause));
          }
        }}
      >
        <label>
          Global default model
          <select
            value={defaultModel}
            onChange={(event) => {
              const next = event.currentTarget.value;
              setDefaultModel(next);
              setDefaultThinking(
                defaultThinkingSelection(
                  models.find((model) => model.id === next),
                  defaultThinking,
                ),
              );
            }}
            required
          >
            {models.map((model) => (
              <option value={model.id} key={model.id}>{model.display_name} · {model.id}</option>
            ))}
          </select>
          <small>
            Base model for interactive threads and settings that inherit the global default.
            Enabled repositories still require an explicit coordinator model.
          </small>
        </label>
        <label>
          {defaultThinkingOptions.budget
            ? "Global thinking budget (tokens)"
            : "Global thinking level"}
          <ThinkingSetting
            options={defaultThinkingOptions}
            value={defaultThinking}
            onChange={setDefaultThinking}
            inheritLabel="Use the model's default"
          />
          <small>
            Base reasoning setting used when a persona or repository does not specify its
            own thinking level.
          </small>
        </label>
        <button type="submit" disabled={!defaultModel}>Save system defaults</button>
      </form>
      <div class="provider-list">
        {providers?.providers.map((provider) => (
          <article key={provider.id}>
            <span>
              <strong>{provider.id}</strong>
              <small>{provider.kind} · {provider.category}</small>
            </span>
            <StatusPill status={provider.has_credentials ? "ready" : "credentials required"} />
            {(provider.auth === "oauth" || provider.auth === "cli") && (
              <button class="ghost compact" type="button" onClick={() => void begin(provider)}>
                {provider.has_credentials ? "Sign in again" : "Sign in"}
              </button>
            )}
          </article>
        ))}
      </div>
      {login && (
        <aside class={`login-card ${login.status}`} aria-live="polite">
          <header><strong>{login.provider.id}</strong><StatusPill status={login.status} /></header>
          {login.started?.verification_url && (
            <ExternalLink href={login.started.verification_url}>Open authorization page ↗</ExternalLink>
          )}
          {login.started?.user_code && <p>Enter code <code>{login.started.user_code}</code> in the authorization page.</p>}
          {login.provider.kind === "claude-cli" && login.status === "pending" && !login.codeSubmitted && (
            <form
              onSubmit={async (event) => {
                event.preventDefault();
                const data = new FormData(event.currentTarget);
                const code = String(data.get("authentication_code") ?? "").trim();
                if (!code) return;
                try {
                  await submitLoginCode(login.provider.id, code);
                  setLogin({ ...login, codeSubmitted: true, error: "" });
                } catch (cause) {
                  setLogin({ ...login, error: cause instanceof Error ? cause.message : String(cause) });
                }
              }}
            >
              <p>After authorizing, copy the authentication code shown by Claude and paste the code itself here.</p>
              <label>
                Claude authentication code
                <input
                  name="authentication_code"
                  autoComplete="off"
                  spellcheck={false}
                  placeholder="Authentication code (not a URL)"
                  required
                />
              </label>
              <button type="submit">Submit code</button>
            </form>
          )}
          {login.codeSubmitted && login.status === "pending" && <p>Authentication code sent. Waiting for Claude Code…</p>}
          {login.status === "success" && <p>Provider credentials are ready.</p>}
          {login.error && <p class="error-text">{login.error}</p>}
        </aside>
      )}
      <section class="provider-setup">
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            if (!selectedSubscription) return;
            if (requiredCli && !cliIsInstalled(requiredCli)) {
              await runCliAction(requiredCli.id, "install");
              return;
            }
            try {
              const configured = await saveProvider(
                selectedSubscription.id,
                selectedSubscription.kind,
                selectedSubscription.base_url,
              );
              onChanged();
              await begin(configured);
            } catch (cause) {
              flash(cause instanceof Error ? cause.message : String(cause));
            }
          }}
        >
          <h3>Subscription provider</h3>
          <p class="muted">Configure a vendor subscription and open its sign-in flow.</p>
          <label>
            Provider
            <select
              value={subscriptionId}
              onChange={(event) => setSubscriptionId(event.currentTarget.value)}
              required
            >
              <option value="">Choose a provider…</option>
              {subscriptionProviders.map((provider) => (
                <option value={provider.id} key={provider.id}>
                  {provider.display_name}{provider.experimental ? " · Experimental" : ""}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" disabled={!selectedSubscription || cliBusy !== ""}>
            {requiredCli && !cliIsInstalled(requiredCli)
              ? `Install ${requiredCli.display_name}`
              : "Configure and sign in"}
          </button>
        </form>
        <form
          onSubmit={async (event) => {
            event.preventDefault();
            try {
              await saveProvider(
                providerId,
                providerKind,
                providerBaseUrl || undefined,
                providerApiKey || undefined,
              );
              flash(`Saved ${providerId}`);
              setProviderApiKey("");
              onChanged();
            } catch (cause) {
              flash(cause instanceof Error ? cause.message : String(cause));
            }
          }}
        >
          <h3>API or custom provider</h3>
          <p class="muted">Use a preset or configure another compatible API endpoint.</p>
          <label>
            Preset
            <select
              value={apiPresetId}
              onChange={(event) => {
                const id = event.currentTarget.value;
                const preset = apiProviders.find((provider) => provider.id === id);
                setApiPresetId(id);
                setProviderId(preset?.id ?? "");
                setProviderKind(preset?.kind ?? "openai-compat");
                setProviderBaseUrl(preset?.base_url ?? "");
              }}
            >
              <option value="">Custom provider</option>
              {apiProviders.map((provider) => (
                <option value={provider.id} key={provider.id}>{provider.display_name}</option>
              ))}
            </select>
          </label>
          <div class="split-fields">
            <label>
              Provider ID
              <input
                value={providerId}
                onInput={(event) => setProviderId(event.currentTarget.value)}
                required
              />
            </label>
            <label>
              Protocol
              <select
                value={providerKind}
                onChange={(event) => setProviderKind(event.currentTarget.value)}
              >
                <option value="openai-compat">OpenAI compatible</option>
                <option value="anthropic">Anthropic</option>
              </select>
            </label>
          </div>
          <label>
            Base URL
            <input
              value={providerBaseUrl}
              onInput={(event) => setProviderBaseUrl(event.currentTarget.value)}
              placeholder="https://api.example.com/v1"
            />
          </label>
          <label>
            API key
            <input
              type="password"
              autoComplete="new-password"
              value={providerApiKey}
              onInput={(event) => setProviderApiKey(event.currentTarget.value)}
              disabled={apiProviders.find((provider) => provider.id === apiPresetId)?.auth === "none"}
            />
            <small>
              {apiProviders.find((provider) => provider.id === apiPresetId)?.api_key_env
                ? `Or set ${apiProviders.find((provider) => provider.id === apiPresetId)?.api_key_env} on the server.`
                : "Stored in trouve's secret store."}
            </small>
          </label>
          <button type="submit">Save API provider</button>
        </form>
      </section>
      <section class="cli-manager">
        <header>
          <div>
            <h3>Subscription CLI binaries</h3>
            <p class="muted">Managed versions take precedence over system copies on PATH. Status updates automatically.</p>
          </div>
        </header>
        <div class="cli-list">
          {clis.map((cli) => {
            const status = cliStatuses[cli.id] ?? idleCliInstallStatus();
            return (
              <article key={cli.id}>
                <span>
                  <strong>{cli.display_name}</strong>
                  <small>{cliVersionLabel(cli)}</small>
                  {status.status === "pending" && <small>{cliProgressLabel(status)}</small>}
                  {status.status === "failed" && <small class="error-text">{status.error}</small>}
                </span>
                <div class="action-row">
                  {status.status === "pending" ? (
                    <button
                      class="ghost compact"
                      type="button"
                      onClick={() => void runCliAction(cli.id, "cancel")}
                    >
                      Cancel
                    </button>
                  ) : (
                    <button
                      class="ghost compact"
                      type="button"
                      disabled={cliBusy === cli.id}
                      onClick={() => void runCliAction(cli.id, "install")}
                    >
                      {cli.update_available ? "Update" : cliIsInstalled(cli) ? "Reinstall" : "Install"}
                    </button>
                  )}
                  {cli.source === "managed" && status.status !== "pending" && (
                    <button
                      class="danger ghost compact"
                      type="button"
                      onClick={() => void runCliAction(cli.id, "uninstall")}
                    >
                      Remove
                    </button>
                  )}
                </div>
              </article>
            );
          })}
        </div>
      </section>
      {message && <p role="status">{message}</p>}
    </section>
  );
}

const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("missing #app");
render(<App />, root);
