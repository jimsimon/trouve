import { describe, expect, it, vi } from "vitest";

import { AppStore } from "../state/app-store.js";
import { readSignal } from "../state/reactivity.js";
import type { CursorEventStream } from "./cursor-event-stream.js";
import { ProtocolIngress, ServerReplayBuffer } from "./protocol-ingress.js";
import {
  type ProtocolIngressEvent,
  ProtocolClient,
  ProtocolClientError,
} from "./protocol-client.js";

type ServerEventOptions = Parameters<ProtocolClient["serverEvents"]>[0];
type KnownIngressEvent = Extract<ProtocolIngressEvent, { readonly kind: "known" }>;
type Sessions = Awaited<ReturnType<ProtocolClient["sessions"]>>;
type Session = Sessions[number];
type Snapshot = Awaited<ReturnType<ProtocolClient["sessionSummaries"]>>;

const info: Awaited<ReturnType<ProtocolClient["serverInfo"]>> = {
  name: "trouve-server",
  version: "3.7.0",
  protocol_version: "2.4",
  online: true,
};

const workspace: Awaited<ReturnType<ProtocolClient["workspaces"]>>[number] = {
  id: "ws_1",
  name: "trouve",
  path: "/src/trouve",
};

const session = (title: string): Session => ({
  id: "se_1",
  workspace_id: "ws_1",
  title,
  branch: "trouve/protocol-ingress",
  worktree_path: "/tmp/protocol-ingress",
  base_ref: "main",
  created_at: "2026-08-01T12:00:00Z",
});

const summary: Snapshot["summaries"][number] = {
  session_id: "se_1",
  workspace_id: "ws_1",
  archived: false,
  active: false,
  attention: "none",
  outcome: "idle",
  latest_cursor: 7,
  updated_at: "2026-08-01T12:01:00Z",
};

const lifecycleEvent = (cursor: number): KnownIngressEvent => ({
  kind: "known",
  cursor,
  envelope: {
    cursor,
    scope: "server",
    ts: `2026-08-01T12:02:${String(cursor).padStart(2, "0")}Z`,
    type: "session.updated",
    session_id: "se_1",
    workspace_id: "ws_1",
  },
});

const activityEvent = (cursor: number, active: boolean): KnownIngressEvent => ({
  kind: "known",
  cursor,
  envelope: {
    cursor,
    scope: "server",
    ts: `2026-08-01T12:02:${String(cursor).padStart(2, "0")}Z`,
    type: "session.activity",
    session_id: "se_1",
    workspace_id: "ws_1",
    active,
  },
});

const githubSnapshotEvent = (
  cursor: number,
  host = "github.com",
  viewer = "octocat",
): KnownIngressEvent => ({
  kind: "known",
  cursor,
  envelope: {
    cursor,
    scope: "server",
    ts: `2026-08-01T12:02:${String(cursor).padStart(2, "0")}Z`,
    type: "github.pull_requests_updated",
    pull_requests: {
      host,
      viewer,
      prs: [],
    },
  },
});

const gitWorktreeSettingsEvent = (cursor: number, state: string): KnownIngressEvent => ({
  kind: "known",
  cursor,
  envelope: {
    cursor,
    scope: "server",
    ts: `2026-08-01T12:03:${String(cursor).padStart(2, "0")}Z`,
    type: "settings.git_worktrees_updated",
    settings: {
      title_model_load_behavior: "auto",
      title_model_resource_policy: "adaptive",
      title_model: {
        state,
        runtime_installed: false,
        model_downloaded: false,
      },
    },
  },
});

const deferred = <T>() => {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
};

const stream = () => ({
  start: vi.fn(),
  close: vi.fn(),
  reconnectNow: vi.fn(),
}) as unknown as CursorEventStream<ProtocolIngressEvent>;

const retrySources = () => {
  let visibilityListener: (() => void) | undefined;
  let onlineListener: (() => void) | undefined;
  const visibility = {
    visibilityState: "visible" as DocumentVisibilityState,
    addEventListener: vi.fn((_type: "visibilitychange", listener: () => void) => {
      visibilityListener = listener;
    }),
    removeEventListener: vi.fn((_type: "visibilitychange", listener: () => void) => {
      if (visibilityListener === listener) visibilityListener = undefined;
    }),
  };
  const online = {
    addEventListener: vi.fn((_type: "online", listener: () => void) => {
      onlineListener = listener;
    }),
    removeEventListener: vi.fn((_type: "online", listener: () => void) => {
      if (onlineListener === listener) onlineListener = undefined;
    }),
  };
  return {
    online,
    visibility,
    emitOnline: () => onlineListener?.(),
    emitVisible: () => visibilityListener?.(),
  };
};

