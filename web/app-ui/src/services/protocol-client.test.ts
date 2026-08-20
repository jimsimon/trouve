import { describe, expect, it, vi } from "vitest";

import {
  assertProtocolCompatibility,
  loadProtocolEventParser,
  ProtocolClient,
  ProtocolClientError,
} from "./protocol-client.js";

const session = {
  id: "se_1",
  workspace_id: "ws_1",
  title: "Protocol ingress",
  branch: "trouve/protocol-ingress",
  worktree_path: "/tmp/protocol-ingress",
  base_ref: "main",
  created_at: "2026-08-01T12:00:00Z",
};

const summary = {
  session_id: "se_1",
  workspace_id: "ws_1",
  archived: false,
  active: true,
  attention: "none",
  outcome: "running",
  latest_thread_id: "th_1",
  latest_cursor: 7,
  updated_at: "2026-08-01T12:01:00Z",
} as const;

describe("ProtocolClient", () => {
  it("uses generated endpoints and runtime-validates session bootstrap responses", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      return request.url.endsWith("/v1/sessions")
        ? Response.json([session])
        : Response.json({ summaries: [summary], cursor: 8 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });

    await expect(client.sessions()).resolves.toEqual([session]);
    await expect(client.sessionSummaries()).resolves.toEqual({
      summaries: [summary],
      cursor: 8,
    });
  });

  it("loads the cursor-bearing server projection used for cold startup", async () => {
    const requests: Request[] = [];
    const projection = {
      github_pull_requests: [],
      session_pull_requests: [],
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
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async (input, init) => {
        requests.push(input instanceof Request ? input : new Request(input, init));
        return Response.json(projection, {
          headers: { "x-trouve-event-cursor": "42" },
        });
      }),
    });

    await expect(client.serverProjectionSnapshot()).resolves.toEqual({
      cursor: 42,
      value: projection,
    });
    expect(new URL(requests[0]!.url).pathname).toBe("/v1/server-projection");
  });

  it("rejects malformed responses without copying payload data into the error", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({ summaries: [{ prompt: "repository secret" }], cursor: "bad" }),
    );
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.sessionSummaries().catch((reason: unknown) => reason);

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect(String(error)).not.toContain("repository secret");
  });

  it("retains a missing session-summary route status for compatibility bootstrap", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({ code: "not_found", message: "not found" }, { status: 404 }),
    );
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.sessionSummaries().catch((reason: unknown) => reason);

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).status).toBe(404);
  });

  it("retains an HTTP failure status without copying the response payload", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json(
        { code: "bad_request", message: "workspace secret has no origin" },
        { status: 400 },
      ),
    );
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.createSessionPr("se_1", {
      title: "Test",
      body: "",
      draft: true,
    }).catch((reason: unknown) => reason);

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).status).toBe(400);
    expect(String(error)).not.toContain("workspace secret");
  });

  it("loads lightweight diff metadata separately from one encoded file patch", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return new URL(request.url).pathname.endsWith("/summary")
        ? Response.json({
            files: [{
              path: "docs/setup guide.md",
              additions: 2,
              deletions: 1,
              binary: false,
            }],
            additions: 2,
            deletions: 1,
          })
        : Response.json({
            path: "docs/setup guide.md",
            diff: "diff --git a/docs/setup guide.md b/docs/setup guide.md\n",
          });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });

    await expect(client.sessionDiffSummary("se_1")).resolves.toMatchObject({
      additions: 2,
      deletions: 1,
    });
    await expect(
      client.sessionFileDiff("se_1", "docs/setup guide.md"),
    ).resolves.toMatchObject({ path: "docs/setup guide.md" });

    expect(new URL(requests[0]!.url).pathname).toBe(
      "/v1/sessions/se_1/diff/summary",
    );
    const fileUrl = new URL(requests[1]!.url);
    expect(fileUrl.pathname).toBe("/v1/sessions/se_1/diff/file");
    expect(fileUrl.searchParams.get("path")).toBe("docs/setup guide.md");
  });

  it("reports an oversized selected-file diff without exposing its response", async () => {
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async () => Response.json(
        { code: "payload_too_large", message: "secret path is too large" },
        { status: 413 },
      )),
    });
    const error = await client.sessionFileDiff("se_1", "secret.txt").catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).status).toBe(413);
    expect(String(error)).toContain("too large to preview");
    expect(String(error)).not.toContain("secret path");
  });

  it("rejects malformed diff metadata without exposing its payload", async () => {
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async () => Response.json({
        files: [{ path: "repository secret", additions: "many" }],
        additions: 1,
        deletions: 0,
      })),
    });
    const error = await client.sessionDiffSummary("se_1").catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).kind).toBe("invalid-response");
    expect(String(error)).not.toContain("repository secret");
  });

  it("rejects malformed lazy pull-request detail without exposing its payload", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () => Response.json({
      id: "PR_secret",
      body: "repository secret",
      merge_queue: { enabled: "yes" },
    }));
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.sessionPrDetail("se_1", 42).catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).kind).toBe("invalid-response");
    expect(String(error)).not.toContain("repository secret");
  });

  it("preserves a bounded GitHub re-authentication error", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () => Response.json({
      code: "github_reauthentication_required",
      message: "Re-authenticate GitHub under Settings → Integrations.",
    }, { status: 401 }));
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.sessionPrDetail("se_1", 42).catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).status).toBe(401);
    expect((error as ProtocolClientError).code).toBe("github_reauthentication_required");
    expect(String(error)).toContain("Re-authenticate GitHub");
  });

  it("rejects malformed lazy pull-request file content without exposing it", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () => Response.json({
      path: "secret.txt",
      change_type: "modified",
      original: { secret: "do not echo" },
    }));
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.sessionPrFileDiff("se_1", 42, "secret.txt").catch(
      (reason: unknown) => reason,
    );

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect((error as ProtocolClientError).kind).toBe("invalid-response");
    expect(String(error)).not.toContain("do not echo");
  });

  it("loads generated workspace/thread models and validates both collections", async () => {
    const workspace = { id: "ws_1", name: "trouve", path: "/src/trouve" };
    const thread = {
      id: "th_1",
      session_id: "se_1",
      model: "openai/gpt-5.6",
      mode: "code",
      permission_mode: "ask",
      created_at: "2026-08-01T12:00:00Z",
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      return request.url.includes("/v1/threads?")
        ? Response.json([thread])
        : Response.json([workspace]);
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });

    await expect(client.workspaces()).resolves.toEqual([workspace]);
    await expect(client.threads("se_1")).resolves.toEqual([thread]);
    const threadsInput = fakeFetch.mock.calls[1]?.[0];
    expect(threadsInput instanceof Request ? threadsInput.url : String(threadsInput)).toContain(
      "session_id=se_1",
    );
  });

  it("loads a bounded folded thread view with its exact stream cursor", async () => {
    const requests: Request[] = [];
    const snapshot = {
      item_offset: 256,
      total_items: 512,
      has_older: true,
      items: [{
        kind: "assistant",
        turn: 7,
        content: "Already folded",
        complete: true,
      }],
      turn_models: { "7": "openai/gpt-5.6" },
      turn_started_at: { "7": "2026-08-01T12:00:00Z" },
      turn_duration_ms: { "7": 2_500 },
    };
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async (input, init) => {
        requests.push(input instanceof Request ? input : new Request(input, init));
        return Response.json(snapshot, {
          headers: { "x-trouve-event-cursor": "91" },
        });
      }),
    });

    await expect(client.threadView("th/folded", 512)).resolves.toEqual({
      cursor: 91,
      value: snapshot,
    });
    const url = new URL(requests[0]!.url);
    expect(url.pathname).toBe("/v1/threads/th%2Ffolded/view");
    expect(url.searchParams.get("limit")).toBe("256");
    expect(url.searchParams.get("turn_aligned")).toBe("true");
    expect(url.searchParams.get("before")).toBe("512");
  });

  it("loads full details for one deferred historical tool call", async () => {
    const requests: Request[] = [];
    const details = {
      call_id: "call / one",
      args: { path: "README.md", content: "complete arguments" },
      result: { content: "complete result" },
    };
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async (input, init) => {
        requests.push(input instanceof Request ? input : new Request(input, init));
        return Response.json(details);
      }),
    });

    await expect(
      client.threadToolDetails("th / one", "call / one"),
    ).resolves.toEqual(details);
    expect(new URL(requests[0]!.url).pathname).toBe(
      "/v1/threads/th%20%2F%20one/tools/call%20%2F%20one",
    );
  });

  it("loads encoded session mention paths and rejects malformed path lists", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return requests.length === 1
        ? Response.json(["src/main.ts", "docs/"])
        : Response.json(["src/main.ts", { prompt: "repository secret" }]);
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });

    await expect(client.sessionPaths("session / one")).resolves.toEqual([
      "src/main.ts",
      "docs/",
    ]);
    expect(requests[0]?.url).toContain("/v1/sessions/session%20%2F%20one/paths");

    const error = await client.sessionPaths("session / one").catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(ProtocolClientError);
    expect(String(error)).not.toContain("repository secret");
  });

  it("loads workspace modes/models and validates a confirmed thread update", async () => {
    const mode = {
      id: "code",
      display_name: "Engineer",
      system_prompt: "Implement the requested change.",
    };
    const model = {
      id: "openai/gpt-5.6",
      display_name: "GPT-5.6",
      context_window: 200_000,
      supports_tools: true,
      options_schema: {},
    };
    const thread = {
      id: "th_1",
      session_id: "se_1",
      model: model.id,
      mode: mode.id,
      permission_mode: "allow_list",
      created_at: "2026-08-01T12:00:00Z",
    };
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.includes("/v1/personas")) return Response.json([mode]);
      if (request.url.endsWith("/v1/models/refresh")) return Response.json([model]);
      if (request.url.endsWith("/v1/models")) return Response.json([model]);
      return Response.json(thread);
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.personas("ws_1")).resolves.toEqual([mode]);
    await expect(client.models()).resolves.toEqual([model]);
    await expect(client.refreshModels()).resolves.toEqual([model]);
    await expect(
      client.updateThread("th_1", { permission_mode: "allow_list" }),
    ).resolves.toEqual(thread);

    expect(requests[0]?.url).toContain("workspace_id=ws_1");
    expect(requests[3]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
  });

  it("attaches the ephemeral desktop CSRF header only through mutation calls", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return Response.json(
        { thread_id: "th/slash", turn: 2, queued: false },
        { status: 202 },
      );
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(
      client.sendMessage("th/slash", { content: "ship it" }),
    ).resolves.toMatchObject({ turn: 2 });
    expect(requests[0]?.url).toContain("/v1/threads/th%2Fslash/messages");
    expect(requests[0]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
  });

  it("steers the exact encoded thread with text and attachments", async () => {
    const requests: Request[] = [];
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async (input, init) => {
        requests.push(input instanceof Request ? input : new Request(input, init));
        return Response.json({ thread_id: "th/slash", turn: 4 }, { status: 202 });
      }),
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.steerTurn("th/slash", {
      content: "Focus on the regression.",
      attachments: [{ name: "view.png", mime: "image/png", data: "AA==" }],
    })).resolves.toBeUndefined();
    expect(requests).toHaveLength(1);
    expect(requests[0]?.method).toBe("POST");
    expect(requests[0]?.url).toContain("/v1/threads/th%2Fslash/steer");
    expect(requests[0]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
    await expect(requests[0]?.clone().json()).resolves.toEqual({
      content: "Focus on the regression.",
      attachments: [{ name: "view.png", mime: "image/png", data: "AA==" }],
    });
  });

  it("validates session/thread management responses and protects every mutation", async () => {
    const requests: Request[] = [];
    const thread = {
      id: "th_1",
      session_id: "se_1",
      model: "openai/gpt-5.6",
      mode: "code",
      permission_mode: "ask",
      created_at: "2026-08-01T12:00:00Z",
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.method === "DELETE") return new Response(null, { status: 204 });
      if (request.url.endsWith("/v1/threads")) return Response.json(thread);
      if (request.method === "PATCH") {
        return Response.json({ ...session, title: "Renamed", archived: true });
      }
      return Response.json(session);
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(
      client.createSession({ workspace_id: "ws_1", title: "Protocol ingress" }),
    ).resolves.toEqual(session);
    await expect(
      client.updateSession("se_1", { title: "Renamed", archived: true }),
    ).resolves.toMatchObject({ title: "Renamed", archived: true });
    await expect(client.createThread({ session_id: "se_1" })).resolves.toEqual(thread);
    await expect(client.deleteSession("se_1")).resolves.toBeUndefined();

    expect(requests.map((request) => request.method)).toEqual([
      "POST",
      "PATCH",
      "POST",
      "DELETE",
    ]);
    expect(
      requests.every(
        (request) => request.headers.get("x-trouve-host-csrf") === "ephemeral-token",
      ),
    ).toBe(true);
    await expect(requests[0]?.json()).resolves.toMatchObject({
      workspace_id: "ws_1",
      title: "Protocol ingress",
    });
  });

  it("restores encoded session checkpoints through CSRF-protected mutations", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.restoreSessionCheckpoint("se/slash", "undo")).resolves.toBeUndefined();
    await expect(client.restoreSessionCheckpoint("se/slash", "redo")).resolves.toBeUndefined();

    expect(requests.map((request) => request.method)).toEqual(["POST", "POST"]);
    expect(requests.map((request) => new URL(request.url).pathname)).toEqual([
      "/v1/sessions/se%2Fslash/undo",
      "/v1/sessions/se%2Fslash/redo",
    ]);
    expect(requests.every(
      (request) => request.headers.get("x-trouve-host-csrf") === "ephemeral-token",
    )).toBe(true);
    expect(requests.every((request) => request.headers.get("content-type") === null)).toBe(true);
  });

  it("restores and forks exact checkpoints through protected generated routes", async () => {
    const requests: Request[] = [];
    const thread = {
      id: "th_fork",
      session_id: session.id,
      model: "openai/gpt-5.6",
      mode: "code",
      permission_mode: "ask",
      created_at: "2026-08-01T12:00:00Z",
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return request.url.endsWith("/restore")
        ? new Response(null, { status: 204 })
        : Response.json({ session, thread });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.restoreCheckpoint("cp/slash")).resolves.toBeUndefined();
    await expect(client.forkCheckpoint("cp/slash")).resolves.toEqual({ session, thread });

    expect(requests.map((request) => new URL(request.url).pathname)).toEqual([
      "/v1/checkpoints/cp%2Fslash/restore",
      "/v1/checkpoints/cp%2Fslash/fork",
    ]);
    expect(requests.every(
      (request) => request.headers.get("x-trouve-host-csrf") === "ephemeral-token",
    )).toBe(true);
  });

  it("generates a validated session title through a protected protocol mutation", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      return Response.json({ title: "Preserve frontend parity", source: "heuristic" });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    const abort = new AbortController();
    await expect(client.generateSessionTitle("Preserve the existing frontend", {
      signal: abort.signal,
    }))
      .resolves.toEqual({ title: "Preserve frontend parity", source: "heuristic" });
    expect(requests[0]?.method).toBe("POST");
    expect(requests[0]?.url).toContain("/v1/session-title");
    expect(requests[0]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
    expect(requests[0]?.signal.aborted).toBe(false);
    abort.abort();
    expect(requests[0]?.signal.aborted).toBe(true);
    await expect(requests[0]?.json()).resolves.toEqual({
      prompt: "Preserve the existing frontend",
    });
  });

  it("pairs Git and worktree settings with a validated server cursor", async () => {
    const settings = {
      derive_branch_name_from_session_title: false,
      title_model_load_behavior: "auto",
      title_model_resource_policy: "adaptive",
      title_model: {
        state: "ready",
        runtime_installed: true,
        model_downloaded: true,
      },
    };
    const withCursor = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async () => Response.json(settings, {
        headers: { "x-trouve-event-cursor": "42" },
      })),
    });

    await expect(withCursor.gitWorktreeSettingsSnapshot()).resolves.toEqual({
      cursor: 42,
      value: settings,
    });

    const withoutCursor = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: vi.fn<typeof fetch>(async () => Response.json(settings)),
    });
    const error = await withoutCursor.gitWorktreeSettingsSnapshot()
      .catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(ProtocolClientError);
    expect(error).toMatchObject({ kind: "invalid-response" });
  });

  it("manages a durable prompt queue through generated endpoints", async () => {
    const queued = {
      id: "qp_1",
      thread_id: "th_1",
      content: "Run the validation suite",
      created_at: "2026-08-01T12:03:00Z",
      position: 0,
    };
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.endsWith("/dispatch")) {
        return Response.json(
          { thread_id: "th_1", turn: 3, queued: false },
          { status: 202 },
        );
      }
      if (request.method === "GET") return Response.json([queued]);
      if (request.method === "PUT") return Response.json([queued]);
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.updateQueuedPrompt("qp_1", {
      content: "Run all tests",
      retained_attachment_ids: ["at_existing"],
      attachments: [{ name: "new.txt", mime: "text/plain", data: "bmV3" }],
    })).resolves.toBeUndefined();
    await expect(client.listQueue("th_1")).resolves.toEqual([queued]);
    await expect(client.reorderQueue("th_1", ["qp_1"])).resolves.toEqual([queued]);
    await expect(client.dispatchQueue("th_1")).resolves.toMatchObject({ turn: 3 });
    await expect(client.dispatchQueuedPrompt("qp_1")).resolves.toMatchObject({ turn: 3 });
    await expect(client.deleteQueuedPrompt("qp_1")).resolves.toBeUndefined();

    expect(requests.map((request) => request.method)).toEqual([
      "PATCH",
      "GET",
      "PUT",
      "POST",
      "POST",
      "DELETE",
    ]);
    expect(requests[0]?.url).toContain("/v1/queue/qp_1");
    await expect(requests[0]?.json()).resolves.toEqual({
      content: "Run all tests",
      retained_attachment_ids: ["at_existing"],
      attachments: [{ name: "new.txt", mime: "text/plain", data: "bmV3" }],
    });
    expect(requests[1]?.url).toContain("/v1/threads/th_1/queue");
    expect(requests[2]?.url).toContain("/v1/threads/th_1/queue");
    expect(requests[4]?.url).toContain("/v1/queue/qp_1/dispatch");
    expect(
      requests.filter((request) => request.method !== "GET").every(
        (request) => request.headers.get("x-trouve-host-csrf") === "ephemeral-token",
      ),
    ).toBe(true);
  });

  it("validates management responses and protects encoded management mutations", async () => {
    const requests: Request[] = [];
    const provider = {
      id: "open/router",
      kind: "openai-compat",
      auth: "api-key",
      has_credentials: true,
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.endsWith("/v1/providers")) {
        return Response.json({ providers: [provider], default_model: "open/router/model" });
      }
      if (request.url.includes("/v1/mcp-servers?")) return Response.json([]);
      if (request.method === "PUT" && request.url.includes("/v1/providers/")) {
        return Response.json(provider);
      }
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.providers()).resolves.toMatchObject({ providers: [provider] });
    await expect(
      client.upsertProvider("open/router", {
        kind: "openai-compat",
        api_key: "write-only",
      }),
    ).resolves.toEqual(provider);
    await expect(client.mcpServers("workspace / one", false)).resolves.toEqual([]);
    await expect(client.setMcpServerEnabled("docs/server", {
      scope: "workspace",
      workspace_id: "workspace / one",
      enabled: false,
    })).resolves.toBeUndefined();
    await expect(
      client.deleteMcpServer("docs/server", "workspace", "workspace / one"),
    ).resolves.toBeUndefined();

    expect(requests[1]?.url).toContain("/v1/providers/open%2Frouter");
    expect(requests[1]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
    expect(requests[2]?.url).toContain("workspace_id=workspace+%2F+one");
    expect(requests[2]?.url).toContain("probe=false");
    expect(requests[3]?.url).toContain("/v1/mcp-servers/docs%2Fserver/enabled");
    expect(requests[3]?.method).toBe("PUT");
    expect(requests[3]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
    expect(requests[4]?.url).toContain("/v1/mcp-servers/docs%2Fserver?");
    expect(requests[4]?.headers.get("x-trouve-host-csrf")).toBe("ephemeral-token");
  });

  it("redacts malformed management payloads from diagnostics", async () => {
    const fakeFetch = vi.fn<typeof fetch>(async () =>
      Response.json({ providers: [{ api_key: "repository secret" }] }),
    );
    const client = new ProtocolClient("http://127.0.0.1:43127", { fetch: fakeFetch });
    const error = await client.providers().catch((reason: unknown) => reason);

    expect(error).toBeInstanceOf(ProtocolClientError);
    expect(String(error)).not.toContain("repository secret");
  });

  it("loads the effective MCP configuration for an encoded session", async () => {
    const requests: Request[] = [];
    const server = {
      name: "docs",
      scope: "branch",
      command: "docs-mcp",
      args: ["--stdio"],
      health: "ok",
      detail: "5 tools",
    };
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: async (input, init) => {
        const request = input instanceof Request ? input : new Request(input, init);
        requests.push(request);
        return Response.json([server]);
      },
    });

    await expect(client.sessionMcpServers("session / one")).resolves.toEqual([server]);
    expect(requests[0]?.url).toContain("/v1/sessions/session%20%2F%20one/mcp-servers");
  });

  it("loads validated aggregate usage for encoded sessions and threads", async () => {
    const requests: Request[] = [];
    const usage = {
      turns: 3,
      input_tokens: 12_500,
      output_tokens: 725,
      cached_input_tokens: 8_000,
      cost_usd: 0.0312,
    };
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: async (input, init) => {
        const request = input instanceof Request ? input : new Request(input, init);
        requests.push(request);
        return Response.json(usage);
      },
    });

    await expect(client.sessionUsage("session / one")).resolves.toEqual(usage);
    await expect(client.threadUsage("thread / one")).resolves.toEqual(usage);
    expect(requests[0]?.url).toContain("/v1/sessions/session%20%2F%20one/usage");
    expect(requests[1]?.url).toContain("/v1/threads/thread%20%2F%20one/usage");
  });

  it("validates workspace and pull-request workflows with encoded mutation paths", async () => {
    const requests: Request[] = [];
    const workspace = { id: "workspace / one", name: "trouve", path: "/src/trouve" };
    const pr = {
      number: 42,
      title: "Ship the web frontend",
      url: "https://github.com/trouve-ai/trouve/pull/42",
      state: "open",
      draft: false,
      head: "trouve/web-frontend",
      base: "main",
      checks: [],
      reviews: [],
    };
    const prDetail = {
      info: pr,
      id: "PR_kwDO_selected",
      viewer: "octocat",
      created_at: "2026-08-01T12:00:00Z",
      updated_at: "2026-08-01T12:01:00Z",
      additions: 12,
      deletions: 3,
      changed_files: 2,
      commit_count: 1,
      capabilities: { can_update: true },
      merge_queue: { enabled: true },
    };
    const prFileDiff = {
      path: "src/a file.ts",
      change_type: "modified",
      original: "const value = 1;\n",
      modified: "const value = 2;\n",
      original_bytes: 17,
      modified_bytes: 17,
      binary: false,
      truncated: false,
      notice: "",
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.endsWith("/branches")) {
        return Response.json({ branches: ["main", "release"], head: "main" });
      }
      if (new URL(request.url).pathname.endsWith("/prs/42/file")) {
        return Response.json(prFileDiff);
      }
      if (request.url.endsWith("/prs/42/actions")) return Response.json(prDetail);
      if (new URL(request.url).pathname.endsWith("/prs/42")) {
        return Response.json(prDetail);
      }
      if (request.url.endsWith("/prs")) return Response.json([pr]);
      if (request.url.endsWith("/pr") && request.method === "POST") {
        return Response.json(pr);
      }
      if (request.url.endsWith("/v1/workspaces") && request.method === "POST") {
        return Response.json(workspace);
      }
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(
      client.registerWorkspace({ path: "/src/trouve", name: "trouve" }),
    ).resolves.toEqual(workspace);
    await expect(client.workspaceBranches("workspace / one")).resolves.toEqual({
      branches: ["main", "release"],
      head: "main",
    });
    await expect(
      client.createSessionPr("session / one", {
        title: pr.title,
        body: "Ready for review.",
        draft: false,
      }),
    ).resolves.toEqual(pr);
    await expect(
      client.sessionPrDetail("session / one", 42, "conversation"),
    ).resolves.toEqual(prDetail);
    await expect(
      client.sessionPrFileDiff("session / one", 42, "src/a file.ts"),
    ).resolves.toEqual(prFileDiff);
    await expect(
      client.actOnSessionPr("session / one", 42, {
        action: "request_reviewers",
        users: ["octocat"],
        teams: [],
        replace: false,
      }),
    ).resolves.toEqual(prDetail);
    await expect(client.closeWorkspace("workspace / one")).resolves.toBeUndefined();

    expect(requests[1]?.url).toContain("/workspaces/workspace%20%2F%20one/branches");
    expect(requests[2]?.url).toContain("/sessions/session%20%2F%20one/pr");
    expect(requests[3]?.url).toContain("/sessions/session%20%2F%20one/prs/42");
    expect(new URL(requests[3]!.url).searchParams.get("section")).toBe("conversation");
    expect(requests[4]?.url).toContain("/sessions/session%20%2F%20one/prs/42/file");
    expect(new URL(requests[4]!.url).searchParams.get("path")).toBe("src/a file.ts");
    expect(requests[5]?.url).toContain("/sessions/session%20%2F%20one/prs/42/actions");
    expect(requests[6]?.url).toContain("/workspaces/workspace%20%2F%20one");
    expect(
      [requests[0], requests[2], requests[5], requests[6]].every(
        (request) => request?.headers.get("x-trouve-host-csrf") === "ephemeral-token",
      ),
    ).toBe(true);
    await expect(requests[5]?.json()).resolves.toEqual({
      action: "request_reviewers",
      users: ["octocat"],
      teams: [],
      replace: false,
    });
  });

  it("manages encoded vendor CLI installs without exposing server payloads", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.endsWith("/v1/clis")) {
        return Response.json({
          clis: [{
            id: "vendor/cli",
            display_name: "Vendor CLI",
            kinds: ["vendor-cli"],
            source: "none",
            update_available: false,
          }],
        });
      }
      if (request.method === "GET") {
        return Response.json({
          status: "pending",
          received_bytes: 10,
          total_bytes: 20,
        });
      }
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.clis()).resolves.toMatchObject({ clis: [{ id: "vendor/cli" }] });
    await expect(client.cliInstallStatus("vendor/cli")).resolves.toMatchObject({
      status: "pending",
    });
    await expect(client.startCliInstall("vendor/cli")).resolves.toBeUndefined();
    await expect(client.cancelCliInstall("vendor/cli")).resolves.toBeUndefined();
    await expect(client.uninstallCli("vendor/cli")).resolves.toBeUndefined();

    expect(requests.slice(1).every((request) =>
      request.url.includes("vendor%2Fcli")
    )).toBe(true);
    expect(requests.slice(2).every((request) =>
      request.headers.get("x-trouve-host-csrf") === "ephemeral-token"
    )).toBe(true);
  });

  it("runtime-validates code-review administration and keeps secret requests write-only", async () => {
    const requests: Request[] = [];
    const reviewer = {
      id: "security/reviewer",
      name: "Security",
      prompt: "Review trust boundaries.",
      built_in: false,
    };
    const repository = {
      repository: "trouve-ai/trouve",
      installation_id: 42,
      mode: "automatic",
      reviewer_ids: [reviewer.id],
    };
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init);
      requests.push(request);
      if (request.url.endsWith("/github-app")) {
        return Response.json({ configured: true, app_id: 1234 });
      }
      if (request.url.endsWith("/reviewer")) return Response.json(reviewer);
      if (request.url.endsWith("/repository")) return Response.json(repository);
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await expect(client.configureCodeReviewGithubApp({
      app_id: 1234,
      private_key_pem: "write-only-private-key",
      webhook_secret: "write-only-webhook-secret",
    })).resolves.toEqual({ configured: true, app_id: 1234 });
    await expect(client.updateCodeReviewRepository({
      repository: repository.repository,
      installation_id: repository.installation_id,
      mode: "automatic",
      reviewer_ids: [reviewer.id],
    })).resolves.toEqual(repository);

    expect(requests.every((request) =>
      request.headers.get("x-trouve-host-csrf") === "ephemeral-token"
    )).toBe(true);
  });

  it("sends global defaults in one replacement request", async () => {
    const requests: Request[] = [];
    const fakeFetch = vi.fn<typeof fetch>(async (input, init) => {
      requests.push(input instanceof Request ? input : new Request(input, init));
      return new Response(null, { status: 204 });
    });
    const client = new ProtocolClient("http://127.0.0.1:43127", {
      fetch: fakeFetch,
      mutationHeaders: () => ({ "x-trouve-host-csrf": "ephemeral-token" }),
    });

    await client.setGlobalDefaults({
      model: "openai/gpt-5.6",
      default_thinking_level: null,
      permission_mode: "allow_list",
    });

    expect(requests).toHaveLength(1);
    const request = requests[0];
    if (request === undefined) throw new Error("global defaults request was not sent");
    expect(request.url).toBe("http://127.0.0.1:43127/v1/config/defaults");
    expect(request.method).toBe("PUT");
    await expect(request.clone().json()).resolves.toEqual({
      model: "openai/gpt-5.6",
      default_thinking_level: null,
      permission_mode: "allow_list",
    });
  });
});

