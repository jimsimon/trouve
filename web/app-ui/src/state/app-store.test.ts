import { describe, expect, it } from "vitest";

import type {
  ProtocolEventEnvelope,
  ProtocolPrInfo,
  ProtocolThread,
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
    ).toBe(true);
    expect(readSignal(store.sessions)).toEqual([]);
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
      expect.objectContaining({ number: 9, title: "Fresh linked PR" }),
      expect.objectContaining({ number: 7 }),
      expect.objectContaining({ number: 8, state: "merged" }),
    ]);
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

  it("orders title-model settings snapshots against delayed SSE replay", () => {
    const store = new AppStore();
    const settings = (state: string) => ({
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
