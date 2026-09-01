import { describe, expect, it } from "vitest";

import type {
  ProtocolEventEnvelope,
  ProtocolPrInfo,
  ProtocolServerProjection,
  ProtocolThread,
  ProtocolThreadStatus,
  ProtocolThreadViewSnapshot,
} from "../services/protocol-client.js";
import { readSignal } from "./reactivity.js";
import { AppStore } from "./app-store.js";

const metadata = {
  id: "se_1",
  workspace_id: "ws_1",
  title: "Generated metadata",
  branch: "trouve/generated-metadata",
  worktree_path: "/tmp/generated-metadata",
  base_ref: "main",
  created_at: "2026-08-01T12:00:00Z",
};

const summary = {
  session_id: "se_1",
  workspace_id: "ws_1",
  archived: false,
  active: false,
  attention: "question",
  outcome: "idle",
  latest_thread_id: "th_1",
  latest_cursor: 5,
  updated_at: "2026-08-01T12:01:00Z",
} as const;

const thread = (id: string, todos?: ProtocolThread["todos"]): ProtocolThread => ({
  id,
  session_id: "se_1",
  model: "openai/gpt-5.6",
  mode: "code",
  permission_mode: "ask",
  created_at: "2026-08-01T12:02:00Z",
  ...(todos === undefined ? {} : { todos }),
});

const threadStatus = (
  threadId: string,
  overrides: Partial<ProtocolThreadStatus> = {},
): ProtocolThreadStatus => ({
  thread_id: threadId,
  session_id: "se_1",
  active: false,
  attention: "none",
  outcome: "idle",
  latest_cursor: 1,
  ...overrides,
});

const todoEvent = (
  cursor: number,
  todos: Extract<ProtocolEventEnvelope, { type: "thread.todos_updated" }>["todos"],
): ProtocolEventEnvelope => ({
  cursor,
  scope: { thread: "th_1" },
  ts: `2026-08-01T12:03:0${cursor}Z`,
  type: "thread.todos_updated",
  todos,
});

const pullRequest = (
  number: number,
  overrides: Partial<ProtocolPrInfo> = {},
): ProtocolPrInfo => ({
  host: "github.com",
  repository: "trouve-ai/trouve",
  workspace_id: "ws_1",
  number,
  url: `https://github.com/trouve-ai/trouve/pull/${number}`,
  title: `Pull request ${number}`,
  state: "open",
  draft: false,
  base: "main",
  head: "trouve/generated-metadata",
  checks: [],
  reviews: [],
  ...overrides,
});