describe("protocol compatibility", () => {
  it("accepts the exact generated protocol version", () => {
    expect(() => assertProtocolCompatibility("7.6")).not.toThrow();
  });

  it("rejects older, newer, other-major, and malformed servers", () => {
    for (const version of ["4.0", "5.2", "6.1", "7.0", "7.1", "7.2", "7.3", "7.4", "7.5", "7.7", "unknown", ""]) {
      expect(() => assertProtocolCompatibility(version)).toThrowError(
        expect.objectContaining({ kind: "incompatible-protocol" }),
      );
    }
  });
});

describe("protocol event parser", () => {
  it("accepts canonical known events and forward-compatible unknown types", async () => {
    const parse = await loadProtocolEventParser();
    const known = parse({
      ...summary,
      cursor: 8,
      scope: "server",
      ts: "2026-08-01T12:01:00Z",
      type: "session.summary_updated",
      session_id: "se_1",
      summary,
    });
    const notification = parse({
      cursor: 9,
      scope: "server",
      ts: "2026-08-01T12:01:30Z",
      type: "session.notification",
      session_id: "se_1",
      thread_id: "th_1",
      kind: "turn_failed",
      detail: "provider unavailable",
    });
    const unknown = parse({
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:02:00Z",
      type: "future.safe_event",
      future_payload: { any: "shape" },
    });

    expect(known).toMatchObject({ kind: "known", cursor: 8 });
    expect(notification).toMatchObject({
      kind: "known",
      cursor: 9,
      envelope: {
        type: "session.notification",
        kind: "turn_failed",
        detail: "provider unavailable",
      },
    });
    expect(unknown).toEqual({
      kind: "unknown",
      cursor: 10,
      scope: "server",
      ts: "2026-08-01T12:02:00Z",
      type: "future.safe_event",
    });
  });

  it("does not disguise a malformed known event as an unknown event", async () => {
    const parse = await loadProtocolEventParser();
    expect(() =>
      parse({
        cursor: 10,
        scope: "server",
        ts: "2026-08-01T12:03:00Z",
        type: "session.summary_updated",
      }),
    ).toThrow("invalid known protocol event");
  });

  it("requires future events to retain the canonical envelope", async () => {
    const parse = await loadProtocolEventParser();
    const base = {
      cursor: 11,
      scope: "server",
      ts: "2026-08-01T12:04:00Z",
      type: "future.safe_event",
    };

    for (const invalid of [
      { ...base, cursor: "11" },
      { ...base, scope: "future-scope" },
      { ...base, ts: 1234 },
      { ...base, type: "" },
    ]) {
      expect(() => parse(invalid)).toThrow("invalid protocol event envelope");
    }
  });
});
