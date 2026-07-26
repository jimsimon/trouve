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
  getModels,
  getProviders,
  getStats,
  installCli,
  loginStatus,
  openJobEvents,
  openServerEvents,
  refreshReviews,
  requestReview,
  retryJob,
  saveDefaultModel,
  saveProvider,
  saveRepository,
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
} from "./model-settings";
import { jobStatusClass, safeExternalUrl } from "./security";
import type {
  Dashboard,
  DurationStats,
  EventEnvelope,
  GithubAppStatus,
  JobDetail,
  KnownProvider,
  LoginStarted,
  Model,
  PersonaResult,
  Provider,
  ProvidersResponse,
  Repository,
  ReviewJob,
  ReviewTask,
  ReviewStats,
  ReviewerProfile,
  StatsRange,
} from "./types";

Chart.register(...registerables);

type Section = "overview" | "jobs" | "repositories" | "reviewers" | "stats" | "settings";

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

function useClock(active = true): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

function useFlash(): [string, (message: string) => void] {
  const [message, setMessage] = useState("");
  const flash = (next: string): void => {
    setMessage(next);
    window.setTimeout(() => setMessage(""), 2_500);
  };
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
      <span style={{ width: `${job.progress.percent}%` }} />
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
  const [models, setModels] = useState<Model[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);

  const load = async (quiet = false): Promise<void> => {
    if (!quiet) setLoading(true);
    try {
      const [nextDashboard, nextProviders, nextModels] = await Promise.all([
        getDashboard(),
        getProviders(),
        getModels(),
      ]);
      setDashboard(nextDashboard);
      setProviders(nextProviders);
      setModels(nextModels);
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (!quiet) setLoading(false);
    }
  };

  useEffect(() => {
    const onHash = (): void => setRoute(routeFromHash());
    window.addEventListener("hashchange", onHash);
    if (!window.location.hash) navigate("overview");
    void load();
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => openServerEvents(() => void load(true)), []);

  const active = dashboard?.jobs.some((job) => job.status === "running" || job.status === "queued");
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => void load(true), 5_000);
    return () => window.clearInterval(timer);
  }, [active]);

  const content = dashboard ? (
    <>
      {route.section === "overview" && (
        <Overview dashboard={dashboard} onRefresh={() => void load(true)} />
      )}
      {route.section === "jobs" && (
        <JobsPage
          dashboard={dashboard}
          selectedId={route.jobId}
          onChanged={() => void load(true)}
        />
      )}
      {route.section === "repositories" && (
        <RepositoriesPage
          dashboard={dashboard}
          models={models}
          onChanged={() => void load(true)}
        />
      )}
      {route.section === "reviewers" && (
        <ReviewersPage
          reviewers={dashboard.reviewers}
          models={models}
          defaultModel={providers?.default_model}
          onChanged={() => void load(true)}
        />
      )}
      {route.section === "stats" && (
        <StatsPage repositories={dashboard.repositories} />
      )}
      {route.section === "settings" && (
        <SettingsPage
          app={dashboard.app}
          providers={providers}
          models={models}
          onChanged={() => void load(true)}
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
            <button type="button" onClick={() => void load()}>
              Retry
            </button>
          </div>
        )}
        {loading && !dashboard ? <div class="loading">Loading review operations…</div> : content}
      </main>
    </div>
  );
}