describe("AppStore", () => {
  it("joins generated session metadata with the durable summary projection", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);

    expect(readSignal(store.sessions)).toEqual([
      expect.objectContaining({
        id: "se_1",
        title: "Generated metadata",
        branch: "trouve/generated-metadata",
        attention: "question",
        state: "attention",
      }),
    ]);
  });

  it("applies full replacement summary events and deletion tombstones", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    store.replaceThreadsForSession("se_1", [thread("th_1")]);
    store.replaceThreadStatusesForSession("se_1", [threadStatus("th_1")]);
    const cachedView = store.threadView("th_1");

    expect(
      store.applyServerEvent({
        cursor: 7,
        scope: "server",
        ts: "2026-08-01T12:02:00Z",
        type: "session.summary_updated",
        session_id: "se_1",
        summary: { ...summary, attention: "none", outcome: "failed", latest_cursor: 6 },
      }),
    ).toBe(false);
    expect(readSignal(store.sessions)[0]?.state).toBe("failed");
    expect(readSignal(store.sessions)[0]?.unread).toBe(true);
    expect(store.markSessionRead("se_1")).toBe(true);
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", unread: false });
    expect(store.markSessionRead("se_1")).toBe(false);

    expect(
      store.applyServerEvent({
        cursor: 9,
        scope: "server",
        ts: "2026-08-01T12:03:00Z",
        type: "session.summary_updated",
        session_id: "se_1",
        summary: null,
      }),
    ).toBe(false);
    expect(readSignal(store.sessions)).toEqual([]);
    expect(store.threadsForSession("se_1")).toEqual([]);
    expect(store.threadStatus("th_1")).toBeUndefined();
    expect(store.threadView("th_1")).not.toBe(cachedView);
  });

  it("purges session state immediately on the deletion lifecycle event", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    store.replaceThreadsForSession("se_1", [thread("th_1")]);

    expect(store.applyServerEvent({
      cursor: 8,
      scope: "server",
      ts: "2026-08-01T12:03:00Z",
      type: "session.deleted",
      session_id: "se_1",
      workspace_id: "ws_1",
    })).toBe(false);
    expect(readSignal(store.sessions)).toEqual([]);
    expect(store.threadsForSession("se_1")).toEqual([]);
  });

  it("purges status-only thread state when its session is deleted", () => {
    const store = new AppStore();
    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_status_only", {
        active: true,
        outcome: "running",
        latest_cursor: 8,
      }),
    ]);
    const cachedView = store.threadView("th_status_only");
    expect(store.thread("th_status_only")).toBeUndefined();
    expect(store.threadStatus("th_status_only")).toBeDefined();

    store.removeSession("se_1");

    expect(store.threadStatus("th_status_only")).toBeUndefined();
    expect(store.threadIndicatorState("th_status_only")).toEqual({
      active: false,
      attention: "none",
      outcome: "idle",
      unread: false,
    });
    expect(store.threadView("th_status_only")).not.toBe(cachedView);
  });

  it("removes status-only orphans when the authoritative thread list arrives", () => {
    const store = new AppStore();
    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_current"),
      threadStatus("th_orphan"),
    ]);
    const orphanView = store.threadView("th_orphan");

    store.replaceThreadsForSession("se_1", [thread("th_current")]);

    expect(store.threadStatus("th_current")).toBeDefined();
    expect(store.threadStatus("th_orphan")).toBeUndefined();
    expect(store.threadView("th_orphan")).not.toBe(orphanView);
  });

  it("does not restore status-only orphans after an authoritative empty thread list", () => {
    const store = new AppStore();
    store.replaceThreadsForSession("se_1", []);

    store.replaceThreadStatusesForSession("se_1", [threadStatus("th_orphan")]);

    expect(store.threadStatus("th_orphan")).toBeUndefined();
  });

  it("keeps deletion tombstones across stale bulk snapshots until a newer creation", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary], 5);
    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:03:00Z",
      type: "session.deleted",
      session_id: "se_1",
      workspace_id: "ws_1",
    });

    store.replaceSessionMetadata([{ ...metadata, title: "Stale list" }]);
    store.upsertSessionMetadata({ ...metadata, title: "Stale detail" });
    store.replaceSessionSummaries([{ ...summary, latest_cursor: 8 }], 8);
    store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:04:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary: { ...summary, latest_cursor: 10 },
    });
    expect(store.isSessionTombstoned("se_1")).toBe(true);
    expect(readSignal(store.sessions)).toEqual([]);

    expect(store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:03:00Z",
      type: "session.created",
      session_id: "se_1",
      workspace_id: "ws_1",
    })).toBe(false);
    expect(store.isSessionTombstoned("se_1")).toBe(true);

    expect(store.applyServerEvent({
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:05:00Z",
      type: "session.created",
      session_id: "se_1",
      workspace_id: "ws_1",
    })).toBe(true);
    store.replaceSessionMetadata([{ ...metadata, title: "Recreated" }]);
    store.replaceSessionSummaries([{ ...summary, latest_cursor: 11 }], 11);
    expect(store.isSessionTombstoned("se_1")).toBe(false);
    expect(store.session("se_1")?.title).toBe("Recreated");
  });

  it("keeps a local tombstone unbounded until its durable delete edge arrives", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.removeSession("se_1");

    expect(store.applyServerEvent({
      cursor: 100,
      scope: "server",
      ts: "2026-08-01T12:05:00Z",
      type: "session.created",
      session_id: "se_1",
      workspace_id: "ws_1",
    })).toBe(false);
    expect(store.isSessionTombstoned("se_1")).toBe(true);

    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:03:00Z",
      type: "session.deleted",
      session_id: "se_1",
      workspace_id: "ws_1",
    });
    expect(store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:04:00Z",
      type: "session.created",
      session_id: "se_1",
      workspace_id: "ws_1",
    })).toBe(true);
    expect(store.isSessionTombstoned("se_1")).toBe(false);
  });

  it("does not roll a refreshed projection backward with delayed SSE", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries(
      [{ ...summary, attention: "none", outcome: "succeeded", latest_cursor: 8 }],
      9,
    );

    expect(
      store.applyServerEvent({
        cursor: 8,
        scope: "server",
        ts: "2026-08-01T12:02:00Z",
        type: "session.summary_updated",
        session_id: "se_1",
        summary: { ...summary, attention: "approval", latest_cursor: 7 },
      }),
    ).toBe(false);
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", unread: false });

    store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:03:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary: { ...summary, attention: "none", outcome: "failed", latest_cursor: 9 },
    });
    expect(readSignal(store.sessions)[0]?.state).toBe("failed");
  });

  it("keeps unread local by comparing seen and latest summary cursors", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([
      { ...summary, attention: "none", outcome: "succeeded", latest_cursor: 8 },
    ], 8);
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", unread: false });

    // A foreground/reconnect snapshot newer than the established baseline
    // still exposes work that completed while this client was suspended.
    store.replaceSessionSummaries([
      { ...summary, attention: "none", outcome: "succeeded", latest_cursor: 12 },
    ], 12);
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "done", unread: true });

    store.markSessionRead("se_1");
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", unread: false });
  });

  it("does not resurrect a read terminal badge for unrelated live summary updates", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([
      { ...summary, attention: "none", outcome: "succeeded", latest_cursor: 8 },
    ], 8);

    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:04:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary: {
        ...summary,
        attention: "none",
        outcome: "succeeded",
        latest_cursor: 9,
        updated_at: "2026-08-01T12:04:00Z",
      },
    });
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", unread: false });

    store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:05:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary: {
        ...summary,
        attention: "none",
        active: true,
        outcome: "running",
        latest_cursor: 10,
        updated_at: "2026-08-01T12:05:00Z",
      },
    });
    store.applyServerEvent({
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:06:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary: {
        ...summary,
        attention: "none",
        outcome: "succeeded",
        latest_cursor: 11,
        updated_at: "2026-08-01T12:06:00Z",
      },
    });
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "done", unread: true });
  });

  it("tracks independent live, attention, and unread terminal states for thread tabs", () => {
    const store = new AppStore();
    store.replaceThreadsForSession("se_1", [thread("th_1"), thread("th_2")]);
    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_1", { outcome: "succeeded", latest_cursor: 5 }),
      threadStatus("th_2", { latest_cursor: 5 }),
    ]);
    expect(store.threadIndicatorState("th_1")).toEqual({
      active: false,
      attention: "none",
      outcome: "succeeded",
      unread: false,
    });
    expect(store.threadStatus("th_1")).toMatchObject({
      thread_id: "th_1",
      outcome: "succeeded",
    });

    store.applyServerEvent({
      cursor: 7,
      scope: "server",
      ts: "2026-08-01T12:07:00Z",
      type: "thread.status_updated",
      status: threadStatus("th_1", {
        active: true,
        outcome: "running",
        latest_cursor: 6,
      }),
    });
    expect(store.threadIndicatorState("th_1")).toMatchObject({
      active: true,
      outcome: "running",
      unread: false,
    });

    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:08:00Z",
      type: "thread.status_updated",
      status: threadStatus("th_1", {
        outcome: "succeeded",
        latest_cursor: 8,
      }),
    });
    expect(store.threadIndicatorState("th_1")).toMatchObject({
      active: false,
      outcome: "succeeded",
      unread: true,
    });
    expect(store.markThreadRead("th_1")).toBe(true);
    expect(store.threadIndicatorState("th_1").unread).toBe(false);
    expect(store.markThreadRead("th_1")).toBe(false);

    store.applyServerEvent({
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:09:00Z",
      type: "thread.status_updated",
      status: threadStatus("th_2", {
        attention: "question",
        latest_cursor: 10,
      }),
    });
    expect(store.threadIndicatorState("th_2")).toMatchObject({
      attention: "question",
      unread: false,
    });

    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_1", { active: true, outcome: "running", latest_cursor: 6 }),
      threadStatus("th_2", { latest_cursor: 5 }),
    ]);
    expect(store.threadIndicatorState("th_1")).toMatchObject({
      active: false,
      outcome: "succeeded",
    });
    expect(store.threadIndicatorState("th_2").attention).toBe("question");

    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_2", { latest_cursor: 5 }),
    ]);
    expect(store.threadStatus("th_1")).toMatchObject({
      outcome: "succeeded",
      latest_cursor: 8,
    });
  });

  it("advances session usage revision when a background thread completes", () => {
    const store = new AppStore();
    store.replaceThreadsForSession("se_1", [thread("th_1"), thread("th_2")]);
    store.replaceThreadStatusesForSession("se_1", [
      threadStatus("th_1", { active: true, outcome: "running", latest_cursor: 2 }),
      threadStatus("th_2", { active: true, outcome: "running", latest_cursor: 3 }),
    ]);
    expect(store.sessionUsageRevision("se_1")).toBe(0);

    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:08:00Z",
      type: "thread.status_updated",
      status: threadStatus("th_2", {
        outcome: "succeeded",
        latest_cursor: 8,
        completed_at: "2026-08-01T12:08:00Z",
      }),
    });
    expect(store.sessionUsageRevision("se_1")).toBe(1);

    store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:08:01Z",
      type: "thread.status_updated",
      status: threadStatus("th_2", {
        attention: "question",
        outcome: "succeeded",
        latest_cursor: 9,
        completed_at: "2026-08-01T12:08:00Z",
      }),
    });
    expect(store.sessionUsageRevision("se_1")).toBe(1);
    expect(store.sessionUsageRevision("se_other")).toBe(0);
  });

  it("folds durable account PR snapshots independently per GitHub host", () => {
    const store = new AppStore();
    const event = (
      cursor: number,
      host: string,
      title: string,
    ): ProtocolEventEnvelope => ({
      cursor,
      scope: "server",
      ts: `2026-08-01T12:0${cursor}:00Z`,
      type: "github.pull_requests_updated",
      pull_requests: {
        host,
        viewer: "octocat",
        prs: [{
          number: cursor,
          title,
          state: "open",
          draft: false,
          base: "main",
          head: "feature",
          url: `https://${host}/acme/app/pull/${cursor}`,
          checks: [],
          reviews: [],
        }],
      },
    });

    expect(store.applyServerEvent(event(4, "github.com", "Current"))).toBe(false);
    store.applyServerEvent(event(3, "github.com", "Stale"));
    store.applyServerEvent(event(5, "github.example.com", "Enterprise"));

    expect(readSignal(store.githubPullRequests)).toEqual([
      expect.objectContaining({
        cursor: 4,
        refreshedAt: "2026-08-01T12:04:00Z",
        pullRequests: expect.objectContaining({
          host: "github.com",
          prs: [expect.objectContaining({ title: "Current" })],
        }),
      }),
      expect.objectContaining({
        cursor: 5,
        pullRequests: expect.objectContaining({ host: "github.example.com" }),
      }),
    ]);
  });

  it("shares account PR updates and authoritative session associations", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    store.replaceSessionPullRequests("se_1", [
      pullRequest(9, { head: "linked-from-session-activity", title: "Stale linked PR" }),
      pullRequest(7, { head: "linked-only", repository: "trouve-ai/other" }),
    ]);

    store.applyServerEvent({
      cursor: 12,
      scope: "server",
      ts: "2026-08-01T12:12:00Z",
      type: "github.pull_requests_updated",
      pull_requests: {
        host: "github.com",
        viewer: "octocat",
        prs: [
          pullRequest(9, {
            head: "linked-from-session-activity",
            title: "Fresh linked PR",
            merge_state_status: "clean",
          }),
          pullRequest(8, { state: "merged" }),
          pullRequest(6, { workspace_id: "ws_other" }),
        ],
      },
    });

    expect(store.sessionPullRequests("se_1")).toEqual([
      expect.objectContaining({ number: 8, state: "merged" }),
      expect.objectContaining({ number: 9, title: "Fresh linked PR" }),
      expect.objectContaining({ number: 7 }),
    ]);
  });

  it("associates chat-mentioned PRs while keeping the session branch first", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    store.applyServerEvent({
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:11:00Z",
      type: "session.pr_mentioned",
      session_id: "se_1",
      number: 20,
      url: "https://github.com/trouve-ai/trouve/pull/20",
    });
    store.applyServerEvent({
      cursor: 12,
      scope: "server",
      ts: "2026-08-01T12:12:00Z",
      type: "github.pull_requests_updated",
      pull_requests: {
        host: "github.com",
        viewer: "octocat",
        prs: [
          pullRequest(20, { head: "mentioned-only" }),
          pullRequest(8, { state: "merged" }),
        ],
      },
    });

    expect(store.sessionPullRequests("se_1").map(({ number }) => number)).toEqual([8, 20]);
  });

  it("hydrates all session PR associations from the cold-start server projection", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    const linked = pullRequest(12, {
      head: "linked-from-durable-activity",
      title: "Linked before selection",
    });
    const projection: ProtocolServerProjection = {
      github_pull_requests: [{
        cursor: 20,
        refreshed_at: "2026-08-01T12:20:00Z",
        pull_requests: {
          host: "github.com",
          viewer: "octocat",
          prs: [linked],
        },
      }],
      session_pull_requests: [{ session_id: "se_1", prs: [linked] }],
      git_worktree_settings: {
        derive_branch_name_from_session_title: false,
        title_model_load_behavior: "auto",
        title_model_resource_policy: "adaptive",
        title_model: {
          state: "ready",
          runtime_installed: true,
          model_downloaded: true,
        },
      },
    };

    expect(store.replaceServerProjection(21, projection)).toBe(true);
    expect(store.sessionPullRequests("se_1")).toEqual([
      expect.objectContaining({ number: 12, title: "Linked before selection" }),
    ]);
    expect(readSignal(store.githubPullRequests)).toEqual([
      expect.objectContaining({ cursor: 20, refreshedAt: "2026-08-01T12:20:00Z" }),
    ]);
    expect(readSignal(store.gitWorktreeSettings)).toMatchObject({ cursor: 21 });
    expect(store.replaceServerProjection(19, {
      ...projection,
      github_pull_requests: [],
      session_pull_requests: [],
    })).toBe(false);
    expect(store.sessionPullRequests("se_1")).toHaveLength(1);
  });

  it("applies confirmed management mutations without waiting for SSE", () => {
    const store = new AppStore();
    store.replaceSessionMetadata([metadata]);
    store.replaceSessionSummaries([summary]);
    store.upsertSessionMetadata({ ...metadata, title: "Renamed", archived: true });

    expect(readSignal(store.sessions)[0]).toMatchObject({
      title: "Renamed",
      archived: true,
    });

    store.upsertThread({
      id: "th_1",
      session_id: "se_1",
      model: "openai/gpt-5.6",
      mode: "code",
      permission_mode: "ask",
      created_at: "2026-08-01T12:02:00Z",
    });
    expect(store.threadsForSession("se_1")).toHaveLength(1);

    store.removeSession("se_1");
    expect(readSignal(store.sessions)).toEqual([]);
    expect(store.threadsForSession("se_1")).toEqual([]);
  });

  it("delegates the session projection to attention-first inbox ordering", () => {
    const store = new AppStore();
    store.replaceSessionSummaries([], 1);
    store.replaceSessionSummaries([
      {
        ...summary,
        session_id: "se_archived",
        archived: true,
        attention: "both",
        outcome: "failed",
        updated_at: "2026-08-01T12:10:00Z",
      },
      {
        ...summary,
        session_id: "se_idle",
        attention: "none",
        outcome: "idle",
        updated_at: "2026-08-01T12:09:00Z",
      },
      {
        ...summary,
        session_id: "se_attention",
        attention: "question",
        outcome: "succeeded",
        updated_at: "2026-08-01T12:00:00Z",
      },
      {
        ...summary,
        session_id: "se_failed",
        attention: "none",
        outcome: "failed",
        updated_at: "2026-08-01T12:01:00Z",
      },
    ], 2);

    expect(readSignal(store.sessions).map(({ id }) => id)).toEqual([
      "se_attention",
      "se_failed",
      "se_idle",
      "se_archived",
    ]);
  });

  it("bounds retained background thread projections with LRU eviction", () => {
    const store = new AppStore({ maxThreadViews: 2 });
    const first = store.threadView("th_first");
    const second = store.threadView("th_second");
    expect(store.threadView("th_first")).toBe(first);

    store.threadView("th_third");
    expect(store.threadView("th_first")).toBe(first);
    expect(store.threadView("th_second")).not.toBe(second);
  });

  it("atomically replaces replay state with a folded tail and prepends older pages", () => {
    const store = new AppStore();
    store.upsertThread(thread("th_1", [
      { id: "stale", content: "Stale", status: "pending" },
    ]));
    store.applyThreadEvent("th_1", {
      cursor: 1,
      scope: { thread: "th_1" },
      ts: "2026-08-01T12:00:00Z",
      type: "user.message",
      turn: 1,
      content: "Replay-built",
      attachments: [],
    });
    const snapshot: ProtocolThreadViewSnapshot = {
      item_offset: 2,
      total_items: 3,
      has_older: true,
      items: [{
        kind: "assistant",
        turn: 2,
        content: "Folded tail",
        complete: true,
      }],
      todos: [{ id: "current", content: "Current", status: "in_progress" }],
    };

    expect(store.replaceThreadViewSnapshot("th_1", 50, snapshot)).toBe(true);
    expect(store.threadView("th_1").items).toMatchObject([
      { kind: "assistant", content: "Folded tail" },
    ]);
    expect(store.thread("th_1")?.todos).toMatchObject([
      { id: "current", status: "in_progress" },
    ]);
    expect(store.prependThreadViewSnapshot("th_1", {
      item_offset: 0,
      total_items: 3,
      has_older: false,
      items: [
        { kind: "user", turn: 1, content: "Earlier", attachments: [] },
        { kind: "assistant", turn: 1, content: "Earlier answer", complete: true },
      ],
    })).toBe(true);
    expect(store.threadView("th_1")).toMatchObject({
      cursor: 50,
      itemOffset: 0,
      hasOlder: false,
    });
    expect(store.threadView("th_1").items).toHaveLength(3);

    expect(store.replaceThreadViewSnapshot("th_1", 49, { items: [] })).toBe(false);
    expect(store.threadView("th_1").cursor).toBe(50);
  });

  it("retains prefetched history when a revisited thread receives a fresh tail", () => {
    const store = new AppStore();
    expect(store.replaceThreadViewSnapshot("th_1", 10, {
      item_offset: 2,
      total_items: 4,
      has_older: true,
      items: [
        { kind: "user", turn: 2, content: "tail", attachments: [] },
        { kind: "assistant", turn: 2, content: "answer", complete: true },
      ],
    })).toBe(true);
    expect(store.prependThreadViewSnapshot("th_1", {
      item_offset: 0,
      total_items: 4,
      has_older: false,
      items: [
        { kind: "user", turn: 1, content: "cached", attachments: [] },
        { kind: "assistant", turn: 1, content: "history", complete: true },
      ],
    })).toBe(true);

    expect(store.replaceThreadViewSnapshot("th_1", 12, {
      item_offset: 2,
      total_items: 5,
      has_older: true,
      items: [
        { kind: "user", turn: 2, content: "fresh tail", attachments: [] },
        { kind: "assistant", turn: 2, content: "fresh answer", complete: true },
        { kind: "user", turn: 3, content: "new", attachments: [] },
      ],
    })).toBe(true);

    const view = store.threadView("th_1");
    expect(view.itemOffset).toBe(0);
    expect(view.totalItems).toBe(5);
    expect(view.items.map((item) => "content" in item ? item.content : "")).toEqual([
      "cached",
      "history",
      "fresh tail",
      "fresh answer",
      "new",
    ]);
  });

  it("seeds new thread projections from the initial todo snapshot", () => {
    const store = new AppStore();
    const todos = [
      { id: "one", content: "Audit", status: "completed" as const },
      { id: "two", content: "Build", status: "in_progress" as const },
    ];
    store.upsertThread(thread("th_1", todos));

    const view = store.threadView("th_1");
    expect(view.todos).toEqual(todos);
    expect(view.todos).not.toBe(todos);
    expect(view.todos[0]).not.toBe(todos[0]);
  });

  it("preserves live todos when a compatible folded snapshot omits them", () => {
    const store = new AppStore();
    const live = [
      { id: "live", content: "Keep current state", status: "in_progress" as const },
    ];
    store.upsertThread(thread("th_1"));
    store.applyThreadEvent("th_1", todoEvent(1, live));

    expect(store.replaceThreadViewSnapshot("th_1", 2, { items: [] })).toBe(true);

    expect(store.threadView("th_1").todos).toEqual(live);
    expect(store.thread("th_1")?.todos).toEqual(live);
  });

  it("projects todo replacement events into both view and thread metadata", () => {
    const store = new AppStore();
    store.upsertThread(thread("th_1", [
      { id: "old", content: "Old", status: "pending" },
    ]));
    const replacement = [
      { id: "second", content: "Second", status: "in_progress" as const },
      { id: "first", content: "First", status: "completed" as const },
    ];

    expect(store.applyThreadEvent("th_1", todoEvent(1, replacement))).toBe(true);
    expect(store.threadView("th_1").todos).toEqual(replacement);
    expect(store.thread("th_1")?.todos).toEqual(replacement);
    expect(store.thread("th_1")?.todos).not.toBe(replacement);
  });

  it("folds replay batches in order through one thread projection", () => {
    const store = new AppStore();
    const event = (cursor: number, content: string): ProtocolEventEnvelope => ({
      cursor,
      scope: { thread: "th_1" },
      ts: `2026-08-01T12:03:0${cursor}Z`,
      type: "user.message",
      turn: cursor,
      content,
      attachments: [],
    });

    expect(store.applyThreadEvents("th_1", [event(1, "first"), event(2, "second")]))
      .toBe(true);
    expect(store.threadView("th_1").items).toMatchObject([
      { kind: "user", content: "first" },
      { kind: "user", content: "second" },
    ]);
    expect(store.threadView("th_1").cursor).toBe(2);
  });

  it("publishes optimistic approval transitions through the store", () => {
    const store = new AppStore();
    store.applyThreadEvent("th_1", {
      cursor: 1,
      scope: { thread: "th_1" },
      ts: "2026-08-01T12:03:01Z",
      type: "tool.requested",
      turn: 1,
      call_id: "call_approval",
      tool: "shell",
      args: { command: "cargo test" },
      requires_approval: true,
    });

    expect(store.resolveApprovalOptimistically("th_1", "call_approval", "approve"))
      .toBe(true);
    expect(store.threadView("th_1").findTool("call_approval")?.status).toBe("running");
    expect(store.resolveApprovalOptimistically("th_1", "call_approval", "deny"))
      .toBe(false);
  });

  it("orders title-model settings snapshots against delayed SSE replay", () => {
    const store = new AppStore();
    const settings = (state: string) => ({
      derive_branch_name_from_session_title: false,
      title_model_load_behavior: "auto" as const,
      title_model_resource_policy: "adaptive" as const,
      title_model: {
        state,
        runtime_installed: false,
        model_downloaded: false,
      },
    });
    expect(store.replaceGitWorktreeSettings(8, settings("installing"))).toBe(true);
    expect(store.applyServerEvent({
      cursor: 7,
      scope: "server",
      ts: "2026-08-01T12:04:00Z",
      type: "settings.git_worktrees_updated",
      settings: settings("stale"),
    })).toBe(false);
    expect(readSignal(store.gitWorktreeSettings)).toMatchObject({
      cursor: 8,
      settings: { title_model: { state: "installing" } },
    });

    store.applyServerEvent({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:04:01Z",
      type: "settings.git_worktrees_updated",
      settings: settings("ready"),
    });
    expect(readSignal(store.gitWorktreeSettings)).toMatchObject({
      cursor: 9,
      settings: { title_model: { state: "ready" } },
    });
  });

  it("projects live connectivity and automation invalidations", () => {
    const store = new AppStore();
    store.replaceServerInfo({
      name: "trouve-server",
      version: "3.7.0",
      protocol_version: "2.4",
      online: true,
    });

    expect(store.applyServerEvent({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:04:02Z",
      type: "server.connectivity_changed",
      online: false,
    })).toBe(false);
    expect(readSignal(store.serverInfo)?.online).toBe(false);

    expect(readSignal(store.automationRevision)).toBe(0);
    expect(store.applyServerEvent({
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:04:03Z",
      type: "automation.fired",
      automation_id: "au_1",
      error: "",
      session_id: "se_2",
    })).toBe(true);
    expect(readSignal(store.automationRevision)).toBe(1);
  });

  it("retains the live todo replacement across late metadata and LRU recreation", () => {
    const store = new AppStore({ maxThreadViews: 1 });
    const live = [
      { id: "live", content: "Current event", status: "in_progress" as const },
      { id: "next", content: "Next", status: "pending" as const },
    ];
    store.applyThreadEvent("th_1", todoEvent(1, live));
    store.upsertThread(thread("th_1", [
      { id: "stale", content: "Stale snapshot", status: "pending" },
    ]));

    expect(store.thread("th_1")?.todos).toEqual(live);
    const originalView = store.threadView("th_1");
    store.threadView("th_other");
    const recreated = store.threadView("th_1");
    expect(recreated).not.toBe(originalView);
    expect(recreated.todos).toEqual(live);
  });
});