describe("ProtocolIngress", () => {
  it("coalesces historical replacement events to the newest snapshot per key", () => {
    const replay = new ServerReplayBuffer();
    expect(replay.push(githubSnapshotEvent(1).envelope)).toBe(true);
    expect(replay.push(githubSnapshotEvent(3, "github.com", "new").envelope)).toBe(true);
    expect(replay.push(githubSnapshotEvent(2, "github.example.com", "enterprise").envelope)).toBe(true);
    expect(replay.push(gitWorktreeSettingsEvent(4, "installing").envelope)).toBe(true);
    expect(replay.push(gitWorktreeSettingsEvent(5, "ready").envelope)).toBe(true);
    expect(replay.push(lifecycleEvent(6).envelope)).toBe(false);

    const buffered = replay.take();
    expect(buffered.map(({ cursor }) => cursor)).toEqual([2, 3, 5]);
    expect(buffered.find(({ cursor }) => cursor === 3)).toMatchObject({
      pull_requests: { viewer: "new" },
    });
    expect(buffered.find(({ cursor }) => cursor === 5)).toMatchObject({
      settings: { title_model: { state: "ready" } },
    });
    expect(replay.take()).toEqual([]);
  });

  it("fetches session metadata after the cursor-bearing initial snapshot", async () => {
    const store = new AppStore();
    const pendingSnapshot = deferred<Snapshot>();
    let currentSessions: Sessions = [];
    const sessions = vi.fn(async () => currentSessions);
    const fakeStream = stream();
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessions,
      sessionSummaries: vi.fn(() => pendingSnapshot.promise),
      workspaces: vi.fn(async () => [workspace]),
      serverEvents: vi.fn(async () => fakeStream),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
    );

    const starting = ingress.start();
    expect(ingress.start()).toBe(starting);
    expect(sessions).not.toHaveBeenCalled();

    currentSessions = [session("Created after the first request began")];
    pendingSnapshot.resolve({ summaries: [summary], cursor: 8 });
    await starting;

    expect(sessions).toHaveBeenCalledOnce();
    expect(store.session("se_1")).toMatchObject({
      title: "Created after the first request began",
      branch: "trouve/protocol-ingress",
    });
    expect(protocol.serverEvents).toHaveBeenCalledWith(
      expect.objectContaining({ after: 0 }),
    );
    ingress.stop();
  });

  it("hydrates PR badges from the server projection and resumes after the summary cursor", async () => {
    const store = new AppStore();
    const fakeStream = stream();
    const linkedPr = {
      host: "github.com",
      repository: "acme/trouve",
      workspace_id: "ws_1",
      number: 141,
      url: "https://github.com/acme/trouve/pull/141",
      title: "Linked from durable session activity",
      state: "open",
      draft: false,
      base: "main",
      head: "different-branch",
      checks: [],
      reviews: [],
    };
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessions: vi.fn(async () => [session("Current")]),
      sessionSummaries: vi.fn(async () => ({ summaries: [summary], cursor: 8 })),
      workspaces: vi.fn(async () => [workspace]),
      serverProjectionSnapshot: vi.fn(async () => ({
        cursor: 10,
        value: {
          github_pull_requests: [{
            cursor: 9,
            refreshed_at: "2026-08-01T12:02:09Z",
            pull_requests: {
              host: "github.com",
              viewer: "octocat",
              prs: [linkedPr],
            },
          }],
          session_pull_requests: [{ session_id: "se_1", prs: [linkedPr] }],
          git_worktree_settings: {
            title_model_load_behavior: "auto",
            title_model_resource_policy: "adaptive",
            title_model: {
              state: "ready",
              runtime_installed: true,
              model_downloaded: true,
            },
          },
        },
      })),
      serverEvents: vi.fn(async () => fakeStream),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
    );

    await ingress.start();

    expect(protocol.serverEvents).toHaveBeenCalledWith(
      expect.objectContaining({ after: 8 }),
    );
    expect(store.sessionPullRequests("se_1")).toEqual([
      expect.objectContaining({ number: 141, head: "different-branch" }),
    ]);
    expect(readSignal(store.githubPullRequests)).toEqual([
      expect.objectContaining({ cursor: 9, refreshedAt: "2026-08-01T12:02:09Z" }),
    ]);
    expect(readSignal(store.gitWorktreeSettings)).toMatchObject({ cursor: 10 });
    ingress.stop();
  });

  it("bootstraps early protocol servers through a cursor-fenced metadata fallback", async () => {
    const store = new AppStore();
    const requestOrder: string[] = [];
    const onSessionSummaries = vi.fn();
    let eventOptions: ServerEventOptions | undefined;
    const fakeStream = stream();
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessionSummaries: vi.fn(async () => {
        throw new ProtocolClientError(
          "request-failed",
          "session summary request failed",
          404,
        );
      }),
      gitWorktreeSettingsSnapshot: vi.fn(async () => {
        requestOrder.push("fence");
        return {
          cursor: 12,
          value: {
            title_model_load_behavior: "auto",
            title_model_resource_policy: "adaptive",
            title_model: {
              state: "ready",
              runtime_installed: true,
              model_downloaded: true,
            },
          },
        };
      }),
      sessions: vi.fn(async () => {
        requestOrder.push("sessions");
        return [{ ...session("Legacy metadata"), active: true }];
      }),
      workspaces: vi.fn(async () => [workspace]),
      serverEvents: vi.fn(async (options: ServerEventOptions) => {
        eventOptions = options;
        return fakeStream;
      }),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
      { onSessionSummaries },
    );

    await ingress.start();

    expect(requestOrder).toEqual(["fence", "sessions"]);
    expect(readSignal(store.sessions)[0]).toMatchObject({
      id: "se_1",
      state: "running",
      active: true,
    });
    expect(onSessionSummaries).toHaveBeenCalledWith(
      [expect.objectContaining({ session_id: "se_1", active: true })],
      12,
    );
    expect(protocol.serverEvents).toHaveBeenCalledWith(
      expect.objectContaining({ after: 0 }),
    );

    eventOptions?.onEvent(githubSnapshotEvent(10));
    eventOptions?.onEvent(activityEvent(11, false));
    expect(readSignal(store.sessions)[0]?.active).toBe(true);
    expect(readSignal(store.githubPullRequests)).toEqual([]);

    eventOptions?.onEvent(activityEvent(13, false));
    expect(readSignal(store.sessions)[0]).toMatchObject({ state: "idle", active: false });
    expect(readSignal(store.githubPullRequests)).toEqual([
      expect.objectContaining({
        cursor: 10,
        pullRequests: expect.objectContaining({ host: "github.com" }),
      }),
    ]);
    ingress.stop();
  });

  it("hydrates durable server snapshots without replaying historical side effects", async () => {
    const store = new AppStore();
    const onKnownEvent = vi.fn();
    const sessions = vi.fn(async () => [session("Current")]);
    let eventOptions: ServerEventOptions | undefined;
    const fakeStream = stream();
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessions,
      sessionSummaries: vi.fn(async () => ({ summaries: [summary], cursor: 10 })),
      workspaces: vi.fn(async () => [workspace]),
      serverEvents: vi.fn(async (options: ServerEventOptions) => {
        eventOptions = options;
        return fakeStream;
      }),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
      { onKnownEvent },
    );
    await ingress.start();

    eventOptions?.onEvent(lifecycleEvent(8));
    eventOptions?.onEvent(githubSnapshotEvent(9));
    eventOptions?.onEvent(gitWorktreeSettingsEvent(10, "ready"));

    expect(readSignal(store.githubPullRequests)).toEqual([]);
    expect(readSignal(store.gitWorktreeSettings)).toBeUndefined();
    expect(onKnownEvent).not.toHaveBeenCalled();

    eventOptions?.onEvent(lifecycleEvent(11));
    expect(readSignal(store.githubPullRequests)).toEqual([
      expect.objectContaining({
        cursor: 9,
        pullRequests: expect.objectContaining({ host: "github.com" }),
      }),
    ]);
    expect(readSignal(store.gitWorktreeSettings)).toMatchObject({
      cursor: 10,
      settings: { title_model: { state: "ready" } },
    });
    expect(onKnownEvent).toHaveBeenCalledWith(
      expect.objectContaining({ cursor: 11, type: "session.updated" }),
    );
    await vi.waitFor(() => expect(sessions).toHaveBeenCalledTimes(2));
    ingress.stop();
  });

  it("runs a trailing metadata refresh when lifecycle events coalesce", async () => {
    const store = new AppStore();
    const firstRefresh = deferred<Sessions>();
    const trailingRefresh = deferred<Sessions>();
    let sessionRequest = 0;
    const sessions = vi.fn(() => {
      sessionRequest += 1;
      if (sessionRequest === 1) return Promise.resolve([session("Initial")]);
      if (sessionRequest === 2) return firstRefresh.promise;
      return trailingRefresh.promise;
    });
    let eventOptions: ServerEventOptions | undefined;
    const fakeStream = stream();
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessions,
      sessionSummaries: vi.fn(async () => ({ summaries: [summary], cursor: 10 })),
      workspaces: vi.fn(async () => [workspace]),
      serverEvents: vi.fn(async (options: ServerEventOptions) => {
        eventOptions = options;
        return fakeStream;
      }),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
    );
    await ingress.start();

    eventOptions?.onEvent(lifecycleEvent(11));
    await vi.waitFor(() => expect(sessions).toHaveBeenCalledTimes(2));

    eventOptions?.onEvent(lifecycleEvent(12));
    firstRefresh.resolve([session("Stale")]);
    await vi.waitFor(() => expect(sessions).toHaveBeenCalledTimes(3));
    expect(store.session("se_1")?.title).toBe("Initial");

    trailingRefresh.resolve([session("Latest")]);
    await vi.waitFor(() => expect(store.session("se_1")?.title).toBe("Latest"));
    expect(sessions).toHaveBeenCalledTimes(3);
    ingress.stop();
  });

  it("ignores projection and metadata responses from a stopped generation", async () => {
    const store = new AppStore();
    const staleMetadata = deferred<Sessions>();
    const staleProjection = deferred<Snapshot>();
    let sessionRequest = 0;
    const sessions = vi.fn(() => {
      sessionRequest += 1;
      if (sessionRequest === 1) return Promise.resolve([session("First run")]);
      if (sessionRequest === 2) return staleMetadata.promise;
      return Promise.resolve([session("Second run")]);
    });
    let snapshotRequest = 0;
    const sessionSummaries = vi.fn(() => {
      snapshotRequest += 1;
      if (snapshotRequest === 2) return staleProjection.promise;
      return Promise.resolve({
        summaries: [summary],
        cursor: snapshotRequest === 1 ? 10 : 20,
      });
    });
    const eventOptions: ServerEventOptions[] = [];
    const streams: CursorEventStream<ProtocolIngressEvent>[] = [];
    const protocol = {
      serverInfo: vi.fn(async () => info),
      sessions,
      sessionSummaries,
      workspaces: vi.fn(async () => [workspace]),
      serverEvents: vi.fn(async (options: ServerEventOptions) => {
        eventOptions.push(options);
        const nextStream = stream();
        streams.push(nextStream);
        return nextStream;
      }),
    };
    const ingress = new ProtocolIngress(
      protocol as unknown as ProtocolClient,
      store,
    );
    await ingress.start();

    eventOptions[0]?.onEvent(lifecycleEvent(11));
    await vi.waitFor(() => expect(sessions).toHaveBeenCalledTimes(2));
    const oldProjection = ingress.refreshProjection();
    await vi.waitFor(() => expect(sessionSummaries).toHaveBeenCalledTimes(2));

    ingress.stop();
    await ingress.start();
    expect(store.session("se_1")?.title).toBe("Second run");

    staleMetadata.resolve([session("Stale metadata")]);
    staleProjection.resolve({ summaries: [summary], cursor: 12 });
    await oldProjection;
    await Promise.resolve();

    expect(store.session("se_1")?.title).toBe("Second run");
    expect(protocol.serverEvents).toHaveBeenCalledTimes(2);
    expect(streams[0]?.close).toHaveBeenCalledOnce();
    expect(streams[1]?.start).toHaveBeenCalledOnce();
    ingress.stop();
  });

  it.each(["online", "visible"] as const)(
    "retries a failed bootstrap when the app becomes %s",
    async (trigger) => {
      const store = new AppStore();
      const sources = retrySources();
      const retryInfo = deferred<typeof info>();
      let infoRequest = 0;
      const serverInfo = vi.fn(() => {
        infoRequest += 1;
        expect(sources.visibility.addEventListener).toHaveBeenCalledOnce();
        expect(sources.online.addEventListener).toHaveBeenCalledOnce();
        return infoRequest === 1
          ? Promise.reject(new Error("offline"))
          : retryInfo.promise;
      });
      const fakeStream = stream();
      const protocol = {
        serverInfo,
        sessions: vi.fn(async () => [session("Recovered")]),
        sessionSummaries: vi.fn(async () => ({ summaries: [summary], cursor: 8 })),
        workspaces: vi.fn(async () => [workspace]),
        serverEvents: vi.fn(async () => fakeStream),
      };
      const ingress = new ProtocolIngress(
        protocol as unknown as ProtocolClient,
        store,
        { online: sources.online, visibility: sources.visibility },
      );

      await expect(ingress.start()).rejects.toThrow("offline");
      if (trigger === "online") sources.emitOnline();
      else sources.emitVisible();

      const retry = ingress.start();
      expect(ingress.start()).toBe(retry);
      expect(serverInfo).toHaveBeenCalledTimes(2);
      retryInfo.resolve(info);
      await retry;

      expect(store.session("se_1")?.title).toBe("Recovered");
      expect(fakeStream.start).toHaveBeenCalledOnce();
      expect(sources.visibility.addEventListener).toHaveBeenCalledOnce();
      expect(sources.online.addEventListener).toHaveBeenCalledOnce();
      ingress.stop();
      expect(sources.visibility.removeEventListener).toHaveBeenCalledOnce();
      expect(sources.online.removeEventListener).toHaveBeenCalledOnce();
    },
  );
});