function Overview({
  dashboard,
  onRefresh,
}: {
  dashboard: Dashboard;
  onRefresh: () => void;
}) {
  const now = useClock();
  const counts = dashboard.jobs.reduce<Record<string, number>>((result, job) => {
    result[job.status] = (result[job.status] ?? 0) + 1;
    return result;
  }, {});
  const active = dashboard.jobs.filter(
    (job) => job.status === "running" || job.status === "queued",
  );
  const recent = dashboard.jobs.filter((job) => !active.includes(job)).slice(0, 8);
  return (
    <section>
      <PageHeader
        eyebrow="Operations"
        title="Review overview"
        description="Live review activity, queue health, and recent outcomes."
        action={
          <button
            class="ghost"
            type="button"
            onClick={async () => {
              await refreshReviews();
              onRefresh();
            }}
          >
            Reconcile now
          </button>
        }
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
  const now = useClock();

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
  const now = useClock(detail?.job.status === "running");
  const aliveRef = useRef<string | null>(jobId);
  const detailStatusRef = useRef(detail?.job.status);
  detailStatusRef.current = detail?.job.status;
  const load = useCallback(async (): Promise<void> => {
    const requestedJobId = jobId;
    try {
      const next = await getJob(requestedJobId);
      if (aliveRef.current === requestedJobId) {
        setDetail(next);
        setError("");
      }
    } catch (cause) {
      if (aliveRef.current === requestedJobId) {
        setError(cause instanceof Error ? cause.message : String(cause));
      }
    }
  }, [jobId]);
  useEffect(() => {
    aliveRef.current = jobId;
    setDetail(null);
    setSelectedTaskId("");
    const timeouts = new Set<number>();
    void load();
    const close = openJobEvents(jobId, (event) => {
      if (aliveRef.current !== jobId) return;
      if (event.type === "code_review.output_delta" && event.task_id && event.text) {
        setDetail((current) => {
          if (!current) return current;
          const field =
            event.stream === "thinking"
              ? "thinking"
              : event.stream === "tool"
                ? "tool_output"
                : "output";
          return {
            ...current,
            tasks: current.tasks.map((task) =>
              task.id === event.task_id
                ? { ...task, [field]: `${task[field]}${event.text}` }
                : task,
            ),
          };
        });
      } else if (event.type === "code_review.task_updated" && event.task) {
        setDetail((current) => {
          if (!current) return current;
          const exists = current.tasks.some((task) => task.id === event.task?.id);
          return {
            ...current,
            tasks: exists
              ? current.tasks.map((task) => (task.id === event.task?.id ? event.task! : task))
              : [...current.tasks, event.task!],
          };
        });
        const timeout = window.setTimeout(() => {
          timeouts.delete(timeout);
          void load();
        }, 150);
        timeouts.add(timeout);
      } else {
        void load();
      }
    });
    return () => {
      if (aliveRef.current === jobId) aliveRef.current = null;
      for (const timeout of timeouts) window.clearTimeout(timeout);
      close();
    };
  }, [jobId, load]);

  useEffect(() => {
    if (!detail?.tasks.length) {
      setSelectedTaskId("");
      return;
    }
    setSelectedTaskId((current) => {
      if (detail.tasks.some((task) => task.id === current)) return current;
      return (
        detail.tasks.find((task) => task.status === "running") ??
        detail.tasks.find((task) => task.status === "failed") ??
        detail.tasks
          .slice()
          .reverse()
          .find((task) => task.role === "coordinator") ??
        detail.tasks[0]
      ).id;
    });
  }, [detail?.tasks]);

  useEffect(() => {
    aliveRef.current = jobId;
    const timer = window.setInterval(() => {
      if (
        detailStatusRef.current === "running" ||
        detailStatusRef.current === "queued"
      ) {
        void load();
      }
    }, 3_000);
    return () => {
      if (aliveRef.current === jobId) aliveRef.current = null;
      window.clearInterval(timer);
    };
  }, [jobId, load]);

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
      if (action !== "cancel") navigate("jobs", replacement.id);
      else await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy("");
    }
  };

  if (!detail) {
    return (
      <aside class="panel job-detail">
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
  }> = detail.personas.map((persona) => ({
    id: `persona:${persona.reviewer_id}`,
    name: persona.reviewer_name,
    status: persona.status,
    subtitle: `${persona.completed_batches}/${persona.total_batches} batches · ${duration(
      liveElapsed(persona.elapsed_ms, persona.status, persona.started_at, now),
    )}`,
    tasks: detail.tasks.filter((task) => task.reviewer_id === persona.reviewer_id),
    persona,
  }));
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
  const selectedTask =
    detail.tasks.find((task) => task.id === selectedTaskId) ?? detail.tasks[0];
  const selectedGroup = activityGroups.find((group) =>
    group.tasks.some((task) => task.id === selectedTask?.id),
  );
  const selectPreferredTask = (tasks: ReviewTask[]): void => {
    const preferred =
      tasks.find((task) => task.status === "running") ??
      tasks.find((task) => task.status === "failed") ??
      tasks
        .slice()
        .reverse()
        .find((task) => task.role === "coordinator") ??
      tasks[0];
    if (preferred) setSelectedTaskId(preferred.id);
  };
  return (
    <aside class="panel job-detail">
      <header class="detail-header">
        <div>
          <StatusPill status={job.status} />
          <h2>
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
            <code>{job.review_base_sha.slice(0, 8)}</code>…<code>{job.head_sha.slice(0, 8)}</code>
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
          <CopyButton text={detail.prompt_for_agents} label="Copy fix-all prompt" />
        </div>
        {detail.summary && <p class="summary">{detail.summary}</p>}
        {detail.findings.map((finding) => (
          <article class={`finding ${finding.severity}`} key={finding.id}>
            <header>
              <strong>
                {finding.severity.toUpperCase()} · {finding.path}:{finding.line}
              </strong>
              <StatusPill status={finding.status} />
            </header>
            <p>{finding.body}</p>
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
                    <strong>
                      {rejection.severity.toUpperCase()} · {rejection.path}:{rejection.line}
                    </strong>
                    <span>{rejection.reviewer_name}</span>
                  </header>
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
        {activityGroups.length ? (
          <div class="activity-layout">
            <nav class="activity-groups" aria-label="Review personas and batches">
              {activityGroups.map((group) => {
                const active = group.id === selectedGroup?.id;
                return (
                  <div class={`activity-group${active ? " active" : ""}`} key={group.id}>
                    <button type="button" onClick={() => selectPreferredTask(group.tasks)}>
                      <span>
                        <strong>{group.name}</strong>
                        <small>{group.subtitle}</small>
                      </span>
                      <StatusPill status={group.status} />
                    </button>
                    {active && group.tasks.length > 1 && (
                      <div class="batch-tabs">
                        {group.tasks.map((task) => (
                          <button
                            class={task.id === selectedTask?.id ? "active" : ""}
                            type="button"
                            onClick={() => setSelectedTaskId(task.id)}
                            key={task.id}
                          >
                            {task.role === "coordinator"
                              ? "Attempt"
                              : `Batch ${task.batch_index + 1}`}
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
                <dl class="task-facts">
                  <div>
                    <dt>Capacity wait</dt>
                    <dd>{duration(selectedTask.provider_wait_ms)}</dd>
                  </div>
                  <div>
                    <dt>Model/tools</dt>
                    <dd>{duration(selectedTask.model_elapsed_ms)}</dd>
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
                    <dt>Candidates / confirmed</dt>
                    <dd>
                      {selectedTask.candidate_issue_count} / {selectedTask.confirmed_issue_count}
                    </dd>
                  </div>
                </dl>
                <OutputBlock
                  title="Assistant output"
                  value={selectedTask.output}
                  followTail={selectedTask.status === "running"}
                />
                <OutputBlock
                  title="Reasoning"
                  value={selectedTask.thinking}
                  followTail={selectedTask.status === "running"}
                />
                <OutputBlock
                  title="Tool output"
                  value={selectedTask.tool_output}
                  followTail={selectedTask.status === "running"}
                />
                <details class="nested">
                  <summary>Prompt</summary>
                  <pre>{selectedTask.prompt}</pre>
                </details>
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
  useEffect(() => {
    if (!followTail || !pinnedRef.current) return;
    const frame = window.requestAnimationFrame(() => {
      const element = preRef.current;
      if (element && pinnedRef.current) element.scrollTop = element.scrollHeight;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [followTail, value]);
  if (!value) return null;
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
        {value}
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
  useEffect(() => setDraft(repository), [repository]);
  const toggleReviewer = (id: string): void => {
    setDraft((current) => ({
      ...current,
      reviewer_ids: current.reviewer_ids.includes(id)
        ? current.reviewer_ids.filter((reviewer) => reviewer !== id)
        : [...current.reviewer_ids, id],
    }));
  };
  return (
    <details class="repository-editor">
      <summary>
        <span>
          <strong>{repository.repository}</strong>
          <small>
            {repository.private ? "private" : "public"} · installation {repository.installation_id}
          </small>
        </span>
        <StatusPill status={repository.mode === "off" ? "disabled" : repository.mode} />
      </summary>
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          setBusy(true);
          try {
            await saveRepository(draft);
            flash("Saved");
            onSaved();
          } catch (cause) {
            flash(cause instanceof Error ? cause.message : String(cause));
          } finally {
            setBusy(false);
          }
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
          </label>
          <label>
            Default model
            <select
              value={draft.model ?? ""}
              onChange={(event) =>
                setDraft({ ...draft, model: event.currentTarget.value || undefined })
              }
            >
              <option value="">System review default</option>
              {models.map((model) => (
                <option value={model.id} key={model.id}>
                  {model.display_name} · {model.id}
                </option>
              ))}
            </select>
          </label>
        </div>
        <label>
          Repository instructions
          <textarea
            rows={4}
            value={draft.prompt}
            onInput={(event) => setDraft({ ...draft, prompt: event.currentTarget.value })}
          />
        </label>
        <fieldset>
          <legend>Reviewer personas</legend>
          <div class="check-grid">
            {reviewers.map((reviewer) => (
              <label class="checkbox" key={reviewer.id}>
                <input
                  type="checkbox"
                  checked={draft.reviewer_ids.includes(reviewer.id)}
                  onChange={() => toggleReviewer(reviewer.id)}
                />
                <span>
                  <strong>{reviewer.name}</strong>
                  <small>{reviewer.model || "inherits model"}</small>
                </span>
              </label>
            ))}
          </div>
        </fieldset>
        <div class="action-row">
          <button type="submit" disabled={busy || draft.reviewer_ids.length === 0}>
            {busy ? "Saving…" : "Save repository"}
          </button>
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
  useEffect(() => setDraft(reviewer ?? empty), [reviewer]);
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
      </label>
      <label>
        Thinking level
        <select
          value={draft.default_thinking_level ?? ""}
          disabled={!reviewerThinking.values.length}
          onChange={(event) =>
            setDraft({
              ...draft,
              default_thinking_level: event.currentTarget.value || undefined,
            })
          }
        >
          <option value="">Inherit default</option>
          {reviewerThinking.values.map((level) => (
            <option value={level} key={level}>
              {thinkingLevelLabel(level)}
            </option>
          ))}
        </select>
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
  models,
  onChanged,
}: {
  app: GithubAppStatus;
  providers: ProvidersResponse | null;
  models: Model[];
  onChanged: () => void;
}) {
  return (
    <section>
      <PageHeader
        eyebrow="Administration"
        title="Settings"
        description="GitHub App credentials, webhook and Checks health, and model-provider authentication."
      />
      <div class="settings-grid">
        <GithubAppSettings app={app} onChanged={onChanged} />
        <ProviderSettings providers={providers} models={models} onChanged={onChanged} />
      </div>
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
  const loadCliData = async (): Promise<void> => {
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
    } catch (cause) {
      flash(cause instanceof Error ? cause.message : String(cause));
    }
  };
  useEffect(() => {
    void loadCliData();
  }, []);
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
              defaultThinkingOptions.values.length ? defaultThinking : undefined,
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
        </label>
        <label>
          Global thinking level
          <select
            value={defaultThinking}
            onChange={(event) => setDefaultThinking(event.currentTarget.value)}
            disabled={!defaultThinkingOptions.values.length}
          >
            {defaultThinkingOptions.values.map((level) => (
              <option value={level} key={level}>{thinkingLevelLabel(level)}</option>
            ))}
          </select>
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
            <p class="muted">Managed versions take precedence over system copies on PATH.</p>
          </div>
          <button class="ghost compact" type="button" onClick={() => void loadCliData()}>
            Refresh
          </button>
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
